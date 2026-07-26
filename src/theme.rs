//! Semantic color palette. Every color the views draw comes from here:
//! `Default` reproduces the look the app had before theming existed, and
//! the `[theme]` table in config.toml overrides slots by name.

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

impl Default for Theme {
    fn default() -> Self {
        Self {
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
        }
    }
}

impl Theme {
    /// Build from the raw `[theme]` table. Never fails: an unknown slot or
    /// an unparsable color appends a warning and keeps that slot's default,
    /// so a typo can not take the whole file (and its API keys) down.
    /// Accepted values: ANSI names (case-insensitive, "light-blue", "Grey"),
    /// "#RRGGBB" hex, or an ANSI-256 index written as a string like "245".
    pub fn from_config(raw: Option<&BTreeMap<String, String>>, warnings: &mut Vec<String>) -> Self {
        let mut theme = Self::default();
        let Some(raw) = raw else { return theme };
        for (slot, value) in raw {
            let target = match slot.as_str() {
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
        assert_eq!(Theme::from_config(None, &mut w), Theme::default());
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
        let t = Theme::from_config(Some(&raw), &mut w);
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
        let t = Theme::from_config(Some(&raw), &mut w);
        assert_eq!(t.up, Theme::default().up);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("banana"), "{w:?}");
    }

    #[test]
    fn unknown_slot_warns_and_is_ignored() {
        let mut w = Vec::new();
        let raw = table(&[("acent", "red"), ("down", "blue")]);
        let t = Theme::from_config(Some(&raw), &mut w);
        assert_eq!(t.down, Color::Blue);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("acent"), "{w:?}");
    }
}
