//! Semantic color palette. Every color the views draw comes from here:
//! `Default` reproduces the look the app had before theming existed,
//! `[theme] preset` swaps in a named palette (see `presets`), and the
//! `[theme]` slots override whatever the preset set.

mod presets;

pub use presets::{DEFAULT_PRESET, PRESETS, cli_theme_help, step_preset};

use std::collections::BTreeMap;
use std::str::FromStr;

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};

/// How panels are told apart: a line frame, or a tinted surface with a
/// narrow gutter around it. Chosen by `[ui] borders` rather than by the
/// preset, and carried on the theme because `panel()` needs both halves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panels {
    Lines(BorderType),
    /// No lines: the inside takes `surface`, and the frame cells are split
    /// by quadrant blocks, the inner half surface and the outer half the
    /// theme's background. The frame keeps its cell, so every "minus two
    /// for the borders" in the layouts still holds and titles still sit on
    /// the top and bottom edges.
    Surface,
}

/// Rounded frame lines, what `[ui] borders` gives when it is not set.
impl Default for Panels {
    fn default() -> Self {
        Self::Lines(BorderType::Rounded)
    }
}

impl Panels {
    /// Every choice `[ui] borders` offers, in the settings row's cycle
    /// order: the default first.
    pub const ALL: [Self; 3] = [
        Self::Lines(BorderType::Rounded),
        Self::Lines(BorderType::Plain),
        Self::Surface,
    ];

    /// The name `[ui] borders` spells it with.
    pub fn name(self) -> &'static str {
        match self {
            Self::Surface => "none",
            Self::Lines(BorderType::Plain) => "plain",
            Self::Lines(_) => "rounded",
        }
    }

    /// The neighbour in cycle order, `dir` 1 forward and -1 back, wrapping.
    pub fn step(self, dir: isize) -> Self {
        let n = Self::ALL.len() as isize;
        let i = Self::ALL.iter().position(|p| *p == self).unwrap_or(0) as isize;
        Self::ALL[(i + dir).rem_euclid(n) as usize]
    }
}

/// The active surface panel's frame: eighth-cell lines on the side of
/// each frame cell that touches the panel.
const ACTIVE_SURFACE: ratatui::symbols::border::Set = ratatui::symbols::border::Set {
    top_left: " ",
    top_right: " ",
    bottom_left: " ",
    bottom_right: " ",
    vertical_left: "▕",
    vertical_right: "▏",
    horizontal_top: "▁",
    horizontal_bottom: "▔",
};

/// One slot per meaning, not per widget: the same red means "price down"
/// everywhere it appears. Unstyled and bold text inherit the body color.
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
    pub pre_market_bg: Color,
    pub post_market_bg: Color,
    /// Panel frames. `Reset` keeps the terminal's own foreground, which is
    /// what the app looked like before this slot existed; themes dim it.
    pub border: Color,
    /// App background, including framed panels and the gutters between
    /// surfaces. `Reset` leaves the terminal's background.
    pub background: Color,
    /// Body text throughout the app. `Reset` leaves the terminal's foreground.
    pub text: Color,
    /// Secondary text that still has to be read: sources, ages, axis
    /// labels, key hints. `Reset` falls back to the terminal's DIM, the
    /// only muted shade an ANSI palette can promise on any background.
    pub subtle: Color,
    /// Inactive or decorative text: separators, low-score rows, the past.
    /// `Reset` falls back to DIM, like `subtle`.
    pub faint: Color,
    /// Background of the selected row. `Reset` falls back to REVERSED,
    /// which is all an ANSI palette can promise; a color keeps the row's
    /// own colors (a green ▲, a yellow score) readable under the cursor.
    pub selection: Color,
    /// Panel background under `[ui] borders = "none"`. `Reset` paints
    /// nothing, so on the ANSI theme panels are told apart by the blank
    /// gutter and their titles alone.
    pub surface: Color,
    /// Not a color slot: frame or surface, resolved from `[ui] borders`.
    pub panels: Panels,
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
        pre_market_bg: Color::Rgb(38, 33, 24),
        post_market_bg: Color::Rgb(25, 28, 42),
        border: Color::Reset,
        background: Color::Reset,
        text: Color::Reset,
        subtle: Color::Reset,
        faint: Color::Reset,
        selection: Color::Reset,
        surface: Color::Reset,
        panels: Panels::Lines(BorderType::Rounded),
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
                "pre_market_bg" => &mut theme.pre_market_bg,
                "post_market_bg" => &mut theme.post_market_bg,
                "border" => &mut theme.border,
                "background" => &mut theme.background,
                "text" => &mut theme.text,
                "subtle" => &mut theme.subtle,
                "faint" => &mut theme.faint,
                "selection" => &mut theme.selection,
                "surface" => &mut theme.surface,
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
        match self.panels {
            Panels::Lines(kind) => Block::bordered()
                .border_type(kind)
                .style(self.base())
                .border_style(Style::new().fg(self.border)),
            // The frame cell is split in two by quadrant blocks: its inner
            // half takes the surface, its outer half the background. Each
            // surface reaches half a cell past its content, so the text
            // gets a margin and two panels side by side keep a one-cell
            // gap between them, half from each. Titles sit on a full cell
            // of the surface, a tab rising half a cell over the edge.
            // The ANSI theme has no surface color (`Reset`): quadrant
            // glyphs would draw in the terminal's foreground, a heavy frame
            // around every panel, so its frame stays blank as before.
            Panels::Surface if self.surface == Color::Reset => Block::bordered()
                .border_set(border::EMPTY)
                .style(self.fill())
                .border_style(self.base()),
            // Title text with no color of its own takes the body text, not
            // the frame's surface-colored glyphs (it would vanish).
            Panels::Surface => Block::bordered()
                .border_type(BorderType::QuadrantInside)
                .style(self.fill())
                .border_style(Style::new().fg(self.surface).bg(self.background))
                .title_style(Style::new().fg(self.text).bg(self.surface)),
        }
    }

    /// `panel` for the one panel the arrow keys move in (the watchlist in
    /// Split and Table, the feed in News and Insider): its frame in the
    /// accent, so the eye finds where the keys go. On surfaces the frame is
    /// an eighth-cell accent line hugging the surface, in place of the
    /// half-cell reach the other panels have; the rest of the frame cell
    /// stays the gutter, and the corners stay open (no eighth-cell corner
    /// glyph exists).
    pub fn panel_active(&self, active: bool) -> Block<'static> {
        if !active {
            return self.panel();
        }
        // Title text with no color of its own (the ANSI theme's tiers are
        // only DIM) would take the frame's accent: it gets the body text.
        match self.panels {
            Panels::Lines(kind) => Block::bordered()
                .border_type(kind)
                .style(self.base())
                .border_style(Style::new().fg(self.accent))
                .title_style(Style::new().fg(self.text)),
            Panels::Surface => self
                .panel()
                .border_set(ACTIVE_SURFACE)
                .border_style(Style::new().fg(self.accent).bg(self.background))
                .title_style(Style::new().fg(self.text).bg(self.surface)),
        }
    }

    /// Modal overlays (article card, help, settings, prompt) keep a line
    /// frame in every panel style: they float over other panels, and on a
    /// surface of the same tint nothing else would say where they end.
    pub fn modal(&self) -> Block<'static> {
        let kind = match self.panels {
            Panels::Lines(kind) => kind,
            Panels::Surface => BorderType::Rounded,
        };
        Block::bordered()
            .border_type(kind)
            .style(self.fill())
            .border_style(Style::new().fg(self.border))
    }

    /// The complete canvas palette. Named presets own both colors so a
    /// light terminal cannot wash out a dark preset (or vice versa).
    pub fn base(&self) -> Style {
        Style::new().fg(self.text).bg(self.background)
    }

    /// The panel background for widgets that paint their own (ratatui's
    /// `Chart` clears its plot to its style's background, which would
    /// punch a hole in a surface panel).
    pub fn fill(&self) -> Style {
        match self.panels {
            Panels::Lines(_) => self.base(),
            Panels::Surface => self.base().bg(self.surface),
        }
    }

    /// The plate a key sits on, over a cell whose background is `under`:
    /// 15% of the way to the body text, softer than the cursor row (22%),
    /// so a key reads as a key without competing with the selected row.
    /// The ANSI theme, whose colors are the terminal's, inverts instead.
    pub fn plate(&self, under: Color) -> Style {
        let bold = Style::new().fg(self.text).add_modifier(Modifier::BOLD);
        match (under, self.text) {
            (Color::Rgb(..), Color::Rgb(..)) => bold.bg(presets::mix(under, self.text, 15)),
            _ => self.selected().patch(bold),
        }
    }

    /// The cursor row in lists and tables.
    pub fn selected(&self) -> Style {
        if self.selection == Color::Reset {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new().bg(self.selection)
        }
    }

    /// Secondary text: metadata a reader still needs (source, age, axis
    /// labels, key hints).
    pub fn subtle(&self) -> Style {
        tier(self.subtle)
    }

    /// Inactive or decorative text: it may recede, nobody has to read it.
    pub fn faint(&self) -> Style {
        tier(self.faint)
    }

    /// `faint` for a style that may already carry a meaning color (a
    /// warning, a direction): the color stays and only fades, since
    /// swapping it for the faint shade would drop what it says.
    pub fn fade(&self, style: Style) -> Style {
        match style.fg {
            Some(c) if c != Color::Reset => style.add_modifier(Modifier::DIM),
            _ => style.patch(self.faint()),
        }
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

/// A text tier as a style: the slot's color, or the terminal's DIM when
/// the slot is `Reset` (the ANSI theme cannot know what reads as muted on
/// the background the terminal has).
fn tier(color: Color) -> Style {
    if color == Color::Reset {
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new().fg(color)
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
