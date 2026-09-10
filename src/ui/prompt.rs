//! The add-ticker prompt (a anywhere): a one-line box over the view.
//!
//! Deliberately not a form. The watchlist used to be reachable only
//! through CLI arguments or by hand-editing the config, which meant
//! quitting the app to follow a name someone just mentioned. One line and
//! two keys is the whole interaction.

use ratatui::Frame;
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::App;
use crate::ui::centered;

/// Wide enough for the hint line, which is the longest thing in the box.
const WIDTH: u16 = 56;

pub fn render(f: &mut Frame, app: &App) {
    let theme = &app.theme;
    let prompt = &app.ticker_prompt;
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
    let hint = Line::from(Span::raw(" enter add · esc cancel · Save in settings keeps it").dim());

    f.render_widget(
        Paragraph::new(vec![typed, message, hint])
            .block(theme.panel_titled(" Add ticker ".to_string())),
        area,
    );
}
