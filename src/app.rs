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
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::widgets::TableState;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::alphai::{self, Article};
use crate::config::{ChartDefaults, Config, UiDefaults};
use crate::domain::{Interval, Range, TickerData};
use crate::keymap::{Action, Keymap};
use crate::poller::{SharedEvery, SharedParams, SharedSource, SourceEvent};
use crate::theme::Theme;
use crate::ui;

/// How long the last-price highlight stays lit after a poll changes a
/// symbol's price. The event loop redraws at least every 100ms, so the
/// pulse both appears and clears promptly.
pub const PRICE_FLASH: Duration = Duration::from_millis(1500);

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
    pub source: SharedSource,
    pub source_name: &'static str,
    pub range: Range,
    pub interval: Interval,
    pub params: SharedParams,
    pub every: SharedEvery,
    pub rx: UnboundedReceiver<SourceEvent>,
    pub refresh: Arc<Notify>,
    pub alphai_tx: UnboundedSender<alphai::Cmd>,
    pub config: Config,
    /// Where Save writes the config back; None disables persisting.
    pub config_path: Option<PathBuf>,
    pub theme: Theme,
    pub chart: ChartDefaults,
    pub ui: UiDefaults,
    /// Resolved from `[keybindings]` (defaults when the section is absent).
    pub keymap: Keymap,
    pub alphai_enabled: bool,
    pub first_run: bool,
}

pub struct App {
    pub symbols: Vec<String>,
    pub data: HashMap<String, TickerData>,
    pub errors: HashMap<String, String>,
    /// When a poll changed a symbol's price: (moment, tick was up). Views
    /// pulse the price for `PRICE_FLASH` after it via `price_flash_dir`;
    /// the first data a symbol ever gets sets no flash (nothing changed).
    pub price_flash: HashMap<String, (Instant, bool)>,
    pub selected: usize,
    pub view_idx: usize,
    pub source_name: &'static str,
    pub range: Range,
    pub interval: Interval,
    pub last_update: Option<DateTime<Local>>,
    pub table_state: TableState,
    // Chart options: seeded from [chart], then session-only toggles
    pub chart_style: ChartStyle,
    pub show_sma: bool,
    pub show_rsi: bool,
    /// Validated [chart] values: indicator periods and the t/T preset cycle.
    pub chart: ChartDefaults,
    // AlphaAI feed state: every fetched feed by cache key (news under the
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
    /// How long a fetched AlphaAI bundle stays fresh. Seeded from
    /// `[ui] alphai_ttl_secs` (default `alphai::CACHE_TTL`), file-only.
    pub alphai_ttl: Duration,
    /// Scroll of the embedded card pane (News view); reset on selection moves.
    pub card_scroll: u16,
    pub news_table_state: TableState,
    pub article_overlay: ArticleOverlay,
    pub help: HelpOverlay,
    pub settings: SettingsState,
    pub config: Config,
    pub config_path: Option<PathBuf>,
    pub theme: Theme,
    pub keymap: Keymap,
    source: SharedSource,
    params: SharedParams,
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
        let mut app = Self {
            symbols: init.symbols,
            data: HashMap::new(),
            errors: HashMap::new(),
            price_flash: HashMap::new(),
            selected: 0,
            view_idx: init.ui.view_idx,
            source_name: init.source_name,
            range: init.range,
            interval: init.interval,
            last_update: None,
            table_state: TableState::default(),
            chart_style: init.chart.style,
            show_sma: init.chart.sma,
            show_rsi: init.chart.rsi,
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
            alphai_ttl: init.ui.alphai_ttl,
            card_scroll: 0,
            news_table_state: TableState::default(),
            article_overlay: ArticleOverlay::default(),
            help: HelpOverlay::default(),
            settings: SettingsState::default(),
            config: init.config,
            config_path: init.config_path,
            theme: init.theme,
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
            SourceEvent::Data { symbol, data } => {
                self.errors.remove(&symbol);
                if let Some(old) = self.data.get(&symbol)
                    && old.quote.price != data.quote.price
                {
                    self.price_flash.insert(
                        symbol.clone(),
                        (Instant::now(), data.quote.price > old.quote.price),
                    );
                }
                self.data.insert(symbol, data);
                self.last_update = Some(Local::now());
            }
            SourceEvent::Error { symbol, error } => {
                self.errors.insert(symbol, error);
                self.last_update = Some(Local::now());
            }
            SourceEvent::Alphai(ev) => self.apply_alphai(ev),
        }
    }

    /// Returns true when the app should quit. Fixed keys resolve first
    /// (Ctrl-C, Esc, the positional 1-9 digits); everything else goes
    /// through the keymap and dispatches on `Action`, with the same view
    /// guards the raw keys used — a remap can never bypass them.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
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
        let news_feed = ui::VIEWS[self.view_idx].feed_shown() == Some(FeedKind::News);
        let any_feed = ui::VIEWS[self.view_idx].feed_shown().is_some();
        let Some(action) = self.keymap.resolve(&key) else {
            return false;
        };
        match action {
            Action::Quit => return true,
            Action::NextView => self.switch_view((self.view_idx + 1) % ui::VIEWS.len()),
            Action::PrevView => {
                self.switch_view((self.view_idx + ui::VIEWS.len() - 1) % ui::VIEWS.len())
            }
            Action::Settings => self.open_settings(),
            Action::Help => self.help = HelpOverlay { open: true, scroll: 0 },
            Action::Refresh => self.manual_refresh(),
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
            Action::Left if news_view => {
                self.selected = self.selected.saturating_sub(1);
                self.news_selected = 0;
                self.card_scroll = 0;
            }
            Action::Right if news_view => {
                self.selected = (self.selected + 1).min(self.symbols.len() - 1);
                self.news_selected = 0;
                self.card_scroll = 0;
            }
            // Card pane scrolling (the list keeps up/down).
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
            Action::Card
                if news_view && self.visible_articles().is_some_and(|list| !list.is_empty()) =>
            {
                self.article_overlay = ArticleOverlay { open: true, scroll: 0 };
            }
            Action::CycleScope if news_feed => {
                self.news_scope = self.news_scope.next();
                self.news_selected = 0;
                self.card_scroll = 0;
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
            Action::NextPreset => self.cycle_range(1),
            Action::PrevPreset => self.cycle_range(-1),
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

    fn switch_view(&mut self, idx: usize) {
        if idx != self.view_idx {
            self.view_idx = idx;
            self.news_selected = 0;
            self.card_scroll = 0;
        }
    }

    /// t / T: jump to the next/previous range/interval preset and wake the
    /// price poller. Only `refresh.notify_one()` here: `manual_refresh()`
    /// would also drop the visible AlphaAI bundle and burn a request from
    /// its budget for what is purely a price-history change.
    fn cycle_range(&mut self, dir: isize) {
        let (range, interval) = next_preset(&self.chart.presets, (self.range, self.interval), dir);
        self.range = range;
        self.interval = interval;
        *self.params.write().unwrap() = (range, interval);
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
