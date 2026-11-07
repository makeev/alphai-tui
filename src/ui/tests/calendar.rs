use super::*;
use crate::alphai::{ReportEvent, ReportRequest};
use crate::app::report_calendar::ReportCalendar;
use crate::ui::calendar::{self as agenda, Kind, OpenTarget};
use chrono::{DateTime, Utc};
use std::time::Duration;

fn now() -> DateTime<Utc> {
    "2026-11-07T15:45:00Z".parse().unwrap()
}
fn event(
    symbol: &str,
    day: &str,
    status: &str,
    session: Option<&str>,
    uid: Option<&str>,
) -> ReportEvent {
    serde_json::from_value(serde_json::json!({
        "ticker": symbol, "listings": [symbol], "company": "EXAMPLE TECHNOLOGIES INC", "sector": "Technology",
        "report_date": day, "session": session, "status": status,
        "reported_at": (status == "reported").then_some("2026-11-07T11:50:00Z"),
        "report_read": uid.map(|uid| serde_json::json!({"uid": uid, "ticker": symbol, "fiscal_period": "Q3 2026", "published_at": "2026-11-07T11:50:00Z", "verdict": "strong"})),
        "previous_read": {"fiscal_period":"Half year 2026", "published_at":"2026-08-01T12:00:00Z", "verdict":"solid", "guidance":{"period":"FY 2026", "revenue":"$1 billion"}, "what_to_watch":["Capacity growth", "Margins", "Customer concentration"]}
    })).unwrap()
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
fn setup(symbols: Vec<String>) -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (mut app, cmds) = empty_app_with_cmds(symbols);
    app.frozen_now = Some(now());
    app.view_idx = ui::view_index(ui::ViewId::Calendar);
    app.calendar = Some(crate::app::CalendarSlot {
        events: vec![],
        fetched: Instant::now(),
        from: "2026-10-31".parse().unwrap(),
        to: "2026-12-23".parse().unwrap(),
    });
    app.report_calendar
        .sync(app.polled_symbols(), crate::market::et_time(now()).date());
    (app, cmds)
}
fn load(app: &mut App, items: Vec<ReportEvent>) {
    app.report_calendar
        .sync(app.polled_symbols(), crate::market::et_time(now()).date());
    for slot in &mut app.report_calendar.slots {
        slot.items = items
            .iter()
            .filter(|e| {
                e.report_date >= slot.request.from
                    && e.report_date < slot.request.to
                    && slot.request.tickers.iter().any(|s| e.contains(s))
            })
            .cloned()
            .collect();
        slot.covered = true;
        slot.fetched = Some(Instant::now());
        slot.error = None;
        app.report_calendar
            .ever_covered
            .extend(slot.request.tickers.clone());
        app.report_calendar
            .ever_covered
            .extend(slot.items.iter().flat_map(|e| e.listings.clone()));
    }
}
fn drain(cmds: &mut tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) -> Vec<alphai::Cmd> {
    std::iter::from_fn(|| cmds.try_recv().ok()).collect()
}
fn next_request(
    app: &mut App,
    cmds: &mut tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>,
    clock: Instant,
) -> ReportRequest {
    app.ensure_alphai_data_at(now(), clock);
    let commands = drain(cmds);
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, alphai::Cmd::FetchEarnings { .. }))
    );
    commands
        .into_iter()
        .find_map(|c| {
            if let alphai::Cmd::FetchReportCalendar(r) = c {
                Some(r)
            } else {
                None
            }
        })
        .expect("one corporate request")
}
fn reply(app: &mut App, request: ReportRequest, items: Vec<ReportEvent>) {
    app.apply_alphai(alphai::Event::ReportCalendar {
        request,
        result: Ok(items),
        blocked: false,
    });
}
fn pick(app: &mut App, symbol: &str) {
    let rows = agenda::agenda(app, now());
    let i = rows
        .iter()
        .position(|r| matches!(&r.kind, Kind::Report {symbol: s} if s == symbol))
        .unwrap();
    app.calendar_selection.choose(&rows, i);
}
fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::from(code));
}
fn fixture() -> (App, tokio::sync::mpsc::UnboundedReceiver<alphai::Cmd>) {
    let (mut app, cmds) = setup(vec!["NVDA".into(), "AVGO".into(), "HUM".into()]);
    load(
        &mut app,
        vec![
            event("NVDA", "2026-11-10", "scheduled", Some("after_close"), None),
            event("AVGO", "2026-11-07", "reported", None, None),
            event("HUM", "2026-11-07", "reported", None, Some("hum-q3")),
        ],
    );
    app.calendar.as_mut().unwrap().events = vec![macro_event("CPI", "2026-11-10T13:30:00Z")];
    (app, cmds)
}

#[test]
fn calendar_uses_two_exact_et_windows_sorted_groups_and_the_union_with_holdings() {
    let mut symbols: Vec<_> = (0..51).map(|i| format!("T{i:02}")).collect();
    symbols.extend([
        "goog ".into(),
        "GOOG".into(),
        "BTC-USD".into(),
        "^GSPC".into(),
        "EURUSD=X".into(),
        "BAD;".into(),
        "ÉÉ".into(),
    ]);
    let (mut app, mut cmds) = setup(symbols);
    app.positions.push(crate::portfolio::Position {
        symbol: "VOD.L".into(),
        qty: 1.,
        avg_price: 1.,
    });
    let clock = Instant::now();
    let near = next_request(&mut app, &mut cmds, clock);
    assert_eq!(near.tickers.len(), 50);
    assert_eq!(near.tickers[0], "GOOG");
    assert_eq!(
        (near.from.to_string(), near.to.to_string()),
        ("2026-10-31".into(), "2026-11-14".into())
    );
    reply(&mut app, near, vec![]);
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(3));
    assert!(drain(&mut cmds).is_empty());
    let second = next_request(&mut app, &mut cmds, clock + Duration::from_secs(4));
    assert_eq!(second.tickers.len(), 3);
    assert!(second.tickers.contains(&"VOD.L".into()));
    reply(&mut app, second, vec![]);
    let far = next_request(&mut app, &mut cmds, clock + Duration::from_secs(8));
    assert_eq!(
        (far.from.to_string(), far.to.to_string()),
        ("2026-11-14".into(), "2026-12-22".into())
    );
    assert_eq!((far.to - far.from).num_days(), 38);
}

#[test]
fn near_is_shared_in_every_view_far_waits_for_calendar_and_floors_hold() {
    assert_eq!(
        ReportCalendar::interval(Duration::from_secs(30), true),
        Duration::from_secs(3600)
    );
    assert_eq!(
        ReportCalendar::interval(Duration::from_secs(30), false),
        Duration::from_secs(21600)
    );
    let (mut app, mut cmds) = setup(vec!["NVDA".into()]);
    app.view_idx = ui::view_index(ui::ViewId::Table);
    app.calendar = None;
    let clock = Instant::now();
    app.ensure_alphai_data_at(now(), clock);
    let commands = drain(&mut cmds);
    assert_eq!(commands.len(), 2);
    let request = commands
        .into_iter()
        .find_map(|c| {
            if let alphai::Cmd::FetchReportCalendar(r) = c {
                Some(r)
            } else {
                None
            }
        })
        .unwrap();
    reply(&mut app, request, vec![]);
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(3601));
    assert!(drain(&mut cmds).is_empty());
    app.view_idx = ui::view_index(ui::ViewId::Calendar);
    let r = next_request(&mut app, &mut cmds, clock + Duration::from_secs(4));
    assert_eq!(r.from.to_string(), "2026-11-14");
}

#[test]
fn failures_preserve_rows_block_only_the_group_and_never_fetch_full_histories() {
    let (mut app, mut cmds) = fixture();
    key(&mut app, KeyCode::Char('r'));
    let clock = Instant::now();
    let r = next_request(&mut app, &mut cmds, clock);
    app.apply_alphai(alphai::Event::ReportCalendar {
        request: r,
        result: Err("API 400 invalid group".into()),
        blocked: false,
    });
    assert_eq!(agenda::agenda(&app, now()).len(), 4);
    let far = next_request(&mut app, &mut cmds, clock + Duration::from_secs(4));
    assert_eq!(far.from.to_string(), "2026-11-14");
    reply(&mut app, far, vec![]);
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(8));
    assert!(drain(&mut cmds).is_empty());
    assert!(render_sized(&mut app, 110, 32).contains("failed"));
    assert!(
        !agenda::flag(&app, "NVDA", now())
            .unwrap()
            .full
            .contains("reports")
    );
}

#[test]
fn rate_error_pauses_all_groups_until_manual_refresh_without_duplicate_sweeps() {
    let (mut app, mut cmds) = setup(vec!["NVDA".into()]);
    let clock = Instant::now();
    let r = next_request(&mut app, &mut cmds, clock);
    app.apply_alphai(alphai::Event::ReportCalendar {
        request: r,
        result: Err("API 429 limit".into()),
        blocked: true,
    });
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(10));
    assert!(drain(&mut cmds).is_empty());
    key(&mut app, KeyCode::Char('r'));
    key(&mut app, KeyCode::Char('r'));
    let r = next_request(&mut app, &mut cmds, clock + Duration::from_secs(12));
    assert!(app.report_calendar.paused.is_none());
    key(&mut app, KeyCode::Char('r'));
    reply(&mut app, r, vec![]);
    let far = next_request(&mut app, &mut cmds, clock + Duration::from_secs(16));
    reply(&mut app, far, vec![]);
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(20));
    assert!(drain(&mut cmds).is_empty());
}

#[test]
fn changed_universe_or_et_day_discards_inflight_results() {
    let (mut app, mut cmds) = setup(vec!["NVDA".into()]);
    let clock = Instant::now();
    let old = next_request(&mut app, &mut cmds, clock);
    app.symbols = vec!["GOOG".into()];
    app.ensure_alphai_data_at(now(), clock + Duration::from_secs(1));
    reply(
        &mut app,
        old,
        vec![event("NVDA", "2026-11-10", "scheduled", None, None)],
    );
    assert!(agenda::agenda(&app, now()).is_empty());
    let current = next_request(&mut app, &mut cmds, clock + Duration::from_secs(4));
    assert_eq!(current.tickers, ["GOOG"]);
    app.ensure_alphai_data_at(
        now() + chrono::Duration::days(1),
        clock + Duration::from_secs(5),
    );
    reply(
        &mut app,
        current,
        vec![event("GOOG", "2026-11-10", "scheduled", None, None)],
    );
    assert!(app.report_calendar.slots.iter().all(|s| !s.covered));
}

#[test]
fn key_barrier_and_overlays_pause_all_requests() {
    let (mut app, mut cmds) = fixture();
    pick(&mut app, "NVDA");
    key(&mut app, KeyCode::Char('v'));
    app.calendar = None;
    app.report_calendar = Default::default();
    app.ensure_alphai_data();
    assert!(drain(&mut cmds).is_empty());
    key(&mut app, KeyCode::Esc);
    app.change_alphai_key(Some("test-only-new-key".into()));
    assert!(matches!(cmds.try_recv(), Ok(alphai::Cmd::SetKey(_))));
    earnings_fetch(&mut app, "NVDA", Default::default());
    app.ensure_alphai_data();
    assert!(drain(&mut cmds).is_empty());
    assert!(app.earnings.is_empty());
    app.apply_alphai(alphai::Event::KeyChanged);
    app.ensure_alphai_data();
    assert_eq!(drain(&mut cmds).len(), 2);
}

#[test]
fn empty_universe_or_no_key_does_not_request_the_market_wide_corporate_calendar() {
    let (mut app, mut cmds) = setup(vec!["BTC-USD".into(), "^GSPC".into()]);
    app.ensure_alphai_data();
    assert!(drain(&mut cmds).is_empty());
    app.alphai_enabled = false;
    assert!(render_sized(&mut app, 110, 24).contains("Get a free API key"));
}

#[test]
fn successful_empty_retires_legacy_date_and_disappearing_past_schedule() {
    let (mut app, mut cmds) = setup(vec!["NVDA".into()]);
    earnings_fetch(
        &mut app,
        "NVDA",
        alphai::TickerEarnings {
            next_report_date: Some("2026-11-10".into()),
            ..Default::default()
        },
    );
    assert!(agenda::flag(&app, "NVDA", now()).is_some());
    let r = next_request(&mut app, &mut cmds, Instant::now());
    reply(&mut app, r, vec![]);
    assert!(agenda::flag(&app, "NVDA", now()).is_none());
    assert!(agenda::next_report_day(&app, "NVDA", now()).is_none());
    load(
        &mut app,
        vec![event("NVDA", "2026-11-07", "scheduled", None, None)],
    );
    assert!(agenda::agenda(&app, now() + chrono::Duration::days(1)).is_empty());
}

#[test]
fn alias_rows_across_batch_boundary_merge_but_different_filings_stay() {
    let mut symbols: Vec<_> = (0..50).map(|i| format!("A{i:02}")).collect();
    symbols.push("Z".into());
    let (mut app, _) = setup(symbols);
    let mut first = event("A00", "2026-11-07", "reported", None, Some("read-one"));
    first.listings.push("Z".into());
    let mut second = first.clone();
    second.ticker = "Z".into();
    load(&mut app, vec![first.clone(), second]);
    assert_eq!(agenda::agenda(&app, now()).len(), 1);
    first.report_date = "2026-11-06".parse().unwrap();
    first.report_read.as_mut().unwrap().uid = "read-two".into();
    app.report_calendar.slots[0].items.push(first);
    assert_eq!(agenda::agenda(&app, now()).len(), 2);
    assert!(
        agenda::flag(&app, "Z", now())
            .unwrap()
            .full
            .contains("reported")
    );
}

#[test]
fn issuer_listings_do_not_add_unwatched_securities() {
    for (common, sibling) in [("VNO", "VNO-PL"), ("VAL", "VAL-WT"), ("NMFC", "NMFCZ")] {
        // A portfolio security can share the issuer's event with a watched common.
        let (mut app, mut cmds) = setup(vec![common.into()]);
        app.positions.push(crate::portfolio::Position {
            symbol: sibling.into(),
            qty: 1.,
            avg_price: 100.,
        });
        let mut e = event(common, "2026-11-10", "scheduled", None, None);
        e.listings = vec![common.into(), sibling.into(), "UNWATCHED".into()];
        load(&mut app, vec![e.clone(), e]);

        let rows = agenda::agenda(&app, now());
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0].kind, Kind::Report { symbol } if symbol == common));
        assert!(agenda::flag(&app, common, now()).is_some());
        assert!(agenda::flag(&app, sibling, now()).is_some());
        assert_eq!(app.symbols, vec![common]);
        assert_eq!(app.positions.len(), 1);
        assert_eq!(app.positions[0].symbol, sibling);

        app.report_calendar.refresh(None);
        let request = next_request(&mut app, &mut cmds, Instant::now());
        assert_eq!(request.tickers, vec![common, sibling]);

        // If only the portfolio security remains, display that security.
        app.symbols.clear();
        let rows = agenda::agenda(&app, now());
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0].kind, Kind::Report { symbol } if symbol == sibling));
    }
}

#[test]
fn portfolio_only_target_works_with_empty_watchlist_without_mutating_config() {
    let (mut app, mut cmds) = setup(vec![]);
    app.positions.push(crate::portfolio::Position {
        symbol: "NVDA".into(),
        qty: 1.,
        avg_price: 100.,
    });
    load(
        &mut app,
        vec![event(
            "NVDA",
            "2026-11-10",
            "scheduled",
            Some("after_close"),
            None,
        )],
    );
    app.view_idx = ui::view_index(ui::ViewId::Portfolio);
    assert_eq!(app.focused_symbol(), "NVDA");
    assert!(
        ui::rail::line(&app, 110, now())
            .to_string()
            .contains("NVDA")
    );
    key(&mut app, KeyCode::Char('9'));
    pick(&mut app, "NVDA");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.earnings_symbol(), "NVDA");
    assert!(app.symbols.is_empty());
    app.ensure_alphai_data();
    assert!(drain(&mut cmds).iter().any(|c| matches!(c, alphai::Cmd::FetchEarnings {symbol, cache_symbol} if symbol == "NVDA" && cache_symbol == "NVDA")));
    key(&mut app, KeyCode::Char('p'));
    assert_eq!(app.prompt.target, "NVDA");
}

#[test]
fn uid_open_fetches_owner_even_when_fresh_then_one_canonical_fallback() {
    let (mut app, mut cmds) = setup(vec!["GOOGL".into()]);
    let mut e = event("GOOGL", "2026-11-07", "reported", None, Some("wanted"));
    e.listings = vec!["GOOG".into(), "GOOGL".into()];
    e.report_read.as_mut().unwrap().ticker = "GOOG".into();
    load(&mut app, vec![e]);
    earnings_fetch(&mut app, "GOOGL", Default::default());
    pick(&mut app, "GOOGL");
    key(&mut app, KeyCode::Enter);
    app.ensure_alphai_data();
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchEarnings {symbol, cache_symbol}) if symbol == "GOOGL" && cache_symbol == "GOOGL")
    );
    earnings_fetch(&mut app, "GOOGL", Default::default());
    app.ensure_alphai_data();
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchEarnings {symbol, cache_symbol}) if symbol == "GOOG" && cache_symbol == "GOOGL")
    );
    earnings_fetch(&mut app, "GOOGL", Default::default());
    for _ in 0..10 {
        app.ensure_alphai_data();
    }
    assert!(drain(&mut cmds).is_empty());
    assert!(ui::earnings::open_url(&app).is_none());
    assert!(render_sized(&mut app, 110, 32).contains("Selected calendar read unavailable"));
}

#[test]
fn uid_open_uses_cached_older_read_without_reordering_history_or_wrong_browser_link() {
    let (mut app, mut cmds) = setup(vec!["NVDA".into()]);
    let mut data = earnings_data("NVDA", "", FULL_METRICS);
    let mut older = data.reports[0].clone();
    older.uid = "older".into();
    older.fiscal_period = "Q2 2026".into();
    let newest = data.reports[0].uid.clone();
    data.reports.push(older);
    earnings_fetch(&mut app, "NVDA", data);
    load(
        &mut app,
        vec![event("NVDA", "2026-11-07", "reported", None, Some("older"))],
    );
    pick(&mut app, "NVDA");
    key(&mut app, KeyCode::Enter);
    app.ensure_alphai_data();
    assert!(drain(&mut cmds).is_empty());
    assert_eq!(app.shown_earnings().unwrap().uid, "older");
    assert_eq!(app.earnings["NVDA"].data.latest().unwrap().uid, newest);
    assert!(ui::earnings::open_url(&app).unwrap().contains("older"));
    key(&mut app, KeyCode::Char('6'));
    assert!(app.earnings_target.is_none());
    assert_eq!(app.shown_earnings().unwrap().uid, newest);
}

#[test]
fn pending_read_announces_availability_without_replacing_the_document_until_r() {
    let (mut app, mut cmds) = setup(vec!["NVDA".into()]);
    let data = earnings_data("NVDA", "", FULL_METRICS);
    let uid = data.reports[0].uid.clone();
    earnings_fetch(&mut app, "NVDA", data.clone());
    load(
        &mut app,
        vec![event("NVDA", "2026-11-07", "reported", None, None)],
    );
    pick(&mut app, "NVDA");
    key(&mut app, KeyCode::Enter);
    assert!(render_sized(&mut app, 110, 32).contains("read pending"));
    load(
        &mut app,
        vec![event(
            "NVDA",
            "2026-11-07",
            "reported",
            None,
            Some("new-read"),
        )],
    );
    let mut newer = data.clone();
    newer.reports[0].uid = "new-read".into();
    earnings_fetch(&mut app, "NVDA", newer);
    assert_eq!(app.shown_earnings().unwrap().uid, uid);
    assert!(render_sized(&mut app, 110, 32).contains("Read available · r to open"));
    key(&mut app, KeyCode::Char('r'));
    app.ensure_alphai_data();
    let request = drain(&mut cmds)
        .into_iter()
        .find_map(|c| {
            if let alphai::Cmd::FetchReportCalendar(r) = c {
                Some(r)
            } else {
                None
            }
        })
        .unwrap();
    reply(
        &mut app,
        request,
        vec![event(
            "NVDA",
            "2026-11-07",
            "reported",
            None,
            Some("new-read"),
        )],
    );
    assert_eq!(app.shown_earnings().unwrap().uid, "new-read");
}

#[test]
fn filter_card_snapshot_and_open_use_the_same_rows_without_requests() {
    let (mut app, mut cmds) = fixture();
    pick(&mut app, "NVDA");
    key(&mut app, KeyCode::Char('f'));
    assert!(
        agenda::agenda(&app, now())
            .iter()
            .all(|r| matches!(r.kind, Kind::Report { .. }))
    );
    key(&mut app, KeyCode::Char('v'));
    let frozen = app.calendar_overlay.as_ref().unwrap().row.title.clone();
    load(&mut app, vec![]);
    app.ensure_alphai_data();
    assert!(drain(&mut cmds).is_empty());
    assert_eq!(app.calendar_overlay.as_ref().unwrap().row.title, frozen);
    let text = render_sized(&mut app, 110, 32);
    assert!(text.contains("Customer concentration"));
    assert!(text.contains("Half year 2026"));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.earnings_symbol(), "NVDA");
    assert!(app.calendar_overlay.is_none());
}

#[test]
fn rendering_covers_report_states_et_times_sizes_themes_and_local_filter_summaries() {
    let (mut app, mut cmds) = fixture();
    pick(&mut app, "NVDA");
    let full = render_sized(&mut app, 110, 32);
    for word in [
        "AMC",
        "◐",
        "●",
        "○",
        "06:50",
        "reported",
        "read pending",
        "strong quarter",
        "solid half-year",
        "near",
        "far",
        "3 tickers",
        "BMO before open",
    ] {
        assert!(full.contains(word), "missing {word}:\n{full}");
    }
    app.chart.timezone = crate::config::ChartTimezone::Utc;
    let utc = render_sized(&mut app, 110, 32);
    assert!(utc.contains("13:30"));
    assert!(utc.contains("06:50"));
    for (w, h) in [(80, 24), (60, 20), (40, 16), (1, 1), (0, 0)] {
        let _ = render_sized(&mut app, w, h);
    }
    app.bare = true;
    let _ = render_sized(&mut app, 60, 20);
    assert!(drain(&mut cmds).is_empty());
}

#[test]
fn summary_loading_and_errors_are_not_zero_and_reported_uses_actual_filing_day() {
    let (mut app, _) = setup(vec!["NVDA".into()]);
    assert!(render_sized(&mut app, 110, 32).contains("reports loading"));
    let mut e = event("NVDA", "2026-11-07", "reported", None, None);
    e.reported_at = Some("2026-11-07T02:00:00Z".parse().unwrap());
    load(&mut app, vec![e]);
    assert!(render_sized(&mut app, 110, 32).contains("0 reported today"));
    app.report_calendar.slots[0].error = Some("offline".into());
    assert!(render_sized(&mut app, 110, 32).contains("known"));
}

#[test]
fn intraday_order_and_exclusive_seven_day_boundary_keep_unknown_status_out() {
    let (mut app, _) = setup(vec![
        "A".into(),
        "B".into(),
        "C".into(),
        "D".into(),
        "E".into(),
    ]);
    load(
        &mut app,
        vec![
            event("A", "2026-11-10", "scheduled", Some("after_close"), None),
            event("B", "2026-11-10", "scheduled", None, None),
            event("C", "2026-11-10", "scheduled", Some("before_open"), None),
            event("D", "2026-11-14", "scheduled", None, None),
            event("E", "2026-11-07", "unexpected", None, None),
        ],
    );
    app.calendar.as_mut().unwrap().events = vec![macro_event("CPI", "2026-11-10T13:30:00Z")];
    let rows = agenda::agenda(&app, now());
    let ordered: Vec<_> = rows
        .iter()
        .filter(|r| r.title.contains("CPI") || r.title.contains("○"))
        .map(|r| match &r.kind {
            Kind::Report { symbol } => symbol.as_str(),
            _ => "CPI",
        })
        .collect();
    assert_eq!(ordered, ["C", "B", "CPI", "A", "D"]);
    assert!(agenda::flag(&app, "D", now()).unwrap().full.contains("CPI"));
    assert!(render_sized(&mut app, 110, 32).contains("3 reports"));
}

#[test]
fn cursor_survives_read_arrival_date_change_and_filter_mouse_indices() {
    use crate::ui::hits::ListKind;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let (mut app, _) = fixture();
    pick(&mut app, "NVDA");
    let mut arrived = event("NVDA", "2026-11-09", "reported", None, Some("landed"));
    arrived.reported_at = Some("2026-11-09T12:00:00Z".parse().unwrap());
    load(&mut app, vec![arrived]);
    assert_eq!(
        agenda::open_target(&app, now()),
        Some(OpenTarget::Earnings("NVDA".into()))
    );
    key(&mut app, KeyCode::Char('f'));
    render_sized(&mut app, 110, 32);
    let hit = app
        .hits
        .lists
        .iter()
        .find(|l| l.kind == ListKind::Calendar)
        .unwrap()
        .clone();
    let y = app
        .hits
        .calendar_rows
        .iter()
        .position(Option::is_some)
        .unwrap() as u16;
    assert!(app.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.area.x + 2,
        row: hit.area.y + y,
        modifiers: KeyModifiers::NONE
    }));
    assert_eq!(app.focused_symbol(), "NVDA");
}

#[test]
fn canonical_fallback_stores_under_owner_and_errors_never_trigger_it() {
    let (mut app, mut cmds) = setup(vec!["GOOGL".into()]);
    let mut e = event("GOOGL", "2026-11-07", "reported", None, Some("wanted"));
    e.report_read.as_mut().unwrap().ticker = "GOOG".into();
    load(&mut app, vec![e]);
    pick(&mut app, "GOOGL");
    key(&mut app, KeyCode::Enter);
    app.ensure_alphai_data();
    drain(&mut cmds);
    app.apply_alphai(alphai::Event::Error {
        key: alphai::earnings_key("GOOGL"),
        error: "offline".into(),
    });
    app.ensure_alphai_data();
    assert!(drain(&mut cmds).is_empty());
    key(&mut app, KeyCode::Char('r'));
    app.ensure_alphai_data();
    drain(&mut cmds);
    earnings_fetch(&mut app, "GOOGL", Default::default());
    app.ensure_alphai_data();
    assert!(
        matches!(cmds.try_recv(), Ok(alphai::Cmd::FetchEarnings {symbol,cache_symbol}) if symbol=="GOOG" && cache_symbol=="GOOGL")
    );
    let mut data = earnings_data("GOOG", "", FULL_METRICS);
    data.reports[0].uid = "wanted".into();
    earnings_fetch(&mut app, "GOOGL", data);
    assert!(!app.earnings.contains_key("GOOG"));
    assert_eq!(app.shown_earnings().unwrap().ticker, "GOOG");
    assert_eq!(app.earnings_symbol(), "GOOGL");
    app.ensure_alphai_data();
    assert!(drain(&mut cmds).is_empty());
}

#[test]
fn schedule_and_pending_rows_do_not_survive_midnight_without_a_read() {
    let (mut app, _) = setup(vec!["NVDA".into()]);
    load(
        &mut app,
        vec![event("NVDA", "2026-11-07", "reported", None, None)],
    );
    assert!(agenda::agenda(&app, now() + chrono::Duration::days(1)).is_empty());
}

#[test]
fn removing_a_watchlist_ticker_keeps_its_calendar_event_while_held() {
    let (mut app, _) = setup(vec!["NVDA".into()]);
    app.positions.push(crate::portfolio::Position {
        symbol: "NVDA".into(),
        qty: 1.,
        avg_price: 1.,
    });
    load(
        &mut app,
        vec![event("NVDA", "2026-11-10", "scheduled", None, None)],
    );
    app.symbols.clear();
    assert_eq!(agenda::agenda(&app, now()).len(), 1);
    app.positions.clear();
    assert!(agenda::agenda(&app, now()).is_empty());
}

#[test]
fn a_paused_manual_sweep_can_resume_even_with_far_groups_still_queued() {
    let (mut app, mut cmds) = fixture();
    key(&mut app, KeyCode::Char('r'));
    let clock = Instant::now();
    let r = next_request(&mut app, &mut cmds, clock);
    app.apply_alphai(alphai::Event::Calendar {
        events: vec![],
        from: "2026-10-31".into(),
        to: "2026-12-23".into(),
    });
    app.apply_alphai(alphai::Event::ReportCalendar {
        request: r,
        result: Err("API 429".into()),
        blocked: true,
    });
    key(&mut app, KeyCode::Char('r'));
    assert!(app.report_calendar.paused.is_none());
    let retry = next_request(&mut app, &mut cmds, clock + Duration::from_secs(4));
    assert_eq!(retry.from.to_string(), "2026-10-31");
}

#[test]
fn calendar_overlay_uses_each_named_theme_after_clear() {
    for name in ["dracula", "catppuccin-latte", "gruvbox-light"] {
        let (mut app, _) = fixture();
        app.set_theme(name);
        pick(&mut app, "NVDA");
        key(&mut app, KeyCode::Char('v'));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| ui::draw(f, &mut app)).unwrap();
        let (area, _) = app.hits.modal.unwrap();
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &terminal.backend().buffer()[(x, y)];
                assert!(
                    matches!(cell.bg, ratatui::style::Color::Rgb(..)),
                    "{name} {x} {y}"
                );
                assert!(
                    matches!(cell.fg, ratatui::style::Color::Rgb(..)),
                    "{name} {x} {y}"
                );
            }
        }
    }
}

#[test]
fn refreshing_groups_qualify_counts_even_when_the_previous_data_is_fresh() {
    let (mut app, mut cmds) = fixture();
    key(&mut app, KeyCode::Char('r'));
    let request = next_request(&mut app, &mut cmds, Instant::now());
    let text = render_sized(&mut app, 160, 32);
    assert!(text.contains("known report"), "{text}");
    assert!(text.contains("near updating"), "{text}");
    reply(
        &mut app,
        request,
        vec![event("NVDA", "2026-11-10", "scheduled", None, None)],
    );
    let text = render_sized(&mut app, 160, 32);
    assert!(text.contains("far updating"), "{text}");
    assert!(
        !text.contains("known report"),
        "the near summaries are complete: {text}"
    );
}
