//! Watchlist agenda. Dates without a time stay dates, and every projection
//! (table, cursor, open target and rail) uses the same normalized events.

use std::collections::HashSet;
use std::time::Instant;

use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table};

use crate::alphai::{self, CalendarEvent};
use crate::app::App;
use crate::config::ChartTimezone;
use crate::keymap::Action;
use crate::market;
use crate::ui::{Hint, View, ViewId, time_axis};

static HINTS: &[Hint] = &[
    Hint::act(&[Action::Quit], "quit"),
    Hint::fixed("tab/1-9", "view"),
    Hint::act(&[Action::Up, Action::Down], "select"),
    Hint::act(&[Action::Open], "open"),
    Hint::act(&[Action::Refresh], "refresh"),
    Hint::act(&[Action::Help], "help"),
];

pub struct CalendarView;

impl View for CalendarView {
    fn id(&self) -> ViewId {
        ViewId::Calendar
    }
    fn title(&self) -> &'static str {
        "Calendar"
    }
    fn hints(&self) -> &'static [Hint] {
        HINTS
    }
    fn shows_calendar(&self) -> bool {
        true
    }
    fn render(&self, f: &mut Frame, area: Rect, app: &mut App) {
        render_calendar_at(f, area, app, Utc::now());
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AgendaId {
    Macro(String),
    Report(String, NaiveDate),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum When {
    Timed(DateTime<Utc>),
    DateOnly(NaiveDate),
    Unknown,
}

impl When {
    fn day(self) -> Option<NaiveDate> {
        match self {
            Self::Timed(at) => Some(market::et_time(at).date()),
            Self::DateOnly(day) => Some(day),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Scheduled,
    Postponed,
    Cancelled,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Macro { source_url: Option<String> },
    Report { symbol: String },
}

#[derive(Clone, Debug)]
pub(crate) struct AgendaRow {
    pub id: AgendaId,
    pub when: When,
    pub title: String,
    pub kind: Kind,
    elapsed: bool,
    status: Status,
    estimated: bool,
    importance: String,
    reference: String,
    conference: Option<DateTime<Utc>>,
    projections: bool,
    cached: bool,
}

impl AgendaRow {
    fn upcoming(&self) -> bool {
        !self.elapsed && self.status == Status::Scheduled && self.when != When::Unknown
    }

    fn tag(&self) -> &'static str {
        match self.status {
            Status::Postponed => "postponed",
            Status::Cancelled => "cancelled",
            Status::Unknown => "status unknown",
            Status::Scheduled if self.when == When::Unknown => "date unavailable",
            Status::Scheduled if self.estimated => "est.",
            _ => "",
        }
    }

    fn until(&self, now: DateTime<Utc>) -> String {
        if self.status != Status::Scheduled {
            return "—".into();
        }
        match self.when {
            When::Timed(at) => fmt_until(now, at),
            When::DateOnly(day) => fmt_until_day(market::et_time(now).date(), day),
            When::Unknown => "—".into(),
        }
    }

    fn notes(&self, zone: ChartTimezone) -> String {
        let mut notes = Vec::new();
        if !self.tag().is_empty() {
            notes.push(
                if self.tag() == "est." {
                    "estimated"
                } else {
                    self.tag()
                }
                .to_string(),
            );
        }
        if self.cached {
            notes.push("cached".into());
        }
        if let Some(at) = self.conference {
            notes.push(format!(
                "press conference {} {}",
                time_axis::label(at.timestamp(), "%H:%M", zone, true),
                time_axis::zone_name(zone, true)
            ));
        }
        if self.projections {
            notes.push("projections".into());
        }
        if matches!(self.kind, Kind::Report { .. }) {
            notes.push("confirmed · time not provided".into());
        }
        if !self.reference.is_empty() {
            notes.push(self.reference.clone());
        }
        notes.join(" · ")
    }
}

#[derive(Default)]
pub(crate) struct Selection {
    id: Option<AgendaId>,
    index: usize,
}

impl Selection {
    pub fn resolve(&self, rows: &[AgendaRow]) -> Option<usize> {
        if rows.is_empty() {
            return None;
        }
        if let Some(id) = &self.id {
            return Some(
                rows.iter()
                    .position(|r| &r.id == id)
                    .unwrap_or(self.index.min(rows.len() - 1)),
            );
        }
        rows.iter()
            .position(AgendaRow::upcoming)
            .or_else(|| rows.iter().rposition(|r| r.when != When::Unknown))
            .or(Some(0))
    }

    pub fn choose(&mut self, rows: &[AgendaRow], index: usize) {
        if let Some(row) = rows.get(index) {
            self.id = Some(row.id.clone());
            self.index = index;
        }
    }

    fn reconcile(&mut self, rows: &[AgendaRow]) -> Option<usize> {
        let selected = self.resolve(rows);
        if self.id.is_some()
            && let Some(i) = selected
        {
            self.choose(rows, i);
        }
        selected
    }
}

pub(crate) fn build_agenda(
    events: &[CalendarEvent],
    reports: &[(String, NaiveDate)],
    now: DateTime<Utc>,
) -> Vec<AgendaRow> {
    let today = market::et_time(now).date();
    let from = today - Duration::days(alphai::CALENDAR_LOOKBACK_DAYS);
    let to = today + Duration::days(alphai::CALENDAR_DAYS);
    let minute = now.with_second(0).unwrap().with_nanosecond(0).unwrap();
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for event in events {
        // Keep the first copy of a uid. Without a version in the response,
        // treating either copy as a more recent correction would be a guess.
        if !event.uid.is_empty() && !seen.insert(event.uid.clone()) {
            continue;
        }
        let when = event.scheduled().map_or(When::Unknown, When::Timed);
        if when.day().is_some_and(|day| day < from || day >= to) {
            continue;
        }
        let identity = if !event.uid.is_empty() {
            format!("uid:{}", event.uid)
        } else if !event.event_key.is_empty() && !event.reference_period.is_empty() {
            format!(
                "series:{:?}",
                (
                    &event.event_key,
                    &event.reference_period,
                    &event.release_stage
                )
            )
        } else {
            format!(
                "fallback:{:?}",
                (&event.title, &event.scheduled_at, &event.release_stage)
            )
        };
        rows.push(AgendaRow {
            id: AgendaId::Macro(identity),
            when,
            title: if event.title.trim().is_empty() {
                "Macro event".into()
            } else {
                event.title.clone()
            },
            kind: Kind::Macro {
                source_url: event.source_url.clone(),
            },
            elapsed: matches!(when, When::Timed(at) if at < minute),
            status: match event.schedule_status.as_str() {
                "scheduled" => Status::Scheduled,
                "postponed" => Status::Postponed,
                "cancelled" => Status::Cancelled,
                _ => Status::Unknown,
            },
            estimated: event.schedule_basis == "inferred",
            importance: event.importance.clone(),
            reference: fmt_reference_period(&event.reference_period),
            conference: event
                .press_conference_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|t| t.with_timezone(&Utc)),
            projections: event.has_sep,
            cached: false,
        });
    }
    let mut seen_reports = HashSet::new();
    for (symbol, day) in reports {
        if *day < from || *day >= to || !seen_reports.insert((symbol, day)) {
            continue;
        }
        rows.push(AgendaRow {
            id: AgendaId::Report(symbol.clone(), *day),
            when: When::DateOnly(*day),
            title: format!("{symbol} reports"),
            kind: Kind::Report {
                symbol: symbol.clone(),
            },
            elapsed: *day < today,
            status: Status::Scheduled,
            estimated: false,
            importance: "report".into(),
            reference: String::new(),
            conference: None,
            projections: false,
            cached: false,
        });
    }
    rows.sort_by_key(|r| {
        let group = if r.when == When::Unknown {
            2
        } else if r.elapsed {
            0
        } else {
            1
        };
        let seconds = match r.when {
            When::Timed(at) => market::et_time(at).time().num_seconds_from_midnight(),
            _ => 0,
        };
        (
            group,
            r.when.day(),
            seconds,
            matches!(r.when, When::Timed(_)),
            r.id.clone(),
        )
    });
    rows
}

pub(crate) fn agenda(app: &App, now: DateTime<Utc>) -> Vec<AgendaRow> {
    let reports: Vec<_> = app
        .calendar_symbols()
        .filter_map(|symbol| {
            let slot = app.earnings.get(symbol)?;
            (!slot.data.unknown)
                .then(|| slot.data.next_report_day())
                .flatten()
                .map(|day| (symbol.clone(), day))
        })
        .collect();
    let events = app
        .calendar
        .as_ref()
        .map_or(&[][..], |slot| slot.events.as_slice());
    let mut rows = build_agenda(events, &reports, now);
    for row in &mut rows {
        row.cached = match &row.kind {
            Kind::Macro { .. } => {
                app.calendar
                    .as_ref()
                    .is_some_and(|s| s.fetched.elapsed() >= app.calendar_ttl())
                    || app.alphai_errors.contains_key(alphai::CALENDAR_KEY)
            }
            Kind::Report { symbol } => {
                app.earnings
                    .get(symbol)
                    .is_some_and(|s| s.fetched.elapsed() >= app.calendar_ttl())
                    || app
                        .alphai_errors
                        .contains_key(&alphai::earnings_key(symbol))
            }
        };
    }
    rows
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum OpenTarget {
    Url(String),
    Earnings(String),
    Unavailable,
}

pub(crate) fn open_target(app: &App, now: DateTime<Utc>) -> Option<OpenTarget> {
    let rows = agenda(app, now);
    let row = &rows[app.calendar_selection.resolve(&rows)?];
    Some(match &row.kind {
        Kind::Report { symbol } => OpenTarget::Earnings(symbol.clone()),
        Kind::Macro { source_url } => match source_url.as_deref().map(str::trim) {
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                OpenTarget::Url(url.into())
            }
            _ => OpenTarget::Unavailable,
        },
    })
}

pub(crate) fn fmt_until(now: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let minutes = at.timestamp().div_euclid(60) - now.timestamp().div_euclid(60);
    if minutes == 0 {
        return "now".into();
    }
    let mins = minutes.unsigned_abs();
    let text = if mins >= 1440 {
        format!("{}d {}h", mins / 1440, (mins % 1440) / 60)
    } else if mins >= 60 {
        format!("{}h {}m", mins / 60, mins % 60)
    } else {
        format!("{mins}m")
    };
    if minutes > 0 {
        format!("in {text}")
    } else {
        format!("{text} ago")
    }
}

fn short_until(now: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let mins = (at.timestamp().div_euclid(60) - now.timestamp().div_euclid(60)).max(0);
    if mins >= 1440 {
        format!("{}d", mins / 1440)
    } else if mins >= 60 {
        format!("{}h", mins / 60)
    } else if mins > 0 {
        format!("{mins}m")
    } else {
        "now".into()
    }
}

fn fmt_until_day(today: NaiveDate, day: NaiveDate) -> String {
    match (day - today).num_days() {
        0 => "today".into(),
        d if d > 0 => format!("in {d}d"),
        d => format!("{}d ago", -d),
    }
}

fn fmt_reference_period(value: &str) -> String {
    if let Ok(day) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return day.format("%-d %b %Y").to_string();
    }
    if let Ok(day) = NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d") {
        return day.format("%b %Y").to_string();
    }
    if let Some((year, quarter)) = value.split_once("-Q")
        && year.len() == 4
        && year.parse::<u16>().is_ok()
        && matches!(quarter, "1" | "2" | "3" | "4")
    {
        return format!("Q{quarter} {year}");
    }
    value.into()
}

pub(crate) struct Flag {
    pub full: String,
    pub compact: String,
    pub urgent: bool,
}

pub(crate) fn flag(app: &App, symbol: &str, now: DateTime<Utc>) -> Option<Flag> {
    if !app.alphai_enabled {
        return None;
    }
    let today = market::et_time(now).date();
    if let Some(slot) = app.earnings.get(symbol)
        && !slot.data.unknown
        && slot.fetched.elapsed() < app.calendar_ttl()
        && !app
            .alphai_errors
            .contains_key(&alphai::earnings_key(symbol))
        && let Some(day) = slot.data.next_report_day()
        && today <= day
        && day <= today + Duration::days(7)
    {
        let days = (day - today).num_days();
        return Some(Flag {
            full: format!("⚑ {symbol} reports {}", fmt_until_day(today, day)),
            compact: format!(
                "⚑ reports {}",
                if days == 0 {
                    "today".into()
                } else {
                    format!("{days}d")
                }
            ),
            urgent: days == 0,
        });
    }
    let slot = app.calendar.as_ref()?;
    if slot.fetched.elapsed() >= app.calendar_ttl()
        || app.alphai_errors.contains_key(alphai::CALENDAR_KEY)
    {
        return None;
    }
    let (event, at) = slot
        .events
        .iter()
        .filter(|e| e.importance == "high" && e.schedule_status == "scheduled")
        .filter_map(|e| e.scheduled().map(|at| (e, at)))
        .filter(|(_, at)| now <= *at && *at <= now + Duration::days(7))
        .min_by_key(|(e, at)| (*at, &e.uid))?;
    let name = event.short_name();
    let est = if event.schedule_basis == "inferred" {
        " est."
    } else {
        ""
    };
    Some(Flag {
        full: format!("⚑ {name}{est} {}", fmt_until(now, at)),
        compact: format!("⚑ {name}{est} {}", short_until(now, at)),
        urgent: at - now < Duration::hours(24),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Col {
    Date,
    Time,
    Title,
    Until,
    Tier,
    Notes,
}

fn columns(avail: u16) -> Vec<(Col, u16)> {
    for (date, time, tier, notes) in [
        (true, true, true, true),
        (true, true, true, false),
        (true, true, false, false),
        (true, false, false, false),
        (false, false, false, false),
    ] {
        let fixed = u16::from(date) * 11 + u16::from(time) * 6 + 13 + u16::from(tier) * 6;
        if avail < fixed + 18 + u16::from(notes) * 16 {
            continue;
        }
        let title = if notes {
            (avail - fixed - 16).min(28)
        } else {
            avail - fixed
        };
        let mut cols = Vec::new();
        if date {
            cols.push((Col::Date, 11));
        }
        if time {
            cols.push((Col::Time, 6));
        }
        cols.extend([(Col::Title, title), (Col::Until, 13)]);
        if tier {
            cols.push((Col::Tier, 6));
        }
        if notes {
            cols.push((Col::Notes, avail - fixed - title));
        }
        return cols;
    }
    vec![(Col::Title, avail)]
}

/// Fit terminal cells, not UTF-8 bytes or character counts. All callers know
/// their available width, including zero (which means show nothing).
fn fit(value: &str, width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    if Span::raw(value).width() <= width as usize {
        return value.into();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in value.chars() {
        let w = Span::raw(c.to_string()).width();
        if used + w + 1 > width as usize {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

fn title_cell(row: &AgendaRow, width: u16, notes_visible: bool) -> String {
    if notes_visible {
        return fit(&row.title, width);
    }
    let mut tags = Vec::new();
    if !row.tag().is_empty() {
        tags.push(row.tag());
    }
    if row.cached {
        tags.push("cached");
    }
    if tags.is_empty() {
        return fit(&row.title, width);
    }
    let suffix = format!(" [{}]", tags.join(", "));
    let suffix_width = Span::raw(&suffix).width() as u16;
    if width <= suffix_width {
        return fit(suffix.trim(), width);
    }
    format!("{}{}", fit(&row.title, width - suffix_width), suffix)
}

fn event_row(row: &AgendaRow, cols: &[(Col, u16)], app: &App, now: DateTime<Utc>) -> Row<'static> {
    let theme = &app.theme;
    let mut style = if row.importance == "high" || matches!(row.kind, Kind::Report { .. }) {
        Style::new().fg(theme.accent).bold()
    } else {
        Style::new()
    };
    if row.status == Status::Postponed || row.status == Status::Unknown {
        style = style.fg(theme.warn);
    }
    if row.elapsed || row.status == Status::Cancelled || row.importance == "low" {
        style = style.remove_modifier(Modifier::BOLD).dim();
    }
    let notes_visible = cols.iter().any(|(c, _)| *c == Col::Notes);
    let zone = app.chart.timezone;
    let cells: Vec<Cell> = cols
        .iter()
        .map(|&(col, width)| {
            let value = match col {
                Col::Date => match row.when {
                    When::Timed(at) => time_axis::label(at.timestamp(), "%a %-d %b", zone, true),
                    When::DateOnly(day) => day.format("%a %-d %b").to_string(),
                    When::Unknown => "—".into(),
                },
                Col::Time => match row.when {
                    When::Timed(at) => time_axis::label(at.timestamp(), "%H:%M", zone, true),
                    _ => "—".into(),
                },
                Col::Title => title_cell(row, width.saturating_sub(1), notes_visible),
                Col::Until => row.until(now),
                Col::Tier => match row.importance.as_str() {
                    "report" => "earn".into(),
                    "high" => "high".into(),
                    "medium" => "med".into(),
                    "low" => "low".into(),
                    _ => "—".into(),
                },
                Col::Notes => row.notes(zone),
            };
            Cell::from(fit(&value, width.saturating_sub(1)))
        })
        .collect();
    Row::new(cells).style(style)
}

fn separator(
    cols: &[(Col, u16)],
    now: DateTime<Utc>,
    zone: ChartTimezone,
    undated: bool,
) -> Row<'static> {
    let cells: Vec<Cell> = cols
        .iter()
        .map(|&(col, width)| {
            let text = match col {
                Col::Date if !undated => time_axis::label(now.timestamp(), "%a %-d %b", zone, true),
                Col::Time if !undated => time_axis::label(now.timestamp(), "%H:%M", zone, true),
                Col::Title if undated => "─ Date unavailable ─".into(),
                Col::Title => format!("─ now · {} ─", time_axis::zone_name(zone, true)),
                _ => "─".repeat(width.saturating_sub(1) as usize),
            };
            Cell::from(fit(&text, width.saturating_sub(1)))
        })
        .collect();
    Row::new(cells).dim()
}

fn age(at: Instant) -> String {
    let minutes = at.elapsed().as_secs() / 60;
    if minutes >= 60 {
        format!("{}h ago", minutes / 60)
    } else if minutes > 0 {
        format!("{minutes}m ago")
    } else {
        "just now".into()
    }
}

fn statuses(app: &App, rows: &[AgendaRow], now: DateTime<Utc>) -> [String; 2] {
    let retry = app.keymap.action_labels(Action::Refresh);
    let retry = if retry.is_empty() {
        "refresh".into()
    } else {
        format!("{retry} to retry")
    };
    let macro_line = if let Some(error) = app.alphai_errors.get(alphai::CALENDAR_KEY) {
        let cached = app
            .calendar
            .as_ref()
            .map_or(String::new(), |s| format!(" · cached {}", age(s.fetched)));
        format!("macro: update failed{cached} · {retry} · {error}")
    } else if app.is_loading(alphai::CALENDAR_KEY) {
        "macro: updating…".into()
    } else if let Some(slot) = &app.calendar {
        let today = market::et_time(now).date();
        let partial = slot.from > today - Duration::days(alphai::CALENDAR_LOOKBACK_DAYS)
            || slot.to < today + Duration::days(alphai::CALENDAR_DAYS + 1);
        format!(
            "macro: updated {}{}",
            age(slot.fetched),
            if partial {
                " · cached window, refresh for full range"
            } else {
                ""
            }
        )
    } else {
        "macro: waiting…".into()
    };
    let symbols: Vec<_> = app.calendar_symbols().collect();
    let checked = symbols
        .iter()
        .filter(|s| {
            app.earnings.contains_key(s.as_str())
                && !app.alphai_errors.contains_key(&alphai::earnings_key(s))
        })
        .count();
    let failed = symbols
        .iter()
        .filter(|s| app.alphai_errors.contains_key(&alphai::earnings_key(s)))
        .count();
    let confirmed = rows
        .iter()
        .filter(|r| matches!(r.kind, Kind::Report { .. }))
        .count();
    let mut dates = format!(
        "dates: {confirmed} in window · {checked}/{} checked",
        symbols.len()
    );
    if let Some(reason) = &app.report_dates_paused {
        dates.push_str(&format!(" · checks paused · {retry} · {reason}"));
    } else if symbols
        .iter()
        .any(|s| app.is_loading(&alphai::earnings_key(s)) || app.report_date_due(s, Instant::now()))
    {
        dates.push_str(" · checking…");
        if failed > 0 {
            dates.push_str(&format!(" · {failed} failed"));
        }
    } else if failed > 0 {
        dates.push_str(&format!(" · {failed} failed · {retry}"));
    } else if confirmed == 0 {
        dates.push_str(" · no confirmed dates in window");
    }
    if symbols.len() >= 25 {
        dates.push_str(&format!(" · {} requests per sweep", symbols.len()));
    }
    [macro_line, dates]
}

pub(crate) fn render_calendar_at(f: &mut Frame, area: Rect, app: &mut App, now: DateTime<Utc>) {
    let zone = time_axis::zone_name(app.chart.timezone, true);
    let title = if area.width >= 58 {
        format!(" Calendar · past 7d · next 45d · {zone} ")
    } else {
        format!(" Calendar · {zone} ")
    };
    let mut block = app
        .theme
        .panel_titled(fit(&title, area.width.saturating_sub(2)));
    if area.width >= 38 {
        block = block.title_bottom(Line::from(" report dates: ET · time not provided ").dim());
    }
    if !app.alphai_enabled {
        super::news::render_gate_with(f, area, &block, app, alphai::CALENDAR_KEY, true);
        return;
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let rows = agenda(app, now);
    let selected = app.calendar_selection.reconcile(&rows);
    let status_height = if inner.height >= 5 {
        2
    } else {
        u16::from(inner.height >= 3)
    };
    let table_area = Rect {
        height: inner.height.saturating_sub(status_height),
        ..inner
    };
    let statuses = statuses(app, &rows, now);
    for (i, status) in statuses
        .into_iter()
        .take(status_height as usize)
        .enumerate()
    {
        let status_area = Rect::new(inner.x, table_area.bottom() + i as u16, inner.width, 1);
        let style = if (i == 0 && app.alphai_errors.contains_key(alphai::CALENDAR_KEY))
            || (i == 1 && app.report_dates_paused.is_some())
        {
            Style::new().fg(app.theme.warn)
        } else {
            Style::new().dim()
        };
        f.render_widget(
            Paragraph::new(fit(&status, inner.width)).style(style),
            status_area,
        );
    }
    if rows.is_empty() {
        app.calendar_state.select(None);
        let text = if app.is_loading(alphai::CALENDAR_KEY)
            || (app.calendar.is_none() && !app.alphai_errors.contains_key(alphai::CALENDAR_KEY))
        {
            "Loading macro schedule…"
        } else if app.alphai_errors.contains_key(alphai::CALENDAR_KEY) {
            "Macro schedule unavailable. Report date checks are shown below."
        } else if app.report_dates_paused.is_none()
            && app.calendar_symbols().any(|s| {
                app.is_loading(&alphai::earnings_key(s)) || app.report_date_due(s, Instant::now())
            })
        {
            "No macro events in this window. Checking report dates…"
        } else {
            "No macro events or confirmed report dates in this window."
        };
        f.render_widget(
            Paragraph::new(fit(text, table_area.width)).dim(),
            table_area,
        );
        return;
    }
    let cols = columns(table_area.width.saturating_sub(2));
    let anchor = rows.iter().position(|r| !r.elapsed).unwrap_or(rows.len());
    let mut displayed = Vec::new();
    let mut selected_display = None;
    let mut undated = false;
    for i in 0..=rows.len() {
        if i == anchor {
            displayed.push(separator(&cols, now, app.chart.timezone, false));
        }
        let Some(row) = rows.get(i) else {
            break;
        };
        if row.when == When::Unknown && !undated {
            displayed.push(separator(&cols, now, app.chart.timezone, true));
            undated = true;
        }
        if selected == Some(i) {
            selected_display = Some(displayed.len());
        }
        displayed.push(event_row(row, &cols, app, now));
    }
    let headers: Vec<Cell> = cols
        .iter()
        .map(|&(col, width)| {
            Cell::from(fit(
                match col {
                    Col::Date => "Date",
                    Col::Time => "Time",
                    Col::Title => "Event",
                    Col::Until => "When",
                    Col::Tier => "Level",
                    Col::Notes => "Details",
                },
                width.saturating_sub(1),
            ))
        })
        .collect();
    let table = Table::new(displayed, cols.iter().map(|(_, w)| Constraint::Length(*w)))
        .header(Row::new(headers).dim())
        .column_spacing(0)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .highlight_symbol("▶ ");
    app.calendar_state.select(selected_display);
    f.render_stateful_widget(table, table_area, &mut app.calendar_state);
}

#[cfg(test)]
mod tests;
