use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use ratatui::widgets::TableState;
use tokio::sync::Notify;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::alphai::{self, Article, InsiderSummary, SentimentSummary};
use crate::config::{self, Config};
use crate::domain::{Interval, Range, TickerData};
use crate::poller::{SharedParams, SharedSource, SourceEvent};
use crate::source::make_source;
use crate::ui;

/// Cached news for one cache key (a symbol, or `alphai::MARKET_KEY`).
pub struct NewsBundle {
    pub articles: Vec<Article>,
    pub sentiment: Option<SentimentSummary>,
    pub fetched: Instant,
}

/// Cached insider feed + 30d rollup for one symbol.
pub struct InsiderBundle {
    pub articles: Vec<Article>,
    pub summary: Option<InsiderSummary>,
    pub fetched: Instant,
}

/// State of the settings overlay. Field order (cursor): price source,
/// finnhub key, alphai key, alpaca key id, alpaca secret, news opens, save.
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
    pub finnhub_key: String,
    pub alphai_key: String,
    pub alpaca_key_id: String,
    pub alpaca_secret: String,
    /// "alphai" (article page on alphai.io) or "original" (source site).
    pub news_open_choice: String,
    pub message: Option<String>,
}

pub const SETTINGS_ROWS: usize = 7;

/// How the price chart draws history: candlesticks or the classic close line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartStyle {
    Candles,
    Line,
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
    pub news_market_wide: bool,
    pub news_table_state: TableState,
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
            view_idx: 0,
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
            news_market_wide: false,
            news_table_state: TableState::default(),
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

    /// Cache key the News view is currently looking at.
    pub fn news_cache_key(&self) -> String {
        if self.news_market_wide {
            alphai::MARKET_KEY.to_string()
        } else {
            self.selected_symbol().to_string()
        }
    }

    /// Articles behind the current News/Insider view, if fetched.
    pub fn visible_articles(&self) -> Option<&[Article]> {
        match self.view_idx {
            ui::VIEW_NEWS => self
                .news
                .get(&self.news_cache_key())
                .map(|b| b.articles.as_slice()),
            ui::VIEW_INSIDER => self
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

    fn apply_alphai(&mut self, event: alphai::Event) {
        match event {
            alphai::Event::News { key, articles, sentiment } => {
                self.inflight.remove(&key);
                self.alphai_errors.remove(&key);
                self.news.insert(
                    key,
                    NewsBundle { articles, sentiment, fetched: Instant::now() },
                );
            }
            alphai::Event::Insider { symbol, articles, summary } => {
                let key = alphai::insider_key(&symbol);
                self.inflight.remove(&key);
                self.alphai_errors.remove(&key);
                self.insider.insert(
                    symbol,
                    InsiderBundle { articles, summary, fetched: Instant::now() },
                );
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
    fn ensure_alphai_data(&mut self) {
        if !self.alphai_enabled || self.settings.open || self.symbols.is_empty() {
            return;
        }
        match self.view_idx {
            // The Split view embeds the compact news strip, so it drives the
            // same demand-driven news fetch as the full News view.
            ui::VIEW_NEWS | ui::VIEW_SPLIT => {
                let key = self.news_cache_key();
                let stale = self
                    .news
                    .get(&key)
                    .is_none_or(|b| b.fetched.elapsed() > alphai::CACHE_TTL);
                if stale && !self.inflight.contains(&key) && !self.alphai_errors.contains_key(&key)
                {
                    self.inflight.insert(key);
                    let symbol =
                        (!self.news_market_wide).then(|| self.selected_symbol().to_string());
                    let _ = self.alphai_tx.send(alphai::Cmd::FetchNews { symbol });
                }
            }
            ui::VIEW_INSIDER => {
                let symbol = self.selected_symbol().to_string();
                let key = alphai::insider_key(&symbol);
                let stale = self
                    .insider
                    .get(&symbol)
                    .is_none_or(|b| b.fetched.elapsed() > alphai::CACHE_TTL);
                if stale && !self.inflight.contains(&key) && !self.alphai_errors.contains_key(&key)
                {
                    self.inflight.insert(key);
                    let _ = self.alphai_tx.send(alphai::Cmd::FetchInsider { symbol });
                }
            }
            _ => {}
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
        let news_view = matches!(self.view_idx, ui::VIEW_NEWS | ui::VIEW_INSIDER);
        let chart_view = matches!(self.view_idx, ui::VIEW_CHART | ui::VIEW_SPLIT);
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
                self.news_selected = self.news_selected.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') if news_view => {
                let max = self.visible_articles().map_or(0, <[Article]>::len);
                self.news_selected = (self.news_selected + 1).min(max.saturating_sub(1));
            }
            KeyCode::Left | KeyCode::Char('h') if news_view => {
                self.selected = self.selected.saturating_sub(1);
                self.news_selected = 0;
            }
            KeyCode::Right | KeyCode::Char('l') if news_view => {
                self.selected = (self.selected + 1).min(self.symbols.len() - 1);
                self.news_selected = 0;
            }
            KeyCode::Enter | KeyCode::Char('o') if news_view => {
                if let Some(a) = self
                    .visible_articles()
                    .and_then(|list| list.get(self.news_selected))
                {
                    open_url(&self.article_url(a));
                }
            }
            KeyCode::Char('f') if matches!(self.view_idx, ui::VIEW_NEWS | ui::VIEW_SPLIT) => {
                self.news_market_wide = !self.news_market_wide;
                self.news_selected = 0;
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

    /// URL Enter opens for an article. News items default to their alphai.io
    /// article page (settings can flip this to the original source); insider
    /// filings always open the original, which points at the SEC filing.
    fn article_url(&self, a: &Article) -> String {
        let url = if self.view_idx == ui::VIEW_NEWS && !self.config.news_open_original() {
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
        match self.view_idx {
            ui::VIEW_NEWS | ui::VIEW_SPLIT => {
                let key = self.news_cache_key();
                self.news.remove(&key);
                self.alphai_errors.remove(&key);
            }
            ui::VIEW_INSIDER => {
                let symbol = self.selected_symbol().to_string();
                self.insider.remove(&symbol);
                self.alphai_errors.remove(&alphai::insider_key(&symbol));
            }
            _ => {}
        }
    }

    // -- settings ----------------------------------------------------------

    pub fn open_settings(&mut self) {
        let s = &mut self.settings;
        s.open = true;
        s.cursor = 0;
        s.editing = false;
        s.message = None;
        s.source_choice = self.source_name.to_string();
        s.finnhub_key = self.config.keys.finnhub.clone().unwrap_or_default();
        s.alphai_key = self.config.keys.alphai.clone().unwrap_or_default();
        s.alpaca_key_id = self.config.keys.alpaca_key_id.clone().unwrap_or_default();
        s.alpaca_secret = self.config.keys.alpaca_secret.clone().unwrap_or_default();
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
                    match s.cursor {
                        1 => s.finnhub_key = value,
                        2 => s.alphai_key = value,
                        3 => s.alpaca_key_id = value,
                        4 => s.alpaca_secret = value,
                        _ => {}
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
                self.settings.cursor = (self.settings.cursor + 1).min(SETTINGS_ROWS - 1)
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if self.settings.cursor == 0 =>
            {
                self.toggle_source_choice()
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                if self.settings.cursor == 5 =>
            {
                self.toggle_news_open_choice()
            }
            KeyCode::Enter => match self.settings.cursor {
                0 => self.toggle_source_choice(),
                5 => self.toggle_news_open_choice(),
                1..=4 => {
                    let s = &mut self.settings;
                    s.input = match s.cursor {
                        1 => s.finnhub_key.clone(),
                        2 => s.alphai_key.clone(),
                        3 => s.alpaca_key_id.clone(),
                        _ => s.alpaca_secret.clone(),
                    };
                    s.editing = true;
                }
                _ => self.settings_save(),
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
        cfg.keys.finnhub = non_empty(&self.settings.finnhub_key);
        cfg.keys.alphai = non_empty(&self.settings.alphai_key);
        cfg.keys.alpaca_key_id = non_empty(&self.settings.alpaca_key_id);
        cfg.keys.alpaca_secret = non_empty(&self.settings.alpaca_secret);
        cfg.news_open = Some(self.settings.news_open_choice.clone());
        // Saving persists the watchlist on screen, so a bare `alphai-tui`
        // reopens exactly this setup.
        cfg.watchlist = self.symbols.clone();

        let source_changed = !self
            .settings
            .source_choice
            .eq_ignore_ascii_case(self.source_name)
            || (self.settings.source_choice == "finnhub"
                && cfg.keys.finnhub != self.config.keys.finnhub)
            || (self.settings.source_choice == "alpaca"
                && (cfg.keys.alpaca_key_id != self.config.keys.alpaca_key_id
                    || cfg.keys.alpaca_secret != self.config.keys.alpaca_secret));
        if source_changed {
            match make_source(
                &self.settings.source_choice,
                cfg.finnhub_key().as_deref(),
                cfg.alpaca_keys(),
            ) {
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

/// Settings source toggle cycle: yahoo -> finnhub -> alpaca -> yahoo.
fn next_source(cur: &str) -> &'static str {
    match cur {
        "yahoo" => "finnhub",
        "finnhub" => "alpaca",
        _ => "yahoo",
    }
}

fn non_empty(v: &str) -> Option<String> {
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
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
        assert_eq!(next_source("yahoo"), "finnhub");
        assert_eq!(next_source("finnhub"), "alpaca");
        assert_eq!(next_source("alpaca"), "yahoo");
        // Anything unexpected resets to the keyless default.
        assert_eq!(next_source("weird"), "yahoo");
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
