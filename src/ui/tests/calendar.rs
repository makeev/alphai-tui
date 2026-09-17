use super::*;
use crate::app::CalendarSlot;
use crate::ui::calendar::{self as agenda, Kind, OpenTarget};
use chrono::{DateTime, Utc};
use std::time::Duration;

fn now() -> DateTime<Utc> {
    "2026-09-17T13:45:00Z".parse().unwrap()
}
fn macro_event(name: &str, at: &str) -> alphai::CalendarEvent {
    alphai::CalendarEvent {
        uid: name.into(),
        title: name.into(),
        scheduled_at: at.into(),
        schedule_status: "scheduled".into(),
        schedule_basis: "official".into(),
        importance: "high".into(),
        source_url: Some("https://example.com/release".into()),
        ..Default::default()
    }
}
fn window(app: &mut App, events: Vec<alphai::CalendarEvent>) {
    app.apply_alphai(alphai::Event::Calendar {
        events,
        from: "2026-09-10".into(),
        to: "2026-11-02".into(),
    });
}
fn report(app: &mut App, symbol: &str, date: Option<&str>) {
    earnings_fetch(
        app,
        symbol,
        alphai::TickerEarnings {
            next_report_date: date.map(str::to_string),
            ..Default::default()
        },
    );
}
fn fixture() -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (mut app, cmds) = empty_app_with_cmds(vec!["NVDA".into(), "AVGO".into(), "BTC-USD".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Calendar);
    let mut fomc = macro_event("FOMC rate decision", "2026-09-23T18:00:00Z");
    fomc.event_key = "fomc_decision".into();
    fomc.press_conference_at = Some("2026-09-23T18:30:00Z".into());
    fomc.has_sep = true;
    let mut cancelled = macro_event("PPI", "2026-09-20T12:30:00Z");
    cancelled.schedule_status = "cancelled".into();
    let mut claims = macro_event("Initial jobless claims", "2026-09-24T12:30:00Z");
    claims.schedule_basis = "inferred".into();
    claims.importance = "low".into();
    window(
        &mut app,
        vec![
            macro_event("GDP (third estimate)", "2026-09-16T12:30:00Z"),
            macro_event("CPI", "2026-09-18T12:30:00Z"),
            cancelled,
            fomc,
            claims,
        ],
    );
    report(&mut app, "NVDA", Some("2026-09-22"));
    report(&mut app, "AVGO", None);
    (app, cmds)
}
fn screen(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| agenda::render_calendar_at(f, f.area(), app, now()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn drain(cmds: &mut tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) -> Vec<alphai::Cmd> {
    std::iter::from_fn(|| cmds.try_recv().ok()).collect()
}

#[test]
fn global_calendar_fetch_is_shared_across_views_and_uses_covering_utc_bounds() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec!["NVDA".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Table);
    let clock = Instant::now();
    app.ensure_alphai_data_at(now(), clock);
    let commands = drain(&mut cmds);
    assert_eq!(commands.len(), 1);
    assert!(
        matches!(&commands[0], alphai::Cmd::FetchCalendar { from, to } if from == "2026-09-10" && to == "2026-11-02")
    );
    for index in 0..ui::VIEWS.len() {
        app.view_idx = index;
        app.ensure_alphai_data_at(now(), clock);
        assert!(
            !drain(&mut cmds)
                .iter()
                .any(|c| matches!(c, alphai::Cmd::FetchCalendar { .. }))
        );
    }
    window(&mut app, vec![]);
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(60));
    assert!(
        !drain(&mut cmds)
            .iter()
            .any(|c| matches!(c, alphai::Cmd::FetchCalendar { .. }))
    );
    app.view_idx = ui::view_index(ui::ViewId::Table);
    app.ensure_alphai_data_at(now(), clock + app.calendar_ttl() + Duration::from_secs(2));
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchCalendar { .. })
    ));
}

#[test]
fn report_dates_wait_for_window_then_follow_pace_only_in_calendar() {
    let (mut app, mut cmds) = empty_app_with_cmds(vec![
        "NVDA".into(),
        "AVGO".into(),
        "BTC-USD".into(),
        "^GSPC".into(),
        "EURUSD=X".into(),
    ]);
    app.view_idx = ui::view_index(ui::ViewId::Calendar);
    let clock = Instant::now();
    app.ensure_alphai_data_at(now(), clock);
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchCalendar { .. })
    ));
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(10));
    assert!(cmds.try_recv().is_err());
    window(&mut app, vec![]);
    app.ensure_alphai_data_at(now(), clock);
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchEarnings { symbol }) if symbol == "NVDA")
    );
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(8));
    assert!(
        cmds.try_recv().is_err(),
        "earnings in flight must block the next symbol"
    );
    report(&mut app, "NVDA", None);
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(3));
    assert!(cmds.try_recv().is_err());
    app.view_idx = ui::view_index(ui::ViewId::Table);
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(4));
    assert!(cmds.try_recv().is_err());
    app.view_idx = ui::view_index(ui::ViewId::Calendar);
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(4));
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchEarnings { symbol }) if symbol == "AVGO")
    );
    earnings_fetch(
        &mut app,
        "AVGO",
        alphai::TickerEarnings {
            unknown: true,
            ..Default::default()
        },
    );
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(8));
    assert!(cmds.try_recv().is_err());
}

#[test]
fn calendar_errors_keep_rows_and_do_not_retry_or_block_healthy_report_dates() {
    let (mut app, mut cmds) = fixture();
    app.earnings.remove("AVGO");
    app.apply_alphai(alphai::Event::Error {
        key: alphai::CALENDAR_KEY.into(),
        error: "network unavailable".into(),
    });
    let before = app.calendar.as_ref().unwrap().fetched;
    app.ensure_alphai_data_at(now(), Instant::now());
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchEarnings { symbol }) if symbol == "AVGO")
    );
    assert!(cmds.try_recv().is_err());
    let rendered = screen(&mut app, 110, 24);
    assert!(
        rendered.contains("CPI") && rendered.contains("NVDA reports"),
        "{rendered}"
    );
    assert!(
        rendered.contains("update failed") && rendered.contains("cached"),
        "{rendered}"
    );
    assert_eq!(app.calendar.as_ref().unwrap().fetched, before);
    assert!(agenda::flag(&app, "AVGO", now()).is_none());
}

#[test]
fn account_error_stops_sweep_and_refresh_retries_only_failed_dates() {
    let (mut app, mut cmds) = fixture();
    app.apply_alphai(alphai::Event::CalendarBlocked {
        key: alphai::earnings_key("NVDA"),
        error: "rate limit hit".into(),
    });
    app.ensure_alphai_data_at(now(), Instant::now());
    assert!(cmds.try_recv().is_err());
    assert!(screen(&mut app, 110, 24).contains("checks paused"));
    press(&mut app, KeyCode::Char('r'));
    assert!(
        app.calendar.is_some(),
        "refresh preserves the last successful window"
    );
    let clock = Instant::now();
    app.ensure_alphai_data_at(now(), clock);
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchCalendar { .. })
    ));
    press(&mut app, KeyCode::Char('r'));
    app.ensure_alphai_data_at(now(), clock);
    assert!(cmds.try_recv().is_err());
    window(&mut app, vec![]);
    app.ensure_alphai_data_at(now(), clock);
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchEarnings { symbol }) if symbol == "NVDA")
    );
    report(&mut app, "NVDA", Some("2026-09-22"));
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(5));
    assert!(
        cmds.try_recv().is_err(),
        "fresh AVGO must not be fetched by r"
    );
}

#[test]
fn refresh_retries_one_symbol_error_while_unknown_is_terminal() {
    let (mut app, mut cmds) = fixture();
    app.earnings.remove("NVDA");
    app.apply_alphai(alphai::Event::Error {
        key: alphai::earnings_key("NVDA"),
        error: "temporary failure".into(),
    });
    earnings_fetch(
        &mut app,
        "AVGO",
        alphai::TickerEarnings {
            unknown: true,
            ..Default::default()
        },
    );
    app.earnings.get_mut("AVGO").unwrap().fetched = Instant::now() - Duration::from_secs(86400);
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err());
    app.view_idx = ui::view_index(ui::ViewId::Earnings);
    app.selected = 1;
    press(&mut app, KeyCode::Char('r'));
    app.ensure_alphai_data();
    assert!(
        cmds.try_recv().is_err(),
        "unknown earnings should not be retried by TTL or r"
    );
}

#[test]
fn calendar_ttl_scales_and_earnings_can_refresh_the_same_slot_sooner() {
    let (mut app, mut cmds) = fixture();
    for secs in [30, 300, 86400] {
        app.alphai_ttl = Duration::from_secs(secs);
        assert_eq!(app.calendar_ttl(), Duration::from_secs(secs * 72));
    }
    app.alphai_ttl = Duration::from_secs(300);
    app.earnings.get_mut("NVDA").unwrap().fetched = Instant::now() - Duration::from_secs(7200);
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err());
    app.view_idx = ui::view_index(ui::ViewId::Earnings);
    app.ensure_alphai_data();
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchEarnings { symbol }) if symbol == "NVDA")
    );
}

#[test]
fn overlays_and_key_change_pause_fetches_and_old_responses_are_discarded() {
    let (mut app, mut cmds) = fixture();
    app.calendar = None;
    app.prompt.open = true;
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err());
    app.prompt.open = false;
    app.help.open = true;
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err());
    app.help.open = false;
    app.change_alphai_key(Some("test-only-new-key".into()));
    assert!(matches!(cmds.try_recv(), Ok(alphai::Cmd::SetKey(_))));
    window(
        &mut app,
        vec![macro_event("OLD KEY", "2026-09-18T12:30:00Z")],
    );
    report(&mut app, "NVDA", Some("2026-09-22"));
    assert!(app.calendar.is_none() && app.earnings.is_empty());
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err());
    app.apply_alphai(alphai::Event::KeyChanged);
    app.ensure_alphai_data_at(now(), Instant::now());
    assert!(matches!(
        cmds.try_recv(),
        Ok(alphai::Cmd::FetchCalendar { .. })
    ));
}

#[test]
fn no_key_causes_no_requests_and_shows_setup() {
    let (mut app, mut cmds) = fixture();
    app.alphai_enabled = false;
    app.ensure_alphai_data();
    assert!(cmds.try_recv().is_err());
    assert!(screen(&mut app, 110, 24).contains("Get a free API key"));
    assert!(agenda::flag(&app, "NVDA", now()).is_none());
}

#[test]
fn agenda_renders_full_and_narrow_without_hiding_status_or_shifting_date_only() {
    let (mut app, _cmds) = fixture();
    let full = screen(&mut app, 110, 32);
    for expected in [
        "Calendar",
        "CPI",
        "now",
        "14:00",
        "in 6d 4h",
        "NVDA reports",
        "in 5d",
        "press conference",
        "projections",
        "2/2 checked",
        "▶",
    ] {
        assert!(full.contains(expected), "missing {expected}:\n{full}");
    }
    let narrow = screen(&mut app, 60, 20);
    assert!(!narrow.contains("Details"), "{narrow}");
    assert!(narrow.contains("[cancelled]"), "{narrow}");
    app.chart.timezone = crate::config::ChartTimezone::Utc;
    let utc = screen(&mut app, 110, 32);
    let report_line = utc.lines().find(|l| l.contains("NVDA reports")).unwrap();
    assert!(
        report_line.contains("Tue 22 Sep") && report_line.contains('—'),
        "{report_line}"
    );
    assert!(!report_line.contains("00:00") && !report_line.contains("04:00"));
    for (w, h) in [(80, 24), (30, 10), (1, 1), (0, 0)] {
        let _ = screen(&mut app, w, h);
    }
    app.bare = true;
    let _ = render_sized(&mut app, 60, 20);
}

#[test]
fn open_routes_to_source_or_earnings_and_cursor_stays_on_the_same_event() {
    let (mut app, _cmds) = fixture();
    let rows = agenda::agenda(&app, now());
    assert!(matches!(
        agenda::open_target(&app, now()),
        Some(OpenTarget::Url(_))
    ));
    let index = rows
        .iter()
        .position(|r| matches!(r.kind, Kind::Report { .. }))
        .unwrap();
    app.calendar_selection.choose(&rows, index);
    assert_eq!(
        agenda::open_target(&app, now()),
        Some(OpenTarget::Earnings("NVDA".into()))
    );
    app.calendar
        .as_mut()
        .unwrap()
        .events
        .push(macro_event("Inserted", "2026-09-18T15:00:00Z"));
    screen(&mut app, 110, 32);
    assert_eq!(
        agenda::open_target(&app, now()),
        Some(OpenTarget::Earnings("NVDA".into()))
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.view_id(), ui::ViewId::Earnings);
    assert_eq!(app.selected_symbol(), "NVDA");
    press(&mut app, KeyCode::Char('9'));
    assert_eq!(app.calendar_state.offset(), 0);
    assert!(matches!(
        agenda::open_target(&app, now()),
        Some(OpenTarget::Url(_))
    ));
}

#[test]
fn rail_prefers_fresh_ticker_report_then_nearest_high_even_without_quotes() {
    let (mut app, _cmds) = fixture();
    assert_eq!(
        agenda::flag(&app, "NVDA", now()).unwrap().compact,
        "⚑ reports 5d"
    );
    let flag = agenda::flag(&app, "AVGO", now()).unwrap();
    assert_eq!(flag.full, "⚑ CPI in 22h 45m");
    assert!(flag.urgent);
    let line = ui::rail::line(&app, 110, now()).to_string();
    assert!(line.contains("NVDA reports"), "{line}");
    app.earnings.get_mut("NVDA").unwrap().fetched = Instant::now() - app.calendar_ttl();
    assert_eq!(
        agenda::flag(&app, "NVDA", now()).unwrap().compact,
        "⚑ CPI 22h"
    );
    let cpi = app
        .calendar
        .as_mut()
        .unwrap()
        .events
        .iter_mut()
        .find(|e| e.title == "CPI")
        .unwrap();
    cpi.schedule_basis = "inferred".into();
    assert!(
        agenda::flag(&app, "AVGO", now())
            .unwrap()
            .compact
            .contains("est.")
    );
    app.calendar.as_mut().unwrap().fetched = Instant::now() - app.calendar_ttl();
    assert!(agenda::flag(&app, "NVDA", now()).is_none());
}

#[test]
fn successful_null_retires_an_old_date_and_watchlist_controls_rows() {
    let (mut app, _cmds) = fixture();
    report(&mut app, "NVDA", None);
    assert!(
        !agenda::agenda(&app, now())
            .iter()
            .any(|r| matches!(r.kind, Kind::Report { .. }))
    );
    report(&mut app, "AVGO", Some("2026-09-22"));
    app.symbols.retain(|s| s != "AVGO");
    assert!(
        !agenda::agenda(&app, now())
            .iter()
            .any(|r| matches!(r.kind, Kind::Report { .. }))
    );
}

#[test]
fn macro_and_date_empty_success_is_not_rendered_as_failure() {
    let (mut app, _cmds) = fixture();
    app.calendar = Some(CalendarSlot {
        events: vec![],
        fetched: Instant::now(),
        from: "2026-09-10".parse().unwrap(),
        to: "2026-11-02".parse().unwrap(),
    });
    report(&mut app, "NVDA", None);
    let text = screen(&mut app, 110, 24);
    assert!(
        text.contains("No macro events or confirmed report dates"),
        "{text}"
    );
    assert!(!text.contains("failed"), "{text}");
}

#[test]
fn paging_skips_the_now_separator_and_reentry_resets_table_offset() {
    let (mut app, _cmds) = fixture();
    app.earnings.clear();
    let clock = Utc::now();
    let events = (-5..30)
        .map(|day| {
            macro_event(
                &format!("Event {day}"),
                &(clock + chrono::Duration::days(day)).to_rfc3339(),
            )
        })
        .collect();
    window(&mut app, events);
    let rows = agenda::agenda(&app, clock);
    let initial = app.calendar_selection.resolve(&rows).unwrap();
    assert_eq!(initial, 5);
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.calendar_selection.resolve(&rows), Some(initial + 10));
    let _ = render_sized(&mut app, 80, 12);
    assert!(app.calendar_state.offset() > 0);
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.calendar_selection.resolve(&rows), Some(initial));
    press(&mut app, KeyCode::Up);
    assert_eq!(app.calendar_selection.resolve(&rows), Some(initial - 1));
    press(&mut app, KeyCode::Down);
    assert_eq!(app.calendar_selection.resolve(&rows), Some(initial));
    press(&mut app, KeyCode::PageDown);
    let _ = render_sized(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('1'));
    press(&mut app, KeyCode::Char('9'));
    assert_eq!(app.calendar_state.offset(), 0);
    assert_eq!(app.calendar_selection.resolve(&rows), Some(initial));
}
