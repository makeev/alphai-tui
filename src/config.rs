use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Used when neither the CLI nor the config file provides symbols.
pub const DEFAULT_WATCHLIST: [&str; 4] = ["AAPL", "MSFT", "NVDA", "BTC-USD"];

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
    pub keys: Keys,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    pub finnhub: Option<String>,
    pub alphai: Option<String>,
    pub alpaca_key_id: Option<String>,
    pub alpaca_secret: Option<String>,
}

impl Config {
    /// Env var wins so a shell-exported key keeps working after a config
    /// file appears.
    pub fn finnhub_key(&self) -> Option<String> {
        env_key("FINNHUB_API_KEY").or_else(|| self.keys.finnhub.clone())
    }

    pub fn alphai_key(&self) -> Option<String> {
        env_key("ALPHAI_API_KEY").or_else(|| self.keys.alphai.clone())
    }

    /// Each half resolves independently (env > file, standard Alpaca SDK
    /// var names); Some only when both the key id and the secret are set.
    pub fn alpaca_keys(&self) -> Option<(String, String)> {
        let file_key = |v: &Option<String>| v.clone().filter(|k| !k.trim().is_empty());
        let id = env_key("APCA_API_KEY_ID").or_else(|| file_key(&self.keys.alpaca_key_id))?;
        let secret = env_key("APCA_API_SECRET_KEY").or_else(|| file_key(&self.keys.alpaca_secret))?;
        Some((id, secret))
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
            keys: Keys {
                finnhub: Some("fh-key".into()),
                alphai: Some("ak_live_x".into()),
                alpaca_key_id: Some("PKTEST123".into()),
                alpaca_secret: Some("alpaca-secret-x".into()),
            },
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
        assert!(cfg.keys.alphai.is_none());
    }
}
