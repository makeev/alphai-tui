//! The Earnings view: AlphAI's structured read of the selected ticker's
//! latest earnings filing, and what is scheduled next.
//!
//! The whole body is one scrollable paragraph, built to the pane's width and
//! pre-wrapped here rather than by the widget: a metrics table wrapped by
//! `Paragraph` loses its columns, and a separate table widget would give the
//! view a second scroll axis. Nothing is dropped when the terminal is short,
//! the section order is the priority order and the rest scrolls.

use chrono::{DateTime, Local, NaiveDate, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::alphai::{
    self, CalendarEvent, EarningsRead, KeyMetric, TickerEarnings, short_metric, signed_change,
};
use crate::app::App;
use crate::keymap::Action;
use crate::theme::Theme;
use crate::ui::{Hint, View, ViewId, ellipsize, news};

/// Room the verdict needs in the top border, right of the title.
const VERDICT_ROOM: u16 = 12;

/// Shortest pane that still gets the schedule strip; below this every row
/// belongs to the read itself.
const AHEAD_MIN_HEIGHT: u16 = 10;

/// Widest a line of prose grows, whatever the terminal allows. Long lines
/// are hard to track back to the next one; every typographic measure worth
/// the name lands near here.
const PROSE_MAX: usize = 96;

/// Narrowest metric name column worth printing. Columns are dropped to hold
/// it (see `metric_columns`).
const NAME_MIN: usize = 18;

/// Widest a figure column grows; a longer value is ellipsized, not carried.
const CELL_MAX: usize = 16;

/// Caps on the long lists, so a filing with fifty concerns cannot bury the
/// analysis. Counting rows here keeps the layout deterministic and testable,
/// unlike dropping sections by the height that happens to be free.
const LIST_CAP: usize = 5;
const SHORT_LIST_CAP: usize = 4;
const SEGMENT_CAP: usize = 6;
const QUOTE_CAP: usize = 2;
/// Older reads rendered in short form under the newest one.
const HISTORY_CAP: usize = 3;

pub struct EarningsView;

impl View for EarningsView {
    fn id(&self) -> ViewId {
        ViewId::Earnings
    }

    fn title(&self) -> &'static str {
        "Earnings"
    }

    fn hints(&self) -> &'static [Hint] {
        const HINTS: &[Hint] = &[
            Hint::act(&[Action::Quit], "quit"),
            Hint::fixed("1-9", "view"),
            Hint::act(&[Action::Up, Action::Down], "scroll"),
            Hint::act(&[Action::Left, Action::Right], "ticker"),
            Hint::act(&[Action::PageUp, Action::PageDown], "page"),
            Hint::act(&[Action::Open], "open"),
            Hint::act(&[Action::Refresh], "refresh"),
            Hint::act(&[Action::Help], "help"),
        ];
        HINTS
    }

    fn shows_earnings(&self) -> bool {
        true
    }

    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        let symbol = app.selected_symbol().to_string();
        let key = alphai::earnings_key(&symbol);
        let theme = app.theme;
        let missing = !app.earnings.contains_key(&symbol);
        let gate = theme.panel_titled(format!(" Earnings · {symbol} "));
        if news::render_gate_with(f, area, &gate, app, &key, missing) {
            return;
        }

        let data = &app.earnings[&symbol].data;
        let latest = data.latest();
        let head = match latest {
            Some(read) => format!(" Earnings · {symbol} · {} ", period_of(read)),
            None => format!(" Earnings · {symbol} "),
        };
        let room = area.width.saturating_sub(VERDICT_ROOM) as usize;
        let mut block = theme.panel_titled(ellipsize(&head, room));
        if let Some(word) = latest.map(|r| r.report().verdict.clone())
            && !word.is_empty()
        {
            block = block.title_top(Line::from(verdict_span(&word, &theme)).right_aligned());
        }
        // With a read on screen the date belongs in the frame; without one
        // the body already answers with it, and twice reads as a glitch.
        if let Some(next) = next_report(data).filter(|_| latest.is_some()) {
            block = block.title_bottom(Line::from(next.dim()).right_aligned());
        }

        let inner = block.inner(area);
        let ahead = app
            .calendar
            .as_ref()
            .map(|slot| ahead_line(&slot.events, Utc::now()))
            .filter(|line| !line.is_empty());
        let strip_h = u16::from(ahead.is_some() && inner.height >= AHEAD_MIN_HEIGHT);
        f.render_widget(block, area);
        let [body, strip] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(strip_h)]).areas(inner);

        let lines = body_lines(data, &symbol, body.width as usize, &theme);
        let max_scroll = lines.len().saturating_sub(body.height as usize);
        app.earnings_scroll = app.earnings_scroll.min(max_scroll as u16);
        f.render_widget(Paragraph::new(lines).scroll((app.earnings_scroll, 0)), body);
        if let Some(text) = ahead {
            let room = strip.width.saturating_sub(6) as usize;
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Ahead ", Style::new().fg(theme.accent)),
                    Span::styled(ellipsize(&text, room), Style::new().dim()),
                ])),
                strip,
            );
        }
    }
}

/// The verdict word, colored the way the site colors it. Only this one span
/// carries a judgement: the metric changes stay uncolored, because whether a
/// rise is good depends on the metric and the client cannot know.
fn verdict_span(verdict: &str, theme: &Theme) -> Span<'static> {
    let style = match verdict {
        "strong" | "solid" => Style::new().fg(theme.pos),
        "weak" => Style::new().fg(theme.neg),
        _ => Style::new().dim(),
    };
    Span::styled(format!(" {verdict} "), style.add_modifier(Modifier::BOLD))
}

/// "next report Nov 17, 2026" for the bottom border, or None when the
/// company has confirmed no date. Never an estimate: the API only serves
/// dates the company itself confirmed, and guessing one here would be worse
/// than saying nothing.
fn next_report(data: &TickerEarnings) -> Option<String> {
    let raw = data.next_report_date.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }
    let shown = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_or_else(|_| raw.to_string(), |d| d.format("%b %-d, %Y").to_string());
    Some(format!(" next report {shown} "))
}

/// The body: the newest read in full, older ones in short form below it, or
/// the state that explains why there is no read.
fn body_lines(
    data: &TickerEarnings,
    symbol: &str,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if data.unknown {
        return vec![
            Line::from(""),
            Line::from(format!("  AlphAI has no earnings coverage for {symbol}.")).bold(),
            Line::from(""),
            Line::from("  Reads are built from a company's own SEC filing, so they exist").dim(),
            Line::from("  for listed companies only.").dim(),
        ];
    }
    let Some(read) = data.latest() else {
        let mut lines = vec![
            Line::from(""),
            Line::from(format!("  No earnings read for {symbol} yet.")).bold(),
            Line::from(""),
            Line::from("  A read is AlphAI's structured analysis of the company's own").dim(),
            Line::from("  earnings filing, with every figure checked against the filing").dim(),
            Line::from("  text. It lands minutes after the filing reaches SEC EDGAR.").dim(),
            Line::from(""),
        ];
        lines.push(match next_report(data) {
            Some(next) => Line::from(format!("  {}", next.trim())).dim(),
            None => Line::from("  The next report date is not confirmed yet.").dim(),
        });
        return lines;
    };

    let mut lines = read_lines(read, symbol, width, theme);
    let older = &data.reports[1..];
    for read in older.iter().take(HISTORY_CAP) {
        lines.push(Line::from(""));
        lines.extend(short_read_lines(read, width, theme));
    }
    if older.len() > HISTORY_CAP {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("  +{} older reads", older.len() - HISTORY_CAP)).dim());
    }
    lines
}

/// One read, in full.
fn read_lines(
    read: &EarningsRead,
    symbol: &str,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let r = read.report();
    let mut lines = vec![meta_line(read, symbol, width, theme)];
    if !r.headline.is_empty() {
        lines.extend(wrap(&r.headline, width, "  ", None));
    }
    if !r.verdict_reason.is_empty() {
        lines.push(Line::from(""));
        lines.extend(wrap(&r.verdict_reason, width, "  ", None));
    }

    lines.extend(metric_lines(&r.key_metrics, width, theme));

    if !r.segments.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Segments", theme));
        for s in r.segments.iter().take(SEGMENT_CAP) {
            let mut head = format!("  {} {}", s.name, short_metric(&s.revenue));
            for (change, label) in [(&s.qoq_change, "q/q"), (&s.yoy_change, "y/y")] {
                if let Some(c) = change.as_deref().filter(|c| !c.is_empty()) {
                    head.push_str(&format!(" · {} {label}", signed_change(c)));
                }
            }
            lines.push(Line::from(ellipsize(&head, width)));
            if !s.driver.is_empty() {
                lines.extend(wrap(&s.driver, width, "    ", Some(Style::new().dim())));
            }
        }
    }

    if let Some(g) = &r.guidance {
        lines.push(Line::from(""));
        let head = if g.period.is_empty() {
            "Guidance".to_string()
        } else {
            format!("Guidance · {}", g.period)
        };
        lines.push(section(&head, theme));
        for (label, value) in [
            ("revenue", &g.revenue),
            ("gross margin", &g.gross_margin),
            ("operating expenses", &g.operating_expenses),
            ("tax rate", &g.tax_rate),
        ] {
            if let Some(v) = value.as_deref().filter(|v| !v.is_empty()) {
                lines.extend(wrap(v, width, &format!("  {label:<18}  "), None));
            }
        }
        for other in g.other.iter().take(SHORT_LIST_CAP) {
            lines.extend(wrap(other, width, "  ", None));
        }
    }

    if !r.vs_prior_guidance.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Versus prior guidance", theme));
        for c in &r.vs_prior_guidance {
            lines.extend(wrap(
                &format!(
                    "{}: {} against {} ({})",
                    c.metric, c.actual, c.prior_guidance, c.verdict
                ),
                width,
                "  ",
                None,
            ));
        }
    }

    for (title, items, cap) in [
        ("Concerns", &r.concerns, LIST_CAP),
        ("What to watch", &r.what_to_watch, LIST_CAP),
        ("Drivers", &r.drivers, LIST_CAP),
        ("Capital returns", &r.capital_returns, SHORT_LIST_CAP),
        (
            "Balance sheet and cash flow",
            &r.balance_sheet_cash_flow,
            SHORT_LIST_CAP,
        ),
    ] {
        if items.is_empty() {
            continue;
        }
        lines.push(Line::from(""));
        lines.push(section(title, theme));
        for item in items.iter().take(cap) {
            lines.extend(wrap(item, width, "  · ", None));
        }
        if items.len() > cap {
            lines.push(Line::from(format!("  +{} more", items.len() - cap)).dim());
        }
    }

    if !r.quotes.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Management, verbatim", theme));
        for q in r.quotes.iter().take(QUOTE_CAP) {
            let who = match q.role.as_deref().filter(|role| !role.is_empty()) {
                Some(role) => format!("{}, {role}", q.speaker),
                None => q.speaker.clone(),
            };
            lines.push(Line::from(ellipsize(&format!("  {who}"), width)).dim());
            lines.extend(wrap(&q.text, width, "    ", None));
        }
    }

    if !r.narrative().is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Analysis", theme));
        for para in r.narrative().split("\n\n") {
            if para.trim().is_empty() {
                continue;
            }
            lines.extend(wrap(para.trim(), width, "  ", None));
            lines.push(Line::from(""));
        }
        lines.pop();
    }

    if !r.missing_items.is_empty() {
        lines.push(Line::from(""));
        lines.push(section("Not in the filing", theme));
        for item in r.missing_items.iter().take(LIST_CAP) {
            lines.extend(wrap(item, width, "  · ", Some(Style::new().dim())));
        }
        if r.missing_items.len() > LIST_CAP {
            lines.push(Line::from(format!("  +{} more", r.missing_items.len() - LIST_CAP)).dim());
        }
    }
    lines
}

/// An older quarter: enough to compare against the one above it.
fn short_read_lines(read: &EarningsRead, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let r = read.report();
    let filed = read
        .filed()
        .map(|t| t.with_timezone(&Local).format("%-d %b %Y").to_string())
        .unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::styled(
            format!(" {} ", period_of(read)),
            Style::new()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        ),
        Span::styled(format!(" {} {filed}", read.form()), Style::new().dim()),
    ])];
    if !r.verdict.is_empty() {
        lines.push(Line::from(vec![
            Span::from("  "),
            verdict_span(&r.verdict, theme),
        ]));
    }
    if !r.verdict_reason.is_empty() {
        lines.extend(wrap(
            &r.verdict_reason,
            width,
            "  ",
            Some(Style::new().dim()),
        ));
    }
    let head: Vec<KeyMetric> = r.key_metrics.iter().take(LIST_CAP).cloned().collect();
    lines.extend(metric_lines(&head, width, theme));
    lines
}

/// The period the read covers: the read row states it, the analysis repeats
/// it, and either may be the one a legacy row filled in.
fn period_of(read: &EarningsRead) -> String {
    if read.fiscal_period.is_empty() {
        read.report().fiscal_period.clone()
    } else {
        read.fiscal_period.clone()
    }
}

/// The filing's own identity line: who filed what, when, and whether the
/// figures cleared the check against the filing text.
fn meta_line(read: &EarningsRead, symbol: &str, width: usize, theme: &Theme) -> Line<'static> {
    let r = read.report();
    let mut parts = Vec::new();
    if !r.company.is_empty() {
        parts.push(r.company.clone());
    }
    let filed = read.filed().map(|t| {
        t.with_timezone(&Local)
            .format("%-d %b %Y %H:%M")
            .to_string()
    });
    parts.push(match filed {
        Some(when) => format!("{} filed {when}", read.form()),
        None => read.form().to_string(),
    });
    if let Some(end) = r.period_end.as_deref().filter(|e| !e.is_empty()) {
        parts.push(format!("period end {end}"));
    }
    // Share classes share a filing: a GOOGL request answers with the read
    // filed under GOOG, and the line says so rather than looking wrong.
    let filed_under = [read.ticker.as_str(), r.ticker.as_str()]
        .into_iter()
        .find(|t| !t.is_empty());
    if let Some(class) = filed_under.filter(|t| *t != symbol) {
        parts.push(format!("filed under {class}"));
    }
    parts.push(
        if r.numbers_verified_from_document {
            "numbers verified"
        } else {
            "numbers not verified"
        }
        .to_string(),
    );
    Line::from(Span::styled(
        ellipsize(&format!("  {}", parts.join(" · ")), width),
        Style::new().fg(theme.accent),
    ))
}

/// Widths of the figure columns, measured on the text that will actually be
/// printed. A column whose every cell is empty disappears, which is what
/// keeps a filing with no comparisons (a foreign issuer's first 6-K) as
/// clean as one carrying all of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cols {
    pub name: usize,
    /// value, prior quarter, prior year, q/q, y/y. Zero means the column is
    /// not printed at all.
    pub cells: [usize; 5],
}

/// Headers of the figure columns, in render order.
const CELL_HEADERS: [&str; 5] = ["value", "prior Q", "prior Y", "q/q", "y/y"];

/// The printable cells of one metric, in the same order as `CELL_HEADERS`.
fn metric_cells(m: &KeyMetric) -> [String; 5] {
    let figure = |v: &Option<String>| {
        v.as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(short_metric)
            .unwrap_or_default()
    };
    let change = |v: &Option<String>| {
        v.as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(signed_change)
            .unwrap_or_default()
    };
    [
        short_metric(&m.value),
        figure(&m.prior_quarter),
        figure(&m.prior_year),
        change(&m.qoq_change),
        change(&m.yoy_change),
    ]
}

pub fn metric_columns(metrics: &[KeyMetric], inner: usize) -> Cols {
    let rows: Vec<[String; 5]> = metrics.iter().map(metric_cells).collect();
    let mut cells = [0usize; 5];
    for (i, w) in cells.iter_mut().enumerate() {
        let longest = rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0);
        *w = if longest == 0 {
            0
        } else {
            longest.max(CELL_HEADERS[i].len()).min(CELL_MAX)
        };
    }
    // The name column is sized to the longest name, not to the pane: a wide
    // terminal must not push the figures half a screen away from the row
    // they belong to. It only shrinks when the pane is too narrow to hold
    // the table, and then the figure columns start dropping — the prior
    // periods first (the changes carry the same story in less space), and
    // never the value itself.
    let longest_name = metrics
        .iter()
        .map(|m| m.name.chars().count())
        .max()
        .unwrap_or(NAME_MIN)
        .max(CELL_HEADERS[0].len());
    let mut droppable = [2usize, 1, 3, 4].into_iter();
    let name = loop {
        let used: usize = cells.iter().filter(|w| **w > 0).map(|w| w + 2).sum();
        let room = inner.saturating_sub(used);
        if room >= longest_name {
            break longest_name;
        }
        if room >= NAME_MIN {
            break room;
        }
        match droppable.next() {
            Some(i) => cells[i] = 0,
            None => break room.max(1),
        }
    };
    Cols { name, cells }
}

/// The metrics table: a basis sub-heading whenever the basis changes, then
/// one row per metric. Changes are printed, never colored: whether a rise is
/// good depends on the metric, and the filing does not say.
fn metric_lines(metrics: &[KeyMetric], width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from("")];
    if metrics.is_empty() {
        lines.push(Line::from("  no metric table in this filing").dim());
        return lines;
    }
    // The table is indented like the prose around it.
    let pad = "  ";
    let cols = metric_columns(metrics, width.saturating_sub(pad.len()));
    let shown: Vec<(usize, usize)> = cols
        .cells
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 0)
        .map(|(i, w)| (i, *w))
        .collect();

    let mut header = format!("{pad}{:<w$}", "Key metrics", w = cols.name);
    for (i, w) in &shown {
        header.push_str(&format!("  {:>w$}", CELL_HEADERS[*i], w = w));
    }
    lines.push(Line::from(Span::styled(
        header,
        Style::new().fg(theme.accent),
    )));

    let mut basis = String::new();
    for (n, m) in metrics.iter().enumerate() {
        if m.basis != basis {
            basis.clone_from(&m.basis);
            // "other" is the catch-all a filing without a GAAP split gets;
            // labelling every row with it would be noise.
            if !basis.is_empty() && basis != "other" {
                lines.push(Line::from(Span::styled(
                    format!("{pad}{basis}"),
                    Style::new().fg(theme.accent),
                )));
            }
        }
        // Banded rows: thirty numeric lines need something for the eye to
        // follow from a name across to its figures, and a leader of dots
        // carries it the rest of the way. The band is a muted row rather
        // than a background color, because the palette is foreground-only
        // and any background chosen here would fight whatever the terminal
        // actually has behind it.
        let style = if n % 2 == 1 {
            Style::new().add_modifier(Modifier::DIM)
        } else {
            Style::new()
        };
        let cells = metric_cells(m);
        let name = ellipsize(&m.name, cols.name);
        let mut figures = String::new();
        for (i, w) in &shown {
            figures.push_str(&format!("  {:>w$}", ellipsize(&cells[*i], *w), w = w));
        }
        lines.push(Line::from(vec![
            Span::styled(format!("{pad}{name}"), style),
            Span::styled(leader(name.chars().count(), cols.name), Style::new().dim()),
            Span::styled(figures, style),
        ]));
    }
    lines
}

/// Fill from the end of a metric name to its first figure. The dots keep a
/// fixed parity, so they line up down the table instead of shimmering, and
/// a gap too small to need guiding stays blank.
fn leader(used: usize, name_w: usize) -> String {
    let gap = name_w.saturating_sub(used);
    if gap < 6 {
        return " ".repeat(gap);
    }
    (0..gap)
        .map(|i| {
            // A space either side, so the dots never touch the name they
            // start from or the figure they run to.
            if (used + i) % 2 == 0 && i > 1 && i + 2 < gap {
                '·'
            } else {
                ' '
            }
        })
        .collect()
}

/// Section heading, styled like the card's.
fn section(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        format!("  {title}"),
        Style::new()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    ))
}

/// Wrap prose with a hanging indent: `indent` prefixes the first line and
/// its width prefixes the rest, so a bullet stays a bullet. Lines stop at a
/// readable measure rather than at the pane edge: a paragraph running the
/// full width of a 250-column terminal is a paragraph nobody reads.
fn wrap(text: &str, width: usize, indent: &str, style: Option<Style>) -> Vec<Line<'static>> {
    let hang = " ".repeat(indent.chars().count());
    let room = width
        .min(PROSE_MAX)
        .saturating_sub(indent.chars().count())
        .max(8);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > room {
            out.push(prefixed(
                &line,
                if out.is_empty() { indent } else { &hang },
                style,
            ));
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(prefixed(
            &line,
            if out.is_empty() { indent } else { &hang },
            style,
        ));
    }
    out
}

fn prefixed(text: &str, indent: &str, style: Option<Style>) -> Line<'static> {
    let full = format!("{indent}{text}");
    match style {
        Some(s) => Line::from(Span::styled(full, s)),
        None => Line::from(full),
    }
}

/// The next couple of scheduled macro releases, as one line. Only the high
/// tier: a month holds four of those and twenty of everything else.
pub fn ahead_line(events: &[CalendarEvent], now: DateTime<Utc>) -> String {
    let mut parts = Vec::new();
    for e in events {
        if e.importance != "high" || e.schedule_status == "cancelled" || e.phase != "upcoming" {
            continue;
        }
        let Some(at) = e.scheduled().filter(|at| *at >= now) else {
            continue;
        };
        let days = (at.date_naive() - now.date_naive()).num_days();
        let when = at.with_timezone(&Local).format("%-d %b").to_string();
        // The parenthetical spelling ("CPI (Consumer Price Index)") is for
        // a page with room; the strip is one line.
        let title = e
            .title
            .split_once(" (")
            .map_or(e.title.as_str(), |(t, _)| t);
        let mut part = match days {
            0 => format!("{title} today"),
            1 => format!("{title} {when} (tomorrow)"),
            n => format!("{title} {when} ({n}d)"),
        };
        let mut notes = Vec::new();
        if e.has_sep {
            notes.push("projections");
        }
        if e.press_conference_at.is_some() {
            notes.push("press conference");
        }
        // An inferred date comes from the agency's published cadence, not
        // from its calendar, and the strip says so rather than implying the
        // date is printed somewhere.
        if e.schedule_basis == "inferred" {
            notes.push("estimated");
        }
        if e.schedule_status == "postponed" {
            notes.push("postponed");
        }
        if !notes.is_empty() {
            part.push_str(&format!(" · {}", notes.join(", ")));
        }
        parts.push(part);
        if parts.len() == 2 {
            break;
        }
    }
    parts.join("  ·  ")
}

/// The article page of the read on screen, for the Open key.
pub fn open_url(app: &App) -> Option<String> {
    let symbol = app.selected_symbol();
    let read = app.earnings.get(symbol)?.data.latest()?;
    read.alphai_url()
}
