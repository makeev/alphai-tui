//! Semantic color palette. Every color the views draw comes from here:
//! `Default` reproduces the look the app had before theming existed,
//! `[theme] preset` swaps in a named palette (see `presets`), and the
//! `[theme]` slots override whatever the preset set.

mod presets;

pub use presets::{DEFAULT_PRESET, PRESETS, cli_theme_help, step_preset};

use std::collections::BTreeMap;
use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};

/// One slot per meaning, not per widget: the same red means "price down"
/// everywhere it appears. Deliberately not themeable: the selection
/// highlight (Modifier::REVERSED) and dim/bold text, which already track
/// the terminal's own palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// Brand accent: header title, active tab, overlay borders, headings.
    pub accent: Color,
    /// Text on top of `accent` (the active tab pill).
    pub accent_text: Color,
    /// Price direction: candles, deltas, sparklines.
    pub up: Color,
    pub down: Color,
    pub flat: Color,
    /// Sentiment and trade side: bullish/buys vs bearish/sells.
    pub pos: Color,
    pub neg: Color,
    pub error: Color,
    /// Notices (the archive upsell) and the editing highlight.
    pub warn: Color,
    /// Relevance score 8 to 10.
    pub score_high: Color,
    pub sma_fast: Color,
    pub sma_slow: Color,
    pub rsi_line: Color,
    /// Reference lines: previous close, RSI 30/70.
    pub ref_line: Color,
    /// Panel frames. `Reset` keeps the terminal's own foreground, which is
    /// what the app looked like before this slot existed; themes dim it.
    pub border: Color,
    /// Not a color slot: which line set panel frames draw with, resolved
    /// from `[ui] borders`. It rides on the theme because every renderer
    /// already carries one, and `panel()` needs both halves.
    pub border_type: BorderType,
}

impl Theme {
    /// The look the app had before theming existed, and the `default`
    /// preset. The only palette written in ANSI names rather than hex, so
    /// it follows whatever colors the terminal itself is set to.
    pub const DEFAULT: Self = Self {
        accent: Color::Cyan,
        accent_text: Color::Black,
        up: Color::Green,
        down: Color::Red,
        flat: Color::Gray,
        pos: Color::Green,
        neg: Color::Red,
        error: Color::Red,
        warn: Color::Yellow,
        score_high: Color::Yellow,
        sma_fast: Color::Yellow,
        sma_slow: Color::Magenta,
        rsi_line: Color::Cyan,
        ref_line: Color::DarkGray,
        border: Color::Reset,
        border_type: BorderType::Rounded,
    };
}

impl Default for Theme {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Theme {
    /// Build from the raw `[theme]` table. Never fails: an unknown slot or
    /// an unparsable color appends a warning and keeps that slot's default,
    /// so a typo can not take the whole file (and its API keys) down.
    /// Accepted values: ANSI names (case-insensitive, "light-blue", "Grey"),
    /// "#RRGGBB" hex, or an ANSI-256 index written as a string like "245".
    ///
    /// Order: the built-in theme, then the preset, then the slots spelled
    /// out in the table. An explicit slot therefore always wins, so
    /// "mocha, but my own green" needs one line rather than fifteen.
    ///
    /// `preset` is the name chosen outside the file (`--theme`, or the
    /// live cycle key) and beats `[theme] preset`. Rebuilding with a new
    /// name is how the app switches themes at runtime, which keeps the
    /// live look identical to what a restart would produce. Returns the
    /// resolved theme and the canonical name of the preset behind it.
    pub fn resolve(
        raw: Option<&BTreeMap<String, String>>,
        preset: Option<&str>,
        warnings: &mut Vec<String>,
    ) -> (Self, &'static str) {
        let name = match preset {
            Some(name) => Self::preset_name(name, "--theme", warnings),
            None => match raw.and_then(|raw| raw.get("preset")) {
                Some(name) => Self::preset_name(name, "[theme] preset", warnings),
                None => DEFAULT_PRESET,
            },
        };
        (Self::from_config(raw, name, warnings), name)
    }

    /// A preset name as the table spells it; unknown names warn and fall
    /// back to the built-in theme, like any other bad value here.
    fn preset_name(name: &str, source: &str, warnings: &mut Vec<String>) -> &'static str {
        presets::canonical(name).unwrap_or_else(|| {
            let known: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
            warnings.push(format!(
                "{source}: unknown preset \"{name}\", keeping the default (available: {})",
                known.join(", ")
            ));
            DEFAULT_PRESET
        })
    }

    /// The named preset with the table's own slots applied over it.
    fn from_config(
        raw: Option<&BTreeMap<String, String>>,
        preset: &str,
        warnings: &mut Vec<String>,
    ) -> Self {
        let mut theme = presets::find(preset).unwrap_or(Self::DEFAULT);
        let Some(raw) = raw else { return theme };
        for (slot, value) in raw {
            let target = match slot.as_str() {
                // Not a slot; it selected the palette above.
                "preset" => continue,
                "accent" => &mut theme.accent,
                "accent_text" => &mut theme.accent_text,
                "up" => &mut theme.up,
                "down" => &mut theme.down,
                "flat" => &mut theme.flat,
                "pos" => &mut theme.pos,
                "neg" => &mut theme.neg,
                "error" => &mut theme.error,
                "warn" => &mut theme.warn,
                "score_high" => &mut theme.score_high,
                "sma_fast" => &mut theme.sma_fast,
                "sma_slow" => &mut theme.sma_slow,
                "rsi_line" => &mut theme.rsi_line,
                "ref_line" => &mut theme.ref_line,
                "border" => &mut theme.border,
                _ => {
                    warnings.push(format!(
                        "[theme] unknown slot \"{slot}\" (the README lists the slots)"
                    ));
                    continue;
                }
            };
            match Color::from_str(value) {
                Ok(color) => *target = color,
                Err(_) => warnings.push(format!(
                    "[theme] {slot}: unknown color \"{value}\", keeping the default"
                )),
            }
        }
        theme
    }

    /// Every framed panel in the app is built here, so the border style has
    /// one source of truth (`borders_are_themed` in `ui::tests` fails if a
    /// panel is built any other way).
    pub fn panel(&self) -> Block<'static> {
        Block::bordered()
            .border_type(self.border_type)
            .border_style(Style::new().fg(self.border))
    }

    /// A panel whose title reads as a heading, e.g. " Watchlist ".
    pub fn panel_titled(&self, title: impl Into<String>) -> Block<'static> {
        self.panel().title(self.heading(title))
    }

    /// Headings (block titles, group labels) carry the accent.
    pub fn heading(&self, text: impl Into<String>) -> Line<'static> {
        Line::from(Span::styled(
            text.into(),
            Style::new().fg(self.accent).add_modifier(Modifier::BOLD),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn default_used_without_a_table() {
        let mut w = Vec::new();
        assert_eq!(Theme::resolve(None, None, &mut w).0, Theme::default());
        assert!(w.is_empty());
    }

    #[test]
    fn parses_names_hex_and_indexed() {
        let mut w = Vec::new();
        let raw = table(&[
            ("accent", "light-blue"),
            ("up", "#00c853"),
            ("ref_line", "245"),
            ("neg", "Grey"),
        ]);
        let t = Theme::resolve(Some(&raw), None, &mut w).0;
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(t.accent, Color::LightBlue);
        assert_eq!(t.up, Color::Rgb(0x00, 0xc8, 0x53));
        assert_eq!(t.ref_line, Color::Indexed(245));
        assert_eq!(t.neg, Color::Gray);
        // Untouched slots keep their defaults.
        assert_eq!(t.down, Theme::default().down);
    }

    #[test]
    fn bad_color_warns_and_keeps_the_default() {
        let mut w = Vec::new();
        let raw = table(&[("up", "banana")]);
        let t = Theme::resolve(Some(&raw), None, &mut w).0;
        assert_eq!(t.up, Theme::default().up);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("banana"), "{w:?}");
    }

    #[test]
    fn preset_fills_every_slot_and_explicit_slots_win() {
        let mut w = Vec::new();
        let raw = table(&[("preset", "catppuccin-mocha"), ("up", "#00c853")]);
        let t = Theme::resolve(Some(&raw), None, &mut w).0;
        assert!(w.is_empty(), "{w:?}");
        // The preset lands...
        assert_eq!(t.accent, Color::Rgb(0xcb, 0xa6, 0xf7));
        assert_eq!(t.border, Color::Rgb(0x58, 0x5b, 0x70));
        assert_eq!(t.down, Color::Rgb(0xf3, 0x8b, 0xa8));
        // ...and the slot written out by hand overrides it.
        assert_eq!(t.up, Color::Rgb(0x00, 0xc8, 0x53));
        // "preset" is a selector, not a slot: it must not warn as unknown.
        let mut w = Vec::new();
        let t = Theme::resolve(Some(&table(&[("preset", "nord")])), None, &mut w).0;
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(t.accent, Color::Rgb(0x88, 0xc0, 0xd0));
    }

    #[test]
    fn unknown_preset_warns_and_keeps_the_default() {
        let mut w = Vec::new();
        let raw = table(&[("preset", "catppuccino"), ("up", "#00c853")]);
        let t = Theme::resolve(Some(&raw), None, &mut w).0;
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("catppuccino"), "{w:?}");
        // The rest of the table still applies over the built-in theme.
        assert_eq!(t.accent, Theme::DEFAULT.accent);
        assert_eq!(t.up, Color::Rgb(0x00, 0xc8, 0x53));
    }

    #[test]
    fn unknown_slot_warns_and_is_ignored() {
        let mut w = Vec::new();
        let raw = table(&[("acent", "red"), ("down", "blue")]);
        let t = Theme::resolve(Some(&raw), None, &mut w).0;
        assert_eq!(t.down, Color::Blue);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("acent"), "{w:?}");
    }
}
