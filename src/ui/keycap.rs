//! Keys drawn into panel titles as keycaps, and the title pills a click
//! acts on: the Insider list's label filter and mode switch, the score
//! stepper of the feeds. A click on any of them works like its key, so a
//! control that shows its key also takes the mouse.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;

use crate::app::App;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::hits::Button;

/// Where each clickable pill sits in its title line (offset, width) and
/// what a click on it does.
pub type Pills = Vec<(u16, u16, Button)>;

/// Appends a span, recording it as a pill when it acts on a click.
pub fn push_pill(
    spans: &mut Vec<Span<'static>>,
    pills: &mut Pills,
    text: String,
    style: Style,
    button: Option<Button>,
) {
    let at = spans.iter().map(Span::width).sum::<usize>() as u16;
    let span = Span::styled(text, style);
    if let Some(button) = button {
        pills.push((at, span.width() as u16, button));
    }
    spans.push(span);
}

/// A key drawn as a keycap: the letter on its own plate (the selection
/// shade, inverted on the ANSI theme), so it reads as the key to press
/// rather than as part of the heading or of a lit pill beside it. No gap
/// around it; the caller spaces it.
pub fn push_cap(
    spans: &mut Vec<Span<'static>>,
    pills: &mut Pills,
    key: &str,
    button: Option<Button>,
    theme: &Theme,
) {
    push_pill(
        spans,
        pills,
        format!(" {} ", label(key)),
        cap_style(theme),
        button,
    );
}

/// The plate a key sits on.
pub fn cap_style(theme: &Theme) -> Style {
    theme.selected().fg(theme.text).add_modifier(Modifier::BOLD)
}

/// A key as a keycap names it: a capital letter is the letter with shift,
/// which a plate saying just "A" would hide (keycaps print capitals).
pub fn label(key: &str) -> String {
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_uppercase() => format!("⇧{c}"),
        _ => key.to_string(),
    }
}

/// A keycap and the gap after it, ahead of the pills it acts on.
pub fn push_keycap(
    spans: &mut Vec<Span<'static>>,
    pills: &mut Pills,
    key: &str,
    button: Option<Button>,
    theme: &Theme,
) {
    push_cap(spans, pills, key, button, theme);
    spans.push(Span::raw(" "));
}

/// The key bound to `action` as a sentence names it ("- lowers the
/// score"), the footer's spelling; None when nothing is bound to it.
pub fn key_word(app: &App, action: Action) -> Option<String> {
    let key = app.keymap.labels(&[action]);
    (!key.is_empty()).then_some(key)
}

/// The keys that move the score filter, as the footer names them (empty
/// when unbound).
pub fn score_keys(app: &App) -> (String, String) {
    (
        app.keymap.labels(&[Action::ScoreDown]),
        app.keymap.labels(&[Action::ScoreUp]),
    )
}

/// The score filter as a stepper: the key that lowers it, the bar in
/// `text`, the key that raises it, the way a numeric field's buttons sit.
pub fn push_score_stepper(
    spans: &mut Vec<Span<'static>>,
    pills: &mut Pills,
    score: u8,
    (down, up): (&str, &str),
    text: Style,
    theme: &Theme,
) {
    if !down.is_empty() {
        push_cap(spans, pills, down, Some(Button::Score(-1)), theme);
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(format!("score {score}+"), text));
    if !up.is_empty() {
        spans.push(Span::raw(" "));
        push_cap(spans, pills, up, Some(Button::Score(1)), theme);
    }
}

/// The screen cells of a title's pills: the line starts at `x0` on row
/// `y`, and a pill cut off at `right` (the panel's corner) is not
/// clickable.
pub fn place(pills: Pills, x0: u16, y: u16, right: u16) -> Vec<(Rect, Button)> {
    pills
        .into_iter()
        .filter_map(|(at, width, button)| {
            let x = x0.saturating_add(at);
            (x + width <= right).then(|| (Rect::new(x, y, width, 1), button))
        })
        .collect()
}

/// `place` for a left-aligned title of a framed panel on `area`: it starts
/// one cell in from the corner.
pub fn place_left(pills: Pills, area: Rect) -> Vec<(Rect, Button)> {
    place(pills, area.x + 1, area.y, area.right().saturating_sub(1))
}

/// A title line and its pills.
pub type Title = (Line<'static>, Pills);

/// A choice drawn as pills, the current one lit: each a click away.
pub fn push_choices(
    spans: &mut Vec<Span<'static>>,
    pills: &mut Pills,
    choices: impl IntoIterator<Item = (String, Button, bool)>,
    theme: &Theme,
) {
    for (label, button, current) in choices {
        let style = if current {
            lit_style(theme)
        } else {
            theme.subtle()
        };
        push_pill(spans, pills, format!(" {label} "), style, Some(button));
    }
}

/// The lit pill: the current value of a choice.
pub fn lit_style(theme: &Theme) -> Style {
    Style::new().fg(theme.accent_text).bg(theme.accent)
}

/// Key hints in a title: each key on its plate, its word after it.
pub fn push_hints(spans: &mut Vec<Span<'static>>, hints: &[(&str, &str)], theme: &Theme) {
    let mut none = Pills::new();
    for (key, word) in hints {
        push_cap(spans, &mut none, key, None, theme);
        spans.push(Span::styled(format!(" {word} "), theme.subtle()));
    }
}

/// The first pair of title forms (left, and an optional right-aligned one)
/// that fits side by side on a panel `width` wide, a cell apart.
pub fn fit(
    width: u16,
    forms: impl IntoIterator<Item = (Title, Option<Title>)>,
) -> Option<(Title, Option<Title>)> {
    let room = usize::from(width.saturating_sub(2));
    forms.into_iter().find(|(left, right)| {
        left.0.width() + right.as_ref().map_or(0, |r| r.0.width() + 1) <= room
    })
}

/// A framed panel on `area` with its titles, and the screen cells of their
/// pills for the mouse.
pub fn panel(
    theme: &Theme,
    area: Rect,
    (left, left_pills): Title,
    right: Option<Title>,
) -> (Block<'static>, Vec<(Rect, Button)>) {
    let mut hits = place_left(left_pills, area);
    let mut block = theme.panel().title(left);
    if let Some((line, pills)) = right {
        let inner_right = area.right().saturating_sub(1);
        let x0 = inner_right.saturating_sub(line.width() as u16);
        hits.extend(place(pills, x0, area.y, inner_right));
        block = block.title(line.right_aligned());
    }
    (block, hits)
}

/// Keys for a panel's corner: each bound key on its plate with its word,
/// a click on the ones marked pressing it (not on one that deletes).
pub fn actions(app: &App, items: &[(Action, &str, bool)]) -> Title {
    let theme = &app.theme;
    let mut spans = Vec::new();
    let mut pills = Pills::new();
    for (action, word, clickable) in items {
        let key = app.keymap.labels(&[*action]);
        if key.is_empty() {
            continue;
        }
        let button = clickable.then_some(Button::Key(*action));
        spans.push(Span::raw(" "));
        push_cap(&mut spans, &mut pills, &key, button, theme);
        push_pill(
            &mut spans,
            &mut pills,
            format!(" {word}"),
            theme.subtle(),
            button,
        );
    }
    spans.push(Span::raw(" "));
    (Line::from(spans), pills)
}

/// A panel titled `heading` with `items` in its right corner where both fit.
pub fn heading_with_actions(
    app: &App,
    area: Rect,
    heading: &str,
    items: &[(Action, &str, bool)],
) -> (Block<'static>, Vec<(Rect, Button)>) {
    let left = || (app.theme.heading(heading.to_string()), Pills::new());
    let forms = [(left(), Some(actions(app, items))), (left(), None)];
    let (left, right) = fit(area.width, forms).unwrap_or_else(|| (left(), None));
    panel(&app.theme, area, left, right)
}
