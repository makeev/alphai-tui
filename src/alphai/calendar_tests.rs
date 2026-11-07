//! Calendar transport checks use loopback and a dummy key, never a real quota.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn server(status: u16, body: &'static str) -> (Client, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut chunk = [0; 2048];
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            request.extend_from_slice(&chunk[..n]);
            if request.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let reply = format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(reply.as_bytes()).await.unwrap();
        String::from_utf8(request)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_owned()
    });
    (
        Client {
            http: reqwest::Client::new(),
            base,
            key: "test-only".into(),
        },
        task,
    )
}

#[tokio::test]
async fn calendar_uses_one_window_request_and_an_empty_success_is_valid() {
    let (client, request) = server(200, "{\"events\":[]}").await;
    let result = client.calendar("2026-09-10", "2026-11-02").await.unwrap();
    assert!(result.is_empty());
    let path = request.await.unwrap();
    assert!(path.starts_with("GET /api/calendar/?"));
    assert!(path.contains("from_date=2026-09-10"));
    assert!(path.contains("to_date=2026-11-02"));
}

#[tokio::test]
async fn access_errors_stop_date_checks_but_network_and_symbol_errors_do_not() {
    for status in [401, 403, 429, 404, 500] {
        let (client, request) = server(status, "{\"detail\":\"test response\"}").await;
        let error = client.earnings("TEST").await.unwrap_err();
        let event = calendar_error(earnings_key("TEST"), error);
        assert_eq!(
            matches!(event, Event::CalendarBlocked { .. }),
            matches!(status, 401 | 403 | 429)
        );
        assert!(
            request
                .await
                .unwrap()
                .contains("/api/symbols/TEST/earnings/")
        );
    }
}

#[tokio::test]
async fn calendar_bad_json_is_a_failure_not_an_empty_window() {
    let (client, request) = server(200, "not json").await;
    let error = client
        .calendar("2026-09-10", "2026-11-02")
        .await
        .unwrap_err();
    assert!(matches!(
        calendar_error(CALENDAR_KEY.into(), error),
        Event::Error { .. }
    ));
    request.await.unwrap();
}

#[tokio::test]
async fn set_key_barrier_follows_old_responses_in_the_worker_channel() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    cmd_tx
        .send(Cmd::FetchCalendar {
            from: "2026-09-10".into(),
            to: "2026-11-02".into(),
        })
        .unwrap();
    cmd_tx.send(Cmd::SetKey(None)).unwrap();
    cmd_tx.send(Cmd::SetKey(None)).unwrap();
    drop(cmd_tx);
    run(None, cmd_rx, tx).await;
    assert!(matches!(
        rx.recv().await,
        Some(SourceEvent::Alphai(Event::Error { .. }))
    ));
    assert!(matches!(
        rx.recv().await,
        Some(SourceEvent::Alphai(Event::KeyChanged))
    ));
    assert!(matches!(
        rx.recv().await,
        Some(SourceEvent::Alphai(Event::KeyChanged))
    ));
    assert!(rx.recv().await.is_none());
}

/// Optional one-request check. Empty windows and missing elapsed rows are valid.
#[tokio::test]
#[ignore = "one live calendar request; needs ALPHAI_API_KEY"]
async fn live_calendar_smoke() {
    let key = std::env::var("ALPHAI_API_KEY").expect("set ALPHAI_API_KEY");
    let client = Client::new(key).unwrap();
    let today = crate::market::et_time(Utc::now()).date();
    let from = today - chrono::Duration::days(CALENDAR_LOOKBACK_DAYS);
    let to = today + chrono::Duration::days(CALENDAR_DAYS + 1);
    let events = client
        .calendar(&from.to_string(), &to.to_string())
        .await
        .unwrap();
    for at in events.iter().filter_map(CalendarEvent::scheduled) {
        assert!(from <= at.date_naive() && at.date_naive() < to);
    }
}

fn report_request() -> ReportRequest {
    ReportRequest {
        generation: 7,
        tickers: vec!["GOOG".into(), "GOOGL".into()],
        from: "2026-10-31".parse().unwrap(),
        to: "2026-11-14".parse().unwrap(),
    }
}

#[tokio::test]
async fn earnings_calendar_transport_uses_one_full_filtered_et_request() {
    let (client, received) = server(
        200,
        r#"{"from_date":"2026-10-31","to_date":"2026-11-14","items":[]}"#,
    )
    .await;
    assert!(
        client
            .earnings_calendar(&report_request())
            .await
            .unwrap()
            .is_empty()
    );
    let path = received.await.unwrap();
    assert!(path.starts_with("GET /api/calendar/earnings/?"));
    assert!(path.contains("ticker=GOOG%2CGOOGL"));
    assert!(path.contains("detail=full"));
    assert!(path.contains("from_date=2026-10-31") && path.contains("to_date=2026-11-14"));
}

#[tokio::test]
async fn earnings_calendar_rejects_missing_items_bad_dates_windows_and_identity() {
    for body in [
        r#"{"from_date":"2026-10-31","to_date":"2026-11-14"}"#,
        r#"{"from_date":"2026-10-31","to_date":"2026-11-15","items":[]}"#,
        r#"{"from_date":"2026-10-31","to_date":"2026-11-14","items":[{"ticker":"GOOG","company":"Alphabet","report_date":"2026-11-14","status":"scheduled"}]}"#,
        r#"{"from_date":"2026-10-31","to_date":"2026-11-14","items":[{"ticker":"GOOG","company":"Alphabet","report_date":"2026-11-7","status":"scheduled"}]}"#,
        r#"{"from_date":"2026-10-31","to_date":"2026-11-14","items":[{"company":"Alphabet","report_date":"2026-11-07","status":"scheduled"}]}"#,
        r#"{"from_date":"2026-10-31","to_date":"2026-11-14","items":[{"ticker":"GOOG","company":"Alphabet","report_date":"2026-11-07"}]}"#,
        r#"{"from_date":"2026-10-31","to_date":"2026-11-14","items":[{"ticker":"GOOG","company":"Alphabet","listings":[],"report_date":"2026-11-07","status":"reported"}]}"#,
        r#"{"from_date":"2026-10-31","to_date":"2026-11-14","items":[{"ticker":"GOOG","company":"Alphabet","listings":["BAD;"],"report_date":"2026-11-07","status":"reported"}]}"#,
    ] {
        let (client, request) = server(200, body).await;
        assert!(
            client.earnings_calendar(&report_request()).await.is_err(),
            "{body}"
        );
        request.await.unwrap();
    }
}

#[tokio::test]
async fn earnings_calendar_preserves_unknown_status_nullable_sessions_and_free_periods() {
    let (client, request) = server(200, r#"{"from_date":"2026-10-31","to_date":"2026-11-14","items":[
        {"ticker":"GOOGL","company":"ALPHABET INC","listings":["GOOGL","GOOG"],"report_date":"2026-11-07","status":"reported","session":null,"reported_at":"2026-11-07T11:50:00Z","previous_read":null,"report_read":{"uid":"read-id","ticker":"GOOG","fiscal_period":"Half-year ended September 2026","published_at":"2026-11-07T11:50:00Z","verdict":null}},
        {"ticker":"GOOG","company":"Alphabet","report_date":"2026-11-08","status":"future_status","session":null}
    ]}"#).await;
    let items = client.earnings_calendar(&report_request()).await.unwrap();
    request.await.unwrap();
    assert_eq!(items[0].listings, ["GOOG", "GOOGL"]);
    assert!(!items[0].incomplete_listings);
    assert_eq!(items[0].report_read.as_ref().unwrap().ticker, "GOOG");
    assert!(
        items[0]
            .report_read
            .as_ref()
            .unwrap()
            .fiscal_period
            .starts_with("Half-year")
    );
    assert_eq!(items[1].status, ReportStatus::Unknown);
    assert_eq!(items[1].listings, ["GOOG"]);
    assert!(items[1].incomplete_listings);
}

#[tokio::test]
async fn earnings_calendar_invalid_local_scope_never_reaches_the_network() {
    let client = Client {
        http: reqwest::Client::new(),
        base: "http://127.0.0.1:1".into(),
        key: "test-only".into(),
    };
    let mut request = report_request();
    request.tickers.clear();
    assert!(
        client
            .earnings_calendar(&request)
            .await
            .unwrap_err()
            .to_string()
            .contains("1 to 50")
    );
    request.tickers = vec!["A".into(); 51];
    assert!(
        client
            .earnings_calendar(&request)
            .await
            .unwrap_err()
            .to_string()
            .contains("1 to 50")
    );
    request.tickers = vec!["AAPL".into()];
    request.to += chrono::Duration::days(45);
    assert!(
        client
            .earnings_calendar(&request)
            .await
            .unwrap_err()
            .to_string()
            .contains("window")
    );
}
