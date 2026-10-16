//! Read-only context for the agenda: near-term counts and the selected event.
//! Both use the table's normalized rows and the existing fetch timestamps.

use chrono::{DateTime, Duration, Utc};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;

use super::{AgendaRow, Kind, Status, When, age};
use crate::alphai;
use crate::app::App;
use crate::keymap::Action;
use crate::market;
use crate::ui::time_axis;

#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct Counts {
    pub macros: usize,
    pub high: usize,
    pub reports: usize,
}

/// Seven ET dates including today, not a rolling 168-hour interval. The
/// minute containing now still counts, like the table's `now` label. A
/// report without a time remains upcoming throughout its ET date.
pub(super) fn counts(rows: &[AgendaRow], now: DateTime<Utc>) -> [Counts; 2] {
    let today = market::et_time(now).date();
    let end = today + Duration::days(7);
    let mut totals = [Counts::default(), Counts::default()];
    for row in rows.iter().filter(|row| row.upcoming()) {
        let Some(day) = row.when.day().filter(|day| today <= *day && *day < end) else {
            continue;
        };
        for (i, total) in totals.iter_mut().enumerate() {
            if i == 0 && day != today {
                continue;
            }
            match row.kind {
                Kind::Macro { .. } => {
                    total.macros += 1;
                    total.high += usize::from(row.importance == "high");
                }
                Kind::Report { .. } => total.reports += 1,
            }
        }
    }
    totals
}

fn summaries(app: &App, rows: &[AgendaRow], now: DateTime<Utc>, width: u16) -> Vec<Line<'static>> {
    let today = market::et_time(now).date();
    let reports_known = app.calendar_symbols().any(|symbol| {
        app.earnings
            .get(symbol)
            .is_none_or(|slot| slot.data.unknown || slot.fetched.elapsed() >= app.calendar_ttl())
            || app
                .alphai_errors
                .contains_key(&alphai::earnings_key(symbol))
            || app.is_loading(&alphai::earnings_key(symbol))
            || app.report_date_retry.contains(symbol)
    });
    let mut lines = Vec::new();
    for ((label, days), count) in [("Today (ET)", 1), ("Next 7d (ET)", 7)]
        .into_iter()
        .zip(counts(rows, now))
    {
        // The API window is in UTC dates. One extra UTC date covers the
        // evening of the final ET date, including either side of DST.
        let macro_known = app.calendar.as_ref().is_some_and(|slot| {
            slot.fetched.elapsed() >= app.calendar_ttl()
                || app.alphai_errors.contains_key(alphai::CALENDAR_KEY)
                || slot.from > today
                || slot.to < today + Duration::days(days + 1)
        });
        let reports = format!(
            "{} {}{}",
            count.reports,
            if reports_known { "known " } else { "" },
            if count.reports == 1 {
                "report"
            } else {
                "reports"
            }
        );
        let macro_text = |compact| {
            if app.calendar.is_none() {
                if app.alphai_errors.contains_key(alphai::CALENDAR_KEY) {
                    "macro unavailable".into()
                } else {
                    "macro loading".into()
                }
            } else {
                let known = if macro_known { "known " } else { "" };
                if compact {
                    format!("{} {known}high", count.high)
                } else {
                    format!("{} {known}macro ({} high)", count.macros, count.high)
                }
            }
        };
        let line = [false, true].into_iter().find_map(|compact| {
            let line = Line::from(vec![
                Span::styled(format!("{label}: "), app.theme.subtle()),
                Span::raw(format!("{} · {reports}", macro_text(compact))),
            ]);
            (line.width() <= width as usize).then_some(line)
        });
        // Never truncate the qualifier on a partial count. Very narrow
        // panes retain the table and its existing coverage/status lines.
        let Some(line) = line else {
            return Vec::new();
        };
        lines.push(line);
    }
    lines
}

/// Prewrap by display cells so the whole detail can be measured before
/// allocating space. A long title or URL cannot silently hide the source
/// or freshness, and a grapheme is never split across lines.
fn wrap(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for word in text.split_whitespace() {
        let word_width = Span::raw(word).width();
        if !line.is_empty() && used + 1 + word_width > width as usize {
            lines.push(Line::from(std::mem::take(&mut line)).style(style));
            used = 0;
        }
        if !line.is_empty() {
            line.push(' ');
            used += 1;
        }
        for grapheme in word.graphemes(true) {
            let size = Span::raw(grapheme).width();
            if used + size > width as usize && !line.is_empty() {
                lines.push(Line::from(std::mem::take(&mut line)).style(style));
                used = 0;
            }
            line.push_str(grapheme);
            used += size;
        }
    }
    if !line.is_empty() {
        lines.push(Line::from(line).style(style));
    }
    lines
}

fn details(row: &AgendaRow, app: &App, width: u16) -> Vec<Line<'static>> {
    let zone = app.chart.timezone;
    let timed = |at: DateTime<Utc>| {
        format!(
            "{} {}",
            time_axis::label(at.timestamp(), "%a %-d %b %Y %H:%M", zone, true),
            time_axis::zone_name(zone, true)
        )
    };
    let status = match row.status {
        Status::Postponed => "postponed",
        Status::Cancelled => "cancelled",
        Status::Unknown => "status unknown",
        Status::Scheduled if row.when == When::Unknown => "date unavailable",
        Status::Scheduled if row.estimated => "estimated schedule",
        Status::Scheduled if matches!(row.kind, Kind::Report { .. }) => "confirmed date",
        Status::Scheduled => "scheduled",
    };
    let mut facts = vec![status.into()];
    match row.when {
        When::Timed(at) => {
            facts.push(timed(at));
            if row.elapsed {
                facts.push("scheduled time passed".into());
            }
        }
        When::DateOnly(day) => {
            facts.push(format!("{} ET", day.format("%a %-d %b %Y")));
            facts.push("time not provided".into());
        }
        When::Unknown => {}
    }
    if matches!(row.importance.as_str(), "high" | "medium" | "low") {
        facts.push(format!("{} importance", row.importance));
    }
    if !row.reference.is_empty() {
        facts.push(format!("period: {}", row.reference));
    }
    if let Some(at) = row.conference {
        facts.push(format!("press conference {}", timed(at)));
    }
    if row.projections {
        facts.push("projections".into());
    }
    let (fetched, source) = match &row.kind {
        Kind::Macro { source_url } => {
            let host = source_url
                .as_deref()
                .and_then(|url| reqwest::Url::parse(url.trim()).ok())
                .filter(|url| matches!(url.scheme(), "http" | "https"))
                .and_then(|url| url.host_str().map(str::to_owned));
            (
                app.calendar.as_ref().map(|slot| slot.fetched),
                host.map_or_else(
                    || "source unavailable".into(),
                    |host| format!("source: {host}"),
                ),
            )
        }
        Kind::Report { symbol } => {
            let open = app.keymap.action_labels(Action::Open);
            (
                app.earnings.get(symbol).map(|slot| slot.fetched),
                if open.is_empty() {
                    "source: AlphAI".into()
                } else {
                    format!("source: AlphAI · {open} Earnings")
                },
            )
        }
    };
    let freshness = fetched.map_or_else(
        || "update time unavailable".into(),
        |at| {
            format!(
                "{} {}",
                if row.cached { "cached" } else { "updated" },
                age(at)
            )
        },
    );
    let mut lines = wrap(&row.title, width, Style::new().fg(app.theme.accent).bold());
    lines.extend(wrap(&facts.join(" · "), width, app.theme.subtle()));
    lines.extend(wrap(
        &format!("{freshness} · {source}"),
        width,
        if row.cached {
            Style::new().fg(app.theme.warn)
        } else {
            app.theme.subtle()
        },
    ));
    lines
}

/// Render context only when it fits, keeping at least eight rows for the
/// table. The caller has already reserved the two fetch-status lines.
pub(super) fn render(
    f: &mut Frame,
    mut area: Rect,
    app: &App,
    rows: &[AgendaRow],
    selected: Option<usize>,
    now: DateTime<Utc>,
) -> Rect {
    if area.width < 50 || area.height < 10 {
        return area;
    }
    let summaries = summaries(app, rows, now, area.width);
    let height = summaries.len() as u16;
    f.render_widget(Paragraph::new(summaries), Rect { height, ..area });
    area.y += height;
    area.height -= height;
    if let Some(row) = selected.and_then(|i| rows.get(i)) {
        let lines = details(row, app, area.width);
        if lines.len() + 1 + 8 <= area.height as usize {
            let height = lines.len() as u16 + 1;
            area.height -= height;
            let detail = Rect::new(area.x, area.bottom(), area.width, height);
            f.render_widget(
                Paragraph::new(Line::from("─ Selected event ─").style(app.theme.faint())),
                Rect {
                    height: 1,
                    ..detail
                },
            );
            f.render_widget(
                Paragraph::new(lines),
                Rect::new(detail.x, detail.y + 1, detail.width, detail.height - 1),
            );
        }
    }
    area
}
