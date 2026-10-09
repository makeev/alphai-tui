use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::alphai;
use crate::app::{ChartStyle, InsiderChartWindow, NewsLayout, NewsScope, RANGE_PRESETS};
use crate::domain::{Interval, Range};
use crate::indicators::{self, MaType};
use crate::keymap::Keymap;
use crate::portfolio::Position;
use crate::theme::{Panels, Theme};
use crate::ui;

/// Used when neither the CLI nor the config file provides symbols.
pub const DEFAULT_WATCHLIST: [&str; 4] = ["AAPL", "MSFT", "NVDA", "BTC-USD"];

/// One credential a price source (or the app itself) needs. `config_name`
/// is the field name inside `[keys]` in config.toml; existing names are
/// frozen, renaming one would orphan the key in users' files. The env var
/// wins over the file at resolution time.
pub struct KeyField {
    pub config_name: &'static str,
    pub env_var: &'static str,
    /// Row label in the settings screen.
    pub label: &'static str,
}

/// The AlphAI news key: app-level rather than a price source, but it lives
/// in the same `[keys]` table and the same settings list.
pub const ALPHAI_KEY_FIELD: KeyField = KeyField {
    config_name: "alphai",
    env_var: "ALPHAI_API_KEY",
    label: "AlphAI key",
};

/// Persisted app settings. Precedence at use time: CLI args > env vars
/// (for API keys) > this file > built-in defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub source: Option<String>,
    pub watchlist: Vec<String>,
    pub every: Option<u64>,
    pub range: Option<String>,
    pub interval: Option<String>,
    /// Whether a source that stops answering is swapped for one that still
    /// does (default true). Yahoo blocks by IP for tens of minutes and no
    /// retry can shorten that, so the only repair is another provider; set
    /// it false to keep the errors and choose by hand.
    pub source_fallback: Option<bool>,
    /// Where pre and post market prices come from: "auto" (the default,
    /// also when absent), "same", "alpaca" or "yahoo". See
    /// `source::extended::ExtendedSource`.
    pub extended_source: Option<String>,
    /// Where Enter opens a news article: "alphai" (article page on
    /// alphai.io, the default) or "original" (the source site).
    pub news_open: Option<String>,
    /// API keys by `KeyField::config_name`. A map rather than a struct so a
    /// key this binary does not know (say, a config written by a newer
    /// version) survives a load -> save round trip instead of being dropped.
    pub keys: BTreeMap<String, String>,
    /// `[ui]` startup defaults (view, news layout and scope).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ui: Option<UiConfig>,
    /// `[chart]` startup defaults and indicator parameters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chart: Option<ChartConfig>,
    /// `[theme]` color overrides by slot name (see `theme::Theme`). Kept as
    /// raw strings: validation happens in `resolve`, per slot, so one typo
    /// never degrades the whole file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<BTreeMap<String, String>>,
    /// `[keybindings]` overrides by action name (see `keymap::ACTIONS`).
    /// Raw specs; `resolve` validates per entry, so one bad key never
    /// degrades the whole file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keybindings: Option<BTreeMap<String, KeysSpec>>,
    /// `[[positions]]`: what the user holds, one entry per ticker, with
    /// the quantity and the average price paid. Raw entries, validated in
    /// `resolve` per entry like every other section. Declared last on
    /// purpose: `toml` serializes in field order, and a scalar written
    /// after an array of tables would be parsed back as part of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub positions: Vec<PositionEntry>,
}

impl Config {
    /// Writes one holding into `[[positions]]`, over the entry for the
    /// same ticker in whatever case it was typed, or at the end. Fields
    /// this binary does not know stay with the entry.
    pub fn set_position(&mut self, position: &Position) {
        let entry = PositionEntry::from(position);
        match self
            .positions
            .iter_mut()
            .find(|e| e.is_for(&position.symbol))
        {
            Some(slot) => {
                *slot = PositionEntry {
                    other: std::mem::take(&mut slot.other),
                    ..entry
                }
            }
            None => self.positions.push(entry),
        }
    }

    /// Drops every entry for the ticker: a duplicate left behind would
    /// bring the holding back on the next start.
    pub fn clear_position(&mut self, symbol: &str) {
        self.positions.retain(|e| !e.is_for(symbol));
    }
}

/// One change a keypress makes to the file: `a`, `d` or `p`. Kept as a
/// change rather than as the resulting list, so it can be applied to the
/// file as another process left it, and replayed after a failed write.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    Watch(String),
    Unwatch(String),
    Hold(Position),
    Clear(String),
}

impl Edit {
    /// Each edit sets the state of one ticker, so replaying a run of them
    /// over a file that already has some comes out the same. `live` is the
    /// watchlist on screen: an empty list in the file means the built-in
    /// default, so the screen's list is what gets written, and the same
    /// goes for a list an edit would empty, since the screen always holds
    /// at least one ticker.
    pub fn apply(&self, cfg: &mut Config, live: &[String]) {
        match self {
            Self::Hold(position) => return cfg.set_position(position),
            Self::Clear(symbol) => return cfg.clear_position(symbol),
            Self::Watch(_) | Self::Unwatch(_) if cfg.watchlist.is_empty() => {
                cfg.watchlist = live.to_vec();
                return;
            }
            Self::Watch(symbol) => {
                if !cfg.watchlist.iter().any(|s| s.eq_ignore_ascii_case(symbol)) {
                    cfg.watchlist.push(symbol.clone());
                }
            }
            Self::Unwatch(symbol) => cfg.watchlist.retain(|s| !s.eq_ignore_ascii_case(symbol)),
        }
        if cfg.watchlist.is_empty() {
            cfg.watchlist = live.to_vec();
        }
    }
}

/// One `[[positions]]` entry as the file has it. Each field is a raw TOML
/// value, so a missing or mistyped one drops this entry with a warning in
/// `resolve` instead of failing the whole file and the keys with it, and
/// the entry is written back as typed until someone fixes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PositionEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<toml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qty: Option<toml::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_price: Option<toml::Value>,
    /// Whatever else the entry holds, a misspelled `avg_prcie` included:
    /// dropping it on the next save would lose the number with the typo.
    #[serde(flatten)]
    pub other: toml::Table,
}

impl PositionEntry {
    fn symbol(&self) -> Option<String> {
        let symbol = self.symbol.as_ref()?.as_str()?.trim().to_uppercase();
        (!symbol.is_empty()).then_some(symbol)
    }

    fn is_for(&self, symbol: &str) -> bool {
        self.symbol()
            .is_some_and(|own| own.eq_ignore_ascii_case(symbol))
    }

    /// A whole number reads as well as a float: `qty = 12` is how people
    /// write a share count.
    fn number(value: Option<&toml::Value>) -> Option<f64> {
        match value? {
            toml::Value::Float(f) => Some(*f),
            toml::Value::Integer(i) => Some(*i as f64),
            _ => None,
        }
    }
}

impl From<&Position> for PositionEntry {
    fn from(p: &Position) -> Self {
        Self {
            symbol: Some(toml::Value::String(p.symbol.clone())),
            qty: Some(toml::Value::Float(p.qty)),
            avg_price: Some(toml::Value::Float(p.avg_price)),
            other: toml::Table::new(),
        }
    }
}

/// Keys of one `[keybindings]` action: a bare string or a list of them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeysSpec {
    One(String),
    Many(Vec<String>),
}

impl KeysSpec {
    fn as_list(&self) -> Vec<&str> {
        match self {
            Self::One(s) => vec![s.as_str()],
            Self::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub default_view: Option<String>,
    pub news_layout: Option<String>,
    pub news_scope: Option<String>,
    /// Panel look: "rounded" (the default) or "plain" frame lines, or
    /// "none" for tinted panels without lines.
    pub borders: Option<String>,
    /// Raw i64 rather than u8: an out-of-range number must degrade to a
    /// warning in `resolve`, not fail deserializing the whole file.
    pub news_min_score: Option<i64>,
    pub insider_min_score: Option<i64>,
    /// Whether the quote rail under the header is drawn (default true).
    pub quote_rail: Option<bool>,
    /// Start in bare mode: no header, no footer (default false).
    pub bare: Option<bool>,
    /// Price color fades and the refresh spinner (default true).
    pub animations: Option<bool>,
    /// Allow synchronized frames when terminfo advertises support.
    pub synchronized_output: Option<bool>,
    /// Startup window of the Insider view's chart panel: "3m" (default),
    /// "12m" or "off"; the g key cycles it live.
    pub insider_chart: Option<String>,
    /// How long fetched AlphAI data (news, sentiment, insider) stays fresh
    /// before the visible view re-fetches it. Seconds; raw i64 for the same
    /// warning-not-error reason as the scores.
    pub alphai_ttl_secs: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChartConfig {
    pub timezone: Option<String>,
    pub session_shading: Option<bool>,
    pub time_grid: Option<bool>,
    pub style: Option<String>,
    pub sma: Option<bool>,
    pub ma_type: Option<String>,
    pub rsi: Option<bool>,
    pub volume: Option<bool>,
    /// Draw pre and post market candles too (Yahoo and Alpaca).
    pub extended_hours: Option<bool>,
    /// Mark the ticker's news on the price chart.
    pub news_markers: Option<bool>,
    pub sma_fast: Option<usize>,
    pub sma_slow: Option<usize>,
    pub rsi_period: Option<usize>,
    /// Percent of the plot width kept free right of the newest candle (the
    /// last-price marker lives there). Raw i64 so an out-of-range number
    /// degrades to a warning in `resolve`, not a whole-file parse error.
    pub right_margin_pct: Option<i64>,
    /// The t/T cycle, as pairs like `["1d", "5m"]`.
    pub presets: Option<Vec<Vec<String>>>,
}

/// Everything derived and validated from the raw config. Semantic problems
/// in these sections warn and fall back per entry; only a TOML syntax error
/// degrades the whole file (in `load_at`).
pub struct Resolved {
    pub theme: Theme,
    /// Which preset `theme` was built from, for the settings row and the
    /// live cycle key.
    pub theme_name: &'static str,
    pub chart: ChartDefaults,
    pub ui: UiDefaults,
    pub keymap: Keymap,
    /// `[[positions]]`, validated; empty when the section is absent.
    pub positions: Vec<Position>,
}

/// Validated `[chart]` values; `Default` is the traditional look. The
/// session keys (c, m, i, t) still toggle everything live, these only seed
/// the startup state and the indicator math.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartDefaults {
    pub timezone: ChartTimezone,
    pub session_shading: bool,
    pub time_grid: bool,
    pub style: ChartStyle,
    pub sma: bool,
    /// Simple or exponential; the periods below serve whichever is picked.
    pub ma_type: MaType,
    pub rsi: bool,
    pub volume: bool,
    /// Start with premarket and after-hours candles drawn.
    pub extended_hours: bool,
    /// Start with the news marks on the price chart. They are drawn from
    /// the news the app already holds for the ticker, so this costs no
    /// request either way.
    pub news_markers: bool,
    pub sma_fast: usize,
    pub sma_slow: usize,
    pub rsi_period: usize,
    /// 0 disables the margin (and the last-price marker drawn in it).
    pub right_margin_pct: u16,
    pub presets: Vec<(Range, Interval)>,
}

impl Default for ChartDefaults {
    fn default() -> Self {
        Self {
            timezone: ChartTimezone::Exchange,
            session_shading: true,
            time_grid: true,
            style: ChartStyle::Candles,
            sma: true,
            ma_type: MaType::Sma,
            rsi: true,
            volume: true,
            extended_hours: true,
            news_markers: true,
            sma_fast: indicators::SMA_FAST,
            sma_slow: indicators::SMA_SLOW,
            rsi_period: indicators::RSI_PERIOD,
            right_margin_pct: DEFAULT_RIGHT_MARGIN_PCT,
            presets: RANGE_PRESETS.to_vec(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartTimezone {
    Exchange,
    Local,
    Utc,
}

/// Startup default of `[chart] right_margin_pct`: a fifth of the plot stays
/// free right of the newest candle, so it never sits glued to the border and
/// the last-price marker has room to live.
pub const DEFAULT_RIGHT_MARGIN_PCT: u16 = 20;

/// Accepted `[chart] right_margin_pct` values; anything past half the plot
/// would squeeze the candles more than it helps.
pub const RIGHT_MARGIN_RANGE: RangeInclusive<i64> = 0..=50;

/// Startup default of the news score filter: relevance 7 and up (the API
/// itself defaults to 4; +/- adjust it live, `[ui] news_min_score` seeds it).
pub const DEFAULT_NEWS_MIN_SCORE: u8 = 7;

/// Startup default of the insider score filter. Insider rows are scored from
/// the event's dollar value, so this is a trade-size filter; 4 matches the
/// server default and keeps every filing the feed used to show.
pub const DEFAULT_INSIDER_MIN_SCORE: u8 = 4;

/// Accepted `[ui] alphai_ttl_secs` values. The floor keeps a typo (or "5"
/// meant as minutes) from burning the free request budget; the ceiling is
/// one day, past which the value is surely a mistake.
pub const ALPHAI_TTL_RANGE: RangeInclusive<i64> = 30..=86_400;

/// Validated `[ui]` startup values.
#[derive(Clone, Debug, PartialEq)]
pub struct UiDefaults {
    pub view_idx: usize,
    /// The quote rail under the header; file-only, like the border style.
    pub quote_rail: bool,
    /// Start without the header and footer; `--bare` also turns it on.
    pub bare: bool,
    pub animations: bool,
    pub synchronized_output: bool,
    pub news_layout: NewsLayout,
    pub news_scope: NewsScope,
    pub news_min_score: u8,
    pub insider_min_score: u8,
    pub insider_chart: InsiderChartWindow,
    pub alphai_ttl: Duration,
}

impl Default for UiDefaults {
    fn default() -> Self {
        Self {
            view_idx: ui::view_index(ui::ViewId::Split),
            quote_rail: true,
            bare: false,
            animations: true,
            synchronized_output: true,
            news_layout: NewsLayout::default(),
            news_scope: NewsScope::default(),
            news_min_score: DEFAULT_NEWS_MIN_SCORE,
            insider_min_score: DEFAULT_INSIDER_MIN_SCORE,
            insider_chart: InsiderChartWindow::default(),
            alphai_ttl: alphai::CACHE_TTL,
        }
    }
}

/// Validate the raw config into ready-to-use values plus human-readable
/// warnings (printed to stderr before the TUI starts).
/// `cli_theme` is `--theme`; it wins over `[theme] preset`, like every
/// other CLI value.
pub fn resolve(cfg: &Config, cli_theme: Option<&str>) -> (Resolved, Vec<String>) {
    let mut warnings = Vec::new();
    let (mut theme, theme_name) = Theme::resolve(cfg.theme.as_ref(), cli_theme, &mut warnings);
    theme.panels = resolve_borders(
        cfg.ui.as_ref().and_then(|u| u.borders.as_deref()),
        &mut warnings,
    );
    let chart = resolve_chart(cfg.chart.as_ref(), &mut warnings);
    let ui = resolve_ui(cfg.ui.as_ref(), &mut warnings);
    let keymap = Keymap::from_config(
        cfg.keybindings
            .iter()
            .flatten()
            .map(|(name, spec)| (name.as_str(), spec.as_list())),
        &mut warnings,
    );
    let positions = resolve_positions(&cfg.positions, &mut warnings);
    resolve_extended_source(cfg, &mut warnings);
    (
        Resolved {
            theme,
            theme_name,
            chart,
            ui,
            keymap,
            positions,
        },
        warnings,
    )
}

/// `extended_source`: an unknown value warns and reads as "auto"; "alpaca"
/// without Alpaca keys warns and reads as the price source's own data,
/// rather than quietly asking another provider.
fn resolve_extended_source(cfg: &Config, warnings: &mut Vec<String>) {
    use crate::source::extended::ExtendedSource;
    let Some(raw) = cfg.extended_source.as_deref() else {
        return;
    };
    match ExtendedSource::parse(raw) {
        None => warnings.push(format!(
            "extended_source = \"{raw}\": expected one of {}, using auto",
            ExtendedSource::ALL.map(ExtendedSource::name).join(", ")
        )),
        Some(ExtendedSource::Alpaca) if crate::source::alpaca_keys(cfg).is_none() => warnings.push(
            "extended_source = \"alpaca\" needs the Alpaca keys, using the price source's own pre and post market"
                .to_string(),
        ),
        Some(_) => {}
    }
}

/// `[[positions]]`: a broken entry warns and is dropped, the rest stand.
/// Symbols are upper-cased the way the watchlist is, so a holding written
/// in lower case still meets its quote.
fn resolve_positions(raw: &[PositionEntry], warnings: &mut Vec<String>) -> Vec<Position> {
    let mut out: Vec<Position> = Vec::new();
    for entry in raw {
        let Some(symbol) = entry.symbol() else {
            warnings.push("[[positions]]: an entry has no symbol, skipping it".to_string());
            continue;
        };
        let Some(qty) =
            PositionEntry::number(entry.qty.as_ref()).filter(|q| q.is_finite() && *q != 0.0)
        else {
            warnings.push(format!(
                "[[positions]] {symbol}: qty must be a non-zero number, skipping it"
            ));
            continue;
        };
        let Some(avg_price) =
            PositionEntry::number(entry.avg_price.as_ref()).filter(|p| p.is_finite() && *p >= 0.0)
        else {
            warnings.push(format!(
                "[[positions]] {symbol}: avg_price must be a number, zero or more, skipping it"
            ));
            continue;
        };
        if out.iter().any(|kept| kept.symbol == symbol) {
            warnings.push(format!(
                "[[positions]] {symbol}: listed twice, keeping the first entry"
            ));
            continue;
        }
        out.push(Position {
            symbol,
            qty,
            avg_price,
        });
    }
    out
}

/// `[ui] borders`: the line set frames draw with (`rounded`, the default,
/// or `plain`), or `none` for tinted panels without lines. Lives in `[ui]` rather than `[theme]`
/// because it is not a color, but it resolves onto the theme, which every
/// renderer already has at hand.
fn resolve_borders(raw: Option<&str>, warnings: &mut Vec<String>) -> Panels {
    let Some(raw) = raw else {
        return Panels::default();
    };
    Panels::ALL
        .into_iter()
        .find(|p| p.name().eq_ignore_ascii_case(raw.trim()))
        .unwrap_or_else(|| {
            warnings.push(format!(
                "[ui] borders: unknown \"{raw}\" (rounded, plain or none), keeping rounded"
            ));
            Panels::default()
        })
}

fn resolve_chart(raw: Option<&ChartConfig>, warnings: &mut Vec<String>) -> ChartDefaults {
    let mut out = ChartDefaults::default();
    let Some(raw) = raw else { return out };
    if let Some(zone) = &raw.timezone {
        match zone.to_lowercase().as_str() {
            "exchange" | "et" => out.timezone = ChartTimezone::Exchange,
            "local" => out.timezone = ChartTimezone::Local,
            "utc" => out.timezone = ChartTimezone::Utc,
            _ => warnings.push(format!(
                "[chart] timezone: unknown \"{zone}\" (exchange, local or utc), keeping exchange"
            )),
        }
    }
    if let Some(v) = raw.session_shading {
        out.session_shading = v;
    }
    if let Some(v) = raw.time_grid {
        out.time_grid = v;
    }
    if let Some(style) = &raw.style {
        match style.to_lowercase().as_str() {
            "candles" => out.style = ChartStyle::Candles,
            "line" => out.style = ChartStyle::Line,
            other => warnings.push(format!(
                "[chart] style: unknown \"{other}\" (candles or line), keeping candles"
            )),
        }
    }
    if let Some(v) = raw.sma {
        out.sma = v;
    }
    if let Some(kind) = &raw.ma_type {
        match kind.to_lowercase().as_str() {
            "sma" => out.ma_type = MaType::Sma,
            "ema" => out.ma_type = MaType::Ema,
            other => warnings.push(format!(
                "[chart] ma_type: unknown \"{other}\" (sma or ema), keeping sma"
            )),
        }
    }
    if let Some(v) = raw.rsi {
        out.rsi = v;
    }
    if let Some(v) = raw.volume {
        out.volume = v;
    }
    if let Some(v) = raw.extended_hours {
        out.extended_hours = v;
    }
    if let Some(v) = raw.news_markers {
        out.news_markers = v;
    }
    out.sma_fast = period(raw.sma_fast, out.sma_fast, "sma_fast", 2..=250, warnings);
    out.sma_slow = period(raw.sma_slow, out.sma_slow, "sma_slow", 2..=250, warnings);
    out.rsi_period = period(
        raw.rsi_period,
        out.rsi_period,
        "rsi_period",
        2..=100,
        warnings,
    );
    if let Some(pct) = raw.right_margin_pct {
        if RIGHT_MARGIN_RANGE.contains(&pct) {
            out.right_margin_pct = pct as u16;
        } else {
            warnings.push(format!(
                "[chart] right_margin_pct: {pct} is outside {}..={}, keeping {}",
                RIGHT_MARGIN_RANGE.start(),
                RIGHT_MARGIN_RANGE.end(),
                DEFAULT_RIGHT_MARGIN_PCT
            ));
        }
    }
    if let Some(rows) = &raw.presets {
        let mut presets = Vec::new();
        for row in rows {
            match parse_preset(row) {
                Some(p) => presets.push(p),
                None => warnings.push(format!(
                    "[chart] presets: bad pair {row:?}, expected like [\"1d\", \"5m\"]"
                )),
            }
        }
        if presets.is_empty() {
            warnings.push("[chart] presets: no valid pairs, keeping the built-in cycle".into());
        } else {
            out.presets = presets;
        }
    }
    out
}

fn period(
    raw: Option<usize>,
    default: usize,
    name: &str,
    valid: RangeInclusive<usize>,
    warnings: &mut Vec<String>,
) -> usize {
    match raw {
        Some(v) if valid.contains(&v) => v,
        Some(v) => {
            warnings.push(format!(
                "[chart] {name}: {v} is outside {}..={}, keeping {default}",
                valid.start(),
                valid.end()
            ));
            default
        }
        None => default,
    }
}

fn parse_preset(row: &[String]) -> Option<(Range, Interval)> {
    let [range, interval] = row else { return None };
    Some((
        Range::from_str(range, true).ok()?,
        Interval::from_str(interval, true).ok()?,
    ))
}

fn resolve_ui(raw: Option<&UiConfig>, warnings: &mut Vec<String>) -> UiDefaults {
    let mut out = UiDefaults::default();
    let Some(raw) = raw else { return out };
    if let Some(name) = &raw.default_view {
        // Matched by title, so the list stays correct as views register.
        match ui::VIEWS
            .iter()
            .position(|v| v.title().eq_ignore_ascii_case(name))
        {
            Some(idx) => out.view_idx = idx,
            None => {
                let names: Vec<String> =
                    ui::VIEWS.iter().map(|v| v.title().to_lowercase()).collect();
                warnings.push(format!(
                    "[ui] default_view: unknown \"{name}\" (one of {}), keeping split",
                    names.join(", ")
                ));
            }
        }
    }
    if let Some(on) = raw.quote_rail {
        out.quote_rail = on;
    }
    if let Some(on) = raw.bare {
        out.bare = on;
    }
    if let Some(on) = raw.animations {
        out.animations = on;
    }
    if let Some(on) = raw.synchronized_output {
        out.synchronized_output = on;
    }
    if let Some(layout) = &raw.news_layout {
        match NewsLayout::from_name(layout) {
            Some(l) => out.news_layout = l,
            None => warnings.push(format!(
                "[ui] news_layout: unknown \"{layout}\" (chart, side or stacked), keeping chart"
            )),
        }
    }
    if let Some(scope) = &raw.news_scope {
        match scope.to_lowercase().as_str() {
            "ticker" => out.news_scope = NewsScope::Ticker,
            "market" => out.news_scope = NewsScope::Market,
            "trending" => out.news_scope = NewsScope::Trending,
            other => warnings.push(format!(
                "[ui] news_scope: unknown \"{other}\" (ticker, market or trending), keeping ticker"
            )),
        }
    }
    if let Some(window) = &raw.insider_chart {
        match window.to_lowercase().as_str() {
            "off" => out.insider_chart = InsiderChartWindow::Off,
            "3m" => out.insider_chart = InsiderChartWindow::M3,
            "12m" => out.insider_chart = InsiderChartWindow::M12,
            other => warnings.push(format!(
                "[ui] insider_chart: unknown \"{other}\" (3m, 12m or off), keeping 3m"
            )),
        }
    }
    out.news_min_score = min_score(
        raw.news_min_score,
        out.news_min_score,
        "news_min_score",
        warnings,
    );
    out.insider_min_score = min_score(
        raw.insider_min_score,
        out.insider_min_score,
        "insider_min_score",
        warnings,
    );
    if let Some(secs) = raw.alphai_ttl_secs {
        if ALPHAI_TTL_RANGE.contains(&secs) {
            out.alphai_ttl = Duration::from_secs(secs as u64);
        } else {
            warnings.push(format!(
                "[ui] alphai_ttl_secs: {secs} is outside {}..={}, keeping {}",
                ALPHAI_TTL_RANGE.start(),
                ALPHAI_TTL_RANGE.end(),
                alphai::CACHE_TTL.as_secs()
            ));
        }
    }
    out
}

/// One `[ui] *_min_score` entry: 1..=10 or a warning plus the default.
fn min_score(raw: Option<i64>, default: u8, name: &str, warnings: &mut Vec<String>) -> u8 {
    match raw {
        Some(n) if (1..=10).contains(&n) => n as u8,
        Some(n) => {
            warnings.push(format!(
                "[ui] {name}: {n} is outside 1..=10, keeping {default}"
            ));
            default
        }
        None => default,
    }
}

impl Config {
    /// Resolved credential for one key field: the env var wins so a
    /// shell-exported key keeps working after a config file appears; blank
    /// values count as unset.
    pub fn key_value(&self, field: &KeyField) -> Option<String> {
        env_key(field.env_var).or_else(|| {
            self.keys
                .get(field.config_name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        })
    }

    pub fn alphai_key(&self) -> Option<String> {
        self.key_value(&ALPHAI_KEY_FIELD)
    }

    /// True when Enter on a news article should open the original source
    /// instead of the alphai.io article page.
    pub fn news_open_original(&self) -> bool {
        self.news_open.as_deref() == Some("original")
    }
}

fn env_key(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Unix (including macOS) follows the terminal-tool convention:
/// `$XDG_CONFIG_HOME` or `~/.config`. Windows uses the platform config dir.
pub fn path() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        dirs::config_dir()?
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or(dirs::home_dir()?.join(".config"))
    };
    Some(base.join("alphai-tui").join("config.toml"))
}

/// What `load_at` found at the config path.
#[derive(Clone, Debug, PartialEq)]
pub enum FileState {
    /// No file yet: a first run.
    Missing,
    Loaded,
    /// A file that does not load, with the reason. The session runs on the
    /// defaults, and nothing writes over the file while it stays that way
    /// (Save in the settings screen keeps a copy first): the keys typed
    /// into it would go with it.
    Broken(String),
}

/// Load from an explicit path (the `--config` flag) or the default
/// location. Returns the config, what was found, and the path Save must
/// write back to (None when the platform has no config dir). A missing
/// file is a normal first run; an unreadable one degrades to defaults with
/// a stderr warning rather than blocking startup.
pub fn load_at(override_path: Option<&Path>) -> (Config, FileState, Option<PathBuf>) {
    let Some(p) = override_path.map(Path::to_path_buf).or_else(path) else {
        return (Config::default(), FileState::Missing, None);
    };
    match load_from(&p) {
        Ok(Some(cfg)) => (cfg, FileState::Loaded, Some(p)),
        Ok(None) => (Config::default(), FileState::Missing, Some(p)),
        Err(e) => {
            eprintln!("warning: ignoring bad config at {}: {e:#}", p.display());
            (
                Config::default(),
                FileState::Broken(load_error(&e)),
                Some(p),
            )
        }
    }
}

pub fn load_from(p: &Path) -> Result<Option<Config>> {
    if !p.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(p).context("read failed")?;
    Ok(Some(toml::from_str(&raw).context("parse failed")?))
}

/// One line on why `load_from` failed, short enough for the footer: the
/// TOML error's first line names the line and column, the rest of it is a
/// drawing of the spot.
fn load_error(e: &anyhow::Error) -> String {
    let cause = e.root_cause().to_string();
    cause.lines().next().unwrap_or_default().trim().to_string()
}

/// Copies a config that does not load to `<name>.broken` before an
/// explicit Save replaces it, so what was typed into it, the keys first,
/// is still on disk to copy back.
fn set_aside(p: &Path) -> Result<PathBuf> {
    let mut aside = p.as_os_str().to_owned();
    aside.push(".broken");
    let aside = PathBuf::from(aside);
    std::fs::copy(p, &aside).context("could not keep a copy of the config that did not load")?;
    Ok(aside)
}

/// The config file's name for a message, or a generic one without a path.
pub fn file_label(path: Option<&Path>) -> String {
    path.and_then(Path::file_name).map_or_else(
        || "The config file".to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Holds `<config>.lock` for one read, change and write of the config.
/// Two processes on one file (tmux panes) each read the same version
/// otherwise, and the later write drops what the earlier one added: the
/// atomic rename keeps the file whole, not the other change. The lock
/// file stays beside the config; released when this is dropped.
struct Lock {
    _file: std::fs::File,
}

/// A write holds the lock for milliseconds. One held much longer belongs
/// to a process that is stuck, and the keypress should say so rather than
/// freeze the screen.
const LOCK_WAIT: Duration = Duration::from_secs(2);

fn lock(p: &Path) -> Result<Lock> {
    lock_within(p, LOCK_WAIT)
}

fn lock_within(p: &Path, wait: Duration) -> Result<Lock> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).context("create config dir failed")?;
    }
    let mut name = p.as_os_str().to_owned();
    name.push(".lock");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options
        .open(name)
        .context("could not open the config lock")?;
    let deadline = std::time::Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Lock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                anyhow::bail!("another alphai-tui held {} too long", file_label(Some(p)))
            }
            // A filesystem without locks (some network mounts) saved fine
            // before there was a lock, and still does, just unguarded.
            Err(std::fs::TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Unsupported => {
                return Ok(Lock { _file: file });
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).context("could not lock the config");
            }
        }
    }
}

/// Applies `change` to the config as it is on disk and writes it back,
/// all under the lock, and returns what was written. `missing` stands in
/// when there is no file yet. A file that does not load is left alone
/// rather than replaced by the defaults a session runs on, keys and all.
pub fn update(p: &Path, missing: &Config, change: impl Fn(&mut Config)) -> Result<Config> {
    let _lock = lock(p)?;
    let mut on_disk = match load_from(p) {
        Ok(Some(cfg)) => cfg,
        Ok(None) => missing.clone(),
        Err(e) => anyhow::bail!("{} did not load ({})", file_label(Some(p)), load_error(&e)),
    };
    change(&mut on_disk);
    save_to(p, &on_disk)?;
    Ok(on_disk)
}

/// Save in the settings screen: writes `cfg` whole, under the lock. The
/// holdings are not on that screen, so they come from the file as it is
/// now, with `pending` (the keypress edits that never reached it) on top.
/// A file that does not load is copied aside first, and the copy's path
/// returned, since what is on screen is about to replace it.
/// `on_defaults` is a session that started on a file that did not load:
/// its `cfg` holds the defaults, so if the file loads now it was fixed
/// while the app ran, and writing would put those defaults over the fix,
/// keys and every section the settings screen does not show.
pub fn save_whole(
    p: &Path,
    cfg: &mut Config,
    on_defaults: bool,
    pending: impl Fn(&mut Config),
) -> Result<Option<PathBuf>> {
    let _lock = lock(p)?;
    let aside = match load_from(p) {
        Ok(Some(_)) if on_defaults => anyhow::bail!(
            "{} loads now, restart alphai-tui to pick it up",
            file_label(Some(p))
        ),
        Ok(Some(on_disk)) => {
            cfg.positions = on_disk.positions;
            pending(cfg);
            None
        }
        Ok(None) => None,
        Err(_) => Some(set_aside(p)?),
    };
    save_to(p, cfg)?;
    Ok(aside)
}

/// Save to the path `load_at` resolved (the settings screen passes it back).
pub fn save_at(path: Option<&Path>, cfg: &Config) -> Result<()> {
    let p = path.context("no config directory on this platform")?;
    save_to(p, cfg)
}

/// The file may hold API keys, so it is created user-only (0600) on unix.
pub fn save_to(p: &Path, cfg: &Config) -> Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).context("create config dir failed")?;
    }
    let raw = toml::to_string_pretty(cfg).context("serialize failed")?;
    // Written beside the target and renamed over it. The position prompt
    // saves on every edit, so a torn write is no longer a once-a-session
    // risk, and this file also holds the API keys.
    let tmp = p.with_extension(format!("toml.tmp{}", std::process::id()));
    let saved = write_private(&tmp, &raw)
        .context("write failed")
        .and_then(|()| std::fs::rename(&tmp, p).context("replace failed"));
    if saved.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    saved
}

/// Creates `path` user-only before a byte of it is written: a mode set
/// after the write left the keys readable to other local users in
/// between, and a failed chmod went unnoticed.
fn write_private(path: &Path, raw: &str) -> std::io::Result<()> {
    use std::io::Write;
    // A leftover from a run that died mid-save (its pid may come round
    // again) would keep its own mode, so it goes first.
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    file.write_all(raw.as_bytes())?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::BorderType;

    /// A key field whose env var can never be set, so tests exercise the
    /// file side of the resolution deterministically.
    const TEST_FIELD: KeyField = KeyField {
        config_name: "finnhub",
        env_var: "ALPHAI_TUI_TEST_NEVER_SET",
        label: "test",
    };

    /// Every section validates per entry: a broken holding warns and is
    /// dropped, the rest of the list still loads.
    #[test]
    fn bad_positions_warn_and_are_dropped() {
        let cfg: Config = toml::from_str(
            r#"
[[positions]]
symbol = "aapl"
qty = 12
avg_price = 182.31

[[positions]]
symbol = "MSFT"
qty = 0
avg_price = 400

[[positions]]
symbol = "NVDA"
qty = 3
avg_price = -1

[[positions]]
symbol = "AAPL"
qty = 5
avg_price = 100
"#,
        )
        .unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(resolved.positions.len(), 1);
        // Upper-cased like the watchlist, or the holding would never meet
        // its quote.
        assert_eq!(resolved.positions[0].symbol, "AAPL");
        assert_eq!(resolved.positions[0].qty, 12.0);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("MSFT")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("NVDA")), "{warnings:?}");
        assert!(warnings.iter().any(|w| w.contains("twice")), "{warnings:?}");
    }

    /// A typo reads as auto, and alpaca without its keys as the source's
    /// own data, each with a warning; a good value says nothing.
    #[test]
    fn extended_source_warns_per_value() {
        let warn = |value: &str, keys: &[(&str, &str)]| {
            let cfg = Config {
                extended_source: Some(value.into()),
                keys: keys
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                ..Config::default()
            };
            resolve(&cfg, None).1
        };
        assert!(warn("yahoo", &[]).is_empty());
        assert!(warn("Same", &[]).is_empty());
        let typo = warn("sip", &[]);
        assert!(
            typo.iter().any(|w| w.contains("auto, same, alpaca, yahoo")),
            "{typo:?}"
        );
        if std::env::var("APCA_API_KEY_ID").is_err() {
            let keyless = warn("alpaca", &[]);
            assert!(
                keyless.iter().any(|w| w.contains("needs the Alpaca keys")),
                "{keyless:?}"
            );
        }
        let keyed = [("alpaca_key_id", "PKTEST"), ("alpaca_secret", "secret")];
        assert!(warn("alpaca", &keyed).is_empty());
    }

    #[test]
    fn round_trip() {
        let dir = std::env::temp_dir().join(format!("alphai-tui-test-{}", std::process::id()));
        let p = dir.join("config.toml");
        let cfg = Config {
            source: Some("finnhub".into()),
            watchlist: vec!["AAPL".into(), "BTC-USD".into()],
            every: Some(30),
            range: Some("5d".into()),
            interval: Some("15m".into()),
            source_fallback: Some(false),
            extended_source: Some("yahoo".into()),
            news_open: Some("original".into()),
            positions: vec![PositionEntry::from(&Position {
                symbol: "AAPL".into(),
                qty: 12.0,
                avg_price: 182.31,
            })],
            keys: BTreeMap::from([
                ("finnhub".to_string(), "fh-key".to_string()),
                ("alphai".to_string(), "ak_live_x".to_string()),
                ("alpaca_key_id".to_string(), "PKTEST123".to_string()),
                ("alpaca_secret".to_string(), "alpaca-secret-x".to_string()),
            ]),
            ui: Some(UiConfig {
                default_view: Some("news".into()),
                news_layout: Some("stacked".into()),
                news_scope: None,
                borders: Some("plain".into()),
                news_min_score: Some(6),
                insider_min_score: Some(5),
                insider_chart: Some("12m".into()),
                alphai_ttl_secs: Some(120),
                quote_rail: Some(false),
                bare: Some(true),
                animations: Some(false),
                synchronized_output: Some(false),
            }),
            chart: Some(ChartConfig {
                style: Some("line".into()),
                sma_slow: Some(200),
                right_margin_pct: Some(25),
                presets: Some(vec![vec!["1d".into(), "5m".into()]]),
                ..Default::default()
            }),
            theme: Some(BTreeMap::from([(
                "accent".to_string(),
                "magenta".to_string(),
            )])),
            keybindings: Some(BTreeMap::from([
                ("quit".to_string(), KeysSpec::One("ctrl-q".into())),
                (
                    "open".to_string(),
                    KeysSpec::Many(vec!["enter".into(), "w".into()]),
                ),
            ])),
        };
        save_to(&p, &cfg).unwrap();
        let loaded = load_from(&p).unwrap().unwrap();
        assert_eq!(loaded, cfg);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("alphai-tui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    /// A holding missing a field, or with a word for a number, used to fail
    /// the whole file: the keys and the watchlist went with it. Now that
    /// entry alone is skipped, and a save writes it back as it was typed.
    #[test]
    fn a_broken_position_is_skipped_alone_and_kept_in_the_file() {
        let raw = r#"
watchlist = ["AAPL"]

[keys]
alphai = "ak_live_x"

[[positions]]
symbol = "AAPL"
qty = 12
avg_prcie = 180

[[positions]]
symbol = "MSFT"
qty = "ten"
avg_price = 400

[[positions]]
symbol = "NVDA"
qty = 3
avg_price = 100.5
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(
            cfg.keys.get("alphai").map(String::as_str),
            Some("ak_live_x")
        );
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(resolved.positions.len(), 1);
        assert_eq!(resolved.positions[0].symbol, "NVDA");
        assert_eq!(resolved.positions[0].qty, 3.0);
        assert!(
            warnings.iter().any(|w| w.contains("AAPL: avg_price")),
            "{warnings:?}"
        );
        assert!(
            warnings.iter().any(|w| w.contains("MSFT: qty")),
            "{warnings:?}"
        );

        let p = scratch("broken-entry");
        save_to(&p, &cfg).unwrap();
        let loaded = load_from(&p).unwrap().unwrap();
        assert_eq!(loaded.positions, cfg.positions);
        assert_eq!(loaded.positions[0].avg_price, None);
        // The misspelled field is the number someone typed: it stays.
        let written = std::fs::read_to_string(&p).unwrap();
        assert!(written.contains("avg_prcie = 180"), "file:\n{written}");

        // So does it when the prompt writes the holding over that entry.
        let mut fixed = loaded;
        fixed.set_position(&Position {
            symbol: "AAPL".into(),
            qty: 12.0,
            avg_price: 180.0,
        });
        assert_eq!(
            fixed.positions[0].avg_price,
            Some(toml::Value::Float(180.0))
        );
        assert_eq!(
            fixed.positions[0].other.get("avg_prcie"),
            Some(&toml::Value::Integer(180))
        );
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// The prompt's write replaces the entry for its ticker however it was
    /// typed, a broken one included, and clearing takes out duplicates too.
    #[test]
    fn set_and_clear_position_find_the_ticker_in_any_case() {
        let mut cfg: Config = toml::from_str(
            r#"
[[positions]]
symbol = "aapl"
qty = 12

[[positions]]
symbol = "MSFT"
qty = 1
avg_price = 400

[[positions]]
symbol = "AAPL"
qty = 5
avg_price = 100
"#,
        )
        .unwrap();
        let held = Position {
            symbol: "AAPL".into(),
            qty: 10.0,
            avg_price: 180.0,
        };
        cfg.set_position(&held);
        assert_eq!(cfg.positions.len(), 3);
        assert_eq!(cfg.positions[0], PositionEntry::from(&held));
        cfg.clear_position("AAPL");
        assert_eq!(cfg.positions.len(), 1);
        assert!(cfg.positions[0].is_for("MSFT"));
    }

    /// A file that does not parse is not a first run: it says why, and it
    /// is copied aside rather than lost when Save replaces it.
    #[test]
    fn a_config_that_does_not_parse_is_broken_not_missing() {
        let p = scratch("broken-file");
        assert_eq!(load_at(Some(&p)).1, FileState::Missing);

        let raw = "every = \"often\"\n[keys]\nalphai = \"ak_live_x\"\n";
        std::fs::write(&p, raw).unwrap();
        let (cfg, state, path) = load_at(Some(&p));
        assert_eq!(cfg, Config::default());
        assert_eq!(path.as_deref(), Some(p.as_path()));
        let FileState::Broken(why) = state else {
            panic!("not reported as broken: {state:?}");
        };
        assert!(why.contains("line 1"), "{why}");
        assert!(!why.contains('\n'), "{why}");

        let aside = set_aside(&p).unwrap();
        assert_eq!(aside.file_name().unwrap(), "config.toml.broken");
        assert_eq!(std::fs::read_to_string(&aside).unwrap(), raw);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// The lock is what keeps two panes' changes apart: held, another
    /// taker waits and then gives up with a reason; let go, it is free.
    #[test]
    fn the_config_lock_is_held_until_dropped() {
        let p = scratch("lock");
        let held = lock(&p).unwrap();
        let refused = lock_within(&p, Duration::from_millis(30))
            .err()
            .expect("lock was shared");
        assert!(
            format!("{refused:#}").contains("held config.toml"),
            "{refused:#}"
        );
        drop(held);
        lock_within(&p, Duration::from_millis(30)).unwrap();
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// Writers that each add their own tickers at the same time: every
    /// one of them ends up in the file. Without the lock two of them read
    /// the same version and the later write drops the earlier ticker.
    #[test]
    fn concurrent_updates_keep_every_change() {
        let p = scratch("concurrent");
        save_to(
            &p,
            &Config {
                watchlist: vec!["AAPL".into()],
                ..Config::default()
            },
        )
        .unwrap();
        std::thread::scope(|scope| {
            for writer in 0..4 {
                let p = &p;
                scope.spawn(move || {
                    for n in 0..10 {
                        let edit = Edit::Watch(format!("T{writer}X{n}"));
                        update(p, &Config::default(), |cfg| edit.apply(cfg, &[])).unwrap();
                    }
                });
            }
        });
        let saved = load_from(&p).unwrap().unwrap();
        assert_eq!(saved.watchlist.len(), 41, "{:?}", saved.watchlist);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    /// The file holds keys: it is user-only from its first byte, and a
    /// temp file a crashed save left behind does not lend it its mode.
    #[cfg(unix)]
    #[test]
    fn a_saved_config_is_private_from_the_first_byte() {
        use std::os::unix::fs::PermissionsExt;
        let p = scratch("private");
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let tmp = p.with_extension(format!("toml.tmp{}", std::process::id()));
        let leave_over = || {
            std::fs::write(&tmp, "left over").unwrap();
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        };
        // The temp file itself, before any rename: writing into a leftover
        // kept its 0644 for as long as the keys sat in it.
        leave_over();
        write_private(&tmp, "alphai = \"ak_live_x\"").unwrap();
        assert_eq!(mode(&tmp), 0o600);

        leave_over();

        let cfg = Config {
            keys: BTreeMap::from([("alphai".to_string(), "ak_live_x".to_string())]),
            ..Config::default()
        };
        save_to(&p, &cfg).unwrap();
        assert_eq!(mode(&p), 0o600);
        assert!(!tmp.exists(), "the temp file was left behind");
        assert_eq!(load_from(&p).unwrap().unwrap(), cfg);
        std::fs::remove_dir_all(p.parent().unwrap()).ok();
    }

    #[test]
    fn missing_file_is_none() {
        let p = Path::new("/nonexistent/alphai-tui/config.toml");
        assert!(load_from(p).unwrap().is_none());
    }

    #[test]
    fn partial_file_fills_defaults() {
        let cfg: Config = toml::from_str("source = \"yahoo\"").unwrap();
        assert_eq!(cfg.source.as_deref(), Some("yahoo"));
        assert!(cfg.watchlist.is_empty());
        assert!(cfg.keys.is_empty());
    }

    /// The exact `[keys]` shape written by pre-registry versions (and shown
    /// in the README) keeps parsing; those field names are frozen.
    #[test]
    fn old_keys_table_parses_verbatim() {
        let cfg: Config = toml::from_str(
            r#"
            source = "yahoo"
            [keys]
            alphai = "ak_live_x"
            finnhub = ""
            alpaca_key_id = "PK123"
            alpaca_secret = "sec"
            "#,
        )
        .unwrap();
        assert_eq!(
            cfg.keys.get("alphai").map(String::as_str),
            Some("ak_live_x")
        );
        assert_eq!(
            cfg.keys.get("alpaca_secret").map(String::as_str),
            Some("sec")
        );
        // A blank file value counts as unset at resolution time.
        assert_eq!(cfg.key_value(&TEST_FIELD), None);
    }

    #[test]
    fn key_value_reads_and_trims_file_values() {
        let cfg: Config = toml::from_str("[keys]\nfinnhub = \" fh-key \"").unwrap();
        assert_eq!(cfg.key_value(&TEST_FIELD).as_deref(), Some("fh-key"));
        assert_eq!(Config::default().key_value(&TEST_FIELD), None);
    }

    /// A `[keys]` entry this binary does not know must survive load -> save
    /// (a struct with named fields would silently drop it).
    #[test]
    fn unknown_key_survives_round_trip() {
        let cfg: Config = toml::from_str("[keys]\nnewsource = \"k\"").unwrap();
        let raw = toml::to_string_pretty(&cfg).unwrap();
        let again: Config = toml::from_str(&raw).unwrap();
        assert_eq!(again.keys.get("newsource").map(String::as_str), Some("k"));
    }

    /// Unknown sections and keys are tolerated on load: a typo never takes
    /// the whole file (and its API keys) down.
    #[test]
    fn unknown_sections_and_fields_are_tolerated() {
        let cfg: Config =
            toml::from_str("nonsense = 1\n[thme]\naccent = \"red\"").expect("must parse");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn theme_section_resolves_and_stays_absent_by_default() {
        let cfg: Config = toml::from_str("[theme]\naccent = \"magenta\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.theme.accent, ratatui::style::Color::Magenta);

        let (_, warnings) = resolve(
            &toml::from_str::<Config>("[theme]\nup = \"banana\"").unwrap(),
            None,
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");

        // Absent sections must not serialize: Save would spray empty tables
        // into every config otherwise.
        let bare = toml::to_string_pretty(&Config::default()).unwrap();
        for section in ["[theme]", "[ui]", "[chart]", "[keybindings]"] {
            assert!(!bare.contains(section), "{bare}");
        }
    }

    /// --theme beats the file, like every other CLI value, and an unknown
    /// name warns instead of failing.
    #[test]
    fn cli_theme_overrides_the_config_preset() {
        let cfg: Config = toml::from_str("[theme]\npreset = \"nord\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.theme_name, "nord");

        let (resolved, warnings) = resolve(&cfg, Some("Dracula"));
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.theme_name, "dracula");

        let (resolved, warnings) = resolve(&cfg, Some("nosuchtheme"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("--theme"), "{warnings:?}");
        assert_eq!(resolved.theme_name, crate::theme::DEFAULT_PRESET);
    }

    #[test]
    fn borders_setting_resolves_onto_the_theme() {
        let cfg: Config = toml::from_str("[ui]\nborders = \"plain\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.theme.panels, Panels::Lines(BorderType::Plain));

        // Unknown value warns and keeps the rounded default; so does no
        // section at all (silently).
        let cfg: Config = toml::from_str("[ui]\nborders = \"fancy\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(resolved.theme.panels, Panels::Lines(BorderType::Rounded));
        let (resolved, warnings) = resolve(&Config::default(), None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.theme.panels, Panels::Lines(BorderType::Rounded));

        let cfg: Config = toml::from_str("[ui]\nborders = \"none\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.theme.panels, Panels::Surface);
    }

    #[test]
    fn keybindings_section_resolves_per_entry() {
        use crate::keymap::Action;
        use crossterm::event::{KeyCode, KeyEvent};

        // A bare string and a list both parse; the keymap follows.
        let cfg: Config =
            toml::from_str("[keybindings]\nrefresh = \"f5\"\nopen = [\"enter\", \"w\"]").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::F(5))),
            Some(Action::Refresh)
        );
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('w'))),
            Some(Action::Open)
        );
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('r'))),
            None
        );

        // A bad entry warns and keeps the default; the good one still lands.
        let cfg: Config = toml::from_str("[keybindings]\nquit = \"supr\"\ncard = \"u\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('u'))),
            Some(Action::Card)
        );

        // No section at all: the untouched defaults.
        let (resolved, warnings) = resolve(&Config::default(), None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
    }

    #[test]
    fn chart_section_validates_per_entry() {
        let cfg: Config = toml::from_str(
            r#"
            [chart]
            style = "line"
            sma_fast = 50
            sma_slow = 9000
            rsi_period = 21
            presets = [["1d", "5m"], ["nope", "5m"], ["1y"]]
            "#,
        )
        .unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(resolved.chart.style, ChartStyle::Line);
        assert_eq!(resolved.chart.sma_fast, 50);
        // Out of range: warned, default kept.
        assert_eq!(resolved.chart.sma_slow, indicators::SMA_SLOW);
        assert_eq!(resolved.chart.rsi_period, 21);
        // One valid pair survives; the two bad ones warn.
        assert_eq!(resolved.chart.presets, vec![(Range::D1, Interval::M5)]);
        assert_eq!(warnings.len(), 3, "{warnings:?}");

        // All presets bad: fall back to the built-in cycle.
        let cfg: Config = toml::from_str("[chart]\npresets = [[\"x\", \"y\"]]").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(resolved.chart.presets, RANGE_PRESETS.to_vec());
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    /// The two panel switches and the average kind: a bad value warns and
    /// keeps the default, exactly like `style`.
    #[test]
    fn chart_panel_and_ma_type_validate_per_entry() {
        let cfg: Config = toml::from_str("[chart]\nvolume = false\nma_type = \"EMA\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!resolved.chart.volume);
        assert_eq!(resolved.chart.ma_type, MaType::Ema);

        let cfg: Config = toml::from_str("[chart]\nma_type = \"wilder\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(resolved.chart.ma_type, MaType::Sma);
        assert_eq!(warnings.len(), 1, "{warnings:?}");

        // Absent section: panels on, averages simple.
        let (resolved, _) = resolve(&Config::default(), None);
        assert!(resolved.chart.volume);
        assert_eq!(resolved.chart.ma_type, MaType::Sma);
    }

    #[test]
    fn right_margin_pct_validates_per_entry() {
        // The full accepted span, including 0 (margin off).
        for (raw, want) in [("0", 0u16), ("20", 20), ("50", 50)] {
            let cfg: Config =
                toml::from_str(&format!("[chart]\nright_margin_pct = {raw}")).unwrap();
            let (resolved, warnings) = resolve(&cfg, None);
            assert!(warnings.is_empty(), "at {raw}: {warnings:?}");
            assert_eq!(resolved.chart.right_margin_pct, want, "at {raw}");
        }

        // Out of range (including negatives, which the raw i64 must survive):
        // warn and keep the default.
        for bad in ["-1", "51", "200"] {
            let cfg: Config =
                toml::from_str(&format!("[chart]\nright_margin_pct = {bad}")).expect("must parse");
            let (resolved, warnings) = resolve(&cfg, None);
            assert_eq!(
                resolved.chart.right_margin_pct, DEFAULT_RIGHT_MARGIN_PCT,
                "at {bad}"
            );
            assert_eq!(warnings.len(), 1, "at {bad}: {warnings:?}");
        }

        assert_eq!(
            ChartDefaults::default().right_margin_pct,
            DEFAULT_RIGHT_MARGIN_PCT
        );
    }

    #[test]
    fn ui_section_validates_per_entry() {
        let cfg: Config = toml::from_str(
            "[ui]\ndefault_view = \"News\"\nnews_layout = \"stacked\"\nnews_scope = \"market\"",
        )
        .unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.ui.view_idx, ui::view_index(ui::ViewId::News));
        assert_eq!(resolved.ui.news_layout, NewsLayout::Stacked);
        assert_eq!(resolved.ui.news_scope, NewsScope::Market);

        let cfg: Config = toml::from_str("[ui]\ndefault_view = \"nwes\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(resolved.ui.view_idx, ui::view_index(ui::ViewId::Split));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("nwes"), "{warnings:?}");
    }

    #[test]
    fn ui_insider_chart_window_parses_and_warns() {
        for (raw, want) in [
            ("off", InsiderChartWindow::Off),
            ("3m", InsiderChartWindow::M3),
            ("12M", InsiderChartWindow::M12),
        ] {
            let cfg: Config = toml::from_str(&format!("[ui]\ninsider_chart = \"{raw}\"")).unwrap();
            let (resolved, warnings) = resolve(&cfg, None);
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(resolved.ui.insider_chart, want);
        }
        // Junk warns and keeps the default window.
        let cfg: Config = toml::from_str("[ui]\ninsider_chart = \"6m\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("insider_chart"), "{warnings:?}");
        assert_eq!(resolved.ui.insider_chart, InsiderChartWindow::M3);
    }

    #[test]
    fn ui_quote_rail_defaults_on_and_switches_off() {
        assert!(UiDefaults::default().quote_rail);
        let cfg: Config = toml::from_str("[ui]\nquote_rail = false").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(!resolved.ui.quote_rail);
    }

    #[test]
    fn ui_bare_defaults_off_and_switches_on() {
        assert!(!UiDefaults::default().bare);
        let cfg: Config = toml::from_str("[ui]\nbare = true").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(resolved.ui.bare);
    }

    #[test]
    fn news_min_score_validates_per_entry() {
        let cfg: Config =
            toml::from_str("[ui]\nnews_min_score = 4\ninsider_min_score = 8").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.ui.news_min_score, 4);
        assert_eq!(resolved.ui.insider_min_score, 8);

        // Out of range warns and keeps the default; a number outside u8 must
        // not fail the whole file (the raw field is i64 for exactly that).
        for bad in ["0", "11", "300", "-2"] {
            let cfg: Config =
                toml::from_str(&format!("[ui]\nnews_min_score = {bad}")).expect("must parse");
            let (resolved, warnings) = resolve(&cfg, None);
            assert_eq!(
                resolved.ui.news_min_score, DEFAULT_NEWS_MIN_SCORE,
                "at {bad}"
            );
            assert_eq!(warnings.len(), 1, "at {bad}: {warnings:?}");
        }

        let defaults = UiDefaults::default();
        assert_eq!(defaults.news_min_score, DEFAULT_NEWS_MIN_SCORE);
        assert_eq!(defaults.insider_min_score, DEFAULT_INSIDER_MIN_SCORE);
    }

    #[test]
    fn alphai_ttl_validates_per_entry() {
        let cfg: Config = toml::from_str("[ui]\nalphai_ttl_secs = 60").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.ui.alphai_ttl, Duration::from_secs(60));

        // Below the budget floor, above a day, or negative: warn and keep
        // the default. "5" is the likely minutes-vs-seconds slip.
        for bad in ["5", "29", "86401", "-300"] {
            let cfg: Config =
                toml::from_str(&format!("[ui]\nalphai_ttl_secs = {bad}")).expect("must parse");
            let (resolved, warnings) = resolve(&cfg, None);
            assert_eq!(resolved.ui.alphai_ttl, alphai::CACHE_TTL, "at {bad}");
            assert_eq!(warnings.len(), 1, "at {bad}: {warnings:?}");
        }

        assert_eq!(UiDefaults::default().alphai_ttl, alphai::CACHE_TTL);
    }

    /// Every ```toml block in the README must parse as a Config and resolve
    /// without warnings, so the documentation cannot rot silently.
    #[test]
    fn readme_toml_examples_parse_and_resolve() {
        let readme = include_str!("../README.md");
        let mut in_block = false;
        let mut block = String::new();
        let mut checked = 0;
        for line in readme.lines() {
            if !in_block && line.trim_start().starts_with("```toml") {
                in_block = true;
                block.clear();
                continue;
            }
            if in_block && line.trim_start().starts_with("```") {
                in_block = false;
                let cfg: Config = toml::from_str(&block)
                    .unwrap_or_else(|e| panic!("README toml does not parse: {e}\n{block}"));
                let (_, warnings) = resolve(&cfg, None);
                assert!(
                    warnings.is_empty(),
                    "README example warns: {warnings:?}\n{block}"
                );
                checked += 1;
                continue;
            }
            if in_block {
                block.push_str(line);
                block.push('\n');
            }
        }
        assert!(
            checked >= 2,
            "expected at least two toml blocks in the README"
        );
    }
}
