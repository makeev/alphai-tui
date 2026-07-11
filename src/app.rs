use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::widgets::TableState;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::alphai::{self, Article, InsiderSummary, SentimentSummary};
use crate::config::{self, ALPHAI_KEY_FIELD, Config, KeyField};
use crate::domain::{Interval, Range, TickerData};
use crate::poller::{SharedParams, SharedSource, SourceEvent};
use crate::source::{make_source, registry};
use crate::ui;

/// Cached news for one cache key (a symbol, or `alphai::MARKET_KEY`).
pub struct NewsBundle {
    pub articles: Vec<Article>,
    pub sentiment: Option<SentimentSummary>,
    /// Cursor for the next (older) page; None = end of the feed (or gated).
    pub next_cursor: Option<String>,
    /// Last load-more failure, shown under the list without dropping it.
    pub page_error: Option<String>,
    /// Paging hit the plan's archive horizon: stop offering more pages.
    pub gated: bool,
    pub fetched: Instant,
}

impl NewsBundle {
    pub fn new(
        articles: Vec<Article>,
        sentiment: Option<SentimentSummary>,
        next_cursor: Option<String>,
    ) -> Self {
        Self {
            articles,
            sentiment,
            next_cursor,
            page_error: None,
            gated: false,
            fetched: Instant::now(),
        }
    }
}

/// Cached insider feed + 30d rollup for one symbol.
pub struct InsiderBundle {
    pub articles: Vec<Article>,
    pub summary: Option<InsiderSummary>,
    pub next_cursor: Option<String>,
    pub page_error: Option<String>,
    pub gated: bool,
    pub fetched: Instant,
}

impl InsiderBundle {
    pub fn new(
        articles: Vec<Article>,
        summary: Option<InsiderSummary>,
        next_cursor: Option<String>,
    ) -> Self {
        Self {
            articles,
            summary,
            next_cursor,
            page_error: None,
            gated: false,
            fetched: Instant::now(),
        }
    }
}

/// State of the settings overlay; the cursor walks `settings_rows()`.
#[derive(Default)]
pub struct SettingsState {
    pub open: bool,
    /// True on the very first launch (no config file yet): the overlay opens
    /// by itself and shows a short welcome text.
    pub first_run: bool,
    pub cursor: usize,
    pub editing: bool,
    pub input: String,
    pub source_choice: String,
    /// Edit buffers for the `Key` rows, by `KeyField::config_name`.
    pub key_values: BTreeMap<&'static str, String>,
    /// "alphai" (article page on alphai.io) or "original" (source site).
    pub news_open_choice: String,
    pub message: Option<String>,
}

/// One row of the settings overlay, in cursor order.
#[derive(Clone, Copy)]
pub enum SettingsRow {
    /// The price-source picker (cycles the registry).
    SourceChoice,
    /// An editable, masked credential.
    Key(&'static KeyField),
    /// Where Enter opens a news article.
    NewsOpen,
    /// The save button.
    Save,
}

/// Rows of the settings overlay: the source picker, every registered
/// source's key fields in registry order, the app-level AlphaAI key, the
/// news-open toggle, Save. Derived from the registry, so a new source's key
/// rows appear (and persist, and mask) with no settings-code changes.
pub fn settings_rows() -> &'static [SettingsRow] {
    static ROWS: LazyLock<Vec<SettingsRow>> = LazyLock::new(|| {
        let mut rows = vec![SettingsRow::SourceChoice];
        rows.extend(
            registry::SOURCES
                .iter()
                .flat_map(|s| s.key_fields)
                .map(SettingsRow::Key),
        );
        rows.push(SettingsRow::Key(&ALPHAI_KEY_FIELD));
        rows.push(SettingsRow::NewsOpen);
        rows.push(SettingsRow::Save);
        rows
    });
    &ROWS
}

/// How the price chart draws history: candlesticks or the classic close line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartStyle {
    Candles,
    Line,
}

/// The AlphaAI feeds a view can display (`View::feed_shown`). Trending is
/// not a kind: it is a news scope, a different cache key of the news feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedKind {
    News,
    Insider,
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
    // AlphaAI news + insider state
    pub news: HashMap<String, NewsBundle>,
    pub insider: HashMap<String, InsiderBundle>,
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
            news: HashMap::new(),
            insider: HashMap::new(),
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

    /// Cache key the News view is currently looking at.
    pub fn news_cache_key(&self) -> String {
        match self.news_scope {
            NewsScope::Ticker => self.selected_symbol().to_string(),
            NewsScope::Market => alphai::MARKET_KEY.to_string(),
            NewsScope::Trending => alphai::TRENDING_KEY.to_string(),
        }
    }

    /// Whether a fetch for this cache key is in flight (drives the
    /// "loading…" hint under the feed lists).
    pub(crate) fn is_loading(&self, key: &str) -> bool {
        self.inflight.contains(key)
    }

    /// Articles behind the current News/Insider view, if fetched.
    pub fn visible_articles(&self) -> Option<&[Article]> {
        match self.view_id() {
            ui::ViewId::News => self
                .news
                .get(&self.news_cache_key())
                .map(|b| b.articles.as_slice()),
            ui::ViewId::Insider => self
                .insider
                .get(self.selected_symbol())
                .map(|b| b.articles.as_slice()),
            _ => None,
        }
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

    pub(crate) fn apply_alphai(&mut self, event: alphai::Event) {
        match event {
            alphai::Event::News { key, articles, sentiment, next_cursor, append } => {
                self.inflight.remove(&key);
                self.alphai_errors.remove(&key);
                match self.news.get_mut(&key) {
                    // A page extends the shown feed (sentiment stays from the
                    // head fetch); a fresh fetch replaces the bundle.
                    Some(b) if append => {
                        b.next_cursor = next_cursor;
                        b.page_error = None;
                        append_page(&mut b.articles, articles);
                    }
                    _ => {
                        self.news.insert(key, NewsBundle::new(articles, sentiment, next_cursor));
                    }
                }
            }
            alphai::Event::Insider { symbol, articles, summary, next_cursor, append } => {
                let key = alphai::insider_key(&symbol);
                self.inflight.remove(&key);
                self.alphai_errors.remove(&key);
                match self.insider.get_mut(&symbol) {
                    Some(b) if append => {
                        b.next_cursor = next_cursor;
                        b.page_error = None;
                        append_page(&mut b.articles, articles);
                    }
                    _ => {
                        self.insider
                            .insert(symbol, InsiderBundle::new(articles, summary, next_cursor));
                    }
                }
            }
            alphai::Event::PageError { key, error, gated } => {
                self.inflight.remove(&key);
                let (page_error, bundle_gated, next_cursor) =
                    match key.strip_prefix("ins:") {
                        Some(symbol) => match self.insider.get_mut(symbol) {
                            Some(b) => (&mut b.page_error, &mut b.gated, &mut b.next_cursor),
                            None => return,
                        },
                        None => match self.news.get_mut(&key) {
                            Some(b) => (&mut b.page_error, &mut b.gated, &mut b.next_cursor),
                            None => return,
                        },
                    };
                *page_error = Some(error);
                if gated {
                    *bundle_gated = true;
                    *next_cursor = None;
                }
            }
            alphai::Event::Error { key, error } => {
                self.inflight.remove(&key);
                self.alphai_errors.insert(key, error);
            }
        }
    }

    /// Demand-driven AlphaAI fetching: only the data behind the visible view,
    /// only when missing or older than `CACHE_TTL`, never while a fetch for
    /// the same key is in flight, and never on top of an error (manual `r`
    /// clears the error and retries) — the free tier is 100 requests/day.
    pub(crate) fn ensure_alphai_data(&mut self) {
        // The overlay gate also keeps a TTL refetch from swapping the article
        // out from under the reader mid-scroll.
        if !self.alphai_enabled
            || self.settings.open
            || self.article_overlay.open
            || self.symbols.is_empty()
        {
            return;
        }
        // A TTL refetch replaces the whole bundle, dropping loaded pages, so
        // it waits until the reader is back at the top row (missing bundles
        // fetch regardless; the Split strip has no selection and stays at 0).
        let at_top = self.news_selected == 0;
        match self.view_id() {
            // The Split view embeds the compact news strip, so it drives the
            // same demand-driven news fetch as the full News view.
            ui::ViewId::News | ui::ViewId::Split => {
                let key = self.news_cache_key();
                let stale = match self.news.get(&key) {
                    None => true,
                    Some(b) => at_top && b.fetched.elapsed() > alphai::CACHE_TTL,
                };
                if stale && !self.inflight.contains(&key) && !self.alphai_errors.contains_key(&key)
                {
                    self.inflight.insert(key);
                    let cmd = match self.news_scope {
                        NewsScope::Ticker => alphai::Cmd::FetchNews {
                            symbol: Some(self.selected_symbol().to_string()),
                            cursor: None,
                        },
                        NewsScope::Market => {
                            alphai::Cmd::FetchNews { symbol: None, cursor: None }
                        }
                        NewsScope::Trending => alphai::Cmd::FetchTrending,
                    };
                    let _ = self.alphai_tx.send(cmd);
                }
            }
            ui::ViewId::Insider => {
                let symbol = self.selected_symbol().to_string();
                let key = alphai::insider_key(&symbol);
                let stale = match self.insider.get(&symbol) {
                    None => true,
                    Some(b) => at_top && b.fetched.elapsed() > alphai::CACHE_TTL,
                };
                if stale && !self.inflight.contains(&key) && !self.alphai_errors.contains_key(&key)
                {
                    self.inflight.insert(key);
                    let _ = self
                        .alphai_tx
                        .send(alphai::Cmd::FetchInsider { symbol, cursor: None });
                }
            }
            _ => {}
        }
    }

    /// j at the last row: ask for the feed's next page (explicitly
    /// user-driven, one request per keypress at most; the shared `inflight`
    /// key also blocks a concurrent TTL refetch of the same feed).
    fn request_more_articles(&mut self) {
        let (key, cmd) = match self.view_id() {
            ui::ViewId::News => {
                let key = self.news_cache_key();
                let Some(cursor) = self.news.get(&key).and_then(|b| b.next_cursor.clone()) else {
                    return;
                };
                let symbol = (self.news_scope == NewsScope::Ticker)
                    .then(|| self.selected_symbol().to_string());
                (key, alphai::Cmd::FetchNews { symbol, cursor: Some(cursor) })
            }
            ui::ViewId::Insider => {
                let symbol = self.selected_symbol().to_string();
                let key = alphai::insider_key(&symbol);
                let Some(cursor) = self
                    .insider
                    .get(&symbol)
                    .and_then(|b| b.next_cursor.clone())
                else {
                    return;
                };
                (key, alphai::Cmd::FetchInsider { symbol, cursor: Some(cursor) })
            }
            _ => return,
        };
        if self.inflight.contains(&key) {
            return;
        }
        self.inflight.insert(key);
        let _ = self.alphai_tx.send(cmd);
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

    /// `r`: immediate price cycle, plus drop the visible AlphaAI bundle (and
    /// any error) so it refetches — this is also the retry path after 401/429.
    fn manual_refresh(&mut self) {
        self.refresh.notify_one();
        match self.view_id() {
            ui::ViewId::News | ui::ViewId::Split => {
                let key = self.news_cache_key();
                self.news.remove(&key);
                self.alphai_errors.remove(&key);
            }
            ui::ViewId::Insider => {
                let symbol = self.selected_symbol().to_string();
                self.insider.remove(&symbol);
                self.alphai_errors.remove(&alphai::insider_key(&symbol));
            }
            _ => {}
        }
    }

    // -- settings ----------------------------------------------------------

    pub fn open_settings(&mut self) {
        let key_values = settings_rows()
            .iter()
            .filter_map(|row| match row {
                SettingsRow::Key(field) => Some((
                    field.config_name,
                    self.config.keys.get(field.config_name).cloned().unwrap_or_default(),
                )),
                _ => None,
            })
            .collect();
        let s = &mut self.settings;
        s.open = true;
        s.cursor = 0;
        s.editing = false;
        s.message = None;
        s.source_choice = self.source_name.to_string();
        s.key_values = key_values;
        s.news_open_choice = if self.config.news_open_original() {
            "original".to_string()
        } else {
            "alphai".to_string()
        };
    }

    fn handle_settings_key(&mut self, key: KeyEvent) -> bool {
        if self.settings.editing {
            let s = &mut self.settings;
            match key.code {
                KeyCode::Enter => {
                    let value = s.input.trim().to_string();
                    if let SettingsRow::Key(field) = settings_rows()[s.cursor] {
                        s.key_values.insert(field.config_name, value);
                    }
                    s.editing = false;
                }
                KeyCode::Esc => s.editing = false,
                KeyCode::Backspace => {
                    s.input.pop();
                }
                KeyCode::Char(c) if !c.is_control() && !c.is_whitespace() => s.input.push(c),
                _ => {}
            }
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc => {
                self.settings.open = false;
                self.settings.first_run = false;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings.cursor = self.settings.cursor.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.settings.cursor = (self.settings.cursor + 1).min(settings_rows().len() - 1)
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                match settings_rows()[self.settings.cursor] {
                    SettingsRow::SourceChoice => self.toggle_source_choice(),
                    SettingsRow::NewsOpen => self.toggle_news_open_choice(),
                    _ => {}
                }
            }
            KeyCode::Enter => match settings_rows()[self.settings.cursor] {
                SettingsRow::SourceChoice => self.toggle_source_choice(),
                SettingsRow::NewsOpen => self.toggle_news_open_choice(),
                SettingsRow::Key(field) => {
                    let s = &mut self.settings;
                    s.input = s.key_values.get(field.config_name).cloned().unwrap_or_default();
                    s.editing = true;
                }
                SettingsRow::Save => self.settings_save(),
            },
            _ => {}
        }
        false
    }

    fn toggle_source_choice(&mut self) {
        let s = &mut self.settings;
        s.source_choice = next_source(&s.source_choice).to_string();
    }

    fn toggle_news_open_choice(&mut self) {
        let s = &mut self.settings;
        s.news_open_choice = if s.news_open_choice == "original" {
            "alphai".to_string()
        } else {
            "original".to_string()
        };
    }

    fn settings_save(&mut self) {
        let mut cfg = self.config.clone();
        cfg.source = Some(self.settings.source_choice.clone());
        // A cleared key leaves the file entirely instead of writing "".
        for (name, value) in &self.settings.key_values {
            let value = value.trim();
            if value.is_empty() {
                cfg.keys.remove(*name);
            } else {
                cfg.keys.insert((*name).to_string(), value.to_string());
            }
        }
        cfg.news_open = Some(self.settings.news_open_choice.clone());
        // Saving persists the watchlist on screen, so a bare `alphai-tui`
        // reopens exactly this setup.
        cfg.watchlist = self.symbols.clone();

        // A swap to another source, or an edit to the selected source's own
        // keys, rebuilds it. Comparing the env-layered values means editing
        // a file key that an env var shadows does not trigger a rebuild.
        let keys_changed = registry::find(&self.settings.source_choice).is_some_and(|info| {
            info.key_fields
                .iter()
                .any(|field| cfg.key_value(field) != self.config.key_value(field))
        });
        let source_changed = !self
            .settings
            .source_choice
            .eq_ignore_ascii_case(self.source_name)
            || keys_changed;
        if source_changed {
            match make_source(&self.settings.source_choice, &cfg) {
                Ok(src) => {
                    self.source_name = src.name();
                    *self.source.write().unwrap() = src;
                    self.data.clear();
                    self.errors.clear();
                    self.refresh.notify_one();
                }
                Err(e) => {
                    self.settings.message = Some(format!("{e:#}"));
                    return;
                }
            }
        }

        if cfg.alphai_key() != self.config.alphai_key() {
            let key = cfg.alphai_key();
            self.alphai_enabled = key.is_some();
            let _ = self.alphai_tx.send(alphai::Cmd::SetKey(key));
            self.news.clear();
            self.insider.clear();
            self.alphai_errors.clear();
            self.inflight.clear();
        }

        match config::save(&cfg) {
            Ok(_) => {
                self.config = cfg;
                self.settings.open = false;
                self.settings.first_run = false;
            }
            Err(e) => {
                // Applied live but not persisted; keep the overlay open so the
                // problem is visible.
                self.config = cfg;
                self.settings.message = Some(format!("could not write config: {e:#}"));
            }
        }
    }
}

/// Extend a feed with the next page, dropping rows already shown (a fresh
/// article can shift the window between requests and repeat on the page
/// boundary). Rows without a uid cannot be matched and are kept.
fn append_page(articles: &mut Vec<Article>, page: Vec<Article>) {
    let seen: HashSet<String> = articles
        .iter()
        .map(|a| a.original.uid.clone())
        .filter(|uid| !uid.is_empty())
        .collect();
    articles.extend(
        page.into_iter()
            .filter(|a| a.original.uid.is_empty() || !seen.contains(&a.original.uid)),
    );
}

/// Settings source toggle: cycles the registry in order; unknown names
/// reset to the keyless default.
fn next_source(cur: &str) -> &'static str {
    match registry::SOURCES.iter().position(|s| s.id == cur) {
        Some(i) => registry::SOURCES[(i + 1) % registry::SOURCES.len()].id,
        None => registry::SOURCES[0].id,
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
    use super::{RANGE_PRESETS, next_preset, next_source, with_utm};
    use crate::domain::{Interval, Range};

    #[test]
    fn source_cycle_covers_all_and_wraps() {
        use crate::source::registry::SOURCES;
        let mut cur = SOURCES[0].id;
        let mut seen = vec![cur];
        for _ in 1..SOURCES.len() {
            cur = next_source(cur);
            seen.push(cur);
        }
        // Every registered source is reachable exactly once, then it wraps.
        let mut ids: Vec<&str> = SOURCES.iter().map(|s| s.id).collect();
        seen.sort_unstable();
        ids.sort_unstable();
        assert_eq!(seen, ids);
        assert_eq!(next_source(cur), SOURCES[0].id);
        // Anything unexpected resets to the keyless default.
        assert_eq!(next_source("weird"), SOURCES[0].id);
    }

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
