pub mod chart;
pub mod split;
pub mod table;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::App;

/// A display mode. Views are stateless renderers: all mutable state
/// (selection, scroll) lives in `App`, so adding a view is just a unit
/// struct + one entry in `VIEWS`.
pub trait View: Sync {
    fn title(&self) -> &'static str;
    fn render(&self, f: &mut Frame, area: Rect, app: &mut App);
}

/// Register new display modes here. Order defines the 1..9 hotkeys.
pub static VIEWS: [&dyn View; 3] = [&table::TableView, &chart::ChartView, &split::SplitView];

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
            .areas(f.area());

    f.render_widget(header_line(app), header);
    VIEWS[app.view_idx].render(f, body, app);
    f.render_widget(footer_line(app), footer);
}

fn header_line(app: &App) -> Paragraph<'static> {
    let mut spans = vec![
        Span::styled(" alphai-tui ", Style::new().bold().fg(Color::Cyan)),
        Span::raw(format!(
            "· {} · {}/{} ",
            app.source_name,
            app.range.as_str(),
            app.interval.as_str()
        ))
        .dim(),
    ];
    for (i, view) in VIEWS.iter().enumerate() {
        let style = if i == app.view_idx {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::new().dim()
        };
        spans.push(Span::styled(format!(" {}:{} ", i + 1, view.title()), style));
    }
    if let Some(ts) = app.last_update {
        spans.push(Span::raw(format!(" · upd {}", ts.format("%H:%M:%S"))).dim());
    } else {
        spans.push(Span::raw(" · loading…").dim());
    }
    Paragraph::new(Line::from(spans))
}

#[cfg(test)]
mod tests;

fn footer_line(app: &App) -> Paragraph<'static> {
    let mut spans = vec![
        Span::raw(" q quit · tab/1-9 view · ↑↓ select · r refresh").dim(),
    ];
    if let Some((symbol, error)) = app.errors.iter().next() {
        let mut msg = format!("  {symbol}: {error}");
        msg.truncate(120);
        spans.push(Span::styled(
            msg,
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
    }
    Paragraph::new(Line::from(spans))
}
