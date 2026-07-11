//! The single registration point for price sources.
//!
//! To add a backend: implement `DataSource` in a new module under
//! `src/source/`, then append one `SourceInfo` entry to `SOURCES` below.
//! Everything else derives from this table: `--source` parsing and help,
//! the "available:" error list, config `[keys]` resolution, the settings
//! screen picker cycle and its key rows, and the missing-key message. The
//! tests at the bottom keep an entry honest.

use std::sync::Arc;

use anyhow::Result;

use super::{DataSource, alpaca, finnhub, yahoo};
use crate::config::KeyField;

pub struct SourceInfo {
    /// Canonical id: the config value, settings cycle, header display and
    /// CLI value. Must equal what `DataSource::name()` returns.
    pub id: &'static str,
    /// Extra accepted `--source` spellings (matched lowercased).
    pub aliases: &'static [&'static str],
    /// One-line description for the settings row.
    pub hint: &'static str,
    /// Credentials in the order `make` receives them; empty = keyless.
    pub key_fields: &'static [KeyField],
    /// Build the source. `keys` holds resolved values in `key_fields`
    /// order; `make_source` checks presence before calling.
    pub make: fn(keys: &[String]) -> Result<Arc<dyn DataSource>>,
}

/// Registry of price sources. Entry 0 is the keyless default: the fallback
/// when neither the CLI nor the config names a source, and the reset target
/// for unknown names in the settings cycle. Order defines the settings
/// cycle and the key rows.
pub static SOURCES: &[SourceInfo] = &[
    SourceInfo {
        id: "yahoo",
        aliases: &["yf", "yfinance"],
        hint: "no key needed, ~15 min delayed",
        key_fields: &[],
        make: |_| Ok(Arc::new(yahoo::Yahoo::new()?)),
    },
    SourceInfo {
        id: "finnhub",
        aliases: &["fh"],
        hint: "real-time quotes, needs a key (free at finnhub.io)",
        key_fields: &[KeyField {
            config_name: "finnhub",
            env_var: "FINNHUB_API_KEY",
            label: "Finnhub key",
        }],
        make: |keys| Ok(Arc::new(finnhub::Finnhub::new(keys[0].clone())?)),
    },
    SourceInfo {
        id: "alpaca",
        aliases: &["alpc"],
        hint: "realtime IEX quotes and real candle history, free keys at alpaca.markets",
        key_fields: &[
            KeyField {
                config_name: "alpaca_key_id",
                env_var: "APCA_API_KEY_ID",
                label: "Alpaca key ID",
            },
            KeyField {
                config_name: "alpaca_secret",
                env_var: "APCA_API_SECRET_KEY",
                label: "Alpaca secret",
            },
        ],
        make: |keys| Ok(Arc::new(alpaca::Alpaca::new(keys[0].clone(), keys[1].clone())?)),
    },
];

/// Look up a source by id or alias, case-insensitively.
pub fn find(name: &str) -> Option<&'static SourceInfo> {
    let name = name.to_lowercase();
    SOURCES
        .iter()
        .find(|s| s.id == name || s.aliases.contains(&name.as_str()))
}

/// Canonical ids, for error messages and docs.
pub fn ids() -> Vec<&'static str> {
    SOURCES.iter().map(|s| s.id).collect()
}

/// `--source` help line, derived so it can never trail the registry.
pub fn cli_source_help() -> String {
    let names = SOURCES
        .iter()
        .map(|s| {
            if s.key_fields.is_empty() {
                format!("{} (keyless)", s.id)
            } else {
                s.id.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("Price source: {names}; keyed sources need an API key")
}

/// How to fix a missing credential, generated from the key fields so a new
/// source cannot forget to write it.
pub fn missing_keys_msg(info: &SourceInfo) -> String {
    let envs: Vec<&str> = info.key_fields.iter().map(|f| f.env_var).collect();
    if envs.len() == 1 {
        format!(
            "{} needs an API key: set it in the settings screen (s) or export {}=<key>",
            info.id, envs[0]
        )
    } else {
        format!(
            "{} needs API keys: set them in the settings screen (s) or export {}",
            info.id,
            envs.join(" / ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ALPHAI_KEY_FIELD;

    #[test]
    fn registry_is_coherent() {
        assert!(
            SOURCES[0].key_fields.is_empty(),
            "entry 0 must be the keyless default"
        );
        let mut names: Vec<&str> = Vec::new();
        let mut config_names: Vec<&str> = Vec::new();
        for s in SOURCES {
            assert_eq!(s.id, s.id.to_lowercase(), "{}: id must be lowercase", s.id);
            assert!(!s.hint.is_empty(), "{}: empty hint", s.id);
            names.push(s.id);
            names.extend(s.aliases);
            for f in s.key_fields {
                assert_ne!(
                    f.config_name, ALPHAI_KEY_FIELD.config_name,
                    "{}: 'alphai' is reserved for the news key",
                    s.id
                );
                assert!(!f.env_var.is_empty() && !f.label.is_empty(), "{}: bare key field", s.id);
                config_names.push(f.config_name);
            }
        }
        for (list, what) in [(&mut names, "source id/alias"), (&mut config_names, "key config_name")] {
            let before = list.len();
            list.sort_unstable();
            list.dedup();
            assert_eq!(list.len(), before, "duplicate {what} in the registry");
        }
    }

    /// Every entry constructs (with dummy keys) and reports the id it was
    /// registered under; a mismatch would desync the header and the cycle.
    #[test]
    fn every_source_builds_and_reports_its_id() {
        for s in SOURCES {
            let dummy = vec!["dummy".to_string(); s.key_fields.len()];
            let built = (s.make)(&dummy).unwrap_or_else(|e| panic!("{}: make failed: {e:#}", s.id));
            assert_eq!(built.name(), s.id, "{}: DataSource::name() != registry id", s.id);
        }
    }

    #[test]
    fn missing_keys_msg_names_every_env_var() {
        for s in SOURCES.iter().filter(|s| !s.key_fields.is_empty()) {
            let msg = missing_keys_msg(s);
            assert!(msg.contains(s.id), "{msg}");
            assert!(msg.contains("settings screen (s)"), "{msg}");
            for f in s.key_fields {
                assert!(msg.contains(f.env_var), "{}: error lacks {}: {msg}", s.id, f.env_var);
            }
        }
    }

    #[test]
    fn find_matches_ids_and_aliases_case_insensitively() {
        assert_eq!(find("YAHOO").unwrap().id, "yahoo");
        assert_eq!(find("yf").unwrap().id, "yahoo");
        assert_eq!(find("Alpc").unwrap().id, "alpaca");
        assert!(find("nope").is_none());
    }
}
