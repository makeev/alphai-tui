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

use crate::config::{self, ALPHAI_KEY_FIELD, Config, KeyField};
use crate::source::extended::ExtendedSource;
use crate::source::{alpaca_keys, make_source, registry};

use super::{App, NewsLayout};
use crate::theme::Panels;

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
    /// Where pre and post market come from; Save writes `extended_source`.
    pub extended_choice: ExtendedSource,
    /// Edit buffers for the `Key` rows, by `KeyField::config_name`.
    pub key_values: BTreeMap<&'static str, String>,
    /// Edit buffer for the poll interval, in whole seconds.
    pub every_input: String,
    /// "alphai" (article page on alphai.io) or "original" (source site).
    pub news_open_choice: String,
    /// Name of the color preset on screen; applies live as it cycles, and
    /// Save writes it to `[theme] preset`.
    pub theme_choice: &'static str,
    /// The News layout on screen; applies live as it cycles, and Save
    /// writes it to `[ui] news_layout`.
    pub news_layout_choice: NewsLayout,
    /// The panel look on screen; applies live as it cycles, and Save
    /// writes it to `[ui] borders`.
    pub borders_choice: Panels,
    pub animations_choice: bool,
    pub message: Option<String>,
}

/// One row of the settings overlay, in cursor order.
#[derive(Clone, Copy)]
pub enum SettingsRow {
    /// The price-source picker (cycles the registry).
    SourceChoice,
    /// Where pre and post market prices come from.
    ExtendedSource,
    /// The price poll interval in seconds; applies live on Save.
    PollEvery,
    /// An editable, masked credential.
    Key(&'static KeyField),
    /// Where Enter opens a news article.
    NewsOpen,
    /// The News view layout (cycles the same list as the x key, live).
    NewsLayout,
    /// Tinted panels or frame lines (live).
    Borders,
    /// The color preset (cycles the same list as the p key, live).
    ThemeChoice,
    /// Price color fades and the refresh spinner (live).
    Animations,
    /// The save button.
    Save,
}

impl SettingsRow {
    /// The heading drawn above a row that starts a section.
    pub fn section(self) -> Option<&'static str> {
        match self {
            Self::SourceChoice => Some("Prices"),
            Self::Key(field) if std::ptr::eq(field, first_key()) => Some("API keys"),
            Self::NewsOpen => Some("News"),
            Self::Borders => Some("Look"),
            _ => None,
        }
    }
}

fn first_key() -> &'static KeyField {
    registry::SOURCES
        .iter()
        .flat_map(|s| s.key_fields)
        .next()
        .unwrap_or(&ALPHAI_KEY_FIELD)
}

/// Rows of the settings overlay, in sections: prices (source, pre and post
/// market, interval), every registered source's key fields in registry
/// order plus the app-level AlphAI key, news, look, Save. Derived from the
/// registry, so a new source's key rows appear (and persist, and mask)
/// with no settings-code changes.
pub fn settings_rows() -> &'static [SettingsRow] {
    static ROWS: LazyLock<Vec<SettingsRow>> = LazyLock::new(|| {
        let mut rows = vec![
            SettingsRow::SourceChoice,
            SettingsRow::ExtendedSource,
            SettingsRow::PollEvery,
        ];
        rows.extend(
            registry::SOURCES
                .iter()
                .flat_map(|s| s.key_fields)
                .map(SettingsRow::Key),
        );
        rows.push(SettingsRow::Key(&ALPHAI_KEY_FIELD));
        rows.push(SettingsRow::NewsOpen);
        rows.push(SettingsRow::NewsLayout);
        rows.push(SettingsRow::Borders);
        rows.push(SettingsRow::ThemeChoice);
        rows.push(SettingsRow::Animations);
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
                    self.config
                        .keys
                        .get(field.config_name)
                        .cloned()
                        .unwrap_or_default(),
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
        s.extended_choice = ExtendedSource::from_config(&self.config);
        s.key_values = key_values;
        s.every_input = self.every.read().unwrap().as_secs().to_string();
        s.news_open_choice = if self.config.news_open_original() {
            "original".to_string()
        } else {
            "alphai".to_string()
        };
        s.theme_choice = self.theme_name;
        s.news_layout_choice = self.news_layout;
        s.borders_choice = self.theme.panels;
        s.animations_choice = self.animations;
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
            KeyCode::Left => self.cycle_row(-1),
            KeyCode::Right | KeyCode::Char(' ') => self.cycle_row(1),
            KeyCode::Enter => match settings_rows()[self.settings.cursor] {
                SettingsRow::SourceChoice
                | SettingsRow::ExtendedSource
                | SettingsRow::NewsOpen
                | SettingsRow::NewsLayout
                | SettingsRow::Borders
                | SettingsRow::Animations
                | SettingsRow::ThemeChoice => self.cycle_row(1),
                SettingsRow::Key(field) => {
                    let s = &mut self.settings;
                    s.input = s
                        .key_values
                        .get(field.config_name)
                        .cloned()
                        .unwrap_or_default();
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

    /// Move the row under the cursor through its choices: left walks back,
    /// right (and space, and enter) walks forward. Direction matters once a
    /// list is longer than a toggle, which the theme row is.
    fn cycle_row(&mut self, dir: isize) {
        match settings_rows()[self.settings.cursor] {
            SettingsRow::SourceChoice => {
                let s = &mut self.settings;
                s.source_choice = step_source(&s.source_choice, dir).to_string();
            }
            SettingsRow::ExtendedSource => {
                let s = &mut self.settings;
                s.extended_choice = s.extended_choice.step(dir);
            }
            SettingsRow::NewsOpen => self.toggle_news_open_choice(),
            SettingsRow::NewsLayout => {
                // Live, like the theme row: the view behind the overlay is
                // the preview.
                let layout = self.settings.news_layout_choice.step(dir);
                self.settings.news_layout_choice = layout;
                self.news_layout = layout;
                self.card_scroll = 0;
            }
            SettingsRow::Borders => {
                let panels = self.settings.borders_choice.step(dir);
                self.settings.borders_choice = panels;
                self.theme.panels = panels;
            }
            SettingsRow::ThemeChoice => self.cycle_theme_choice(dir),
            SettingsRow::Animations => {
                self.animations = !self.animations;
                self.settings.animations_choice = self.animations;
                self.price_flash.clear();
            }
            _ => {}
        }
    }

    /// The theme row previews as it cycles, exactly like the p key: the
    /// point of a theme picker is seeing the theme.
    fn cycle_theme_choice(&mut self, dir: isize) {
        self.set_theme(crate::theme::step_preset(self.settings.theme_choice, dir));
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
        // The default is no line at all, like the News layout below.
        let extended = self.settings.extended_choice;
        cfg.extended_source =
            (extended != ExtendedSource::default()).then(|| extended.name().to_string());
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
        // Same for the News layout: the default is no line at all, and a
        // `[ui]` table left with nothing in it goes too.
        let mut ui = cfg.ui.take().unwrap_or_default();
        let layout = self.settings.news_layout_choice;
        ui.news_layout = (layout != NewsLayout::default()).then(|| layout.name().to_string());
        let panels = self.settings.borders_choice;
        ui.borders = (panels != Panels::default()).then(|| panels.name().to_string());
        ui.animations = (!self.settings.animations_choice).then_some(false);
        cfg.ui = (ui != config::UiConfig::default()).then_some(ui);
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
            self.settings.message = Some("poll interval: whole seconds, 2 or more".to_string());
            return;
        };
        let cfg = self.settings_merged_config();
        if self.settings.extended_choice == ExtendedSource::Alpaca && alpaca_keys(&cfg).is_none() {
            self.settings.message =
                Some("pre/after hours from alpaca needs the Alpaca key ID and secret".to_string());
            return;
        }

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
        // So does a new pre and post market provider, or new Alpaca keys
        // under it, but the prices on screen stay: they are the same feed's.
        let borrowing_changed = ExtendedSource::from_config(&cfg)
            != ExtendedSource::from_config(&self.config)
            || alpaca_keys(&cfg) != alpaca_keys(&self.config);
        if source_changed || borrowing_changed {
            match make_source(&self.settings.source_choice, &cfg) {
                Ok(src) => {
                    self.set_price_source(src);
                    if source_changed {
                        self.data.clear();
                        self.data_window.clear();
                        self.from_cache.clear();
                        self.quote_fetched.clear();
                        self.abandoned.clear();
                        self.fallback_exhausted = false;
                    }
                }
                Err(e) => {
                    self.settings.message = Some(format!("{e:#}"));
                    return;
                }
            }
        }

        // Adding credentials for an alternative source makes an exhausted
        // fallback search useful again, even if the active source stays.
        if cfg.keys != self.config.keys {
            self.fallback_exhausted = false;
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
            self.change_alphai_key(key);
        }

        // The list on screen is the saved one from here on, so `a` and `d`
        // write it through even in a session started from the command line.
        self.watchlist_saved = true;
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

/// Settings source picker: walks the registry in order, both ways;
/// unknown names reset to the keyless default.
fn step_source(cur: &str, dir: isize) -> &'static str {
    let n = registry::SOURCES.len() as isize;
    match registry::SOURCES.iter().position(|s| s.id == cur) {
        Some(i) => registry::SOURCES[((i as isize + dir).rem_euclid(n)) as usize].id,
        None => registry::SOURCES[0].id,
    }
}

#[cfg(test)]
mod tests {
    use super::step_source;

    #[test]
    fn source_cycle_covers_all_and_wraps_both_ways() {
        use crate::source::registry::SOURCES;
        let mut cur = SOURCES[0].id;
        let mut seen = vec![cur];
        for _ in 1..SOURCES.len() {
            cur = step_source(cur, 1);
            seen.push(cur);
        }
        // Every registered source is reachable exactly once, then it wraps.
        let mut ids: Vec<&str> = SOURCES.iter().map(|s| s.id).collect();
        seen.sort_unstable();
        ids.sort_unstable();
        assert_eq!(seen, ids);
        assert_eq!(step_source(cur, 1), SOURCES[0].id);
        // The left arrow walks the other way and wraps too.
        assert_eq!(
            step_source(SOURCES[0].id, -1),
            SOURCES[SOURCES.len() - 1].id
        );
        // Anything unexpected resets to the keyless default.
        assert_eq!(step_source("weird", 1), SOURCES[0].id);
    }
}
