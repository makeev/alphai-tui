//! Named palettes for `[theme] preset`.
//!
//! Every family has its own color vocabulary, so the map from palette
//! names to semantic slots is written once, in `theme_from`, and a family
//! only fills in eleven values. Adding a preset is one `Palette` constant
//! plus one row in `PRESETS`; `presets_are_coherent` fails if that leaves
//! any slot behind.
//!
//! Sources of the hex values: catppuccin/palette (palette.json),
//! draculatheme.com/contribute, morhetz/gruvbox (colors/gruvbox.vim),
//! nordtheme.com/docs/colors-and-palettes.

use ratatui::style::Color;
use ratatui::widgets::BorderType;

use super::Theme;

const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// The vocabulary shared by the palettes: enough names to fill every slot,
/// few enough that any family maps onto it.
#[derive(Clone, Copy)]
struct Palette {
    /// The family's signature color: headings, the active tab, breaking rows.
    accent: Color,
    /// The background the palette was drawn for. Only used as the text
    /// color on top of `accent`, since the app never paints a background.
    base: Color,
    green: Color,
    red: Color,
    /// A second red so an app error reads apart from a falling price.
    /// Families with a single red repeat it, as the built-in theme does.
    red_alt: Color,
    orange: Color,
    yellow: Color,
    violet: Color,
    cyan: Color,
    /// Quiet but still readable: unchanged prices, empty values.
    muted: Color,
    /// Structure rather than text: panel frames, reference lines.
    dim: Color,
}

/// The one place palette names become semantic slots.
const fn theme_from(p: Palette) -> Theme {
    Theme {
        accent: p.accent,
        accent_text: p.base,
        up: p.green,
        down: p.red,
        flat: p.muted,
        pos: p.green,
        neg: p.red,
        error: p.red_alt,
        warn: p.orange,
        score_high: p.yellow,
        sma_fast: p.yellow,
        sma_slow: p.violet,
        rsi_line: p.cyan,
        ref_line: p.dim,
        border: p.dim,
        // Frames are shaped by `[ui] borders`, which `config::resolve`
        // stamps on afterwards; a preset never decides that.
        border_type: BorderType::Rounded,
    }
}

const CATPPUCCIN_MOCHA: Palette = Palette {
    accent: rgb(0xcba6f7),
    base: rgb(0x1e1e2e),
    green: rgb(0xa6e3a1),
    red: rgb(0xf38ba8),
    red_alt: rgb(0xeba0ac),
    orange: rgb(0xfab387),
    yellow: rgb(0xf9e2af),
    violet: rgb(0xb4befe),
    cyan: rgb(0x89dceb),
    muted: rgb(0x7f849c),
    dim: rgb(0x585b70),
};

const CATPPUCCIN_MACCHIATO: Palette = Palette {
    accent: rgb(0xc6a0f6),
    base: rgb(0x24273a),
    green: rgb(0xa6da95),
    red: rgb(0xed8796),
    red_alt: rgb(0xee99a0),
    orange: rgb(0xf5a97f),
    yellow: rgb(0xeed49f),
    violet: rgb(0xb7bdf8),
    cyan: rgb(0x91d7e3),
    muted: rgb(0x8087a2),
    dim: rgb(0x5b6078),
};

const CATPPUCCIN_FRAPPE: Palette = Palette {
    accent: rgb(0xca9ee6),
    base: rgb(0x303446),
    green: rgb(0xa6d189),
    red: rgb(0xe78284),
    red_alt: rgb(0xea999c),
    orange: rgb(0xef9f76),
    yellow: rgb(0xe5c890),
    violet: rgb(0xbabbf1),
    cyan: rgb(0x99d1db),
    muted: rgb(0x838ba7),
    dim: rgb(0x626880),
};

const CATPPUCCIN_LATTE: Palette = Palette {
    accent: rgb(0x8839ef),
    base: rgb(0xeff1f5),
    green: rgb(0x40a02b),
    red: rgb(0xd20f39),
    red_alt: rgb(0xe64553),
    orange: rgb(0xfe640b),
    yellow: rgb(0xdf8e1d),
    violet: rgb(0x7287fd),
    cyan: rgb(0x04a5e5),
    muted: rgb(0x8c8fa1),
    dim: rgb(0xacb0be),
};

/// One red in the palette, so errors and falling prices share it.
const DRACULA: Palette = Palette {
    accent: rgb(0xbd93f9),
    base: rgb(0x282a36),
    green: rgb(0x50fa7b),
    red: rgb(0xff5555),
    red_alt: rgb(0xff5555),
    orange: rgb(0xffb86c),
    yellow: rgb(0xf1fa8c),
    violet: rgb(0xff79c6),
    cyan: rgb(0x8be9fd),
    muted: rgb(0x6272a4),
    dim: rgb(0x44475a),
};

/// Blue carries the accent so orange stays free for warnings, which is
/// where gruvbox itself puts it.
const GRUVBOX_DARK: Palette = Palette {
    accent: rgb(0x83a598),
    base: rgb(0x282828),
    green: rgb(0xb8bb26),
    red: rgb(0xfb4934),
    red_alt: rgb(0xcc241d),
    orange: rgb(0xfe8019),
    yellow: rgb(0xfabd2f),
    violet: rgb(0xd3869b),
    cyan: rgb(0x8ec07c),
    muted: rgb(0x928374),
    dim: rgb(0x665c54),
};

const GRUVBOX_LIGHT: Palette = Palette {
    accent: rgb(0x076678),
    base: rgb(0xfbf1c7),
    green: rgb(0x79740e),
    red: rgb(0x9d0006),
    red_alt: rgb(0xcc241d),
    orange: rgb(0xaf3a03),
    yellow: rgb(0xb57614),
    violet: rgb(0x8f3f71),
    cyan: rgb(0x427b58),
    muted: rgb(0x928374),
    dim: rgb(0xbdae93),
};

/// `muted` is nord3-bright, which the project added to nord.vim precisely
/// because nord3 sat too dark for text; nord3 stays on the frames.
const NORD: Palette = Palette {
    accent: rgb(0x88c0d0),
    base: rgb(0x2e3440),
    green: rgb(0xa3be8c),
    red: rgb(0xbf616a),
    red_alt: rgb(0xbf616a),
    orange: rgb(0xd08770),
    yellow: rgb(0xebcb8b),
    violet: rgb(0xb48ead),
    cyan: rgb(0x8fbcbb),
    muted: rgb(0x616e88),
    dim: rgb(0x4c566a),
};

/// Every preset, in the order the cycle key walks them: the built-in ANSI
/// theme first, then the dark ones, and the light ones last so a stray
/// keypress does not flash a white screen on a dark terminal.
pub static PRESETS: &[(&str, Theme)] = &[
    ("default", Theme::DEFAULT),
    ("catppuccin-mocha", theme_from(CATPPUCCIN_MOCHA)),
    ("catppuccin-macchiato", theme_from(CATPPUCCIN_MACCHIATO)),
    ("catppuccin-frappe", theme_from(CATPPUCCIN_FRAPPE)),
    ("dracula", theme_from(DRACULA)),
    ("gruvbox-dark", theme_from(GRUVBOX_DARK)),
    ("nord", theme_from(NORD)),
    ("catppuccin-latte", theme_from(CATPPUCCIN_LATTE)),
    ("gruvbox-light", theme_from(GRUVBOX_LIGHT)),
];

/// The name every preset falls back to; also the first of the cycle.
pub const DEFAULT_PRESET: &str = PRESETS[0].0;

/// A preset by name, case-insensitively.
pub fn find(name: &str) -> Option<Theme> {
    entry(name).map(|(_, theme)| *theme)
}

/// The canonical spelling of a preset name, if it names one.
pub fn canonical(name: &str) -> Option<&'static str> {
    entry(name).map(|(n, _)| *n)
}

fn entry(name: &str) -> Option<&'static (&'static str, Theme)> {
    PRESETS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name.trim()))
}

/// The neighbouring preset in cycle order: `dir` 1 forward, -1 back, both
/// wrapping. An unknown name restarts the cycle.
pub fn step_preset(name: &str, dir: isize) -> &'static str {
    let count = PRESETS.len() as isize;
    match PRESETS.iter().position(|(n, _)| n.eq_ignore_ascii_case(name)) {
        Some(i) => PRESETS[((i as isize + dir).rem_euclid(count)) as usize].0,
        None => DEFAULT_PRESET,
    }
}

/// Help for `--theme`, built from the table so the two can not drift.
pub fn cli_theme_help() -> String {
    let names: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
    format!("Color preset: {} [default: {DEFAULT_PRESET}]", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slot by slot, by destructuring: the compiler makes this fail to
    /// build when a slot is added to `Theme`, and the assertion makes it
    /// fail when a palette forgets to fill one.
    fn slots(theme: Theme) -> Vec<(&'static str, Color)> {
        let Theme {
            accent,
            accent_text,
            up,
            down,
            flat,
            pos,
            neg,
            error,
            warn,
            score_high,
            sma_fast,
            sma_slow,
            rsi_line,
            ref_line,
            border,
            border_type: _,
        } = theme;
        vec![
            ("accent", accent),
            ("accent_text", accent_text),
            ("up", up),
            ("down", down),
            ("flat", flat),
            ("pos", pos),
            ("neg", neg),
            ("error", error),
            ("warn", warn),
            ("score_high", score_high),
            ("sma_fast", sma_fast),
            ("sma_slow", sma_slow),
            ("rsi_line", rsi_line),
            ("ref_line", ref_line),
            ("border", border),
        ]
    }

    #[test]
    fn presets_are_coherent() {
        assert_eq!(PRESETS[0].0, "default", "the cycle starts at the built-in theme");
        for (name, theme) in PRESETS {
            assert_eq!(
                PRESETS.iter().filter(|(n, _)| n == name).count(),
                1,
                "{name} is listed twice"
            );
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name} is not lower-kebab-case"
            );
            assert_eq!(find(&name.to_uppercase()), Some(*theme));
            if *name == "default" {
                continue;
            }
            // Named ANSI colors would mean a slot the palette never filled:
            // it would follow the terminal instead of the preset.
            for (slot, color) in slots(*theme) {
                assert!(
                    matches!(color, Color::Rgb(..)),
                    "{name}: slot {slot} is not part of the palette ({color:?})"
                );
            }
        }
    }

    #[test]
    fn lookup_is_case_and_space_tolerant() {
        assert_eq!(find(" Catppuccin-Mocha "), find("catppuccin-mocha"));
        assert_eq!(canonical("NORD"), Some("nord"));
        assert!(find("catppuccino").is_none());
    }

    /// The cycle key must reach every preset and come back, and the CLI
    /// help must list all of them (both read the same table).
    #[test]
    fn cycle_reaches_every_preset_and_wraps() {
        let names: Vec<&str> = PRESETS.iter().map(|(n, _)| *n).collect();
        let mut seen = vec![DEFAULT_PRESET];
        let mut cur = DEFAULT_PRESET;
        for _ in 1..PRESETS.len() {
            cur = step_preset(cur, 1);
            seen.push(cur);
        }
        assert_eq!(seen, names);
        assert_eq!(step_preset(cur, 1), DEFAULT_PRESET, "the cycle must wrap");
        assert_eq!(step_preset("nonsense", 1), DEFAULT_PRESET);
        // Backwards, too: the picker has a left arrow.
        assert_eq!(step_preset(DEFAULT_PRESET, -1), names[names.len() - 1]);
        assert_eq!(step_preset(names[1], -1), DEFAULT_PRESET);

        let help = cli_theme_help();
        for name in names {
            assert!(help.contains(name), "--theme help omits {name}: {help}");
        }
    }
}
