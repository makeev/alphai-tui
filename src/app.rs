//! App state and the event loop. Feed caching with its request-budget
//! guards lives in `feeds`, the settings overlay in `settings`; views under
//! `crate::ui` are stateless renderers over `&mut App`.

mod feeds;
mod settings;

pub use feeds::{FeedBundle, FeedKind};
pub use settings::{SettingsRow, SettingsState, settings_rows};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::widgets::TableState;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::alphai::{self, Article};
use crate::config::Config;
use crate::domain::{Interval, Range, TickerData};
use crate::poller::{SharedParams, SharedSource, SourceEvent};
use crate::ui;

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

/// The combos the t and T keys cycle through; wraps at the ends. A startup
/// combo not in the table (e.g. -r 3mo) jumps to the first preset on t and
/// to the last on T.
pub const RANGE_PRESETS: [(Range, Interval); 5] = [
    (Range::D1, Interval::M5),
    (Range::D5, Interval::M15),
    (Range::Mo1, Interval::M60),
    (Range::Mo6, Interval::D1),
    (Range::Y1, Interval::D1),
];

fn next_preset(cur: (Range, Interval), dir: isize) -> (Range, Interval) {
    let n = RANGE_PRESETS.len() as isize;
    match RANGE_PRESETS.iter().position(|&p| p == cur) {
        Some(i) => RANGE_PRESETS[((i as isize + dir).rem_euclid(n)) as usize],
        None if dir > 0 => RANGE_PRESETS[0],
        None => RANGE_PRESETS[RANGE_PRESETS.len() - 1],
    }
}

pub struct AppInit {
    pub symbols: Vec<String>,
    pub source: SharedSource,
    pub source_name: &'static str,
    pub range: Range,
    pub interval: Interval,
    pub params: SharedParams,
    pub rx: UnboundedReceiver<SourceEvent>,
    pub refresh: Arc<Notify>,
    pub alphai_tx: UnboundedSender<alphai::Cmd>,
    pub config: Config,
    pub alphai_enabled: bool,
    pub first_run: bool,
}

pub struct App {
    pub symbols: Vec<String>,
    pub data: HashMap<String, TickerData>,
    pub errors: HashMap<String, String>,
    pub selected: usize,
    pub view_idx: usize,
    pub source_name: &'static str,
    pub range: Range,
    pub interval: Interval,
    pub last_update: Option<DateTime<Local>>,
    pub table_state: TableState,
    // Chart options (session-only, deliberately not persisted)
    pub chart_style: ChartStyle,
    pub show_sma: bool,
    pub show_rsi: bool,
    // AlphaAI feed state: every fetched feed by cache key (news under the
    // symbol/market/trending keys, insider under `ins:SYM`)
    pub feeds: HashMap<String, FeedBundle>,
    pub alphai_errors: HashMap<String, String>,
    pub alphai_enabled: bool,
    pub news_selected: usize,
    pub news_scope: NewsScope,
    pub news_layout: NewsLayout,
    /// Scroll of the embedded card pane (News view); reset on selection moves.
    pub card_scroll: u16,
    pub news_table_state: TableState,
    pub article_overlay: ArticleOverlay,
    pub settings: SettingsState,
    pub config: Config,
    source: SharedSource,
    params: SharedParams,
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
            selected: 0,
            view_idx: ui::view_index(ui::ViewId::Split),
            source_name: init.source_name,
            range: init.range,
            interval: init.interval,
            last_update: None,
            table_state: TableState::default(),
            chart_style: ChartStyle::Candles,
            show_sma: true,
            show_rsi: true,
            feeds: HashMap::new(),
            alphai_errors: HashMap::new(),
            alphai_enabled: init.alphai_enabled,
            news_selected: 0,
            news_scope: NewsScope::default(),
            news_layout: NewsLayout::default(),
            card_scroll: 0,
            news_table_state: TableState::default(),
            article_overlay: ArticleOverlay::default(),
            settings: SettingsState::default(),
            config: init.config,
            source: init.source,
            params: init.params,
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

    fn apply(&mut self, event: SourceEvent) {
        match event {
            SourceEvent::Data { symbol, data } => {
                self.errors.remove(&symbol);
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

    /// Returns true when the app should quit.
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
        let news_view = ui::VIEWS[self.view_idx].navigates_articles();
        let chart_view = ui::VIEWS[self.view_idx].has_chart_panel();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Tab => self.switch_view((self.view_idx + 1) % ui::VIEWS.len()),
            KeyCode::BackTab => {
                self.switch_view((self.view_idx + ui::VIEWS.len() - 1) % ui::VIEWS.len())
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = c as usize - '1' as usize;
                if idx < ui::VIEWS.len() {
                    self.switch_view(idx);
                }
            }
            KeyCode::Char('s') => self.open_settings(),
            KeyCode::Char('r') => self.manual_refresh(),
            // News/Insider: up/down scroll articles, left/right switch ticker.
            KeyCode::Up | KeyCode::Char('k') if news_view => {
                self.news_selected = self.news_selected.saturating_sub(1);
                self.card_scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') if news_view => {
                let max = self.visible_articles().map_or(0, <[Article]>::len);
                if self.news_selected + 1 < max {
                    self.news_selected += 1;
                    self.card_scroll = 0;
                } else {
                    // Already on the last row: page the feed instead.
                    self.request_more_articles();
                }
            }
            KeyCode::Left | KeyCode::Char('h') if news_view => {
                self.selected = self.selected.saturating_sub(1);
                self.news_selected = 0;
                self.card_scroll = 0;
            }
            KeyCode::Right | KeyCode::Char('l') if news_view => {
                self.selected = (self.selected + 1).min(self.symbols.len() - 1);
                self.news_selected = 0;
                self.card_scroll = 0;
            }
            // Card pane scrolling (the list keeps ↑↓/jk).
            KeyCode::PageUp if self.view_id() == ui::ViewId::News => {
                self.card_scroll = self.card_scroll.saturating_sub(10)
            }
            KeyCode::PageDown if self.view_id() == ui::ViewId::News => {
                self.card_scroll = self.card_scroll.saturating_add(10)
            }
            KeyCode::Char('x') if self.view_id() == ui::ViewId::News => {
                self.news_layout = self.news_layout.next();
                self.card_scroll = 0;
            }
            KeyCode::Enter | KeyCode::Char('o') if news_view => {
                if let Some(a) = self
                    .visible_articles()
                    .and_then(|list| list.get(self.news_selected))
                {
                    open_url(&self.article_url(a));
                }
            }
            KeyCode::Char('v')
                if news_view && self.visible_articles().is_some_and(|list| !list.is_empty()) =>
            {
                self.article_overlay = ArticleOverlay { open: true, scroll: 0 };
            }
            KeyCode::Char('f')
                if ui::VIEWS[self.view_idx].feed_shown() == Some(FeedKind::News) =>
            {
                self.news_scope = self.news_scope.next();
                self.news_selected = 0;
                self.card_scroll = 0;
            }
            KeyCode::Char('c') if chart_view => {
                self.chart_style = match self.chart_style {
                    ChartStyle::Candles => ChartStyle::Line,
                    ChartStyle::Line => ChartStyle::Candles,
                }
            }
            KeyCode::Char('m') if chart_view => self.show_sma = !self.show_sma,
            KeyCode::Char('i') if chart_view => self.show_rsi = !self.show_rsi,
            KeyCode::Char('t') => self.cycle_range(1),
            KeyCode::Char('T') => self.cycle_range(-1),
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.symbols.len() - 1)
            }
            _ => {}
        }
        false
    }

    /// Keys while the article card overlay is open; it swallows everything so
    /// list navigation does not shift under the reader.
    fn handle_overlay_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc | KeyCode::Char('v') => self.article_overlay = ArticleOverlay::default(),
            KeyCode::Up | KeyCode::Char('k') => {
                self.article_overlay.scroll = self.article_overlay.scroll.saturating_sub(1)
            }
            // The render pass clamps the scroll to the card's real height.
            KeyCode::Down | KeyCode::Char('j') => {
                self.article_overlay.scroll = self.article_overlay.scroll.saturating_add(1)
            }
            KeyCode::PageUp => {
                self.article_overlay.scroll = self.article_overlay.scroll.saturating_sub(10)
            }
            KeyCode::PageDown => {
                self.article_overlay.scroll = self.article_overlay.scroll.saturating_add(10)
            }
            KeyCode::Enter | KeyCode::Char('o') => {
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
        let (range, interval) = next_preset((self.range, self.interval), dir);
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
        assert_eq!(next_preset(first, 1), RANGE_PRESETS[1]);
        assert_eq!(next_preset(last, 1), first);
        assert_eq!(next_preset(first, -1), last);
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
        assert_eq!(next_preset(odd, 1), RANGE_PRESETS[0]);
        assert_eq!(next_preset(odd, -1), RANGE_PRESETS[RANGE_PRESETS.len() - 1]);
    }
}
