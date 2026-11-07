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
    let near: Vec<_> = app
        .report_calendar
        .slots
        .iter()
        .filter(|s| s.window == crate::app::report_calendar::Window::Near)
        .collect();
    let reports_loaded = near.iter().any(|s| s.fetched.is_some());
    let reports_known = near.is_empty()
        || app
            .report_calendar
            .updating(crate::app::report_calendar::Window::Near)
        || near
            .iter()
            .any(|s| s.items.iter().any(|e| e.incomplete_listings))
        || near
            .iter()
            .any(|s| !app.report_slot_fresh(s, std::time::Instant::now()));
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
        let mut reports = format!(
            "{} {}{}",
            count.reports,
            if reports_known { "known " } else { "" },
            if count.reports == 1 {
                "report"
            } else {
                "reports"
            }
        );
        if !reports_loaded && !app.report_calendar.universe.is_empty() {
            reports =
                if app.report_calendar.paused.is_some() || near.iter().any(|s| s.error.is_some()) {
                    "reports unavailable".into()
                } else {
                    "reports loading".into()
                };
        }
        if width >= 100 && days == 1 {
            let reported = rows
                .iter()
                .filter(|r| {
                    r.report.as_ref().is_some_and(|e| {
                        e.status == alphai::ReportStatus::Reported
                            && e.reported_at
                                .is_some_and(|at| market::et_time(at).date() == today)
                    })
                })
                .count();
            reports.push_str(&format!(
                " · {reported} {}reported today",
                if reports_known { "known " } else { "" }
            ));
        }
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
                Span::raw(match app.calendar_filter {
                    super::Filter::All => format!("{} · {reports}", macro_text(compact)),
                    super::Filter::Reports => reports.clone(),
                    super::Filter::Macro => macro_text(compact),
                }),
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
pub(super) fn wrap(text: &str, width: u16, style: Style) -> Vec<Line<'static>> {
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
    if row.report.is_some() {
        return report_details(row, app, width, false);
    }
    let zone = app.chart.timezone;
    let timed = |at: DateTime<Utc>| {
        format!(
            "{} {}",
            time_axis::label(at.timestamp(), "%a %-d %b %Y %H:%M", zone, true),
            time_axis::zone_name(zone, true)
        )
    };
    let status = match row.status {
        Status::Reported => "reported",
        Status::Postponed => "postponed",
        Status::Cancelled => "cancelled",
        Status::Unknown => "status unknown",
        Status::Scheduled if row.when == When::Unknown => "date unavailable",
        Status::Scheduled if row.estimated => "estimated schedule",
        Status::Scheduled if matches!(row.kind, Kind::Report { .. }) => "scheduled",
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
        let mut lines = details(row, app, area.width);
        lines.truncate(5);
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

fn report_details(row: &AgendaRow, app: &App, width: u16, full: bool) -> Vec<Line<'static>> {
    let event = row.report.as_ref().unwrap();
    let mut facts = vec![format!(
        "{}{}",
        super::reports::company_name(&event.company),
        event
            .sector
            .as_ref()
            .map_or(String::new(), |s| format!(" · {s}"))
    )];
    let filing = event
        .reported_at
        .map(|at| format!("{} ET", market::et_time(at).format("%a %-d %b %Y %H:%M")));
    let status = match event.status {
        alphai::ReportStatus::Scheduled => format!(
            "{} ET · {} · scheduled",
            event.report_date,
            event.session_label()
        ),
        alphai::ReportStatus::Reported => format!(
            "{} · reported · {}",
            filing.as_deref().unwrap_or("Filing time unavailable"),
            super::reports::event_note(event)
        ),
        alphai::ReportStatus::Unknown => format!("{} ET · status unknown", event.report_date),
    };
    let freshness = row.fetched.map_or_else(|| "age unavailable".into(), age);
    facts.push(format!(
        "{status} · {}{freshness}",
        if row.cached { "cached " } else { "" }
    ));
    if let Some(read) = &event.previous_read {
        facts.push(format!(
            "Previous {} · {}",
            read.fiscal_period,
            super::reports::verdict_label(read.verdict.as_deref(), &read.fiscal_period)
        ));
        if let Some(guidance) = &read.guidance {
            facts.push(format!(
                "Guidance {}: {}",
                guidance.period.as_deref().unwrap_or(""),
                guidance.revenue
            ));
        }
        if full {
            facts.push(format!(
                "Previous filing: {} ET",
                market::et_time(read.published_at).format("%Y-%m-%d %H:%M")
            ));
            facts.extend(read.what_to_watch.iter().map(|s| format!("Watch: {s}")));
        } else if let Some(watch) = read.what_to_watch.first() {
            facts.push(format!("Watch: {watch}"));
        }
    }
    if full {
        facts.push(format!("Report date: {} ET", event.report_date));
        facts.push(format!("Listings: {}", event.listings.join(", ")));
        if event.incomplete_listings {
            facts.push("Listing aliases incomplete in this response.".into());
        }
        if let Some(read) = &event.report_read {
            facts.push(format!(
                "Read: {} · {} · {}",
                read.fiscal_period, read.ticker, read.uid
            ));
            facts.push(format!(
                "Read filing: {} ET",
                market::et_time(read.published_at).format("%Y-%m-%d %H:%M")
            ));
        } else if event.status == alphai::ReportStatus::Reported {
            facts.push("Read pending: no linked read is available yet. Processing and publication time are unknown.".into());
        }
        facts.push(
            "Source: AlphAI · BMO before open · AMC after close · report dates and filings ET"
                .into(),
        );
        facts
            .into_iter()
            .flat_map(|s| wrap(&s, width, app.theme.subtle()))
            .collect()
    } else {
        facts.truncate(5);
        let more = format!(" · {} more", app.keymap.action_labels(Action::Card));
        if let Some(last) = facts.last_mut() {
            *last = format!(
                "{}{}",
                super::fit(last, width.saturating_sub(Span::raw(&more).width() as u16)),
                more
            );
        }
        facts
            .into_iter()
            .map(|s| Line::from(super::fit(&s, width)).style(app.theme.subtle()))
            .collect()
    }
}

pub(super) fn full_details(row: &AgendaRow, app: &App, width: u16) -> Vec<Line<'static>> {
    if row.report.is_some() {
        report_details(row, app, width, true)
    } else {
        details(row, app, width)
    }
}
