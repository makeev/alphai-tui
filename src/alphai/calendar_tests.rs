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
