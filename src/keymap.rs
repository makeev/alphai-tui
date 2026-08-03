//! Semantic actions and the key table that binds them.
//!
//! `handle_key` matches on `Action`, never on raw key codes, and the footer
//! renders whatever keys are actually bound (`Keymap::labels`), so bindings
//! have a single source of truth. Directional actions are contextual on
//! purpose: `Down` means "next article" in feed views and "next symbol"
//! elsewhere — the view guards in `App::handle_key` resolve that, exactly
//! like the raw keys used to.
//!
//! Fixed and never remappable: Ctrl-C (the force quit that survives any
//! keymap), Esc (quit or close, always works), the 1-9 digits (positional,
//! matching the numbered header pills) and every key inside the settings
//! form (it is a text input).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Quit,
    NextView,
    PrevView,
    Settings,
    Help,
    Refresh,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Open,
    Card,
    CycleScope,
    CycleLayout,
    ScoreUp,
    ScoreDown,
    ChartStyle,
    ToggleSma,
    ToggleRsi,
    ToggleVolume,
    MaType,
    NextPreset,
    PrevPreset,
    NextTheme,
    PrevTheme,
}

/// Every action with its snake_case config name, the single source of truth
/// for `[keybindings]` parsing and for the coverage test.
pub const ACTIONS: [(Action, &str); 27] = [
    (Action::Quit, "quit"),
    (Action::NextView, "next_view"),
    (Action::PrevView, "prev_view"),
    (Action::Settings, "settings"),
    (Action::Help, "help"),
    (Action::Refresh, "refresh"),
    (Action::Up, "up"),
    (Action::Down, "down"),
    (Action::Left, "left"),
    (Action::Right, "right"),
    (Action::PageUp, "page_up"),
    (Action::PageDown, "page_down"),
    (Action::Open, "open"),
    (Action::Card, "card"),
    (Action::CycleScope, "cycle_scope"),
    (Action::CycleLayout, "cycle_layout"),
    (Action::ScoreUp, "score_up"),
    (Action::ScoreDown, "score_down"),
    (Action::ChartStyle, "chart_style"),
    (Action::ToggleSma, "toggle_sma"),
    (Action::ToggleRsi, "toggle_rsi"),
    (Action::ToggleVolume, "toggle_volume"),
    (Action::MaType, "ma_type"),
    (Action::NextPreset, "next_preset"),
    (Action::PrevPreset, "prev_preset"),
    (Action::NextTheme, "next_theme"),
    (Action::PrevTheme, "prev_theme"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

const fn plain(code: KeyCode) -> KeyCombo {
    KeyCombo { code, mods: KeyModifiers::NONE }
}

const fn ch(c: char) -> KeyCombo {
    plain(KeyCode::Char(c))
}

/// Action -> bound keys, in `ACTIONS` order. The first key of an action is
/// the one the footer shows.
pub struct Keymap {
    bindings: Vec<(Action, Vec<KeyCombo>)>,
}

impl Default for Keymap {
    fn default() -> Self {
        let bindings = ACTIONS
            .iter()
            .map(|(action, _)| (*action, default_keys(*action)))
            .collect();
        Self { bindings }
    }
}

/// The traditional keys of each action; the exhaustive match makes the
/// compiler insist every new action ships with a default binding.
fn default_keys(action: Action) -> Vec<KeyCombo> {
    match action {
        Action::Quit => vec![ch('q')],
        Action::NextView => vec![plain(KeyCode::Tab)],
        Action::PrevView => vec![plain(KeyCode::BackTab)],
        Action::Settings => vec![ch('s')],
        Action::Help => vec![ch('?')],
        Action::Refresh => vec![ch('r')],
        Action::Up => vec![plain(KeyCode::Up), ch('k')],
        Action::Down => vec![plain(KeyCode::Down), ch('j')],
        Action::Left => vec![plain(KeyCode::Left), ch('h')],
        Action::Right => vec![plain(KeyCode::Right), ch('l')],
        Action::PageUp => vec![plain(KeyCode::PageUp)],
        Action::PageDown => vec![plain(KeyCode::PageDown)],
        Action::Open => vec![plain(KeyCode::Enter), ch('o')],
        Action::Card => vec![ch('v')],
        Action::CycleScope => vec![ch('f')],
        Action::CycleLayout => vec![ch('x')],
        // '=' is unshifted '+' on most layouts; both raise the filter.
        Action::ScoreUp => vec![ch('+'), ch('=')],
        Action::ScoreDown => vec![ch('-')],
        Action::ChartStyle => vec![ch('c')],
        Action::ToggleSma => vec![ch('m')],
        Action::ToggleRsi => vec![ch('i')],
        // b for bars: 'v' already opens the article card.
        Action::ToggleVolume => vec![ch('b')],
        // e for exponential: the key it switches the averages to.
        Action::MaType => vec![ch('e')],
        Action::NextPreset => vec![ch('t')],
        Action::PrevPreset => vec![ch('T')],
        // p for palette, in the t/T shape; session-only until Save.
        Action::NextTheme => vec![ch('p')],
        Action::PrevTheme => vec![ch('P')],
    }
}

impl Keymap {
    /// Build from the `[keybindings]` entries (as flat name -> specs pairs,
    /// so this module stays serde-free). A listed action REPLACES its
    /// default keys; unlisted actions keep theirs. Every problem warns and
    /// falls back instead of failing: unknown actions are ignored, bad or
    /// reserved keys are dropped, and a key claimed twice goes to the
    /// action earlier in `ACTIONS`.
    pub fn from_config<'a>(
        entries: impl IntoIterator<Item = (&'a str, Vec<&'a str>)>,
        warnings: &mut Vec<String>,
    ) -> Self {
        let mut map = Self::default();
        for (name, specs) in entries {
            let Some(action) = ACTIONS.iter().find(|(_, n)| *n == name).map(|(a, _)| *a) else {
                warnings.push(format!("[keybindings] unknown action \"{name}\", ignoring it"));
                continue;
            };
            let mut keys: Vec<KeyCombo> = Vec::new();
            for spec in &specs {
                match parse_key(spec) {
                    Ok(c) if is_reserved(&c) => warnings.push(format!(
                        "[keybindings] {name}: \"{spec}\" is reserved, dropping it"
                    )),
                    Ok(c) => {
                        if !keys.contains(&c) {
                            keys.push(c);
                        }
                    }
                    Err(e) => warnings.push(format!("[keybindings] {name}: {e}")),
                }
            }
            if keys.is_empty() {
                warnings.push(format!(
                    "[keybindings] {name}: no usable keys, keeping the default"
                ));
            } else {
                let slot = map.bindings.iter_mut().find(|(a, _)| *a == action).unwrap();
                slot.1 = keys;
            }
        }
        map.resolve_conflicts(warnings);
        map
    }

    /// A key bound to two actions goes to the one earlier in `ACTIONS`; an
    /// action stripped of its every key falls back to whichever of its
    /// default keys are still free.
    fn resolve_conflicts(&mut self, warnings: &mut Vec<String>) {
        let mut claimed: Vec<(KeyCombo, &'static str)> = Vec::new();
        for i in 0..self.bindings.len() {
            let (action, _) = self.bindings[i];
            let name = action_name(action);
            let keys = &mut self.bindings[i].1;
            keys.retain(|k| match claimed.iter().find(|(c, _)| c == k) {
                Some((_, owner)) => {
                    warnings.push(format!(
                        "[keybindings] {name}: \"{}\" is taken by {owner}, dropping it",
                        combo_label(k)
                    ));
                    false
                }
                None => true,
            });
            if keys.is_empty() {
                let fallback: Vec<KeyCombo> = default_keys(action)
                    .into_iter()
                    .filter(|k| claimed.iter().all(|(c, _)| c != k))
                    .collect();
                if fallback.is_empty() {
                    warnings.push(format!("[keybindings] {name}: left without any key"));
                } else {
                    warnings.push(format!(
                        "[keybindings] {name}: falling back to its default keys"
                    ));
                    *keys = fallback;
                }
            }
            claimed.extend(keys.iter().map(|k| (*k, name)));
        }
    }

    pub fn resolve(&self, ev: &KeyEvent) -> Option<Action> {
        let combo = normalize(ev.code, ev.modifiers);
        self.bindings
            .iter()
            .find_map(|(action, keys)| keys.contains(&combo).then_some(*action))
    }

    /// Footer key text for a hint group: the first bound key per action.
    /// Non-alphanumeric glyphs concatenate ("↑↓"), anything else joins with
    /// a slash ("c/m/i"), matching the traditional footer shapes.
    pub fn labels(&self, actions: &[Action]) -> String {
        let labels: Vec<String> = actions
            .iter()
            .filter_map(|a| self.first_label(*a))
            .collect();
        let all_glyphs = labels
            .iter()
            .all(|l| l.chars().count() == 1 && !l.chars().next().unwrap().is_alphanumeric());
        if all_glyphs { labels.concat() } else { labels.join("/") }
    }

    fn first_label(&self, action: Action) -> Option<String> {
        self.bindings
            .iter()
            .find(|(a, _)| *a == action)
            .and_then(|(_, keys)| keys.first())
            .map(combo_label)
    }

    /// Every key of an action, space-separated ("↑ k"), for the help
    /// overlay. Empty when the action lost all keys to conflicts.
    pub fn action_labels(&self, action: Action) -> String {
        self.bindings
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, keys)| keys.iter().map(combo_label).collect::<Vec<_>>().join(" "))
            .unwrap_or_default()
    }
}

/// The snake_case `[keybindings]` name of an action (the help overlay
/// shows it next to each key).
pub fn action_name(action: Action) -> &'static str {
    ACTIONS
        .iter()
        .find(|(a, _)| *a == action)
        .map(|(_, n)| *n)
        .expect("every action is listed in ACTIONS")
}

/// One key spec from `[keybindings]`: `[ctrl-][alt-][shift-]<base>`, base a
/// single character or a named key. The spec is normalized the way live
/// events are: `shift-` on a character folds into its uppercase form and
/// the modifier is dropped (crossterm reports shifted letters that way),
/// `shift-tab` becomes BackTab.
pub fn parse_key(spec: &str) -> Result<KeyCombo, String> {
    let bad = || format!("bad key \"{spec}\" (grammar: [ctrl-][alt-][shift-]key)");
    let mut rest = spec.trim();
    let mut mods = KeyModifiers::NONE;
    loop {
        let mut stripped = false;
        for (prefix, m) in [
            ("ctrl-", KeyModifiers::CONTROL),
            ("alt-", KeyModifiers::ALT),
            ("shift-", KeyModifiers::SHIFT),
        ] {
            if rest.len() > prefix.len()
                && rest.get(..prefix.len()).is_some_and(|p| p.eq_ignore_ascii_case(prefix))
            {
                mods |= m;
                rest = &rest[prefix.len()..];
                stripped = true;
            }
        }
        if !stripped {
            break;
        }
    }
    let mut chars = rest.chars();
    let code = match (chars.next(), chars.next()) {
        (Some(c), None) => KeyCode::Char(c),
        (Some(_), Some(_)) => named_key(rest).ok_or_else(bad)?,
        (None, _) => return Err(bad()),
    };
    // Fold SHIFT the way `normalize` folds live events.
    let combo = match code {
        KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => KeyCombo {
            code: KeyCode::BackTab,
            mods: mods - KeyModifiers::SHIFT,
        },
        KeyCode::Char(c) if mods.contains(KeyModifiers::SHIFT) => KeyCombo {
            code: KeyCode::Char(c.to_ascii_uppercase()),
            mods: mods - KeyModifiers::SHIFT,
        },
        code => KeyCombo { code, mods },
    };
    Ok(combo)
}

/// Multi-character base names, with the common aliases.
fn named_key(name: &str) -> Option<KeyCode> {
    let lower = name.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
        && (1..=12).contains(&n)
    {
        return Some(KeyCode::F(n));
    }
    match lower.as_str() {
        "esc" => Some(KeyCode::Esc),
        "enter" | "return" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "backtab" => Some(KeyCode::BackTab),
        "space" => Some(KeyCode::Char(' ')),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        "pgup" | "pageup" => Some(KeyCode::PageUp),
        "pgdn" | "pagedown" => Some(KeyCode::PageDown),
        "backspace" => Some(KeyCode::Backspace),
        "delete" | "del" => Some(KeyCode::Delete),
        "insert" => Some(KeyCode::Insert),
        _ => None,
    }
}

/// The keys `[keybindings]` may not touch: Ctrl-C (the force quit that
/// survives any keymap), Esc (quit or close, always works) and the plain
/// 1-9 digits (positional, matching the numbered header pills).
fn is_reserved(c: &KeyCombo) -> bool {
    match c.code {
        KeyCode::Esc => true,
        KeyCode::Char('c') if c.mods.contains(KeyModifiers::CONTROL) => true,
        KeyCode::Char('1'..='9') => c.mods.is_empty(),
        _ => false,
    }
}

/// Crossterm reports a shifted letter as the uppercase char WITH the SHIFT
/// modifier; bindings store just the uppercase char, so SHIFT is stripped
/// for Char codes (shift-Tab already arrives as BackTab).
fn normalize(code: KeyCode, mods: KeyModifiers) -> KeyCombo {
    match code {
        KeyCode::Char(c) => KeyCombo { code: KeyCode::Char(c), mods: mods - KeyModifiers::SHIFT },
        code => KeyCombo { code, mods },
    }
}

fn combo_label(combo: &KeyCombo) -> String {
    let mut s = String::new();
    if combo.mods.contains(KeyModifiers::CONTROL) {
        s.push_str("ctrl-");
    }
    if combo.mods.contains(KeyModifiers::ALT) {
        s.push_str("alt-");
    }
    match combo.code {
        KeyCode::Char(c) => s.push(c),
        KeyCode::Enter => s.push('⏎'),
        KeyCode::Up => s.push('↑'),
        KeyCode::Down => s.push('↓'),
        KeyCode::Left => s.push('←'),
        KeyCode::Right => s.push('→'),
        KeyCode::Tab => s.push_str("tab"),
        KeyCode::BackTab => s.push_str("shift-tab"),
        KeyCode::PageUp => s.push_str("pgup"),
        KeyCode::PageDown => s.push_str("pgdn"),
        KeyCode::Esc => s.push_str("esc"),
        KeyCode::F(n) => s.push_str(&format!("f{n}")),
        other => s.push_str(&format!("{other:?}").to_lowercase()),
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_binds_every_action_exactly_once() {
        let map = Keymap::default();
        assert_eq!(map.bindings.len(), ACTIONS.len());
        for (i, (action, name)) in ACTIONS.iter().enumerate() {
            assert_eq!(map.bindings[i].0, *action, "order drift at {name}");
            assert!(!map.bindings[i].1.is_empty(), "{name} has no keys");
        }
        // Config names are unique (they become [keybindings] keys later).
        let mut names: Vec<&str> = ACTIONS.iter().map(|(_, n)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ACTIONS.len());
    }

    #[test]
    fn no_key_is_bound_twice() {
        let map = Keymap::default();
        let mut all: Vec<KeyCombo> = map.bindings.iter().flat_map(|(_, k)| k.clone()).collect();
        let before = all.len();
        all.sort_by_key(|c| format!("{c:?}"));
        all.dedup();
        assert_eq!(all.len(), before, "a key is bound to two actions");
    }

    #[test]
    fn resolve_normalizes_shifted_letters() {
        let map = Keymap::default();
        let shifted_t = KeyEvent::new(KeyCode::Char('T'), KeyModifiers::SHIFT);
        assert_eq!(map.resolve(&shifted_t), Some(Action::PrevPreset));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('t'))), Some(Action::NextPreset));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::BackTab)), Some(Action::PrevView));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('z'))), None);
        // Ctrl-modified chars are distinct from plain ones.
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(map.resolve(&ctrl_r), None);
    }

    #[test]
    fn score_keys_resolve_shifted_and_unshifted() {
        let map = Keymap::default();
        // '+' usually arrives shifted; '=' is its unshifted position.
        let shifted_plus = KeyEvent::new(KeyCode::Char('+'), KeyModifiers::SHIFT);
        assert_eq!(map.resolve(&shifted_plus), Some(Action::ScoreUp));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('='))), Some(Action::ScoreUp));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('-'))), Some(Action::ScoreDown));
        // The footer shows the two filter keys as one glyph pair.
        assert_eq!(map.labels(&[Action::ScoreUp, Action::ScoreDown]), "+-");
    }

    #[test]
    fn parse_key_accepts_the_grammar() {
        assert_eq!(parse_key("q"), Ok(ch('q')));
        assert_eq!(parse_key("Q"), Ok(ch('Q')));
        // shift- on a letter folds into the uppercase char, like live events.
        assert_eq!(parse_key("shift-q"), Ok(ch('Q')));
        assert_eq!(
            parse_key("ctrl-x"),
            Ok(KeyCombo { code: KeyCode::Char('x'), mods: KeyModifiers::CONTROL })
        );
        assert_eq!(
            parse_key("ctrl-alt-x"),
            Ok(KeyCombo {
                code: KeyCode::Char('x'),
                mods: KeyModifiers::CONTROL | KeyModifiers::ALT,
            })
        );
        assert_eq!(parse_key("f5"), Ok(plain(KeyCode::F(5))));
        assert_eq!(parse_key("shift-tab"), Ok(plain(KeyCode::BackTab)));
        assert_eq!(parse_key("backtab"), Ok(plain(KeyCode::BackTab)));
        assert_eq!(parse_key("PgUp"), Ok(plain(KeyCode::PageUp)));
        assert_eq!(parse_key("pagedown"), Ok(plain(KeyCode::PageDown)));
        assert_eq!(parse_key("space"), Ok(ch(' ')));
        assert_eq!(parse_key("enter"), Ok(plain(KeyCode::Enter)));
        assert_eq!(parse_key("return"), Ok(plain(KeyCode::Enter)));
        assert_eq!(parse_key("-"), Ok(ch('-')));
    }

    #[test]
    fn parse_key_rejects_junk() {
        for bad in ["", "ctrl-", "supr", "qq", "f13", "f0"] {
            assert!(parse_key(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn from_config_replaces_listed_actions_only() {
        let mut warnings = Vec::new();
        let map = Keymap::from_config([("open", vec!["z", "enter"])], &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('z'))), Some(Action::Open));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Enter)), Some(Action::Open));
        // Replace semantics: the default 'o' is gone with the new list.
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('o'))), None);
        // Unlisted actions keep their defaults.
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('q'))), Some(Action::Quit));
        // The footer follows the remap through the same labels() path.
        assert_eq!(map.labels(&[Action::Open]), "z");
    }

    #[test]
    fn from_config_warns_and_keeps_defaults_on_bad_entries() {
        // Unknown action: ignored with a warning.
        let mut warnings = Vec::new();
        let map = Keymap::from_config([("opne", vec!["z"])], &mut warnings);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('z'))), None);

        // Every key bad or reserved: per-key warnings plus the default kept.
        let mut warnings = Vec::new();
        let map = Keymap::from_config([("quit", vec!["supr", "esc"])], &mut warnings);
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('q'))), Some(Action::Quit));

        // One good key among bad ones is enough to remap.
        let mut warnings = Vec::new();
        let map = Keymap::from_config([("quit", vec!["supr", "ctrl-q"])], &mut warnings);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(map.resolve(&ctrl_q), Some(Action::Quit));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('q'))), None);
    }

    #[test]
    fn from_config_conflicts_go_to_the_earlier_action() {
        // 'r' belongs to refresh, which precedes card in ACTIONS: card loses
        // the key and falls back to its default 'v'.
        let mut warnings = Vec::new();
        let map = Keymap::from_config([("card", vec!["r"])], &mut warnings);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('r'))), Some(Action::Refresh));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('v'))), Some(Action::Card));

        // Both sides remapped by the user: no conflict remains.
        let mut warnings = Vec::new();
        let map = Keymap::from_config(
            [("refresh", vec!["f5"]), ("card", vec!["r"])],
            &mut warnings,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::F(5))), Some(Action::Refresh));
        assert_eq!(map.resolve(&KeyEvent::from(KeyCode::Char('r'))), Some(Action::Card));
    }

    #[test]
    fn labels_match_the_footer_conventions() {
        let map = Keymap::default();
        assert_eq!(map.labels(&[Action::Up, Action::Down]), "↑↓");
        assert_eq!(map.labels(&[Action::Left, Action::Right]), "←→");
        assert_eq!(
            map.labels(&[Action::ChartStyle, Action::ToggleSma, Action::ToggleRsi]),
            "c/m/i"
        );
        assert_eq!(map.labels(&[Action::Open]), "⏎");
        assert_eq!(map.labels(&[Action::NextPreset]), "t");
    }
}
