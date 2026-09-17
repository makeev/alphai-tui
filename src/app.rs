//! App state and the event loop. Feed caching with its request-budget
//! guards lives in `feeds`, the settings overlay in `settings`; views under
//! `crate::ui` are stateless renderers over `&mut App`.

mod feeds;
mod settings;

pub use feeds::{FeedBundle, FeedKind};
pub use settings::{SettingsRow, SettingsState, settings_rows};

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Local, NaiveDate, Utc};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::widgets::TableState;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::alphai::{self, Article};
use crate::config::{self, ChartDefaults, Config, UiDefaults};
use crate::domain::{Interval, Range, Sessions, TickerData};
use crate::indicators::MaType;
use crate::keymap::{Action, Keymap};
use crate::poller::{SharedEvery, SharedParams, SharedSource, SharedSymbols, SourceEvent};
use crate::portfolio::{self, Position};
use crate::theme::Theme;
use crate::ui;

/// How long the last-price highlight stays lit after a poll changes a
/// symbol's price. The event loop redraws at least every 100ms, so the
/// pulse both appears and clears promptly.
pub const PRICE_FLASH: Duration = Duration::from_millis(1500);

/// How long every symbol has to be failing before the app looks for another
/// source. Three cycles at the default 15s poll, which is enough to tell a
/// blocked provider from one unlucky round of timeouts, and on a long poll
/// interval it simply means the next cycle.
const FALLBACK_AFTER: Duration = Duration::from_secs(45);

/// How long a footer notice stays up. Long enough to be read on a glance
/// back at the terminal, short enough not to sit on top of the key hints
/// for the rest of the session; the header keeps naming the live source
/// after it fades.
const NOTICE: Duration = Duration::from_secs(60);

/// How the price chart draws history: candlesticks or the classic close line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartStyle {
    Candles,
    Line,
}

/// News feed scope the f key cycles: the selected ticker, the whole market
/// (story-collapsed), or the 48h trending top 10. Session-only, like the
/// chart options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewsScope {
    #[default]
    Ticker,
    Market,
    Trending,
}

impl NewsScope {
    pub fn next(self) -> Self {
        match self {
            Self::Ticker => Self::Market,
            Self::Market => Self::Trending,
            Self::Trending => Self::Ticker,
        }
    }

    /// Label for block titles and head lines.
    pub fn label(self, symbol: &str) -> &str {
        match self {
            Self::Ticker => symbol,
            Self::Market => "market",
            Self::Trending => "trending",
        }
    }
}

/// State of the full-article card overlay (v in the News/Insider views).
#[derive(Default)]
pub struct ArticleOverlay {
    pub open: bool,
    pub scroll: u16,
}

/// Which line the one-line prompt is taking.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum PromptKind {
    /// A ticker for the watchlist (a).
    #[default]
    Ticker,
    /// A holding: quantity and average price, for `Prompt::target` (p).
    Position,
}

/// State of the prompt overlay (a and p anywhere). A typed symbol is not
/// validated here: whether it exists is the source's answer, and the row
/// carries that answer already, as a price or as an error.
#[derive(Default)]
pub struct Prompt {
    pub open: bool,
    pub kind: PromptKind,
    pub input: String,
    /// The ticker a position line applies to when the line names none.
    pub target: String,
    /// Set for the cases the prompt itself can rule on, such as a symbol
    /// already on the list or a quantity that is not a number.
    pub error: Option<String>,
}

/// State of the help overlay (? anywhere): the full key table.
#[derive(Default)]
pub struct HelpOverlay {
    pub open: bool,
    pub scroll: u16,
}

/// Where the News view puts the article card pane relative to the list
/// (x cycles). Session-only, like the chart options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewsLayout {
    /// List on the left, card on the right.
    #[default]
    Side,
    /// List on top, card below.
    Stacked,
}

impl NewsLayout {
    pub fn next(self) -> Self {
        match self {
            Self::Side => Self::Stacked,
            Self::Stacked => Self::Side,
        }
    }
}

/// Window of the Insider view's Form 4 chart panel, cycled by the g key:
/// hidden, trailing 3 months, trailing 12 months. Seeded from
/// `[ui] insider_chart`, then session-only, like the chart options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InsiderChartWindow {
    Off,
    #[default]
    M3,
    M12,
}

impl InsiderChartWindow {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::M3,
            Self::M3 => Self::M12,
            Self::M12 => Self::Off,
        }
    }

    /// Calendar days the window covers; whole weeks, so the weekly bars
    /// stay uniform. None = the panel is hidden.
    pub fn days(self) -> Option<u32> {
        match self {
            Self::Off => None,
            Self::M3 => Some(91),
            Self::M12 => Some(364),
        }
    }

    /// Label for the panel title and the config value.
    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::M3 => "3m",
            Self::M12 => "12m",
        }
    }
}

/// One ticker's earnings reads with the moment they were fetched. Cached far
/// longer than a feed (a read changes once a quarter), so the timestamp lives
/// beside the data rather than in a feed bundle.
pub struct EarningsSlot {
    pub data: alphai::TickerEarnings,
    pub fetched: Instant,
}

/// The last successful macro response and its requested UTC date window.
pub struct CalendarSlot {
    pub events: Vec<alphai::CalendarEvent>,
    pub fetched: Instant,
    pub from: NaiveDate,
    pub to: NaiveDate,
}

/// The default combos the t and T keys cycle through (`[chart] presets`
/// overrides them); wraps at the ends. A startup combo not in the table
/// (e.g. -r 3mo) jumps to the first preset on t and to the last on T.
pub const RANGE_PRESETS: [(Range, Interval); 5] = [
    (Range::D1, Interval::M5),
    (Range::D5, Interval::M15),
    (Range::Mo1, Interval::M60),
    (Range::Mo6, Interval::D1),
    (Range::Y1, Interval::D1),
];

fn next_preset(
    presets: &[(Range, Interval)],
    cur: (Range, Interval),
    dir: isize,
) -> (Range, Interval) {
    let n = presets.len() as isize;
    match presets.iter().position(|&p| p == cur) {
        Some(i) => presets[((i as isize + dir).rem_euclid(n)) as usize],
        None if dir > 0 => presets[0],
        None => presets[presets.len() - 1],
    }
}

pub struct AppInit {
    pub symbols: Vec<String>,
    /// Validated `[[positions]]`; empty when the section is absent.
    pub positions: Vec<Position>,
    pub shared_symbols: SharedSymbols,
    pub source: SharedSource,
    pub source_name: &'static str,
    pub range: Range,
    pub interval: Interval,
    /// Whether the chart draws the pre and post market candles too.
    pub sessions: Sessions,
    pub params: SharedParams,
    pub every: SharedEvery,
    pub rx: UnboundedReceiver<SourceEvent>,
    pub refresh: Arc<Notify>,
    pub alphai_tx: UnboundedSender<alphai::Cmd>,
    pub config: Config,
    /// Where Save writes the config back; None disables persisting.
    pub config_path: Option<PathBuf>,
    pub theme: Theme,
    pub theme_name: &'static str,
    pub chart: ChartDefaults,
    pub ui: UiDefaults,
    /// Resolved from `[keybindings]` (defaults when the section is absent).
    pub keymap: Keymap,
    pub alphai_enabled: bool,
    pub first_run: bool,
    /// Whether a source that stops answering may be swapped for one that
    /// still does (`source_fallback` in the config).
    pub source_fallback: bool,
}

pub struct App {
    pub symbols: Vec<String>,
    /// What the user holds, in config order. Off-watchlist holdings are
    /// polled too (`polled_symbols`), so every row can be valued.
    pub positions: Vec<Position>,
    pub data: HashMap<String, TickerData>,
    pub errors: HashMap<String, String>,
    /// When a poll changed a symbol's price: (moment, tick was up). Views
    /// pulse the price for `PRICE_FLASH` after it via `price_flash_dir`;
    /// the first data a symbol ever gets sets no flash (nothing changed).
    pub price_flash: HashMap<String, (Instant, bool)>,
    /// Symbols whose rows came off disk or from an abandoned source,
    /// with the unix second they were fetched. The rail
    /// badges them so a blocked source never passes old prices off as live;
    /// the first real poll of a symbol clears its entry.
    pub from_cache: HashMap<String, i64>,
    /// Last successful fetch per symbol, for labelling retained prices
    /// when a fallback changes the source before new quotes arrive.
    quote_fetched: HashMap<String, i64>,
    /// Since when every symbol on the watchlist has been failing. The
    /// fallback is measured from here; any successful poll clears it.
    pub(crate) source_trouble_since: Option<Instant>,
    /// Sources this session has already given up on, so a fallback never
    /// walks back into the one that just failed.
    abandoned: Vec<&'static str>,
    /// Whether an unusable source may be swapped for a working one
    /// (`source_fallback` in the config).
    pub(crate) source_fallback: bool,
    /// Set once there is nothing left to fall back to: without it the app
    /// would rebuild every source on every frame of a long outage. Cleared
    /// whenever the user picks a source themselves.
    fallback_exhausted: bool,
    /// A short-lived line for the footer: what the app did on its own, such
    /// as switching source. Errors have their own place; this is for
    /// actions, and it fades.
    pub notice: Option<(String, Instant)>,
    pub selected: usize,
    pub view_idx: usize,
    pub source_name: &'static str,
    /// How stale the active source's prices are (`DataSource::delay_note`),
    /// re-read whenever the settings screen swaps the source. The quote
    /// rail badges it so a delayed price is never read as a live one.
    pub source_delay: Option<&'static str>,
    pub range: Range,
    pub interval: Interval,
    /// Whether the chart draws the pre and post market candles too.
    pub sessions: Sessions,
    pub last_update: Option<DateTime<Local>>,
    pub table_state: TableState,
    /// The portfolio view's own cursor: its rows are the positions, which
    /// are neither the watchlist nor in its order.
    pub portfolio_selected: usize,
    pub portfolio_state: TableState,
    // Chart options: seeded from [chart], then session-only toggles
    pub chart_style: ChartStyle,
    pub show_sma: bool,
    pub show_rsi: bool,
    pub show_volume: bool,
    /// Whether the price chart marks the ticker's news on its candles
    /// (`n`, `[chart] news_markers`). The marks are drawn from the feed
    /// already in the cache, so they never cost a request.
    pub show_news_markers: bool,
    pub ma_type: MaType,
    /// Validated [chart] values: indicator periods and the t/T preset cycle.
    pub chart: ChartDefaults,
    // AlphAI feed state: every fetched feed by cache key (news under the
    // symbol/market/trending keys, insider under `ins:SYM`)
    pub feeds: HashMap<String, FeedBundle>,
    /// Uids the reader has had under the cursor, plus the baseline of each
    /// feed's first fetch, by feed cache key. Rows outside this set are new
    /// since the user last looked and render the unseen marker; resting the
    /// cursor on a row retires it (`mark_selected_seen`). Session-only and
    /// survives bundle replacement, so markers live through TTL refetches
    /// and manual `r`.
    pub feed_seen: HashMap<String, HashSet<String>>,
    pub alphai_errors: HashMap<String, String>,
    pub alphai_enabled: bool,
    pub news_selected: usize,
    pub news_scope: NewsScope,
    pub news_layout: NewsLayout,
    /// Minimum relevance score in the ticker and market news feeds (1..=10,
    /// applied server-side). Seeded from `[ui] news_min_score`, then
    /// session-only via +/-, like the scope and layout.
    pub news_min_score: u8,
    /// Same for the insider feed, where the score tracks the trade size, so
    /// this filters by dollar value. Seeded from `[ui] insider_min_score`;
    /// +/- adjust whichever feed the visible view shows.
    pub insider_min_score: u8,
    /// Window of the Insider view's chart panel (g cycles off/3m/12m).
    pub insider_chart: InsiderChartWindow,
    /// Whether the quote rail is drawn (`[ui] quote_rail`).
    pub show_rail: bool,
    /// Bare mode (`z`, `--bare`, `[ui] bare`): the header and footer give
    /// their rows to the view. For a tmux pane, which carries its own
    /// status bar and needs the rows more than it needs the app's chrome.
    pub bare: bool,
    /// Earnings reads by symbol, each with the moment it was fetched. Its
    /// own map rather than a feed bundle: the payload has no pagination, no
    /// sort and no score filter, and it outlives a feed's TTL by an order of
    /// magnitude (a read changes once a quarter).
    pub earnings: HashMap<String, EarningsSlot>,
    /// The macro calendar window and when it landed. One market-wide payload
    /// for every ticker, so it is not keyed by symbol.
    pub calendar: Option<CalendarSlot>,
    pub calendar_selection: ui::calendar::Selection,
    pub calendar_state: TableState,
    pub(crate) calendar_refresh_requested: bool,
    pub(crate) report_date_asked: Option<Instant>,
    pub(crate) report_date_retry: HashSet<String>,
    pub(crate) report_dates_paused: Option<String>,
    /// SetKey acknowledgements form a barrier against old-key responses.
    pub(crate) alphai_key_pending: usize,
    /// Scroll of the Earnings view's body; reset when the ticker changes.
    pub earnings_scroll: u16,
    /// How long a fetched AlphAI bundle stays fresh. Seeded from
    /// `[ui] alphai_ttl_secs` (default `alphai::CACHE_TTL`), file-only.
    pub alphai_ttl: Duration,
    /// Scroll of the embedded card pane (News view); reset on selection moves.
    pub card_scroll: u16,
    pub news_table_state: TableState,
    pub article_overlay: ArticleOverlay,
    pub help: HelpOverlay,
    pub settings: SettingsState,
    pub prompt: Prompt,
    /// The watchlist as the price poller sees it; kept in step with
    /// `symbols` by `add_symbol` and `remove_selected_symbol`.
    pub shared_symbols: SharedSymbols,
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub theme: Theme,
    /// The preset `theme` was built from. The cycle key and the settings
    /// row move this name and rebuild the theme from it, so what is on
    /// screen always matches what the same name would give after a
    /// restart (explicit `[theme]` slots included).
    pub theme_name: &'static str,
    pub keymap: Keymap,
    source: SharedSource,
    /// The fetch window the poller re-reads each cycle. Public like the
    /// shared watchlist: what the keys push into it is the behaviour worth
    /// asserting on, not the field they set on `App`.
    pub params: SharedParams,
    /// The poll interval the poller re-reads before every sleep; the
    /// settings screen edits it live.
    every: SharedEvery,
    inflight: HashSet<String>,
    alphai_tx: UnboundedSender<alphai::Cmd>,
    rx: UnboundedReceiver<SourceEvent>,
    refresh: Arc<Notify>,
}

impl App {
    pub fn new(init: AppInit) -> Self {
        let source_delay = init.source.read().unwrap().delay_note();
        let mut app = Self {
            symbols: init.symbols,
            shared_symbols: init.shared_symbols,
            prompt: Prompt::default(),
            data: HashMap::new(),
            errors: HashMap::new(),
            price_flash: HashMap::new(),
            from_cache: HashMap::new(),
            quote_fetched: HashMap::new(),
            source_trouble_since: None,
            abandoned: Vec::new(),
            source_fallback: init.source_fallback,
            fallback_exhausted: false,
            notice: None,
            selected: 0,
            view_idx: init.ui.view_idx,
            source_name: init.source_name,
            source_delay,
            range: init.range,
            interval: init.interval,
            sessions: init.sessions,
            last_update: None,
            table_state: TableState::default(),
            chart_style: init.chart.style,
            show_sma: init.chart.sma,
            show_rsi: init.chart.rsi,
            show_volume: init.chart.volume,
            show_news_markers: init.chart.news_markers,
            ma_type: init.chart.ma_type,
            chart: init.chart,
            feeds: HashMap::new(),
            feed_seen: HashMap::new(),
            alphai_errors: HashMap::new(),
            alphai_enabled: init.alphai_enabled,
            news_selected: 0,
            news_scope: init.ui.news_scope,
            news_layout: init.ui.news_layout,
            news_min_score: init.ui.news_min_score,
            insider_min_score: init.ui.insider_min_score,
            insider_chart: init.ui.insider_chart,
            show_rail: init.ui.quote_rail,
            bare: init.ui.bare,
            earnings: HashMap::new(),
            calendar: None,
            calendar_selection: ui::calendar::Selection::default(),
            calendar_state: TableState::default(),
            calendar_refresh_requested: false,
            report_date_asked: None,
            report_date_retry: HashSet::new(),
            report_dates_paused: None,
            alphai_key_pending: 0,
            earnings_scroll: 0,
            alphai_ttl: init.ui.alphai_ttl,
            card_scroll: 0,
            news_table_state: TableState::default(),
            positions: init.positions,
            portfolio_selected: 0,
            portfolio_state: TableState::default(),
            article_overlay: ArticleOverlay::default(),
            help: HelpOverlay::default(),
            settings: SettingsState::default(),
            config: init.config,
            config_path: init.config_path,
            theme: init.theme,
            theme_name: init.theme_name,
            keymap: init.keymap,
            source: init.source,
            params: init.params,
            every: init.every,
            inflight: HashSet::new(),
            alphai_tx: init.alphai_tx,
            rx: init.rx,
            refresh: init.refresh,
        };
        if init.first_run {
            app.open_settings();
            app.settings.first_run = true;
        }
        app
    }

    pub fn selected_symbol(&self) -> &str {
        &self.symbols[self.selected]
    }

    /// Puts one ticker's last-good rows on screen before the first poll
    /// lands. Marked as cached until that poll replaces them, so the age of
    /// what is on screen is never a guess.
    pub fn seed_cached(&mut self, symbol: String, entry: crate::cache::Entry) {
        self.quote_fetched.insert(symbol.clone(), entry.fetched);
        self.from_cache.insert(symbol.clone(), entry.fetched);
        self.data.insert(symbol, entry.data);
    }

    /// How old the retained rows are, as a label.
    pub fn cached_age(&self, symbol: &str) -> Option<String> {
        self.from_cache
            .get(symbol)
            .map(|at| crate::cache::age_label(*at))
    }

    /// The still-current footer notice, if it has not faded yet.
    pub fn notice(&self) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTICE)
            .map(|(text, _)| text.as_str())
    }

    /// The still-active price pulse of a symbol: Some(tick was up) for
    /// `PRICE_FLASH` after a poll changed its price, then None.
    pub fn price_flash_dir(&self, symbol: &str) -> Option<bool> {
        self.price_flash
            .get(symbol)
            .filter(|(at, _)| at.elapsed() < PRICE_FLASH)
            .map(|&(_, up)| up)
    }

    /// Identity of the visible view (its `view_idx` is the tab position).
    pub fn view_id(&self) -> ui::ViewId {
        ui::VIEWS[self.view_idx].id()
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            while let Ok(ev) = self.rx.try_recv() {
                self.apply(ev);
            }
            self.fallback_if_stuck();
            self.ensure_alphai_data();
            terminal.draw(|f| ui::draw(f, self))?;
            if event::poll(std::time::Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && self.handle_key(key)
            {
                return Ok(());
            }
        }
    }

    pub(crate) fn apply(&mut self, event: SourceEvent) {
        match event {
            SourceEvent::Data {
                params,
                source,
                symbol,
                mut data,
            } => {
                if !self.accepts_price_event(&source, &symbol)
                    || params.is_some_and(|p| p != (self.range, self.interval, self.sessions))
                {
                    return;
                }
                self.errors.remove(&symbol);
                self.quote_fetched
                    .insert(symbol.clone(), chrono::Utc::now().timestamp());
                // A live answer clears the outage clock and retires the
                // cached rows this symbol started on.
                self.source_trouble_since = None;
                let was_cached = self.from_cache.remove(&symbol).is_some();
                // A successful but partial response must not make a known
                // current-session print disappear. Its original timestamp
                // and reference close remain visible; a new session retires it.
                if let Some(old) = self.data.get(&symbol)
                    && old.quote.timing.extended.is_some()
                    && old.quote.extended_price().is_some()
                    && data.quote.extended_price().is_none()
                {
                    data.quote.extended = old.quote.extended;
                    data.quote.timing.extended = old.quote.timing.extended;
                    data.quote.timing.extended_feed = old.quote.timing.extended_feed;
                    data.quote.timing.extended_reference = Some(old.quote.extended_reference());
                }
                if let Some(old) = self.data.get(&symbol)
                    && old.quote.price != data.quote.price
                    // The first real price after a cached one is not a tick:
                    // pulsing it would report yesterday's move as just now.
                    && !was_cached
                {
                    self.price_flash.insert(
                        symbol.clone(),
                        (Instant::now(), data.quote.price > old.quote.price),
                    );
                }
                data.update_current_bar(self.interval);
                self.data.insert(symbol, data);
                self.last_update = Some(Local::now());
            }
            SourceEvent::Error {
                params,
                source,
                symbol,
                error,
            } => {
                if !self.accepts_price_event(&source, &symbol)
                    || params.is_some_and(|p| p != (self.range, self.interval, self.sessions))
                {
                    return;
                }
                self.errors.insert(symbol, error);
                self.last_update = Some(Local::now());
                // One failing ticker is a bad symbol; all of them is the
                // source, and that is what the fallback waits on.
                if self.source_trouble_since.is_none() && self.all_prices_failing() {
                    self.source_trouble_since = Some(Instant::now());
                }
            }
            SourceEvent::Alphai(ev) => self.apply_alphai(ev),
        }
    }

    fn accepts_price_event(
        &self,
        source: &Arc<dyn crate::source::DataSource>,
        symbol: &str,
    ) -> bool {
        // Name alone cannot distinguish rebuilt credentials for the same
        // provider. Pending responses retain the exact source instance.
        Arc::ptr_eq(source, &self.price_source())
            && self
                .shared_symbols
                .read()
                .unwrap()
                .iter()
                .any(|s| s == symbol)
    }

    pub(crate) fn price_source(&self) -> Arc<dyn crate::source::DataSource> {
        self.source.read().unwrap().clone()
    }

    fn all_prices_failing(&self) -> bool {
        let symbols = self.shared_symbols.read().unwrap();
        !symbols.is_empty() && symbols.iter().all(|s| self.errors.contains_key(s))
    }

    /// State shared by manual and automatic source changes. The caller
    /// decides whether to retain quotes and reset the fallback history.
    fn set_price_source(&mut self, source: Arc<dyn crate::source::DataSource>) {
        self.source_name = source.name();
        self.source_delay = source.delay_note();
        *self.source.write().unwrap() = source;
        self.errors.clear();
        self.source_trouble_since = None;
        self.price_flash.clear();
        self.notice = None;
        self.refresh.notify_one();
    }

    /// Swaps a source that has stopped answering for one that has not.
    ///
    /// Yahoo, the keyless default, blocks by IP for tens of minutes at a
    /// time and no retry can shorten that, so the only real repair is a
    /// different provider. The swap waits for every symbol to be failing
    /// (one bad ticker is a bad ticker) and for `FALLBACK_AFTER` on top of
    /// that, keeps the rows that are on screen, and says what it did. It
    /// never switches back on its own: probing a throttled source is how
    /// the block gets extended, and the header names the source anyway.
    pub(crate) fn fallback_if_stuck(&mut self) {
        if !self.source_fallback || self.fallback_exhausted || self.settings.open {
            return;
        }
        // The polled symbols can change while the grace period runs.
        if !self.all_prices_failing() {
            self.source_trouble_since = None;
            return;
        }
        let Some(since) = self.source_trouble_since else {
            return;
        };
        if since.elapsed() < FALLBACK_AFTER {
            return;
        }
        let Some(src) = self.fallback_source() else {
            // Nothing else is configured: stop looking until the user picks
            // a source themselves, or this rebuilds a client every frame.
            self.fallback_exhausted = true;
            return;
        };
        let failed = self.source_name;
        self.abandoned.push(failed);
        self.from_cache.extend(self.quote_fetched.clone());
        self.set_price_source(src);
        let name = self.source_name;
        self.notice = Some((
            format!("{failed} stopped answering, switched to {name} (s to choose another)"),
            Instant::now(),
        ));
    }

    /// The first registered source that is not the current one, has not
    /// already failed this session, and has the credentials it needs.
    fn fallback_source(&self) -> Option<Arc<dyn crate::source::DataSource>> {
        crate::source::registry::SOURCES
            .iter()
            .filter(|info| {
                !info.id.eq_ignore_ascii_case(self.source_name)
                    && !self.abandoned.contains(&info.id)
            })
            .find_map(|info| crate::source::make_source(info.id, &self.config).ok())
    }

    /// Returns true when the app should quit. Fixed keys resolve first
    /// (Ctrl-C, Esc, the positional 1-9 digits); everything else goes
    /// through the keymap and dispatches on `Action`, with the same view
    /// guards the raw keys used — a remap can never bypass them.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if self.prompt.open {
            return self.handle_prompt_key(key);
        }
        if self.settings.open {
            return self.handle_settings_key(key);
        }
        if self.article_overlay.open {
            return self.handle_overlay_key(key);
        }
        if self.help.open {
            return self.handle_help_key(key);
        }
        if key.code == KeyCode::Esc {
            return true;
        }
        if let KeyCode::Char(c @ '1'..='9') = key.code {
            let idx = c as usize - '1' as usize;
            if idx < ui::VIEWS.len() {
                self.switch_view(idx);
            }
            return false;
        }
        let news_view = ui::VIEWS[self.view_idx].navigates_articles();
        let chart_view = ui::VIEWS[self.view_idx].has_chart_panel();
        let earnings_view = ui::VIEWS[self.view_idx].shows_earnings();
        let calendar_view = ui::VIEWS[self.view_idx].shows_calendar();
        // ←→ walk the watchlist wherever the view is scoped to one ticker.
        let lr_ticker = news_view || earnings_view;
        let news_feed = ui::VIEWS[self.view_idx].feed_shown() == Some(FeedKind::News);
        let any_feed = ui::VIEWS[self.view_idx].feed_shown().is_some();
        let Some(action) = self.keymap.resolve(&key) else {
            return false;
        };
        match action {
            Action::Quit => return true,
            Action::AddTicker => {
                self.prompt = Prompt {
                    open: true,
                    ..Default::default()
                }
            }
            Action::RemoveTicker => self.remove_selected_symbol(),
            Action::Position => self.open_position_prompt(),
            Action::NextView => self.switch_view((self.view_idx + 1) % ui::VIEWS.len()),
            Action::PrevView => {
                self.switch_view((self.view_idx + ui::VIEWS.len() - 1) % ui::VIEWS.len())
            }
            Action::Settings => self.open_settings(),
            Action::Help => {
                self.help = HelpOverlay {
                    open: true,
                    scroll: 0,
                }
            }
            Action::Refresh => self.manual_refresh(),
            Action::Up if calendar_view => self.move_calendar(-1),
            Action::Down if calendar_view => self.move_calendar(1),
            Action::PageUp if calendar_view => self.move_calendar(-10),
            Action::PageDown if calendar_view => self.move_calendar(10),
            Action::Open if calendar_view => self.open_calendar_row(),
            // News/Insider: up/down scroll articles, left/right switch ticker.
            Action::Up if news_view => {
                self.news_selected = self.news_selected.saturating_sub(1);
                self.card_scroll = 0;
            }
            Action::Down if news_view => {
                let max = self.visible_articles().map_or(0, <[Article]>::len);
                if self.news_selected + 1 < max {
                    self.news_selected += 1;
                    self.card_scroll = 0;
                } else {
                    // Already on the last row: page the feed instead.
                    self.request_more_articles();
                }
            }
            Action::Left if lr_ticker => self.select_symbol(self.selected.saturating_sub(1)),
            Action::Right if lr_ticker => {
                self.select_symbol((self.selected + 1).min(self.symbols.len() - 1))
            }
            // The earnings body is a document: up/down scroll it, and these
            // arms must stay above the unguarded ones at the end of the
            // match, which move the watchlist selection instead.
            Action::Up if earnings_view => {
                self.earnings_scroll = self.earnings_scroll.saturating_sub(1)
            }
            Action::Down if earnings_view => {
                self.earnings_scroll = self.earnings_scroll.saturating_add(1)
            }
            // Card pane scrolling (the list keeps up/down).
            Action::PageUp if earnings_view => {
                self.earnings_scroll = self.earnings_scroll.saturating_sub(10)
            }
            Action::PageDown if earnings_view => {
                self.earnings_scroll = self.earnings_scroll.saturating_add(10)
            }
            Action::PageUp if self.view_id() == ui::ViewId::News => {
                self.card_scroll = self.card_scroll.saturating_sub(10)
            }
            Action::PageDown if self.view_id() == ui::ViewId::News => {
                self.card_scroll = self.card_scroll.saturating_add(10)
            }
            Action::CycleLayout if self.view_id() == ui::ViewId::News => {
                self.news_layout = self.news_layout.next();
                self.card_scroll = 0;
            }
            Action::Open if news_view => {
                if let Some(a) = self
                    .visible_articles()
                    .and_then(|list| list.get(self.news_selected))
                {
                    open_url(&self.article_url(a));
                }
            }
            // The read's own page on alphai.io, which also links the filing.
            // The payload carries no source URL of its own, so this ignores
            // the "open the original source" setting.
            Action::Open if earnings_view => {
                if let Some(url) = ui::earnings::open_url(self) {
                    open_url(&url);
                }
            }
            Action::Card
                if news_view && self.visible_articles().is_some_and(|list| !list.is_empty()) =>
            {
                self.article_overlay = ArticleOverlay {
                    open: true,
                    scroll: 0,
                };
            }
            Action::CycleScope if news_feed => {
                self.news_scope = self.news_scope.next();
                self.news_selected = 0;
                self.card_scroll = 0;
            }
            Action::InsiderChart if self.view_id() == ui::ViewId::Insider => {
                self.insider_chart = self.insider_chart.next();
            }
            Action::ScoreUp if any_feed => self.adjust_min_score(1),
            Action::ScoreDown if any_feed => self.adjust_min_score(-1),
            Action::ChartStyle if chart_view => {
                self.chart_style = match self.chart_style {
                    ChartStyle::Candles => ChartStyle::Line,
                    ChartStyle::Line => ChartStyle::Candles,
                }
            }
            Action::ToggleSma if chart_view => self.show_sma = !self.show_sma,
            Action::ToggleRsi if chart_view => self.show_rsi = !self.show_rsi,
            Action::ToggleVolume if chart_view => self.show_volume = !self.show_volume,
            Action::NewsMarkers if chart_view => self.show_news_markers = !self.show_news_markers,
            Action::MaType if chart_view => {
                self.ma_type = match self.ma_type {
                    MaType::Sma => MaType::Ema,
                    MaType::Ema => MaType::Sma,
                }
            }
            Action::NextPreset => self.cycle_range(1),
            Action::PrevPreset => self.cycle_range(-1),
            Action::ToggleBare => self.bare = !self.bare,
            Action::ToggleExtended => self.toggle_sessions(),
            Action::NextTheme => self.cycle_theme(1),
            Action::PrevTheme => self.cycle_theme(-1),
            Action::Up if self.view_id() == ui::ViewId::Portfolio => self.move_portfolio(-1),
            Action::Down if self.view_id() == ui::ViewId::Portfolio => self.move_portfolio(1),
            Action::Up => self.selected = self.selected.saturating_sub(1),
            Action::Down => self.selected = (self.selected + 1).min(self.symbols.len() - 1),
            _ => {}
        }
        false
    }

    /// Keys while the article card overlay is open; it swallows everything so
    /// list navigation does not shift under the reader. Esc always closes.
    fn handle_overlay_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Esc {
            self.article_overlay = ArticleOverlay::default();
            return false;
        }
        match self.keymap.resolve(&key) {
            Some(Action::Quit) => return true,
            Some(Action::Card) => self.article_overlay = ArticleOverlay::default(),
            Some(Action::Up) => {
                self.article_overlay.scroll = self.article_overlay.scroll.saturating_sub(1)
            }
            // The render pass clamps the scroll to the card's real height.
            Some(Action::Down) => {
                self.article_overlay.scroll = self.article_overlay.scroll.saturating_add(1)
            }
            Some(Action::PageUp) => {
                self.article_overlay.scroll = self.article_overlay.scroll.saturating_sub(10)
            }
            Some(Action::PageDown) => {
                self.article_overlay.scroll = self.article_overlay.scroll.saturating_add(10)
            }
            Some(Action::Open) => {
                if let Some(a) = self
                    .visible_articles()
                    .and_then(|list| list.get(self.news_selected))
                {
                    open_url(&self.article_url(a));
                }
            }
            _ => {}
        }
        false
    }

    /// Keys while the help overlay is open; like the article card it swallows
    /// everything. Esc or ? closes, q still quits.
    /// Keys while the add-ticker prompt is open. It swallows everything,
    /// the settings pattern, so typing "d" is a letter rather than the
    /// remove action.
    fn handle_prompt_key(&mut self, key: KeyEvent) -> bool {
        // Tickers are short; a position line carries two numbers as well.
        // The caps are there so a stuck key cannot grow the line past its
        // box.
        let cap = match self.prompt.kind {
            PromptKind::Ticker => 16,
            PromptKind::Position => 32,
        };
        match key.code {
            KeyCode::Esc => self.prompt = Prompt::default(),
            KeyCode::Enter => match self.commit_prompt() {
                Ok(()) => self.prompt = Prompt::default(),
                Err(msg) => self.prompt.error = Some(msg),
            },
            KeyCode::Backspace => {
                self.prompt.input.pop();
                self.prompt.error = None;
            }
            // The readline gesture for "start over". The position prompt
            // opens prefilled with what is held, so clearing the line is a
            // common move rather than an exotic one.
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.prompt.input.clear();
                self.prompt.error = None;
            }
            // Modified keys are gestures, not text: ctrl-h used to arrive
            // as a plain "h" and land in the middle of a quantity.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && self.prompt.input.chars().count() < cap =>
            {
                self.prompt.input.push(c);
                self.prompt.error = None;
            }
            _ => {}
        }
        false
    }

    fn commit_prompt(&mut self) -> Result<(), String> {
        let typed = self.prompt.input.clone();
        match self.prompt.kind {
            PromptKind::Ticker => self.add_symbol(typed.trim().to_uppercase().as_str()),
            PromptKind::Position => {
                let target = self.prompt.target.clone();
                let entry = portfolio::parse_entry(&typed, &target)?;
                self.apply_position(entry)
            }
        }
    }

    /// Opens the position prompt on the ticker in front of the reader: the
    /// row under the portfolio cursor in that view, the selected ticker
    /// anywhere else. Prefilled with what is held already, so correcting a
    /// number is not a retype, and an emptied line clears the holding.
    fn open_position_prompt(&mut self) {
        let target = self.position_target();
        let input = self
            .position(&target)
            .map(|p| format!("{} {}", p.qty, p.avg_price))
            .unwrap_or_default();
        self.prompt = Prompt {
            open: true,
            kind: PromptKind::Position,
            input,
            target,
            error: None,
        };
    }

    fn position_target(&self) -> String {
        if self.view_id() == ui::ViewId::Portfolio
            && let Some(p) = self.positions.get(self.portfolio_selected)
        {
            return p.symbol.clone();
        }
        self.selected_symbol().to_string()
    }

    pub fn position(&self, symbol: &str) -> Option<&Position> {
        self.positions.iter().find(|p| p.symbol == symbol)
    }

    /// Moves the portfolio cursor, and the watchlist cursor with it when
    /// the row's ticker is on the watchlist: the rail sits above every
    /// view and would otherwise quote a different ticker than the row the
    /// cursor is on.
    fn move_portfolio(&mut self, dir: isize) {
        if self.positions.is_empty() {
            return;
        }
        let last = self.positions.len() as isize - 1;
        let next = (self.portfolio_selected as isize + dir).clamp(0, last) as usize;
        self.portfolio_selected = next;
        let symbol = self.positions[next].symbol.clone();
        if let Some(at) = self.symbols.iter().position(|s| *s == symbol) {
            self.select_symbol(at);
        }
    }

    /// Writes a position through and saves it. Unlike every other runtime
    /// change here it does not wait for Save in the settings screen: a
    /// quantity and a price someone typed are their data, not a display
    /// preference, and losing them on quit would read as a bug.
    fn apply_position(&mut self, entry: portfolio::Entry) -> Result<(), String> {
        match entry {
            portfolio::Entry::Set {
                symbol,
                qty,
                avg_price,
            } => {
                let next = Position {
                    symbol,
                    qty,
                    avg_price,
                };
                match self.positions.iter_mut().find(|p| p.symbol == next.symbol) {
                    Some(slot) => *slot = next,
                    None => self.positions.push(next),
                }
            }
            portfolio::Entry::Clear { symbol } => self.positions.retain(|p| p.symbol != symbol),
        }
        self.portfolio_selected = self
            .portfolio_selected
            .min(self.positions.len().saturating_sub(1));
        // A holding off the watchlist is polled too, and the nudge saves
        // the new row a full interval of waiting for its first price.
        self.sync_shared_symbols();
        self.refresh.notify_one();
        self.persist_positions()
    }

    /// Saves the positions into the config file, leaving every other
    /// section as it was loaded. Deliberately not `settings_merged_config`:
    /// that one also persists the live watchlist and the settings rows,
    /// which nobody asked this keypress to do.
    fn persist_positions(&mut self) -> Result<(), String> {
        self.config.positions = self.positions.clone();
        if self.config_path.is_none() {
            return Ok(());
        }
        config::save_at(self.config_path.as_deref(), &self.config)
            .map_err(|e| format!("kept for this session only: {e}"))
    }

    /// Adds a symbol to the live watchlist and selects it. The poller reads
    /// the shared list at the top of its next cycle, and the nudge makes
    /// that cycle start now instead of up to one interval later.
    ///
    /// Session-only, like every other runtime change here (chart options,
    /// scope, theme): Save in the settings screen writes the watchlist to
    /// the config.
    fn add_symbol(&mut self, symbol: &str) -> Result<(), String> {
        if symbol.is_empty() {
            return Err("type a ticker, e.g. AAPL".into());
        }
        if let Some(at) = self.symbols.iter().position(|s| s == symbol) {
            // Not an error worth dwelling on: put the cursor where the
            // ticker already is, and say why nothing was added.
            self.select_symbol(at);
            return Err(format!("{symbol} is already on the watchlist"));
        }
        self.symbols.push(symbol.to_string());
        self.sync_shared_symbols();
        self.select_symbol(self.symbols.len() - 1);
        self.refresh.notify_one();
        Ok(())
    }

    /// Drops the selected symbol. The last one stays: every view reads
    /// `selected_symbol()` directly, so an empty watchlist needs empty
    /// states before it can be reached.
    fn remove_selected_symbol(&mut self) {
        if self.symbols.len() < 2 {
            return;
        }
        let gone = self.symbols.remove(self.selected);
        self.report_date_retry.remove(&gone);
        self.sync_shared_symbols();
        // Prices are cheap to fetch again; the AlphAI feeds are not, so
        // their cache survives a removal and a re-add costs no request.
        // A holding keeps its price: it is still polled, and blanking the
        // row would leave the portfolio view showing "…" until the tick.
        if self.position(&gone).is_none() {
            self.data.remove(&gone);
            self.errors.remove(&gone);
            self.price_flash.remove(&gone);
            self.from_cache.remove(&gone);
            self.quote_fetched.remove(&gone);
        }
        self.select_symbol(self.selected.min(self.symbols.len() - 1));
    }

    /// Moves the watchlist cursor. Everything scoped to the selected
    /// ticker starts over, or the new ticker's feed would open at the old
    /// one's scroll position.
    fn select_symbol(&mut self, idx: usize) {
        self.selected = idx;
        self.news_selected = 0;
        self.card_scroll = 0;
        self.earnings_scroll = 0;
    }

    /// Every symbol the poller fetches: the watchlist plus anything held
    /// that is not on it. A holding with no price is a row that cannot be
    /// valued, so it is worth the request; the budget warning in `main`
    /// counts this list for the same reason.
    pub fn polled_symbols(&self) -> Vec<String> {
        portfolio::polled_symbols(&self.symbols, &self.positions)
    }

    fn sync_shared_symbols(&self) {
        *self.shared_symbols.write().unwrap() = self.polled_symbols();
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Esc {
            self.help = HelpOverlay::default();
            return false;
        }
        match self.keymap.resolve(&key) {
            Some(Action::Quit) => return true,
            Some(Action::Help) => self.help = HelpOverlay::default(),
            Some(Action::Up) => self.help.scroll = self.help.scroll.saturating_sub(1),
            // The render pass clamps the scroll to the table's real height.
            Some(Action::Down) => self.help.scroll = self.help.scroll.saturating_add(1),
            Some(Action::PageUp) => self.help.scroll = self.help.scroll.saturating_sub(10),
            Some(Action::PageDown) => self.help.scroll = self.help.scroll.saturating_add(10),
            _ => {}
        }
        false
    }

    /// URL Enter opens for an article. News items default to their alphai.io
    /// article page (settings can flip this to the original source); insider
    /// filings always open the original, which points at the SEC filing.
    fn article_url(&self, a: &Article) -> String {
        let url = if self.view_id() == ui::ViewId::News && !self.config.news_open_original() {
            a.alphai_url().unwrap_or_else(|| a.original.url.clone())
        } else {
            a.original.url.clone()
        };
        with_utm(&url)
    }

    /// p / P: walk the preset list forward or back.
    fn cycle_theme(&mut self, dir: isize) {
        self.set_theme(crate::theme::step_preset(self.theme_name, dir));
    }

    /// Switch to a named preset (the p key, the settings row). Rebuilt
    /// through the same path the config takes, so the `[theme]` slots the
    /// user spelled out keep overriding the palette; the frame style comes
    /// from `[ui] borders` and does not belong to the preset. Session-only
    /// until Save writes the name into the config.
    pub(crate) fn set_theme(&mut self, name: &'static str) {
        let border_type = self.theme.border_type;
        let mut warnings = Vec::new();
        let (mut theme, name) =
            Theme::resolve(self.config.theme.as_ref(), Some(name), &mut warnings);
        theme.border_type = border_type;
        self.theme = theme;
        self.theme_name = name;
    }

    fn switch_view(&mut self, idx: usize) {
        if idx != self.view_idx {
            self.view_idx = idx;
            self.news_selected = 0;
            self.card_scroll = 0;
            self.earnings_scroll = 0;
            if ui::VIEWS[idx].shows_calendar() {
                self.calendar_selection = ui::calendar::Selection::default();
                self.calendar_state = TableState::default();
            }
        }
    }

    fn move_calendar(&mut self, delta: isize) {
        let rows = ui::calendar::agenda(self, Utc::now());
        let Some(current) = self.calendar_selection.resolve(&rows) else {
            return;
        };
        let next = current.saturating_add_signed(delta).min(rows.len() - 1);
        self.calendar_selection.choose(&rows, next);
        if let ui::calendar::Kind::Report { symbol } = &rows[next].kind
            && let Some(idx) = self.symbols.iter().position(|s| s == symbol)
        {
            self.select_symbol(idx);
        }
    }

    fn open_calendar_row(&mut self) {
        match ui::calendar::open_target(self, Utc::now()) {
            Some(ui::calendar::OpenTarget::Url(url)) => open_url(&url),
            Some(ui::calendar::OpenTarget::Earnings(symbol)) => {
                if let Some(idx) = self.symbols.iter().position(|s| s == &symbol) {
                    self.select_symbol(idx);
                    self.switch_view(ui::view_index(ui::ViewId::Earnings));
                }
            }
            Some(ui::calendar::OpenTarget::Unavailable) => {
                self.notice = Some(("No source link for this event".into(), Instant::now()));
            }
            None => {}
        }
    }

    /// t / T: jump to the next/previous range/interval preset and wake the
    /// price poller. Only `refresh.notify_one()` here: `manual_refresh()`
    /// would also drop the visible AlphAI bundle and burn a request from
    /// its budget for what is purely a price-history change.
    fn cycle_range(&mut self, dir: isize) {
        let (range, interval) = next_preset(&self.chart.presets, (self.range, self.interval), dir);
        self.range = range;
        self.interval = interval;
        self.push_params();
    }

    /// E: draw the pre and post market candles, or stop. Same shape as the
    /// preset cycle, and the same reason for only nudging the price
    /// poller: nothing about the AlphAI feeds changes.
    fn toggle_sessions(&mut self) {
        self.sessions = self.sessions.toggled();
        if self.sessions == Sessions::Regular && self.interval != Interval::D1 {
            for (symbol, data) in &mut self.data {
                if crate::market::is_us_equity(symbol) {
                    data.candles.retain(|c| {
                        crate::market::window_at(c.ts)
                            .is_some_and(|w| w.session == crate::market::Session::Open)
                    });
                }
            }
        }
        self.push_params();
    }

    fn push_params(&mut self) {
        *self.params.write().unwrap() = (self.range, self.interval, self.sessions);
        self.refresh.notify_one();
    }
}

/// Tag an outgoing article link with this client as the traffic source, so
/// alphai.io and original publishers can attribute the referral. Left as-is
/// when the URL already carries a utm_source (never clobber the feed's own
/// attribution); the fragment, if any, stays at the end where it belongs.
fn with_utm(url: &str) -> String {
    if url.contains("utm_source=") {
        return url.to_string();
    }
    let (base, frag) = match url.split_once('#') {
        Some((base, frag)) => (base, Some(frag)),
        None => (url, None),
    };
    let sep = if base.contains('?') { '&' } else { '?' };
    match frag {
        Some(frag) => format!("{base}{sep}utm_source=alphai-tui#{frag}"),
        None => format!("{base}{sep}utm_source=alphai-tui"),
    }
}

/// Open a URL with the platform handler; failures are ignored (worst case the
/// article just does not open — never crash the TUI over it).
pub fn open_url(url: &str) {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return;
    }
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    let _ = cmd
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::{RANGE_PRESETS, next_preset, with_utm};
    use crate::domain::{Interval, Range};

    #[test]
    fn range_presets_wrap_both_ways() {
        let first = RANGE_PRESETS[0];
        let last = RANGE_PRESETS[RANGE_PRESETS.len() - 1];
        assert_eq!(next_preset(&RANGE_PRESETS, first, 1), RANGE_PRESETS[1]);
        assert_eq!(next_preset(&RANGE_PRESETS, last, 1), first);
        assert_eq!(next_preset(&RANGE_PRESETS, first, -1), last);
    }

    #[test]
    fn utm_tag_appended_to_plain_and_query_urls() {
        assert_eq!(
            with_utm("https://alphai.io/news/article/07-10/abc/slug"),
            "https://alphai.io/news/article/07-10/abc/slug?utm_source=alphai-tui"
        );
        assert_eq!(
            with_utm("https://example.com/story?id=7"),
            "https://example.com/story?id=7&utm_source=alphai-tui"
        );
    }

    #[test]
    fn utm_tag_respects_existing_source_and_fragment() {
        // A feed URL that already attributes its source is left untouched.
        let tagged = "https://example.com/story?utm_source=newsletter";
        assert_eq!(with_utm(tagged), tagged);
        // The fragment stays terminal, the query lands before it.
        assert_eq!(
            with_utm("https://example.com/story#section"),
            "https://example.com/story?utm_source=alphai-tui#section"
        );
    }

    #[test]
    fn range_presets_absorb_unknown_startup_combo() {
        // A CLI combo outside the table joins the cycle at the nearest edge.
        let odd = (Range::Mo3, Interval::M5);
        assert_eq!(next_preset(&RANGE_PRESETS, odd, 1), RANGE_PRESETS[0]);
        assert_eq!(
            next_preset(&RANGE_PRESETS, odd, -1),
            RANGE_PRESETS[RANGE_PRESETS.len() - 1]
        );
    }
}
