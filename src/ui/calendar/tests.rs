use super::*;

fn time(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}
fn day(value: &str) -> NaiveDate {
    value.parse().unwrap()
}
fn event(uid: &str, at: &str) -> CalendarEvent {
    CalendarEvent {
        uid: uid.into(),
        title: uid.into(),
        scheduled_at: at.into(),
        schedule_status: "scheduled".into(),
        schedule_basis: "official".into(),
        importance: "high".into(),
        ..Default::default()
    }
}

#[test]
fn full_wire_fixture_preserves_identity_source_and_optional_fields() {
    let data: alphai::CalendarEvents =
        serde_json::from_str(include_str!("../../../tests/fixtures/calendar.json")).unwrap();
    let e = &data.events[0];
    assert_eq!(e.uid, "US-FOMC-2026-09-DECISION");
    assert_eq!(e.short_name(), "FOMC");
    assert_eq!(e.reference_period, "2026-09");
    assert_eq!(e.release_stage, None);
    assert!(
        e.source_url
            .as_ref()
            .unwrap()
            .starts_with("https://www.federalreserve.gov/")
    );
    assert!(e.has_sep);
    let minimal: CalendarEvent = serde_json::from_str("{}").unwrap();
    assert_eq!(minimal.scheduled(), None);
    assert_eq!(minimal.short_name(), "Macro event");
    let unknown: CalendarEvent = serde_json::from_str(
        r#"{"event_key":"new-series","title":"New series (first release)","source_url":null}"#,
    )
    .unwrap();
    assert_eq!(unknown.short_name(), "New series");
    let undated: CalendarEvent = serde_json::from_str(
        r#"{"scheduled_at":null,"uid":null,"reference_period":null,"schedule_status":null}"#,
    )
    .unwrap();
    assert!(undated.scheduled().is_none());
    assert!(undated.schedule_status.is_empty());
}

#[test]
fn report_day_is_strict_and_never_invents_a_time() {
    for (raw, expected) in [
        (" 2026-09-17 ", Some(day("2026-09-17"))),
        ("", None),
        ("2026-02-30", None),
        ("2026-9-7", None),
        ("2026-09-17T12:00:00Z", None),
    ] {
        let data = alphai::TickerEarnings {
            next_report_date: Some(raw.into()),
            ..Default::default()
        };
        assert_eq!(data.next_report_day(), expected, "{raw}");
    }
}

#[test]
fn elapsed_releases_precede_today_reports_without_moving_the_report_date() {
    let now = time("2026-09-17T14:00:00Z");
    let events = [
        event("past", "2026-09-17T12:30:00Z"),
        event("future", "2026-09-17T18:00:00Z"),
    ];
    let reports = [("NVDA".into(), day("2026-09-17"))];
    let rows = build_agenda(&events, &reports, now);
    assert_eq!(
        rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
        ["past", "NVDA reports", "future"]
    );
    assert!(rows[0].elapsed);
    assert!(!rows[1].elapsed);
    assert_eq!(rows[1].until(now), "today");
    assert_eq!(Selection::default().resolve(&rows), Some(1));
    let late = build_agenda(&events, &reports, time("2026-09-18T03:59:59Z"));
    assert!(
        !late
            .iter()
            .find(|r| r.title == "NVDA reports")
            .unwrap()
            .elapsed
    );
    let next = build_agenda(&events, &reports, time("2026-09-18T04:00:00Z"));
    assert!(
        next.iter()
            .find(|r| r.title == "NVDA reports")
            .unwrap()
            .elapsed
    );
}

#[test]
fn agenda_clips_both_sources_by_et_day_and_deduplicates_uid() {
    let now = time("2026-09-17T14:00:00Z");
    let events = [
        event("too old", "2026-09-10T03:59:59Z"),
        event("lower", "2026-09-10T04:00:00Z"),
        event("last", "2026-11-01T03:59:59Z"),
        event("outside", "2026-11-01T04:00:00Z"),
        event("lower", "2026-09-11T12:00:00Z"),
    ];
    let reports = [
        ("OLD".into(), day("2026-09-09")),
        ("OK".into(), day("2026-09-10")),
        ("FAR".into(), day("2026-11-01")),
    ];
    let rows = build_agenda(&events, &reports, now);
    assert_eq!(
        rows.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(),
        ["OK reports", "lower", "last"]
    );
}

#[test]
fn statuses_override_phase_and_survive_narrow_columns() {
    let now = time("2026-09-17T14:00:00Z");
    let mut cancelled = event("Very long cancelled release name", "2026-09-18T12:30:00Z");
    cancelled.schedule_status = "cancelled".into();
    let mut estimated = event("Claims", "2026-09-18T12:30:00Z");
    estimated.schedule_basis = "inferred".into();
    estimated.phase = "elapsed".into();
    let mut postponed = event("JOLTS", "2026-09-18T12:30:00Z");
    postponed.schedule_status = "postponed".into();
    let mut unknown = event("Unknown", "bad timestamp");
    unknown.schedule_status = "new-status".into();
    let rows = build_agenda(&[cancelled, estimated, postponed, unknown], &[], now);
    for r in &rows {
        match r.title.as_str() {
            "Claims" => {
                assert!(r.upcoming());
                assert!(title_cell(r, 20, false).contains("[est.]"));
            }
            "JOLTS" => {
                assert_eq!(r.until(now), "—");
                assert!(!r.upcoming());
            }
            "Unknown" => assert_eq!(r.when, When::Unknown),
            _ => {
                assert!(title_cell(r, 20, false).contains("[cancelled]"));
                assert_eq!(r.until(now), "—");
            }
        }
    }
}

#[test]
fn manual_cursor_follows_uid_through_insertions_and_clamps_on_removal() {
    let now = time("2026-09-17T14:00:00Z");
    let a = event("a", "2026-09-18T12:30:00Z");
    let b = event("b", "2026-09-19T12:30:00Z");
    let mut selection = Selection::default();
    selection.choose(&build_agenda(&[a.clone(), b.clone()], &[], now), 1);
    let rows = build_agenda(
        &[event("new", "2026-09-17T18:00:00Z"), a.clone(), b],
        &[],
        now,
    );
    assert_eq!(selection.reconcile(&rows), Some(2));
    let reduced = build_agenda(&[a], &[], now);
    assert_eq!(selection.reconcile(&reduced), Some(0));
    assert_eq!(selection.resolve(&[]), None);
}

#[test]
fn countdowns_cross_both_dst_changes_in_utc() {
    assert_eq!(
        fmt_until(time("2026-03-08T06:30:00Z"), time("2026-03-08T07:30:00Z")),
        "in 1h 0m"
    );
    assert_eq!(
        fmt_until(time("2026-11-01T05:30:00Z"), time("2026-11-01T06:30:00Z")),
        "in 1h 0m"
    );
    assert_eq!(
        fmt_until(time("2026-09-17T14:00:59Z"), time("2026-09-17T14:00:00Z")),
        "now"
    );
    assert_eq!(
        fmt_until(time("2026-09-17T14:00:00Z"), time("2026-09-16T11:00:00Z")),
        "1d 3h ago"
    );
    assert_eq!(
        fmt_until(time("2026-09-17T14:00:00Z"), time("2026-09-19T18:00:00Z")),
        "in 2d 4h"
    );
}

#[test]
fn column_ladder_and_unicode_fitting_stay_inside_the_cell_budget() {
    let has = |w, col| columns(w).iter().any(|(c, _)| *c == col);
    assert!(has(106, Col::Notes));
    assert!(!has(56, Col::Notes));
    assert!(has(56, Col::Tier));
    assert!(!has(50, Col::Tier));
    assert!(has(50, Col::Time));
    assert!(!has(44, Col::Time));
    assert!(has(44, Col::Date));
    assert!(!has(36, Col::Date));
    for width in 0..180 {
        assert!(columns(width).iter().map(|(_, w)| *w).sum::<u16>() <= width);
        assert!(Span::raw(fit("日本語 earnings report", width)).width() <= width as usize);
    }
    assert_eq!(fmt_reference_period("2026-Q2"), "Q2 2026");
    assert_eq!(fmt_reference_period("2026-08"), "Aug 2026");
    assert_eq!(fmt_reference_period("2026-09-12"), "12 Sep 2026");
}
