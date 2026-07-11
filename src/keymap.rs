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
    ChartStyle,
    ToggleSma,
    ToggleRsi,
    NextPreset,
    PrevPreset,
}

/// Every action with its snake_case config name, the single source of truth
/// for the future `[keybindings]` parsing and for the coverage test.
pub const ACTIONS: [(Action, &str); 20] = [
    (Action::Quit, "quit"),
    (Action::NextView, "next_view"),
    (Action::PrevView, "prev_view"),
    (Action::Settings, "settings"),
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
    (Action::ChartStyle, "chart_style"),
    (Action::ToggleSma, "toggle_sma"),
    (Action::ToggleRsi, "toggle_rsi"),
    (Action::NextPreset, "next_preset"),
    (Action::PrevPreset, "prev_preset"),
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
        Action::ChartStyle => vec![ch('c')],
        Action::ToggleSma => vec![ch('m')],
        Action::ToggleRsi => vec![ch('i')],
        Action::NextPreset => vec![ch('t')],
        Action::PrevPreset => vec![ch('T')],
    }
}

impl Keymap {
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
