use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::App;
use crate::ui::View;
use crate::ui::{chart, table};

pub struct SplitView;

impl View for SplitView {
    fn title(&self) -> &'static str {
        "Split"
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)])
                .areas(area);
        table::render_table(f, left, app);
        chart::render_chart(f, right, app);
    }
}
