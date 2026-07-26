//! The settings overlay: state, key handling and Save.
//!
//! The row list derives from the source registry (`settings_rows`), so a
//! new source's key fields appear, edit, mask and persist with no changes
//! here. Save starts from the loaded `Config` and replaces only the fields
//! this screen edits, so config-file-only settings survive a Save.

use std::collections::BTreeMap;
use std::sync::LazyLock;
use std::time::Duration;

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
    /// Edit buffer for the poll interval, in whole seconds.
    pub every_input: String,
    /// "alphai" (article page on alphai.io) or "original" (source site).
    pub news_open_choice: String,
    /// Name of the color preset on screen; applies live as it cycles, and
    /// Save writes it to `[theme] preset`.
    pub theme_choice: &'static str,
    pub message: Option<String>,
}

/// One row of the settings overlay, in cursor order.
#[derive(Clone, Copy)]
pub enum SettingsRow {
    /// The price-source picker (cycles the registry).
    SourceChoice,
    /// An editable, masked credential.
    Key(&'static KeyField),
    /// The price poll interval in seconds; applies live on Save.
    PollEvery,
    /// Where Enter opens a news article.
    NewsOpen,
    /// The color preset (cycles the same list as the p key, live).
    ThemeChoice,
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
        rows.push(SettingsRow::PollEvery);
        rows.push(SettingsRow::NewsOpen);
        rows.push(SettingsRow::ThemeChoice);
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
        s.every_input = self.every.read().unwrap().as_secs().to_string();
        s.news_open_choice = if self.config.news_open_original() {
            "original".to_string()
        } else {
            "alphai".to_string()
        };
        s.theme_choice = self.theme_name;
    }

    pub(super) fn handle_settings_key(&mut self, key: KeyEvent) -> bool {
        if self.settings.editing {
            let s = &mut self.settings;
            match key.code {
                KeyCode::Enter => {
                    let value = s.input.trim().to_string();
                    match settings_rows()[s.cursor] {
                        SettingsRow::Key(field) => {
                            s.key_values.insert(field.config_name, value);
                        }
                        // Committed raw; Save validates and complains.
                        SettingsRow::PollEvery => s.every_input = value,
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
                self.settings.cursor = (self.settings.cursor + 1).min(settings_rows().len() - 1)
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') => {
                match settings_rows()[self.settings.cursor] {
                    SettingsRow::SourceChoice => self.toggle_source_choice(),
                    SettingsRow::NewsOpen => self.toggle_news_open_choice(),
                    SettingsRow::ThemeChoice => self.cycle_theme_choice(),
                    _ => {}
                }
            }
            KeyCode::Enter => match settings_rows()[self.settings.cursor] {
                SettingsRow::SourceChoice => self.toggle_source_choice(),
                SettingsRow::NewsOpen => self.toggle_news_open_choice(),
                SettingsRow::ThemeChoice => self.cycle_theme_choice(),
                SettingsRow::Key(field) => {
                    let s = &mut self.settings;
                    s.input = s.key_values.get(field.config_name).cloned().unwrap_or_default();
                    s.editing = true;
                }
                SettingsRow::PollEvery => {
                    let s = &mut self.settings;
                    s.input = s.every_input.clone();
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

    /// The theme row previews as it cycles, exactly like the p key: the
    /// point of a theme picker is seeing the theme.
    fn cycle_theme_choice(&mut self) {
        self.set_theme(crate::theme::next_preset(self.settings.theme_choice));
        self.settings.theme_choice = self.theme_name;
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
        // The default preset is the absence of the key, so picking it
        // takes the line back out (and the [theme] table with it, when
        // nothing else lives there) instead of writing a no-op.
        let mut theme = cfg.theme.take().unwrap_or_default();
        if self.settings.theme_choice == crate::theme::DEFAULT_PRESET {
            theme.remove("preset");
        } else {
            theme.insert("preset".to_string(), self.settings.theme_choice.to_string());
        }
        cfg.theme = (!theme.is_empty()).then_some(theme);
        if let Some(secs) = parse_every(&self.settings.every_input) {
            cfg.every = Some(secs);
        }
        // Saving persists the watchlist on screen, so a bare `alphai-tui`
        // reopens exactly this setup.
        cfg.watchlist = self.symbols.clone();
        cfg
    }

    fn settings_save(&mut self) {
        let Some(every_secs) = parse_every(&self.settings.every_input) else {
            self.settings.message =
                Some("poll interval: whole seconds, 2 or more".to_string());
            return;
        };
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

        // The poller re-reads the interval before every sleep; the nudge
        // makes the new cadence take effect now rather than after the
        // current (possibly long) sleep runs out.
        let every = Duration::from_secs(every_secs);
        if *self.every.read().unwrap() != every {
            *self.every.write().unwrap() = every;
            self.refresh.notify_one();
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

/// The poll-interval edit buffer as seconds: whole numbers of 2 and up
/// (the same floor `main` applies to --every), anything else is invalid.
fn parse_every(input: &str) -> Option<u64> {
    input.trim().parse::<u64>().ok().filter(|&v| v >= 2)
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
