pub mod article;
pub mod calendar;
pub mod chart;
pub mod earnings;
pub mod help;
pub mod insider;
pub mod insider_chart;
pub mod news;
pub mod news_marks;
pub mod portfolio;
pub mod prompt;
pub mod rail;
pub mod settings;
pub mod split;
pub mod summary;
pub mod table;
mod time_axis;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{App, FeedKind, PromptKind};
use crate::keymap::Action;

/// Stable identity of a display mode, decoupled from its position in the
/// tab order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewId {
    Split,
    News,
    Table,
    Chart,
    Insider,
    Earnings,
    Summary,
    Portfolio,
    Calendar,
}

/// One footer hint: the keys of `actions` (looked up in the live keymap so
/// the footer always shows what is actually bound) plus a short label.
/// `fixed` names keys outside the keymap (the positional 1-9 digits).
pub struct Hint {
    pub actions: &'static [Action],
    pub fixed: &'static str,
    pub label: &'static str,
}

impl Hint {
    pub const fn act(actions: &'static [Action], label: &'static str) -> Self {
        Self {
            actions,
            fixed: "",
            label,
        }
    }

    pub const fn fixed(fixed: &'static str, label: &'static str) -> Self {
        Self {
            actions: &[],
            fixed,
            label,
        }
    }
}

/// Footer hints of views that only navigate the watchlist (the trait
/// default; Table uses it as is).
pub static DEFAULT_HINTS: &[Hint] = &[
    Hint::act(&[Action::Quit], "quit"),
    Hint::fixed("tab/1-9", "view"),
    Hint::act(&[Action::Up, Action::Down], "select"),
    Hint::act(&[Action::AddTicker], "add"),
    Hint::act(&[Action::NextPreset], "interval"),
    Hint::act(&[Action::Refresh], "refresh"),
    Hint::act(&[Action::Settings], "settings"),
    Hint::act(&[Action::Help], "help"),
];

/// A display mode. Views are stateless renderers: all mutable state
/// (selection, scroll) lives in `App`, so adding a view is a unit struct, a
/// `ViewId` variant and one entry in `VIEWS`. The capability methods drive
/// key handling and the demand-driven AlphAI fetch centrally in `App`: a
/// view declares what it shows and never fetches anything itself.
pub trait View: Sync {
    fn id(&self) -> ViewId;
    fn title(&self) -> &'static str;

    /// Footer hints while this view is active; keys render from the keymap.
    fn hints(&self) -> &'static [Hint] {
        DEFAULT_HINTS
    }

    /// The AlphAI feed to keep fresh while this view is visible (drives
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

    /// True when this view shows the selected ticker's earnings read, which
    /// is fetched per ticker on its own long TTL rather than as a feed.
    fn shows_earnings(&self) -> bool {
        false
    }

    /// The watchlist agenda and its paced report-date sweep.
    fn shows_calendar(&self) -> bool {
        false
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App);
}

/// Register new display modes here. Order defines the tab cycle and the
/// 1..9 hotkeys.
pub static VIEWS: [&dyn View; 9] = [
    &split::SplitView,
    &news::NewsView,
    &table::TableView,
    &chart::ChartView,
    &insider::InsiderView,
    &earnings::EarningsView,
    // Appended rather than slotted next to Table: the array order is the
    // tab cycle and the 1-9 hotkeys, so inserting one would renumber every
    // view behind it and break the muscle memory of anyone using them.
    &summary::SummaryView,
    &portfolio::PortfolioView,
    &calendar::CalendarView,
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
    // Before rows are built, so the hovered row renders without its unseen
    // marker on this very frame.
    app.mark_selected_seen();
    // The quote rail takes a row from the body in every view, so the
    // selected price is on screen even where the view itself has no room
    // for one (News, Insider, Earnings). Off via `[ui] quote_rail`, and
    // dropped on a terminal too short to spare the row.
    let rail_h = u16::from(rail::visible(app, f.area().height));
    // Bare mode (z) hands the header's and footer's rows to the view: in a
    // tmux pane the window name and the key hints are chrome the pane can
    // spare, while the rail keeps the ticker and its price on screen.
    let chrome_h = u16::from(!app.bare);
    let [header, rail_area, body, footer] = Layout::vertical([
        Constraint::Length(chrome_h),
        Constraint::Length(rail_h),
        Constraint::Min(0),
        Constraint::Length(chrome_h),
    ])
    .areas(f.area());

    if chrome_h > 0 {
        f.render_widget(header_line(app, header.width), header);
    }
    if rail_h > 0 {
        rail::render(f, rail_area, app);
    }
    app.article_overlay.inline = false;
    VIEWS[app.view_idx].render(f, body, app);
    if chrome_h > 0 {
        f.render_widget(footer_line(app), footer);
    }
    if app.article_overlay.open && !app.article_overlay.inline {
        article::render(f, app);
    }
    if app.help.open {
        help::render(f, app);
    }
    if app.settings.open {
        settings::render(f, app);
    }
    if app.prompt.open {
        prompt::render(f, app);
    }
}

/// Cut to display cells, preserving grapheme clusters (wide characters,
/// combining accents and joined emoji). Zero still means no known limit.
pub(crate) fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 || text.width() <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cells = grapheme.width();
        if used + cells > width - 1 {
            break;
        }
        out.push_str(grapheme);
        used += cells;
    }
    out.push('…');
    out
}

/// Reserve cells without truncating a meaningful price or label.
pub(crate) fn pad_right(text: &str, width: usize) -> String {
    format!("{text}{}", " ".repeat(width.saturating_sub(text.width())))
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

fn header_line(app: &App, width: u16) -> Paragraph<'static> {
    let mut spans = vec![Span::styled(
        " alphai-tui ",
        Style::new().bold().fg(app.theme.accent),
    )];
    let source = format!(" · {} {}", app.source_name, app.refresh_marker());
    let key_state = if app.alphai_enabled {
        " · alphai ✓"
    } else {
        " · alphai: no key (s)"
    };
    let status_room = spans.iter().map(Span::width).sum::<usize>()
        + Span::raw(&source).width()
        + Span::raw(key_state).width();
    // The header is navigation and connection context. Quote timestamps
    // belong to their prices; chart controls belong to the chart.
    let named: usize = VIEWS.iter().map(|v| v.title().len() + 4).sum();
    let compact = named + status_room > width as usize;
    for (i, view) in VIEWS.iter().enumerate() {
        let active = i == app.view_idx;
        let style = if active {
            Style::new().fg(app.theme.accent_text).bg(app.theme.accent)
        } else {
            app.theme.subtle()
        };
        let label = if compact && !active {
            if width < 80 {
                format!(" {}", i + 1)
            } else {
                format!(" {} ", i + 1)
            }
        } else {
            format!(" {}:{} ", i + 1, view.title())
        };
        spans.push(Span::styled(label, style));
    }
    let used = spans.iter().map(Span::width).sum::<usize>();
    let status = if used + Span::raw(format!("{source}{key_state}")).width() <= width as usize {
        format!("{source}{key_state}")
    } else {
        source
    };
    let status_w = Span::raw(&status).width();
    if used + status_w <= width as usize {
        spans.push(Span::raw(" ".repeat(width as usize - used - status_w)));
        spans.push(Span::styled(status, app.theme.subtle()));
    }
    Paragraph::new(Line::from(spans))
}

#[cfg(test)]
mod tests;

/// The current view's hints with their live keys. Split out of
/// `footer_line` so a test can measure it: the whole line has to fit the
/// 110-column terminal the README promises.
fn hints_text(app: &App) -> String {
    let parts: Vec<String> = VIEWS[app.view_idx]
        .hints()
        .iter()
        .map(|h| {
            let keys = if h.actions.is_empty() {
                h.fixed.to_string()
            } else {
                app.keymap.labels(h.actions)
            };
            format!("{keys} {}", h.label)
        })
        .collect();
    format!(" {}", parts.join(" · "))
}

fn footer_line(app: &App) -> Paragraph<'static> {
    let hints = if app.prompt.open {
        match app.prompt.kind {
            PromptKind::Ticker => " type a ticker · enter add · esc cancel".to_string(),
            PromptKind::Position => {
                " qty and average price · enter save · empty clears · esc cancel".to_string()
            }
        }
    } else if app.settings.open {
        // The settings form is a text input; its keys are not remappable.
        " ↑↓ move · ←→ change · enter edit/save · esc close".to_string()
    } else if app.article_overlay.open {
        " ↑↓/jk scroll · pgup/pgdn page · ⏎ open in browser · esc/v close".to_string()
    } else if app.help.open {
        " ↑↓/jk scroll · pgup/pgdn page · esc/? close".to_string()
    } else {
        hints_text(app)
    };
    let mut spans = vec![Span::styled(hints, app.theme.subtle())];
    // What the app did on its own outranks what a source is complaining
    // about: after a fallback the errors are gone anyway, and the notice is
    // the only place the swap is explained. It goes in front of the hints
    // for its minute, since behind them it started past column 95 and a
    // common terminal cut it off.
    if let Some(notice) = app.notice() {
        spans.insert(
            0,
            Span::styled(
                ellipsize(&format!(" {notice} "), 120),
                Style::new().fg(app.theme.warn).add_modifier(Modifier::BOLD),
            ),
        );
    } else if let Some((symbol, error)) = app.errors.iter().next() {
        let msg = ellipsize(&format!("  {symbol}: {error}"), 120);
        spans.push(Span::styled(
            msg,
            Style::new()
                .fg(app.theme.error)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Paragraph::new(Line::from(spans))
}
