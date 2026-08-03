use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::{App, FeedKind};
use crate::keymap::Action;
use crate::ui::{Hint, View, ViewId};
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

    fn hints(&self) -> &'static [Hint] {
        const HINTS: &[Hint] = &[
            Hint::act(&[Action::Quit], "quit"),
            Hint::fixed("tab/1-9", "view"),
            Hint::act(&[Action::Up, Action::Down], "select"),
            Hint::act(&[Action::CycleScope], "scope"),
            Hint::act(
                &[
                    Action::ChartStyle,
                    Action::ToggleSma,
                    Action::ToggleRsi,
                    Action::ToggleVolume,
                    Action::MaType,
                ],
                "chart",
            ),
            Hint::act(&[Action::NextPreset], "interval"),
            Hint::act(&[Action::Refresh], "refresh"),
            Hint::act(&[Action::Settings], "settings"),
            Hint::act(&[Action::Help], "help"),
        ];
        HINTS
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
