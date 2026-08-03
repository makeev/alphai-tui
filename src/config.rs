use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::ValueEnum;
use ratatui::widgets::BorderType;
use serde::{Deserialize, Serialize};

use crate::alphai;
use crate::app::{ChartStyle, NewsLayout, NewsScope, RANGE_PRESETS};
use crate::domain::{Interval, Range};
use crate::indicators::{self, MaType};
use crate::keymap::Keymap;
use crate::theme::Theme;
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

/// The AlphaAI news key: app-level rather than a price source, but it lives
/// in the same `[keys]` table and the same settings list.
pub const ALPHAI_KEY_FIELD: KeyField = KeyField {
    config_name: "alphai",
    env_var: "ALPHAI_API_KEY",
    label: "AlphaAI key",
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
    /// Line set of the panel frames: "rounded" (default) or "plain".
    pub borders: Option<String>,
    /// Raw i64 rather than u8: an out-of-range number must degrade to a
    /// warning in `resolve`, not fail deserializing the whole file.
    pub news_min_score: Option<i64>,
    pub insider_min_score: Option<i64>,
    /// How long fetched AlphaAI data (news, sentiment, insider) stays fresh
    /// before the visible view re-fetches it. Seconds; raw i64 for the same
    /// warning-not-error reason as the scores.
    pub alphai_ttl_secs: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChartConfig {
    pub style: Option<String>,
    pub sma: Option<bool>,
    pub ma_type: Option<String>,
    pub rsi: Option<bool>,
    pub volume: Option<bool>,
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
}

/// Validated `[chart]` values; `Default` is the traditional look. The
/// session keys (c, m, i, t) still toggle everything live, these only seed
/// the startup state and the indicator math.
#[derive(Clone, Debug, PartialEq)]
pub struct ChartDefaults {
    pub style: ChartStyle,
    pub sma: bool,
    /// Simple or exponential; the periods below serve whichever is picked.
    pub ma_type: MaType,
    pub rsi: bool,
    pub volume: bool,
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
            style: ChartStyle::Candles,
            sma: true,
            ma_type: MaType::Sma,
            rsi: true,
            volume: true,
            sma_fast: indicators::SMA_FAST,
            sma_slow: indicators::SMA_SLOW,
            rsi_period: indicators::RSI_PERIOD,
            right_margin_pct: DEFAULT_RIGHT_MARGIN_PCT,
            presets: RANGE_PRESETS.to_vec(),
        }
    }
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
    pub news_layout: NewsLayout,
    pub news_scope: NewsScope,
    pub news_min_score: u8,
    pub insider_min_score: u8,
    pub alphai_ttl: Duration,
}

impl Default for UiDefaults {
    fn default() -> Self {
        Self {
            view_idx: ui::view_index(ui::ViewId::Split),
            news_layout: NewsLayout::default(),
            news_scope: NewsScope::default(),
            news_min_score: DEFAULT_NEWS_MIN_SCORE,
            insider_min_score: DEFAULT_INSIDER_MIN_SCORE,
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
    theme.border_type = resolve_borders(
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
    (Resolved { theme, theme_name, chart, ui, keymap }, warnings)
}

/// `[ui] borders`: the line set panel frames draw with. Lives in `[ui]`
/// rather than `[theme]` because it is not a color, but it resolves onto
/// the theme, which every renderer already has at hand.
fn resolve_borders(raw: Option<&str>, warnings: &mut Vec<String>) -> BorderType {
    match raw.map(str::to_lowercase).as_deref() {
        None | Some("rounded") => BorderType::Rounded,
        Some("plain") => BorderType::Plain,
        Some(other) => {
            warnings.push(format!(
                "[ui] borders: unknown \"{other}\" (rounded or plain), keeping rounded"
            ));
            BorderType::Rounded
        }
    }
}

fn resolve_chart(raw: Option<&ChartConfig>, warnings: &mut Vec<String>) -> ChartDefaults {
    let mut out = ChartDefaults::default();
    let Some(raw) = raw else { return out };
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
    out.sma_fast = period(raw.sma_fast, out.sma_fast, "sma_fast", 2..=250, warnings);
    out.sma_slow = period(raw.sma_slow, out.sma_slow, "sma_slow", 2..=250, warnings);
    out.rsi_period = period(raw.rsi_period, out.rsi_period, "rsi_period", 2..=100, warnings);
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
        match ui::VIEWS.iter().position(|v| v.title().eq_ignore_ascii_case(name)) {
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
    if let Some(layout) = &raw.news_layout {
        match layout.to_lowercase().as_str() {
            "side" => out.news_layout = NewsLayout::Side,
            "stacked" => out.news_layout = NewsLayout::Stacked,
            other => warnings.push(format!(
                "[ui] news_layout: unknown \"{other}\" (side or stacked), keeping side"
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
    out.news_min_score = min_score(raw.news_min_score, out.news_min_score, "news_min_score", warnings);
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
            warnings.push(format!("[ui] {name}: {n} is outside 1..=10, keeping {default}"));
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

/// Load from an explicit path (the `--config` flag) or the default
/// location. Returns the config, whether a file existed, and the path Save
/// must write back to (None when the platform has no config dir). A missing
/// file is a normal first run; an unreadable one degrades to defaults with
/// a stderr warning rather than blocking startup.
pub fn load_at(override_path: Option<&Path>) -> (Config, bool, Option<PathBuf>) {
    let Some(p) = override_path.map(Path::to_path_buf).or_else(path) else {
        return (Config::default(), false, None);
    };
    match load_from(&p) {
        Ok(Some(cfg)) => (cfg, true, Some(p)),
        Ok(None) => (Config::default(), false, Some(p)),
        Err(e) => {
            eprintln!("warning: ignoring bad config at {}: {e:#}", p.display());
            (Config::default(), false, Some(p))
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
    std::fs::write(p, raw).context("write failed")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key field whose env var can never be set, so tests exercise the
    /// file side of the resolution deterministically.
    const TEST_FIELD: KeyField = KeyField {
        config_name: "finnhub",
        env_var: "ALPHAI_TUI_TEST_NEVER_SET",
        label: "test",
    };

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
            news_open: Some("original".into()),
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
                alphai_ttl_secs: Some(120),
            }),
            chart: Some(ChartConfig {
                style: Some("line".into()),
                sma_slow: Some(200),
                right_margin_pct: Some(25),
                presets: Some(vec![vec!["1d".into(), "5m".into()]]),
                ..Default::default()
            }),
            theme: Some(BTreeMap::from([("accent".to_string(), "magenta".to_string())])),
            keybindings: Some(BTreeMap::from([
                ("quit".to_string(), KeysSpec::One("ctrl-q".into())),
                (
                    "open".to_string(),
                    KeysSpec::Many(vec!["enter".into(), "z".into()]),
                ),
            ])),
        };
        save_to(&p, &cfg).unwrap();
        let loaded = load_from(&p).unwrap().unwrap();
        assert_eq!(loaded, cfg);
        std::fs::remove_dir_all(&dir).ok();
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
        assert_eq!(cfg.keys.get("alphai").map(String::as_str), Some("ak_live_x"));
        assert_eq!(cfg.keys.get("alpaca_secret").map(String::as_str), Some("sec"));
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

        let (_, warnings) = resolve(&toml::from_str::<Config>("[theme]\nup = \"banana\"").unwrap(), None);
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
        assert_eq!(resolved.theme.border_type, BorderType::Plain);

        // Unknown value warns and keeps the rounded default; so does no
        // section at all (silently).
        let cfg: Config = toml::from_str("[ui]\nborders = \"fancy\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(resolved.theme.border_type, BorderType::Rounded);
        let (resolved, warnings) = resolve(&Config::default(), None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.theme.border_type, BorderType::Rounded);
    }

    #[test]
    fn keybindings_section_resolves_per_entry() {
        use crossterm::event::{KeyCode, KeyEvent};
        use crate::keymap::Action;

        // A bare string and a list both parse; the keymap follows.
        let cfg: Config =
            toml::from_str("[keybindings]\nrefresh = \"f5\"\nopen = [\"enter\", \"z\"]").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::F(5))),
            Some(Action::Refresh)
        );
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('z'))),
            Some(Action::Open)
        );
        assert_eq!(resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('r'))), None);

        // A bad entry warns and keeps the default; the good one still lands.
        let cfg: Config =
            toml::from_str("[keybindings]\nquit = \"supr\"\ncard = \"n\"").unwrap();
        let (resolved, warnings) = resolve(&cfg, None);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('q'))),
            Some(Action::Quit)
        );
        assert_eq!(
            resolved.keymap.resolve(&KeyEvent::from(KeyCode::Char('n'))),
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
        let cfg: Config =
            toml::from_str("[chart]\nvolume = false\nma_type = \"EMA\"").unwrap();
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
            assert_eq!(resolved.chart.right_margin_pct, DEFAULT_RIGHT_MARGIN_PCT, "at {bad}");
            assert_eq!(warnings.len(), 1, "at {bad}: {warnings:?}");
        }

        assert_eq!(ChartDefaults::default().right_margin_pct, DEFAULT_RIGHT_MARGIN_PCT);
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
            assert_eq!(resolved.ui.news_min_score, DEFAULT_NEWS_MIN_SCORE, "at {bad}");
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
                assert!(warnings.is_empty(), "README example warns: {warnings:?}\n{block}");
                checked += 1;
                continue;
            }
            if in_block {
                block.push_str(line);
                block.push('\n');
            }
        }
        assert!(checked >= 2, "expected at least two toml blocks in the README");
    }
}
