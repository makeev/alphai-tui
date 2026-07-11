use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::{App, FeedKind};
use crate::ui::{View, ViewId};
use crate::ui::{chart, news, table};

/// Below this body height the news half is dropped so the table and chart
/// keep usable space on tiny terminals.
const NEWS_MIN_BODY_HEIGHT: u16 = 16;

pub struct SplitView;

impl View for SplitView {
    fn id(&self) -> ViewId {
        ViewId::Split
    }

    fn title(&self) -> &'static str {
        "Split"
    }

    fn footer_hints(&self) -> &'static str {
        " q quit · tab/1-9 view · ↑↓ select · f scope · c/m/i chart · t interval · r refresh · s settings"
    }

    /// The embedded news strip drives the same demand-driven fetch as the
    /// full News view; jk still navigate the watchlist, not the strip.
    fn feed_shown(&self) -> Option<FeedKind> {
        Some(FeedKind::News)
    }

    fn has_chart_panel(&self) -> bool {
        true
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
