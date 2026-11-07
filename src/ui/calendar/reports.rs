//! Presentation of corporate events. Issuer identity always comes from listings.

use std::time::Instant;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Cell;

use super::{AgendaId, AgendaRow, Flag, Kind, Status, When, age, fit, fmt_until_day};
use crate::alphai::{self, ReportEvent, ReportStatus};
use crate::app::{App, report_calendar::Window};
use crate::{market, theme::Theme};

pub(super) fn rows(app: &App, now: DateTime<Utc>) -> Vec<AgendaRow> {
    let today = market::et_time(now).date();
    let mut universe: Vec<_> = app
        .polled_symbols()
        .into_iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .collect();
    universe.sort();
    universe.dedup();
    let mut events: Vec<(ReportEvent, bool, Option<Instant>)> = Vec::new();
    for slot in &app.report_calendar.slots {
        for event in &slot.items {
            if event.report_date < today - Duration::days(7)
                || event.report_date >= today + Duration::days(45)
                || (event.report_read.is_none() && event.report_date < today)
                || !universe.iter().any(|s| event.contains(s))
            {
                continue;
            }
            let cached = !app.report_slot_fresh(slot, Instant::now());
            if let Some(old) = events.iter_mut().find(|(old, _, _)| event.same_report(old)) {
                if (old.1 && !cached) || old.2 < slot.fetched {
                    *old = (event.clone(), cached, slot.fetched);
                }
            } else {
                events.push((event.clone(), cached, slot.fetched));
            }
        }
    }
    // The old history hint is useful only until the calendar has covered a
    // symbol once. Empty success and reported rows both retire that hint.
    for symbol in &universe {
        if app.report_calendar.ever_covered.contains(symbol)
            || events.iter().any(|(e, _, _)| e.contains(symbol))
        {
            continue;
        }
        let Some(slot) = app.earnings.get(symbol) else {
            continue;
        };
        let Some(day) = slot
            .data
            .next_report_day()
            .filter(|d| *d >= today && *d < today + Duration::days(45))
        else {
            continue;
        };
        if slot.data.unknown {
            continue;
        }
        events.push((
            ReportEvent {
                ticker: symbol.clone(),
                company: symbol.clone(),
                listings: vec![symbol.clone()],
                sector: None,
                report_date: day,
                session: None,
                status: ReportStatus::Scheduled,
                reported_at: None,
                previous_read: None,
                report_read: None,
                incomplete_listings: true,
            },
            slot.fetched.elapsed() >= app.calendar_ttl()
                || app
                    .alphai_errors
                    .contains_key(&alphai::earnings_key(symbol)),
            Some(slot.fetched),
        ));
    }
    events
        .into_iter()
        .map(|(event, cached, fetched)| {
            let symbol = if event.contains(app.selected_symbol()) {
                app.selected_symbol().to_string()
            } else {
                universe
                    .iter()
                    .find(|s| event.contains(s))
                    .cloned()
                    .unwrap_or_else(|| event.ticker.clone())
            };
            let id = event.report_read.as_ref().map_or_else(
                || AgendaId::Report(event.listings.join(","), event.report_date),
                |r| AgendaId::Read(r.uid.clone()),
            );
            AgendaRow {
                id,
                when: When::DateOnly(event.report_date),
                title: format!(
                    "{} {symbol} {}",
                    glyph(&event),
                    company_name(&event.company)
                ),
                kind: Kind::Report { symbol },
                elapsed: event.status == ReportStatus::Reported,
                status: match event.status {
                    ReportStatus::Scheduled => Status::Scheduled,
                    ReportStatus::Reported => Status::Reported,
                    ReportStatus::Unknown => Status::Unknown,
                },
                estimated: false,
                importance: "report".into(),
                reference: String::new(),
                conference: None,
                projections: false,
                cached,
                report: Some(event),
                fetched,
            }
        })
        .collect()
}

pub(super) fn glyph(event: &ReportEvent) -> &'static str {
    match event.status {
        ReportStatus::Scheduled => "○",
        ReportStatus::Reported if event.report_read.is_some() => "●",
        ReportStatus::Reported => "◐",
        ReportStatus::Unknown => "?",
    }
}

pub(super) fn time(event: &ReportEvent) -> String {
    if event.status == ReportStatus::Reported {
        event.reported_at.map_or_else(
            || "—".into(),
            |at| market::et_time(at).format("%H:%M").to_string(),
        )
    } else {
        event.session_label().into()
    }
}

pub(super) fn event_note(event: &ReportEvent) -> String {
    if let Some(read) = &event.report_read {
        verdict_label(read.verdict.as_deref(), &read.fiscal_period)
    } else if event.status == ReportStatus::Reported {
        "read pending".into()
    } else if let Some(read) = &event.previous_read {
        format!(
            "previous: {}",
            verdict_label(read.verdict.as_deref(), &read.fiscal_period)
        )
    } else {
        String::new()
    }
}

pub(super) fn verdict_label(verdict: Option<&str>, period: &str) -> String {
    let lower = period.to_ascii_lowercase();
    let words: Vec<_> = lower.split(|c: char| !c.is_alphanumeric()).collect();
    let has = |terms: &[&str]| terms.iter().any(|s| words.contains(s));
    let noun = if has(&["h1", "h2", "half"])
        || lower.contains("six-month")
        || lower.contains("six month")
    {
        "half-year"
    } else if has(&["quarter", "q1", "q2", "q3", "q4"]) {
        "quarter"
    } else if lower.starts_with("fy") || has(&["year", "annual"]) || lower.contains("twelve-month")
    {
        "year"
    } else {
        "report"
    };
    match verdict.filter(|s| !s.trim().is_empty()) {
        Some(word) => format!("{word} {noun}"),
        None => "read available".into(),
    }
}

pub(super) fn title_cell(row: &AgendaRow, width: u16, theme: &Theme) -> Cell<'static> {
    let event = row.report.as_ref().unwrap();
    let Kind::Report { symbol } = &row.kind else {
        unreachable!()
    };
    let prefix = format!("{} {symbol} ", glyph(event));
    let head = fit(&prefix, width);
    let room = width.saturating_sub(Span::raw(&head).width() as u16);
    Cell::from(Line::from(vec![
        Span::styled(head, Style::new().fg(theme.accent).bold()),
        Span::styled(fit(&company_name(&event.company), room), theme.subtle()),
    ]))
}

pub(crate) fn next_report_day(app: &App, symbol: &str, now: DateTime<Utc>) -> Option<NaiveDate> {
    rows(app, now)
        .iter()
        .filter(|r| {
            !r.cached
                && r.report
                    .as_ref()
                    .is_some_and(|e| e.contains(symbol) && e.status == ReportStatus::Scheduled)
        })
        .filter_map(|r| r.when.day())
        .min()
}

pub(super) fn flag(app: &App, symbol: &str, now: DateTime<Utc>) -> Option<Flag> {
    let today = market::et_time(now).date();
    let rows = rows(app, now);
    let fresh: Vec<_> = rows
        .iter()
        .filter(|r| !r.cached)
        .filter_map(|r| r.report.as_ref())
        .filter(|e| e.contains(symbol))
        .collect();
    if let Some(event) = fresh
        .iter()
        .filter(|e| {
            e.status == ReportStatus::Reported
                && e.reported_at
                    .is_some_and(|at| market::et_time(at).date() == today)
        })
        .max_by_key(|e| e.reported_at)
    {
        let note = event_note(event);
        return Some(Flag {
            full: format!("{} {symbol} reported · {note}", glyph(event)),
            compact: if let Some(read) = &event.report_read {
                format!(
                    "● {}",
                    read.verdict
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .unwrap_or("reported")
                )
            } else {
                "◐ read pending".into()
            },
            urgent: true,
        });
    }
    let event = fresh
        .into_iter()
        .filter(|e| {
            e.status == ReportStatus::Scheduled
                && e.report_date >= today
                && e.report_date < today + Duration::days(7)
        })
        .min_by_key(|e| e.report_date)?;
    let text = format!(
        "reports {} {}",
        fmt_until_day(today, event.report_date),
        event.session_label()
    );
    Some(Flag {
        full: format!("⚑ {symbol} {text}"),
        compact: format!(
            "⚑ {} {}",
            if event.report_date == today {
                "today".into()
            } else {
                format!("{}d", (event.report_date - today).num_days())
            },
            event.session_label()
        ),
        urgent: event.report_date == today,
    })
}

pub(super) fn status(app: &App, rows: &[AgendaRow]) -> String {
    let describe = |window| {
        let slots: Vec<_> = app
            .report_calendar
            .slots
            .iter()
            .filter(|s| s.window == window)
            .collect();
        if slots.is_empty() {
            return "—".into();
        }
        let failed = slots.iter().filter(|s| s.error.is_some()).count();
        if failed > 0 {
            return format!("{failed}/{} failed · r to retry", slots.len());
        }
        let covered = slots.iter().filter(|s| s.covered).count();
        if covered < slots.len() {
            return format!("{covered}/{} groups loaded", slots.len());
        }
        let age = slots
            .iter()
            .filter_map(|s| s.fetched)
            .min()
            .map_or_else(|| "waiting".into(), age);
        if app.report_calendar.updating(window) {
            format!("updating · cached {age}")
        } else {
            age
        }
    };
    let count = rows.iter().filter(|r| r.report.is_some()).count();
    let n = app.report_calendar.universe.len();
    let groups = n.div_ceil(50);
    let mut text = format!(
        "reports: {count} events · {n} tickers / {groups} {} · near {} · far {}",
        if groups == 1 { "group" } else { "groups" },
        describe(Window::Near),
        describe(Window::Far)
    );
    if let Some(error) = &app.report_calendar.paused {
        text.push_str(&format!(" · paused · r to retry · {error}"));
    } else if let Some(error) = app
        .report_calendar
        .slots
        .iter()
        .find_map(|s| s.error.as_deref())
    {
        text.push_str(&format!(" · {error}"));
    }
    if rows
        .iter()
        .any(|r| r.report.as_ref().is_some_and(|e| e.incomplete_listings))
    {
        text.push_str(" · listing aliases incomplete");
    }
    text
}

// Same display rules as AlphAI's displayCompanyName. Never used for identity.
pub(super) fn company_name(raw: &str) -> String {
    let mut name = raw.trim();
    loop {
        let trimmed = name.trim_end_matches(['/', '\\']).trim();
        if trimmed != name {
            name = trimmed;
            continue;
        }
        if let Some(i) = name.rfind(['/', '\\']) {
            let code = &name[i + 1..];
            if (2..=4).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_uppercase()) {
                name = name[..i].trim();
                continue;
            }
        }
        break;
    }
    let key = |s: &str| s.replace(['.', ','], "").to_ascii_uppercase();
    let suffixes = "INC INCORPORATED CORP CORPORATION CO COMPANY LTD LIMITED PLC LLC LP LLP SA NV AG SE ASA AB OYJ CV SAB ADR ADS THE";
    let mut tokens: Vec<_> = name.split_whitespace().collect();
    while tokens.len() > 1
        && suffixes
            .split_whitespace()
            .any(|s| s == key(tokens.last().unwrap()))
    {
        tokens.pop();
    }
    while tokens.len() > 1
        && matches!(
            tokens.last().unwrap().to_ascii_lowercase().as_str(),
            "&" | "and"
        )
    {
        tokens.pop();
    }
    let acronyms = "AI IT US USA UK EU ETF ETN REIT AT&T II III IV VI VII AAON ABM ADMA AEHR AES AGCO AMC AMTD ASML AZZ BCE CAE CBRE CECO CME CNA COHU DTE EOG EVI GATX IDT IMAX ING OFG POET RELX RLI SAP VICI VSEE WAFD WEC";
    let shouty = name == name.to_uppercase();
    let out = tokens
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let k = key(t);
            if !shouty
                || acronyms.split_whitespace().any(|a| a == k)
                || (k.len() >= 2 && !k.contains(['A', 'E', 'I', 'O', 'U', 'Y']))
            {
                return t.to_string();
            }
            let lower = t.to_lowercase();
            if i > 0
                && "of and the for in on at to de van"
                    .split_whitespace()
                    .any(|w| w == lower.trim_matches(['.', ',']))
            {
                return lower;
            }
            let mut cap = true;
            lower
                .chars()
                .flat_map(|c| {
                    let upper = cap;
                    cap = matches!(c, '-' | '\'' | '’' | '/');
                    if upper {
                        c.to_uppercase().collect::<Vec<_>>()
                    } else {
                        vec![c]
                    }
                })
                .collect()
        })
        .collect::<Vec<_>>()
        .join(" ");
    let out = out.trim_end_matches([' ', ',']);
    if out.is_empty() {
        raw.to_string()
    } else {
        out.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn company_display_preserves_acronyms_and_mixed_case_without_merging_identity() {
        for (raw, display) in [
            ("ALPHABET INC", "Alphabet"),
            ("ASML HOLDING NV", "ASML Holding"),
            ("iRobot Corporation", "iRobot"),
            ("LEVI STRAUSS & CO", "Levi Strauss"),
            ("SPDR GOLD TRUST", "SPDR Gold Trust"),
            ("DANAHER CORP /DE/", "Danaher"),
            ("LA-Z-BOY INC", "La-Z-Boy"),
        ] {
            assert_eq!(company_name(raw), display);
        }
    }
    #[test]
    fn period_labels_never_invent_a_quarter_or_verdict() {
        for (period, noun) in [
            ("H1 2026", "half-year"),
            ("Six-month period", "half-year"),
            ("FY 2026", "year"),
            ("Annual 2026", "year"),
            ("Fourth Quarter 2026", "quarter"),
            ("Q3 FY2026", "quarter"),
            ("FY26 H2", "half-year"),
            ("2026 report", "report"),
        ] {
            assert_eq!(
                verdict_label(Some("solid"), period),
                format!("solid {noun}")
            );
        }
        assert_eq!(verdict_label(None, "Q3 2026"), "read available");
    }
}
