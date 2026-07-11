use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    /// `[theme]` color overrides by slot name (see `theme::Theme`). Kept as
    /// raw strings: validation happens in `resolve`, per slot, so one typo
    /// never degrades the whole file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<BTreeMap<String, String>>,
}

/// Everything derived and validated from the raw config. Semantic problems
/// in these sections warn and fall back per entry; only a TOML syntax error
/// degrades the whole file (in `load`).
pub struct Resolved {
    pub theme: crate::theme::Theme,
}

/// Validate the raw config into ready-to-use values plus human-readable
/// warnings (printed to stderr before the TUI starts).
pub fn resolve(cfg: &Config) -> (Resolved, Vec<String>) {
    let mut warnings = Vec::new();
    let theme = crate::theme::Theme::from_config(cfg.theme.as_ref(), &mut warnings);
    (Resolved { theme }, warnings)
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

/// Returns the config and whether a file existed. A missing file is a normal
/// first run; an unreadable one degrades to defaults with a stderr warning
/// rather than blocking startup.
pub fn load() -> (Config, bool) {
    let Some(p) = path() else {
        return (Config::default(), false);
    };
    match load_from(&p) {
        Ok(Some(cfg)) => (cfg, true),
        Ok(None) => (Config::default(), false),
        Err(e) => {
            eprintln!("warning: ignoring bad config at {}: {e:#}", p.display());
            (Config::default(), false)
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

pub fn save(cfg: &Config) -> Result<PathBuf> {
    let p = path().context("no config directory on this platform")?;
    save_to(&p, cfg)?;
    Ok(p)
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
            theme: Some(BTreeMap::from([("accent".to_string(), "magenta".to_string())])),
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
        let (resolved, warnings) = resolve(&cfg);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(resolved.theme.accent, ratatui::style::Color::Magenta);

        let (_, warnings) = resolve(&toml::from_str::<Config>("[theme]\nup = \"banana\"").unwrap());
        assert_eq!(warnings.len(), 1, "{warnings:?}");

        // No [theme] in the file: Save must not spray an empty table in.
        let bare = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!bare.contains("[theme]"), "{bare}");
    }
}
