//! The category picker (C where a news feed is shown): one row per category
//! with a box to tick, the full name and the label the list column uses for
//! it, so the column's short words can be read off here.
//!
//! A checklist rather than a key that cycles: fourteen categories and two
//! useful shapes of choice ("only these", "all but these") are too many to
//! step through one refetch at a time. The picker edits a draft, and the
//! one request the change costs goes out when Enter applies it.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};

use crate::app::{App, NewsScope};
use crate::category;
use crate::ui::hits::{ListKind, Modal};
use crate::ui::{centered, pad_right};

/// Room for the longest row: marker, box, full name, gap, column label.
const WIDTH: u16 = 48;
const NAME_W: usize = 28;

pub fn render(f: &mut Frame, app: &mut App) {
    let theme = &app.theme;
    let picker = &app.category_picker;
    // Every row, the summary and the key line, plus the frame.
    let area = centered(f.area(), WIDTH, category::ALL.len() as u16 + 4);
    f.render_widget(Clear, area);
    let block = theme.modal().title(theme.heading(" News categories "));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let [list, summary, keys] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);

    // A short terminal shows a window of the rows that keeps the cursor in
    // it, rather than cutting the list off below the fold.
    let visible = list.height.max(1) as usize;
    let skip = picker.cursor.saturating_sub(visible - 1);
    let rows: Vec<Line> = category::ALL
        .iter()
        .enumerate()
        .skip(skip)
        .take(visible)
        .map(|(i, c)| {
            let on = picker.draft.shows(i);
            let marker = if i == picker.cursor { "▶ " } else { "  " };
            let (tick, name) = if on {
                (
                    Span::styled("[x] ", Style::new().fg(theme.accent)),
                    Style::new(),
                )
            } else {
                (Span::styled("[ ] ", theme.faint()), theme.faint())
            };
            let line = Line::from(vec![
                Span::raw(marker),
                tick,
                Span::styled(pad_right(c.name, NAME_W), name),
                Span::styled(
                    format!("{:>w$}", c.short, w = category::COLUMN_WIDTH as usize),
                    theme.subtle(),
                ),
            ]);
            if i == picker.cursor {
                line.style(theme.selected())
            } else {
                line
            }
        })
        .collect();
    f.render_widget(Paragraph::new(rows), list);

    let shown = if picker.draft.is_all() {
        "every category shown".to_string()
    } else {
        format!("{} of {} shown", picker.draft.shown(), category::ALL.len())
    };
    // Trending is a fixed top ten picked on the server: the choice applies
    // to the ticker and market scopes, and saying so beats a filter that
    // silently does nothing.
    let note = if app.news_scope == NewsScope::Trending {
        " · not for trending"
    } else {
        ""
    };
    f.render_widget(
        Paragraph::new(Line::from(format!(" {shown}{note}")).style(theme.subtle())),
        summary,
    );
    f.render_widget(
        Paragraph::new(
            Line::from(" space toggle · o only · a all · enter apply").style(theme.subtle()),
        ),
        keys,
    );
    app.hits.modal = Some((area, Modal::Categories));
    app.hits.list(list, skip, ListKind::Categories);
}
