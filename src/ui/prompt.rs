//! The one-line prompt over the view: a ticker to watch (a) or a holding
//! to record (p).
//!
//! Deliberately not a form. The watchlist used to be reachable only
//! through CLI arguments or by hand-editing the config, which meant
//! quitting the app to follow a name someone just mentioned. One line and
//! two keys is the whole interaction, and a position is two numbers, so
//! it fits the same line rather than earning a screen.

use ratatui::Frame;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, PromptKind};
use crate::ui::centered;

/// Wide enough for the hint line, which is the longest thing in the box.
const WIDTH: u16 = 56;

pub fn render(f: &mut Frame, app: &App) {
    let theme = &app.theme;
    let prompt = &app.prompt;
    // Three lines of content plus the frame: input, message, hint.
    let area = centered(f.area(), WIDTH, 5);
    f.render_widget(Clear, area);

    // A block cursor rather than a real one: the terminal's cursor is left
    // hidden for the whole app, and one more glyph is cheaper than turning
    // it on and placing it for a single overlay.
    let typed = Line::from(vec![
        Span::raw(" "),
        Span::styled(prompt.input.clone(), Style::new().bold()),
        Span::styled("▏", Style::new().fg(theme.accent)),
    ]);
    let message = match &prompt.error {
        Some(err) => Line::from(Span::styled(
            format!(" {err}"),
            Style::new().fg(theme.error),
        )),
        None => Line::from(Span::raw(" ").dim()),
    };
    let (title, hint) = match prompt.kind {
        PromptKind::Ticker => (
            " Add ticker ".to_string(),
            " enter add · esc cancel · Save in settings keeps it",
        ),
        // The position line is saved to the config the moment it is
        // entered, so the hint says so rather than pointing at Save.
        PromptKind::Position => (
            format!(" Position · {} ", prompt.target),
            " qty avg · enter save · empty clears · esc cancel",
        ),
    };
    let hint = Line::from(Span::raw(hint).dim());

    f.render_widget(
        Paragraph::new(vec![typed, message, hint]).block(theme.panel_titled(title)),
        area,
    );
}
