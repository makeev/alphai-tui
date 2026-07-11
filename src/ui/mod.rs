pub mod article;
pub mod chart;
pub mod insider;
pub mod news;
pub mod settings;
pub mod split;
pub mod table;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::{App, FeedKind};

/// Stable identity of a display mode, decoupled from its position in the
/// tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewId {
    Split,
    News,
    Table,
    Chart,
    Insider,
}

/// A display mode. Views are stateless renderers: all mutable state
/// (selection, scroll) lives in `App`, so adding a view is a unit struct, a
/// `ViewId` variant and one entry in `VIEWS`. The capability methods drive
/// key handling and the demand-driven AlphaAI fetch centrally in `App`: a
/// view declares what it shows and never fetches anything itself.
pub trait View: Sync {
    fn id(&self) -> ViewId;
    fn title(&self) -> &'static str;

    /// Footer hint line while this view is active.
    fn footer_hints(&self) -> &'static str {
        " q quit · tab/1-9 view · ↑↓ select · t interval · r refresh · s settings"
    }

    /// The AlphaAI feed to keep fresh while this view is visible (drives
    /// the demand-driven fetch, TTL refresh and the r retry). The
    /// request-budget guards stay in `App`.
    fn feed_shown(&self) -> Option<FeedKind> {
        None
    }

    /// True when ↑↓/jk drive the article list (with j paging the feed at
    /// the last row) instead of the watchlist selection.
    fn navigates_articles(&self) -> bool {
        false
    }

    /// True when the chart option keys (c, m, i) apply.
    fn has_chart_panel(&self) -> bool {
        false
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App);
}

/// Register new display modes here. Order defines the tab cycle and the
/// 1..9 hotkeys.
pub static VIEWS: [&dyn View; 5] = [
    &split::SplitView,
    &news::NewsView,
    &table::TableView,
    &chart::ChartView,
    &insider::InsiderView,
];

/// Index of a view in `VIEWS`. Every `ViewId` is registered exactly once
/// (the tests enforce it), so this is total.
pub fn view_index(id: ViewId) -> usize {
    VIEWS
        .iter()
        .position(|v| v.id() == id)
        .unwrap_or_else(|| panic!("view {id:?} is not registered in VIEWS"))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [header, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
            .areas(f.area());

    f.render_widget(header_line(app), header);
    VIEWS[app.view_idx].render(f, body, app);
    f.render_widget(footer_line(app), footer);
    if app.article_overlay.open {
        article::render(f, app);
    }
    if app.settings.open {
        settings::render(f, app);
    }
}

/// Centered modal rect used by the settings and article overlays.
pub(crate) fn centered(r: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(r.width.saturating_sub(2));
    let h = height.min(r.height.saturating_sub(2));
    Rect {
        x: r.x + (r.width.saturating_sub(w)) / 2,
        y: r.y + (r.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

fn header_line(app: &App) -> Paragraph<'static> {
    let mut spans = vec![
        Span::styled(" alphai-tui ", Style::new().bold().fg(Color::Cyan)),
        Span::raw(format!("· {} ", app.source_name)).dim(),
    ];
    for (i, view) in VIEWS.iter().enumerate() {
        let style = if i == app.view_idx {
            Style::new().fg(Color::Black).bg(Color::Cyan)
        } else {
            Style::new().dim()
        };
        spans.push(Span::styled(format!(" {}:{} ", i + 1, view.title()), style));
    }
    if app.alphai_enabled {
        spans.push(Span::raw(" · alphai ✓").dim());
    } else {
        spans.push(Span::raw(" · alphai: no key (s)").dim());
    }
    if let Some(ts) = app.last_update {
        spans.push(Span::raw(format!(" · upd {}", ts.format("%H:%M:%S"))).dim());
    } else {
        spans.push(Span::raw(" · loading…").dim());
    }
    spans.push(Span::raw(format!(" · {}", app.interval.as_str())).dim());
    Paragraph::new(Line::from(spans))
}

#[cfg(test)]
mod tests;

fn footer_line(app: &App) -> Paragraph<'static> {
    let hints = if app.settings.open {
        " ↑↓ move · enter edit/toggle/save · esc close"
    } else if app.article_overlay.open {
        " ↑↓/jk scroll · pgup/pgdn page · ⏎ open in browser · esc/v close"
    } else {
        VIEWS[app.view_idx].footer_hints()
    };
    let mut spans = vec![Span::raw(hints).dim()];
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
