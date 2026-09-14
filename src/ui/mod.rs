pub mod article;
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
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

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

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App);
}

/// Register new display modes here. Order defines the tab cycle and the
/// 1..9 hotkeys.
pub static VIEWS: [&dyn View; 8] = [
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
    let rail_h = u16::from(app.show_rail && f.area().height >= rail::MIN_HEIGHT);
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
    VIEWS[app.view_idx].render(f, body, app);
    if chrome_h > 0 {
        f.render_widget(footer_line(app), footer);
    }
    if app.article_overlay.open {
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

/// `text` cut to `width` columns with a trailing ellipsis. Counts
/// characters rather than display cells (close enough for headlines, and
/// it never splits a character the way byte truncation would); a width of
/// 0 means "no limit known", so the text passes through untouched.
pub(crate) fn ellipsize(text: &str, width: usize) -> String {
    if width == 0 || text.chars().count() <= width {
        return text.to_string();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
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
    let mut spans = vec![
        Span::styled(" alphai-tui ", Style::new().bold().fg(app.theme.accent)),
        Span::raw(format!("· {} ", app.source_name)).dim(),
    ];
    let key = app.keymap.labels(&[Action::NextPreset]);
    let window = format!("{} / {}", app.range.as_str(), app.interval.as_str());
    let interval = if key.is_empty() {
        format!(" · {window}")
    } else {
        format!(" · {window} ({key}: change)")
    };
    let key_state = if app.alphai_enabled {
        " · alphai ✓"
    } else {
        " · alphai: no key (s)"
    };
    let clock = app.last_update.map_or_else(
        || " · loading…".to_string(),
        |ts| format!(" · upd {}", ts.format("%H:%M:%S")),
    );
    let status_room = spans.iter().map(Span::width).sum::<usize>()
        + key_state.chars().count()
        + clock.chars().count()
        + interval.chars().count();
    // Named tabs take about 60 columns; when the line would not fit, the
    // inactive ones shrink to their hotkey digit so the status at the end
    // (the clock and the chart interval) is not the part that gets cut.
    let named: usize = VIEWS.iter().map(|v| v.title().len() + 4).sum();
    let compact = named + status_room > width as usize;
    for (i, view) in VIEWS.iter().enumerate() {
        let active = i == app.view_idx;
        let style = if active {
            Style::new().fg(app.theme.accent_text).bg(app.theme.accent)
        } else {
            Style::new().dim()
        };
        let label = if compact && !active {
            format!(" {} ", i + 1)
        } else {
            format!(" {}:{} ", i + 1, view.title())
        };
        spans.push(Span::styled(label, style));
    }
    // Reserve space for the interval control before optional status text.
    for status in [key_state, clock.as_str()] {
        if spans.iter().map(Span::width).sum::<usize>()
            + status.chars().count()
            + interval.chars().count()
            <= width as usize
        {
            spans.push(Span::raw(status.to_string()).dim());
        }
    }
    spans.push(Span::raw(interval).dim());
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
    let mut spans = vec![Span::raw(hints).dim()];
    // What the app did on its own outranks what a source is complaining
    // about: after a fallback the errors are gone anyway, and the notice is
    // the only place the swap is explained.
    if let Some(notice) = app.notice() {
        spans.push(Span::styled(
            ellipsize(&format!("  {notice}"), 120),
            Style::new().fg(app.theme.warn).add_modifier(Modifier::BOLD),
        ));
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
