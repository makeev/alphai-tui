//! The settings overlay: state, key handling and Save.
//!
//! The row list derives from the source registry (`settings_rows`), so a
//! new source's key fields appear, edit, mask and persist with no changes
//! here. Save starts from the loaded `Config` and replaces only the fields
//! this screen edits, so config-file-only settings survive a Save.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crossterm::event::{KeyCode, KeyEvent};

use crate::alphai;
use crate::config::{self, ALPHAI_KEY_FIELD, Config, KeyField};
use crate::source::{make_source, registry};

use super::App;

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

impl App {
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

    pub(super) fn handle_settings_key(&mut self, key: KeyEvent) -> bool {
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

    /// The full config Save persists: everything loaded from disk, with
    /// only the fields this screen edits (and the live watchlist) replaced.
    /// Config-file-only sections like `[theme]` survive a Save untouched.
    pub(crate) fn settings_merged_config(&self) -> Config {
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
        cfg
    }

    fn settings_save(&mut self) {
        let cfg = self.settings_merged_config();

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
            self.feeds.clear();
            self.alphai_errors.clear();
            self.inflight.clear();
        }

        match config::save_at(self.config_path.as_deref(), &cfg) {
            Ok(()) => {
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

/// Settings source toggle: cycles the registry in order; unknown names
/// reset to the keyless default.
fn next_source(cur: &str) -> &'static str {
    match registry::SOURCES.iter().position(|s| s.id == cur) {
        Some(i) => registry::SOURCES[(i + 1) % registry::SOURCES.len()].id,
        None => registry::SOURCES[0].id,
    }
}

#[cfg(test)]
mod tests {
    use super::next_source;

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
}
