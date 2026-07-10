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
use crate::poller::{SharedSource, SourceEvent};
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
/// finnhub key, alphai key, save.
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
    pub message: Option<String>,
}

pub const SETTINGS_ROWS: usize = 4;

pub struct AppInit {
    pub symbols: Vec<String>,
    pub source: SharedSource,
    pub source_name: &'static str,
    pub range: Range,
    pub interval: Interval,
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
            ui::VIEW_NEWS => {
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
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return true;
        }
        if self.settings.open {
            return self.handle_settings_key(key);
        }
        let news_view = matches!(self.view_idx, ui::VIEW_NEWS | ui::VIEW_INSIDER);
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
                    open_url(&a.original.url);
                }
            }
            KeyCode::Char('f') if self.view_idx == ui::VIEW_NEWS => {
                self.news_market_wide = !self.news_market_wide;
                self.news_selected = 0;
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.symbols.len() - 1)
            }
            _ => {}
        }
        false
    }

    fn switch_view(&mut self, idx: usize) {
        if idx != self.view_idx {
            self.view_idx = idx;
            self.news_selected = 0;
        }
    }

    /// `r`: immediate price cycle, plus drop the visible AlphaAI bundle (and
    /// any error) so it refetches — this is also the retry path after 401/429.
    fn manual_refresh(&mut self) {
        self.refresh.notify_one();
        match self.view_idx {
            ui::VIEW_NEWS => {
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
            KeyCode::Enter => match self.settings.cursor {
                0 => self.toggle_source_choice(),
                1 | 2 => {
                    let s = &mut self.settings;
                    s.input = if s.cursor == 1 {
                        s.finnhub_key.clone()
                    } else {
                        s.alphai_key.clone()
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
        s.source_choice = if s.source_choice == "finnhub" {
            "yahoo".into()
        } else {
            "finnhub".into()
        };
    }

    fn settings_save(&mut self) {
        let mut cfg = self.config.clone();
        cfg.source = Some(self.settings.source_choice.clone());
        cfg.keys.finnhub = non_empty(&self.settings.finnhub_key);
        cfg.keys.alphai = non_empty(&self.settings.alphai_key);
        // Saving persists the watchlist on screen, so a bare `alphai-tui`
        // reopens exactly this setup.
        cfg.watchlist = self.symbols.clone();

        let source_changed = !self
            .settings
            .source_choice
            .eq_ignore_ascii_case(self.source_name)
            || (self.settings.source_choice == "finnhub"
                && cfg.keys.finnhub != self.config.keys.finnhub);
        if source_changed {
            match make_source(&self.settings.source_choice, cfg.finnhub_key().as_deref()) {
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

fn non_empty(v: &str) -> Option<String> {
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
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
