use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::App;
use crate::ui::View;
use crate::ui::{chart, news, table};

/// Below this body height the news half is dropped so the table and chart
/// keep usable space on tiny terminals.
const NEWS_MIN_BODY_HEIGHT: u16 = 16;

pub struct SplitView;

impl View for SplitView {
    fn title(&self) -> &'static str {
        "Split"
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let top = if area.height >= NEWS_MIN_BODY_HEIGHT {
            let [top, bottom] =
                Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .areas(area);
            news::render_panel(f, bottom, app);
            top
        } else {
            area
        };
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                .areas(top);
        table::render_table(f, left, app);
        chart::render_chart(f, right, app);
    }
}
