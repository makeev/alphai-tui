use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::domain::{Candle, Interval, Quote, Range, TickerData};
use crate::source::DataSource;

pub const DEFAULT_DATA_URL: &str = "https://data.alpaca.markets";

/// Alpaca Market Data API: the free Basic plan gives realtime IEX quotes and
/// real historical bars, so charts are full from the first poll (unlike the
/// Finnhub free tier where history accrues over the session). Crypto pairs
/// in Yahoo form (`BTC-USD`) are routed to the separate v1beta3 crypto
/// endpoints. Two requests per symbol per poll (snapshot + bars); Basic
/// allows 200 req/min.
pub struct Alpaca {
    client: reqwest::Client,
    base: String,
    feed: String,
}

impl Alpaca {
    pub fn new(key_id: String, secret: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "APCA-API-KEY-ID",
            HeaderValue::from_str(&key_id).context("invalid Alpaca key id")?,
        );
        headers.insert(
            "APCA-API-SECRET-KEY",
            HeaderValue::from_str(&secret).context("invalid Alpaca secret")?,
        );
        let client = reqwest::Client::builder()
            .user_agent(concat!("alphai-tui/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .timeout(Duration::from_secs(10))
            .build()?;
        let base = std::env::var("ALPACA_DATA_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_DATA_URL.to_string());
        // Passed explicitly on every stock request so behavior is
        // deterministic: the API default depends on the account's data plan.
        let feed = std::env::var("ALPACA_FEED")
            .ok()
            .filter(|f| !f.trim().is_empty())
            .unwrap_or_else(|| "iex".to_string());
        Ok(Self { client, base, feed })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        let resp = self
            .client
            .get(format!("{}{}", self.base, path))
            .query(query)
            .send()
            .await
            .context("request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("{}", error_message(status, &body));
        }
        resp.json().await.context("bad JSON from alpaca")
    }

    async fn fetch_stock(&self, symbol: &str, timeframe: &str, start: &str) -> Result<TickerData> {
        let snapshot_path = format!("/v2/stocks/{symbol}/snapshot");
        let snapshot_query = [("feed", self.feed.as_str())];
        // `end` is deliberately omitted: its default is the latest moment the
        // plan allows, so free-tier requests never trip a 422 on bars.
        // `sort=desc` keeps the NEWEST bars when a long range/interval combo
        // exceeds the 10000 limit; candles_from_desc restores ascending order.
        let bars_path = format!("/v2/stocks/{symbol}/bars");
        let bars_query = [
            ("timeframe", timeframe),
            ("start", start),
            ("limit", "10000"),
            ("sort", "desc"),
            ("adjustment", "split"),
            ("feed", self.feed.as_str()),
        ];
        let (snapshot, bars) = tokio::join!(
            self.get_json::<Snapshot>(&snapshot_path, &snapshot_query),
            self.get_json::<StockBars>(&bars_path, &bars_query),
        );
        Ok(TickerData {
            quote: quote_from_snapshot(symbol, &snapshot?)?,
            candles: candles_from_desc(bars?.bars.unwrap_or_default()),
        })
    }

    async fn fetch_crypto(
        &self,
        symbol: &str,
        pair: &str,
        timeframe: &str,
        start: &str,
    ) -> Result<TickerData> {
        let snapshots_query = [("symbols", pair)];
        let bars_query = [
            ("symbols", pair),
            ("timeframe", timeframe),
            ("start", start),
            ("limit", "10000"),
            ("sort", "desc"),
        ];
        let (snapshots, bars) = tokio::join!(
            self.get_json::<CryptoSnapshots>("/v1beta3/crypto/us/snapshots", &snapshots_query),
            self.get_json::<CryptoBars>("/v1beta3/crypto/us/bars", &bars_query),
        );
        let snap = snapshots?
            .snapshots
            .unwrap_or_default()
            .remove(pair)
            .ok_or_else(|| anyhow!("no data for '{symbol}' (unknown symbol?)"))?;
        let bars = bars?.bars.unwrap_or_default().remove(pair).unwrap_or_default();
        Ok(TickerData {
            quote: quote_from_snapshot(symbol, &snap)?,
            candles: candles_from_desc(bars),
        })
    }
}

#[async_trait]
impl DataSource for Alpaca {
    fn name(&self) -> &'static str {
        "alpaca"
    }

    async fn fetch(&self, symbol: &str, range: Range, interval: Interval) -> Result<TickerData> {
        let timeframe = timeframe(interval);
        let start = (Utc::now() - chrono::Duration::seconds(range.secs()))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        match crypto_pair(symbol) {
            Some(pair) => self.fetch_crypto(symbol, &pair, timeframe, &start).await,
            None => self.fetch_stock(symbol, timeframe, &start).await,
        }
    }
}

/// `BTC-USD` (Yahoo convention, used across the app) -> `BTC/USD` (Alpaca
/// crypto pair). Non-crypto symbols return None and go to the stock endpoints.
fn crypto_pair(symbol: &str) -> Option<String> {
    let base = symbol.strip_suffix("-USD")?;
    if base.is_empty() {
        return None;
    }
    Some(format!("{base}/USD"))
}

fn timeframe(interval: Interval) -> &'static str {
    match interval {
        Interval::M1 => "1Min",
        Interval::M2 => "2Min",
        Interval::M5 => "5Min",
        Interval::M15 => "15Min",
        Interval::M30 => "30Min",
        Interval::M60 => "1Hour",
        Interval::D1 => "1Day",
    }
}

/// Price fallback chain for thin IEX data: illiquid names may have no
/// `latestTrade`, so fall through to the latest minute bar, then the daily
/// bar. A snapshot with none of them is an unknown or dead symbol.
fn quote_from_snapshot(symbol: &str, snap: &Snapshot) -> Result<Quote> {
    let close = |bar: &Option<AlpacaBar>| bar.as_ref().and_then(|b| b.c);
    let price = snap
        .latest_trade
        .as_ref()
        .and_then(|t| t.p)
        .or_else(|| close(&snap.minute_bar))
        .or_else(|| close(&snap.daily_bar));
    let Some(price) = price else {
        bail!("no data for '{symbol}' (unknown symbol?)");
    };
    Ok(Quote {
        symbol: symbol.to_string(),
        price,
        prev_close: close(&snap.prev_daily_bar),
        // Both endpoint families quote in USD; the API reports no currency.
        currency: Some("USD".into()),
    })
}

/// Bars arrive newest-first (`sort=desc`); the UI assumes ascending ts, so
/// reverse after parsing. Bars without a close are dropped.
fn candles_from_desc(bars: Vec<AlpacaBar>) -> Vec<Candle> {
    let mut candles: Vec<Candle> = bars
        .into_iter()
        .filter_map(|b| {
            let ts = chrono::DateTime::parse_from_rfc3339(&b.t).ok()?.timestamp();
            let close = b.c?;
            Some(Candle {
                ts,
                open: b.o.unwrap_or(close),
                high: b.h.unwrap_or(close),
                low: b.l.unwrap_or(close),
                close,
                volume: b.v,
            })
        })
        .collect();
    candles.reverse();
    candles
}

fn error_message(status: StatusCode, body: &str) -> String {
    let msg = serde_json::from_str::<ErrorBody>(body)
        .ok()
        .and_then(|e| e.message)
        .unwrap_or_else(|| body.chars().take(120).collect());
    // The data-plan error ("subscription does not permit querying recent SIP
    // data") comes back as 403 from snapshots and 422 from other endpoints;
    // it must not be mistaken for a bad key. The message itself is clear,
    // just add the way out.
    if msg.to_lowercase().contains("subscription") {
        return format!("{msg} (free plan uses ALPACA_FEED=iex)");
    }
    match status.as_u16() {
        401 | 403 => {
            "invalid Alpaca key id or secret, press s to update them \
             (free keys: alpaca.markets)"
                .into()
        }
        429 => "Alpaca rate limit hit (200 req/min on the free plan), \
                raise --every or drop tickers"
            .into(),
        _ => format!("Alpaca API {status}: {msg}"),
    }
}

// ---------------------------------------------------------------------------
// API shapes (tolerant subset). Stock and crypto bars/snapshots share the
// same inner fields; only the wrappers differ (stocks: flat snapshot and a
// bar array; crypto: maps keyed by pair).

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Snapshot {
    latest_trade: Option<Trade>,
    minute_bar: Option<AlpacaBar>,
    daily_bar: Option<AlpacaBar>,
    prev_daily_bar: Option<AlpacaBar>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Trade {
    p: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AlpacaBar {
    t: String,
    o: Option<f64>,
    h: Option<f64>,
    l: Option<f64>,
    c: Option<f64>,
    v: Option<f64>,
}

/// `bars` is `null` (not `[]`) when the window has no data.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StockBars {
    bars: Option<Vec<AlpacaBar>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CryptoSnapshots {
    snapshots: Option<HashMap<String, Snapshot>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CryptoBars {
    bars: Option<HashMap<String, Vec<AlpacaBar>>>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ErrorBody {
    message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_to_timeframe() {
        assert_eq!(timeframe(Interval::M1), "1Min");
        assert_eq!(timeframe(Interval::M2), "2Min");
        assert_eq!(timeframe(Interval::M5), "5Min");
        assert_eq!(timeframe(Interval::M15), "15Min");
        assert_eq!(timeframe(Interval::M30), "30Min");
        assert_eq!(timeframe(Interval::M60), "1Hour");
        assert_eq!(timeframe(Interval::D1), "1Day");
    }

    #[test]
    fn crypto_pair_detection() {
        assert_eq!(crypto_pair("BTC-USD").as_deref(), Some("BTC/USD"));
        assert_eq!(crypto_pair("SOL-USD").as_deref(), Some("SOL/USD"));
        assert_eq!(crypto_pair("AAPL"), None);
        assert_eq!(crypto_pair("-USD"), None);
    }

    const SNAPSHOT_FULL: &str = r#"{
      "symbol": "AAPL",
      "latestTrade": {"t": "2026-07-10T15:59:58Z", "x": "V", "p": 214.53, "s": 100},
      "latestQuote": {"t": "2026-07-10T15:59:59Z", "bp": 214.5, "ap": 214.55},
      "minuteBar": {"t": "2026-07-10T15:59:00Z", "o": 214.4, "h": 214.6, "l": 214.3, "c": 214.5, "v": 12000},
      "dailyBar": {"t": "2026-07-10T04:00:00Z", "o": 212.0, "h": 215.0, "l": 211.5, "c": 214.5, "v": 5000000},
      "prevDailyBar": {"t": "2026-07-09T04:00:00Z", "o": 210.0, "h": 212.5, "l": 209.0, "c": 210.4, "v": 4800000}
    }"#;

    #[test]
    fn quote_uses_latest_trade() {
        let snap: Snapshot = serde_json::from_str(SNAPSHOT_FULL).unwrap();
        let q = quote_from_snapshot("AAPL", &snap).unwrap();
        assert_eq!(q.price, 214.53);
        assert_eq!(q.prev_close, Some(210.4));
        assert_eq!(q.currency.as_deref(), Some("USD"));
        assert_eq!(q.symbol, "AAPL");
    }

    #[test]
    fn quote_falls_back_to_daily_bar() {
        let raw = r#"{
          "symbol": "THIN",
          "dailyBar": {"t": "2026-07-10T04:00:00Z", "o": 9.0, "h": 10.0, "l": 8.5, "c": 9.8, "v": 300}
        }"#;
        let snap: Snapshot = serde_json::from_str(raw).unwrap();
        let q = quote_from_snapshot("THIN", &snap).unwrap();
        assert_eq!(q.price, 9.8);
        assert_eq!(q.prev_close, None);
    }

    #[test]
    fn empty_snapshot_is_an_error() {
        let snap: Snapshot = serde_json::from_str("{}").unwrap();
        let err = quote_from_snapshot("NOPE", &snap).unwrap_err();
        assert!(err.to_string().contains("no data for 'NOPE'"), "{err}");
    }

    #[test]
    fn desc_bars_reverse_to_ascending_candles() {
        let raw = r#"{
          "bars": [
            {"t": "2026-07-10T15:55:00Z", "o": 214.4, "h": 214.6, "l": 214.3, "c": 214.5, "v": 900},
            {"t": "2026-07-10T15:50:00Z", "o": 214.1, "h": 214.5, "l": 214.0, "c": 214.4, "v": 800},
            {"t": "2026-07-10T15:45:00Z", "o": 214.0, "h": 214.2, "l": 213.9, "c": 214.1}
          ],
          "symbol": "AAPL",
          "next_page_token": null
        }"#;
        let parsed: StockBars = serde_json::from_str(raw).unwrap();
        let candles = candles_from_desc(parsed.bars.unwrap());
        assert_eq!(candles.len(), 3);
        assert!(candles.windows(2).all(|w| w[0].ts < w[1].ts), "not ascending");
        assert_eq!(candles[0].close, 214.1);
        assert_eq!(candles[0].volume, None);
        assert_eq!(candles[2].close, 214.5);
        assert_eq!(candles[2].volume, Some(900.0));
    }

    #[test]
    fn bars_without_close_or_timestamp_are_dropped() {
        let bars = vec![
            AlpacaBar { t: "2026-07-10T15:55:00Z".into(), c: Some(1.0), ..Default::default() },
            AlpacaBar { t: "2026-07-10T15:50:00Z".into(), c: None, ..Default::default() },
            AlpacaBar { t: "not-a-date".into(), c: Some(2.0), ..Default::default() },
        ];
        let candles = candles_from_desc(bars);
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].close, 1.0);
        assert_eq!(candles[0].open, 1.0); // open falls back to close
    }

    #[test]
    fn null_bars_parse_as_empty() {
        let parsed: StockBars =
            serde_json::from_str(r#"{"bars": null, "symbol": "X", "next_page_token": null}"#)
                .unwrap();
        assert!(candles_from_desc(parsed.bars.unwrap_or_default()).is_empty());
    }

    #[test]
    fn crypto_wrappers_are_maps_by_pair() {
        let raw = r#"{
          "snapshots": {
            "BTC/USD": {
              "latestTrade": {"t": "2026-07-10T12:00:00Z", "p": 65000.5, "s": 0.01},
              "prevDailyBar": {"t": "2026-07-09T05:00:00Z", "o": 64000.0, "h": 65500.0, "l": 63800.0, "c": 64500.0, "v": 5000.0}
            }
          }
        }"#;
        let mut parsed: CryptoSnapshots = serde_json::from_str(raw).unwrap();
        let snap = parsed.snapshots.as_mut().unwrap().remove("BTC/USD").unwrap();
        let q = quote_from_snapshot("BTC-USD", &snap).unwrap();
        assert_eq!(q.symbol, "BTC-USD");
        assert_eq!(q.price, 65000.5);
        assert_eq!(q.prev_close, Some(64500.0));

        let raw = r#"{
          "bars": {
            "BTC/USD": [
              {"t": "2026-07-10T12:00:00Z", "o": 64990.0, "h": 65010.0, "l": 64980.0, "c": 65000.0, "v": 12.5},
              {"t": "2026-07-10T11:55:00Z", "o": 64970.0, "h": 64995.0, "l": 64960.0, "c": 64990.0, "v": 10.0}
            ]
          },
          "next_page_token": null
        }"#;
        let mut parsed: CryptoBars = serde_json::from_str(raw).unwrap();
        let candles = candles_from_desc(parsed.bars.as_mut().unwrap().remove("BTC/USD").unwrap());
        assert_eq!(candles.len(), 2);
        assert!(candles[0].ts < candles[1].ts);
    }

    #[test]
    fn error_messages_by_status() {
        let auth = error_message(StatusCode::FORBIDDEN, r#"{"message":"forbidden."}"#);
        assert!(auth.contains("invalid Alpaca key"), "{auth}");
        assert!(auth.contains("alpaca.markets"), "{auth}");

        // Bad keys answer 401 with an nginx HTML page, not JSON.
        let html = error_message(StatusCode::UNAUTHORIZED, "<html>401 Authorization Required</html>");
        assert!(html.contains("invalid Alpaca key"), "{html}");

        // The subscription error is a 403 on snapshots and a 422 elsewhere;
        // both must surface the API message, not the bad-key hint.
        for status in [StatusCode::FORBIDDEN, StatusCode::UNPROCESSABLE_ENTITY] {
            let sip = error_message(
                status,
                r#"{"code":42210000,"message":"subscription does not permit querying recent SIP data"}"#,
            );
            assert!(sip.starts_with("subscription does not permit"), "{sip}");
            assert!(sip.contains("ALPACA_FEED=iex"), "{sip}");
        }

        let limit = error_message(StatusCode::TOO_MANY_REQUESTS, "");
        assert!(limit.contains("200 req/min"), "{limit}");

        let other = error_message(StatusCode::INTERNAL_SERVER_ERROR, "oops");
        assert!(other.contains("Alpaca API 500"), "{other}");
        assert!(other.contains("oops"), "{other}");
    }

    /// Live end-to-end check against the real API (4 requests: one equity,
    /// one crypto pair, snapshot + bars each).
    /// Run: APCA_API_KEY_ID=… APCA_API_SECRET_KEY=… cargo test live_alpaca -- --ignored
    #[tokio::test]
    #[ignore = "live API call; needs APCA_API_KEY_ID and APCA_API_SECRET_KEY"]
    async fn live_alpaca_smoke() {
        let id = std::env::var("APCA_API_KEY_ID").expect("set APCA_API_KEY_ID");
        let secret = std::env::var("APCA_API_SECRET_KEY").expect("set APCA_API_SECRET_KEY");
        let client = Alpaca::new(id, secret).unwrap();
        for symbol in ["AAPL", "BTC-USD"] {
            let data = client.fetch(symbol, Range::D5, Interval::M15).await.unwrap();
            assert!(data.quote.price > 0.0, "{symbol}: no price");
            assert!(!data.candles.is_empty(), "{symbol}: no candles");
            assert!(
                data.candles.windows(2).all(|w| w[0].ts <= w[1].ts),
                "{symbol}: candles not ascending"
            );
        }
    }
}
