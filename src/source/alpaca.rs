use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::domain::{Candle, Interval, PriceFeed, Quote, QuoteTiming, Range, Sessions, TickerData};
use crate::market;
use crate::source::{
    DataSource, candle_from_ohlc, extended, http, normalize_sessions, sort_ascending, yahoo::Yahoo,
};

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
    extended: Mutex<extended::Cache>,
    yahoo: Yahoo,
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
        let client = http::client_with(http::APP_UA, Some(headers))?;
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
        Ok(Self {
            client,
            base,
            feed,
            extended: Mutex::new(extended::Cache::default()),
            yahoo: Yahoo::new()?,
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        let url = format!("{}{}", self.base, path);
        http::get_json(&self.client, "alpaca", &url, query, error_message).await
    }

    async fn stock_parts(
        &self,
        symbol: &str,
        interval: Interval,
        start: &str,
        feed: &str,
    ) -> (Result<Quote>, Result<Vec<Candle>>) {
        let snapshot_path = format!("/v2/stocks/{symbol}/snapshot");
        let snapshot_query = [("feed", feed)];
        // The historical endpoint calls delayed SIP `sip`. Set its end
        // explicitly so paid accounts also match the delayed snapshot.
        let bars_feed = if feed == "delayed_sip" { "sip" } else { feed };
        let bars_path = format!("/v2/stocks/{symbol}/bars");
        // An API hour straddles 09:30. Fetch halves and aggregate inside
        // sessions, preserving the opening half hour in regular-only mode.
        let raw_interval = if interval == Interval::M60 {
            Interval::M30
        } else {
            interval
        };
        let mut bars_query = vec![
            ("timeframe", timeframe(raw_interval)),
            ("start", start),
            ("limit", "10000"),
            ("sort", "desc"),
            ("adjustment", "split"),
            ("feed", bars_feed),
        ];
        let delayed_end = (Utc::now() - chrono::Duration::minutes(15))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        if feed == "delayed_sip" {
            bars_query.push(("end", delayed_end.as_str()));
        }
        let (snapshot, bars) = tokio::join!(
            self.get_json::<Snapshot>(&snapshot_path, &snapshot_query),
            self.get_json::<StockBars>(&bars_path, &bars_query),
        );
        let provenance = match feed {
            "iex" => PriceFeed::Iex,
            "delayed_sip" => PriceFeed::DelayedSip,
            _ => PriceFeed::Sip,
        };
        let quote = snapshot.and_then(|snap| {
            let mut q = quote_from_snapshot(symbol, &snap, feed != "iex")?;
            q.timing.regular_feed = provenance;
            q.timing.extended_feed = provenance;
            Ok(q)
        });
        let candles = bars.map(|bars| {
            let mut candles = candles_from_desc(bars.bars.unwrap_or_default());
            for c in &mut candles {
                c.feed = provenance;
            }
            normalize_sessions(candles, symbol, interval, Sessions::Extended)
        });
        (quote, candles)
    }

    async fn fetch_stock(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        sessions: Sessions,
        start: &str,
    ) -> Result<TickerData> {
        let (quote, candles) = self.stock_parts(symbol, interval, start, &self.feed).await;
        let mut data = TickerData {
            quote: quote?,
            candles: candles?,
        };
        let now = Utc::now();
        let draw_extended = sessions == Sessions::Extended && interval != Interval::D1;
        if self.feed == "iex"
            && market::is_us_equity(symbol)
            && (draw_extended || market::extended_window(now).is_some())
        {
            let extra = self
                .supplement(symbol, range, interval, draw_extended, now)
                .await;
            // Daily candles stay regular-only, while the extended quote is
            // still useful to the rail and portfolio.
            if interval == Interval::D1 || !draw_extended {
                extended::apply(
                    &mut data,
                    &extended::Supplement {
                        quote: extra.quote,
                        candles: Vec::new(),
                    },
                    now,
                );
            } else {
                extended::apply(&mut data, &extra, now);
            }
        }
        data.candles = normalize_sessions(data.candles, symbol, interval, sessions);
        Ok(data)
    }

    async fn supplement(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        history: bool,
        now: chrono::DateTime<Utc>,
    ) -> extended::Supplement {
        let (range, interval) = if history {
            (range, interval)
        } else {
            (Range::D1, Interval::M5)
        };
        let key = format!("{symbol}:{}:{}", range.as_str(), interval.as_str());
        let (mut cached, due) = self.extended.lock().unwrap().begin(&key, Instant::now());
        if !due {
            return cached;
        }
        let start = (now - chrono::Duration::seconds(range.secs()))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let (quote, bars) = self
            .stock_parts(symbol, interval, &start, "delayed_sip")
            .await;
        let sip_failed = quote.is_err() || (history && bars.is_err());
        extended::merge_quote(&mut cached.quote, quote.ok());
        if let Ok(bars) = bars {
            cached.candles = extended::merge_history(&cached.candles, &bars);
        }
        // At 04:00 the delayed tape naturally cannot yet have today's PRE.
        // Empty responses during those first 15 minutes are not failures.
        let missing_current = market::extended_window(now).is_some_and(|w| {
            now.timestamp() >= w.start + 16 * 60
                && (cached
                    .quote
                    .as_ref()
                    .and_then(|q| q.extended_price_at(now))
                    .is_none()
                    || (now.timestamp() < w.end
                        && cached
                            .quote
                            .as_ref()
                            .and_then(|q| q.timing.extended)
                            .is_some_and(|ts| ts < now.timestamp() - 30 * 60))
                    || (history
                        && !cached
                            .candles
                            .iter()
                            .any(|c| c.ts >= w.start && c.ts < w.end)))
        });
        let missing_history = history && cached.candles.is_empty();
        if (sip_failed || missing_current || missing_history)
            && self.extended.lock().unwrap().claim_yahoo(Instant::now())
        {
            match self
                .yahoo
                .fetch(symbol, range, interval, Sessions::Extended)
                .await
            {
                Ok(yahoo) => {
                    extended::merge_quote(&mut cached.quote, Some(yahoo.quote));
                    cached.candles = extended::merge_history(&cached.candles, &yahoo.candles);
                }
                Err(error) => {
                    let blocked = error.to_string().contains("rate limiting");
                    self.extended
                        .lock()
                        .unwrap()
                        .yahoo_failed(Instant::now(), blocked);
                }
            }
        }
        let cutoff = now.timestamp() - range.secs();
        cached.candles.retain(|c| c.ts >= cutoff);
        self.extended.lock().unwrap().finish(&key, cached.clone());
        cached
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
            .ok_or_else(|| http::unknown_symbol(symbol, None))?;
        let bars = bars?
            .bars
            .unwrap_or_default()
            .remove(pair)
            .unwrap_or_default();
        let mut quote = quote_from_snapshot(symbol, &snap, false)?;
        quote.timing.regular_feed = PriceFeed::Crypto;
        let mut candles = candles_from_desc(bars);
        for c in &mut candles {
            c.feed = PriceFeed::Crypto;
        }
        Ok(TickerData { quote, candles })
    }
}

#[async_trait]
impl DataSource for Alpaca {
    fn name(&self) -> &'static str {
        "alpaca"
    }

    /// Only the delayed SIP feed is behind: IEX is real time (on IEX
    /// volume alone) and full SIP is real time on a paid plan.
    fn delay_note(&self) -> Option<&'static str> {
        (self.feed == "delayed_sip").then_some("delayed 15m")
    }

    async fn fetch(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        sessions: Sessions,
    ) -> Result<TickerData> {
        let timeframe = timeframe(interval);
        let start = (Utc::now() - chrono::Duration::seconds(range.secs()))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        match crypto_pair(symbol) {
            Some(pair) => self.fetch_crypto(symbol, &pair, timeframe, &start).await,
            None => {
                self.fetch_stock(symbol, range, interval, sessions, &start)
                    .await
            }
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
///
/// IEX reports extended-hours trades, so `latestTrade` outside the regular
/// session is a pre or post market print. Those are split out rather than
/// shown as *the* price: quoting the last trade whenever it happened made
/// this source disagree with yahoo after the bell, and with the way every
/// broker screen reads, where the headline number stays the regular close
/// and the extended move sits beside it.
/// `whole_market_volume` says whether the snapshot's share count is the
/// market's or one venue's. It is the market's only on a SIP feed: IEX is a
/// single exchange carrying a few percent of the tape, so reporting its
/// daily bar as "volume" would understate AAPL by a factor of twenty five
/// (measured 2026-09-09: 2.46M against a consolidated 64.9M). Alpaca's
/// crypto venue is its own book for the same reason.
fn quote_from_snapshot(symbol: &str, snap: &Snapshot, whole_market_volume: bool) -> Result<Quote> {
    let close = |bar: &Option<AlpacaBar>| bar.as_ref().and_then(|b| b.c);
    let latest = snap.latest_trade.as_ref().and_then(|t| t.p);
    let latest_ts = snap
        .latest_trade
        .as_ref()
        .and_then(|t| t.t.as_deref())
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.timestamp());
    let extended_now = market::is_us_equity(symbol)
        && latest_ts
            .and_then(market::window_at)
            .is_some_and(|w| matches!(w.session, market::Session::Pre | market::Session::Post));

    // Outside the regular session the daily bar is the session's own close,
    // which is exactly the headline number; the late print becomes the
    // extended one. Crypto never lands here: it has no session to be
    // outside of, so `clock_at` leaves it on the regular path.
    let regular = if extended_now {
        close(&snap.daily_bar).or(latest)
    } else {
        latest
            .or_else(|| close(&snap.minute_bar))
            .or_else(|| close(&snap.daily_bar))
    };
    let Some(price) = regular else {
        return Err(http::unknown_symbol(symbol, None));
    };
    Ok(Quote {
        symbol: symbol.to_string(),
        price,
        prev_close: close(&snap.prev_daily_bar),
        // Both endpoint families quote in USD; the API reports no currency.
        currency: Some("USD".into()),
        extended: extended_now.then_some(latest).flatten(),
        timing: QuoteTiming {
            regular: if extended_now { None } else { latest_ts },
            extended: extended_now.then_some(latest_ts).flatten(),
            extended_reference: extended_now.then_some(price),
            ..Default::default()
        },
        // The snapshot carries no 52 week range.
        fifty_two_week: None,
        // Daily price OHLC excludes extended-hours trades on SIP and IEX.
        day_range: snap.daily_bar.as_ref().and_then(|b| b.l.zip(b.h)),
        volume: whole_market_volume
            .then(|| snap.daily_bar.as_ref().and_then(|b| b.v))
            .flatten(),
    })
}

/// Bars arrive newest-first (`sort=desc`); the UI assumes ascending ts.
/// Bars without a parseable timestamp or a close are dropped.
fn candles_from_desc(bars: Vec<AlpacaBar>) -> Vec<Candle> {
    let mut candles: Vec<Candle> = bars
        .into_iter()
        .filter_map(|b| {
            let ts = chrono::DateTime::parse_from_rfc3339(&b.t).ok()?.timestamp();
            candle_from_ohlc(ts, b.o, b.h, b.l, b.c, b.v)
        })
        .collect();
    sort_ascending(&mut candles);
    candles
}

fn error_message(status: StatusCode, body: &str) -> String {
    // Bad keys answer 401 with an nginx HTML page: body_message returns None
    // there and the snippet fallback keeps the raw noise out of the 401 arm.
    let msg = http::body_message(body).unwrap_or_else(|| http::snippet(body));
    // The data-plan error ("subscription does not permit querying recent SIP
    // data") comes back as 403 from snapshots and 422 from other endpoints;
    // it must not be mistaken for a bad key. The message itself is clear,
    // just add the way out.
    if msg.to_lowercase().contains("subscription") {
        return format!("{msg} (free plan uses ALPACA_FEED=iex)");
    }
    match status.as_u16() {
        401 | 403 => "invalid Alpaca key id or secret, press s to update them \
             (free keys: alpaca.markets)"
            .into(),
        429 => http::rate_limit_msg("Alpaca rate limit hit (200 req/min on the free plan)"),
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
    /// RFC 3339 print time, which is what says whether the trade landed
    /// inside the regular session or after it.
    t: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn supplemental_http_case(sip_fails: bool) {
        use serde_json::json;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let now = Utc::now();
        let mut date = market::et_date(now.timestamp()).unwrap();
        while market::windows(date).is_empty() {
            date = date.pred_opt().unwrap();
        }
        let windows = market::windows(date);
        let ext = market::extended_window(now).unwrap_or(windows[0]);
        let ext_ts = (now.timestamp() - 1).min(ext.end - 1).max(ext.start);
        let format_ts = |ts| {
            chrono::DateTime::from_timestamp(ts, 0)
                .unwrap()
                .to_rfc3339()
        };
        let regular_ts = format_ts(windows[1].start);
        let extra_ts = format_ts(ext_ts);
        let first = format_ts(ext.start);
        let requests = if sip_fails { 7 } else { 6 };
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut byte = [0; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                let text = String::from_utf8(request).unwrap();
                let path = text.split_whitespace().nth(1).unwrap().to_string();
                let supplemental = path.contains("feed=sip") || path.contains("feed=delayed_sip");
                let (status, body) = if sip_fails && supplemental {
                    ("403 Forbidden", json!({"message":"test SIP unavailable"}))
                } else if path.starts_with("/yahoo/") {
                    (
                        "200 OK",
                        json!({"chart":{"result":[{"meta":{"symbol":"AAPL", "regularMarketPrice":100.0},
                        "timestamp":[ext.start, ext.start+60], "indicators":{"quote":[{"open":[101.0,102.0],
                        "high":[101.0,102.0],"low":[101.0,102.0],"close":[101.0,102.0],"volume":[10.0,20.0]}]}}]}}),
                    )
                } else if path.contains("/snapshot") {
                    (
                        "200 OK",
                        json!({"latestTrade":{"t":if supplemental {&extra_ts} else {&regular_ts},"p":if supplemental {101.0} else {100.0}},
                        "dailyBar":{"t":regular_ts,"c":100.0},"prevDailyBar":{"t":regular_ts,"c":99.0}}),
                    )
                } else {
                    assert!(!path.contains("feed=delayed_sip"), "bars must use feed=sip");
                    if supplemental {
                        let url = reqwest::Url::parse(&format!("http://localhost{path}")).unwrap();
                        let end = url.query_pairs().find(|(key, _)| key == "end").unwrap().1;
                        let end = chrono::DateTime::parse_from_rfc3339(&end).unwrap();
                        let delay = Utc::now().signed_duration_since(end).num_seconds();
                        assert!((900..930).contains(&delay), "SIP bars must lag by 15m");
                    }
                    (
                        "200 OK",
                        json!({"bars":[{"t":if supplemental {&first} else {&regular_ts},"o":100.0,"h":102.0,"l":99.0,"c":101.0,"v":10.0}]}),
                    )
                };
                paths.push(path);
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            paths
        });
        let source = Alpaca {
            client: http::client().unwrap(),
            base: base.clone(),
            feed: "iex".into(),
            extended: Mutex::new(extended::Cache::default()),
            yahoo: Yahoo::test_at(format!("{base}/yahoo")),
        };
        for _ in 0..2 {
            let data = source
                .fetch("AAPL", Range::D5, Interval::M5, Sessions::Extended)
                .await
                .unwrap();
            assert_eq!(data.quote.price, 100.0);
            let expected = if sip_fails {
                PriceFeed::Yahoo
            } else {
                PriceFeed::DelayedSip
            };
            assert!(data.candles.iter().any(|c| c.feed == expected));
            assert!(data.candles.iter().any(|c| c.feed == PriceFeed::Iex));
        }
        let paths = tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            paths
                .iter()
                .filter(|p| p.contains("feed=delayed_sip"))
                .count(),
            1
        );
        assert_eq!(paths.iter().filter(|p| p.contains("feed=sip")).count(), 1);
        assert_eq!(
            paths.iter().filter(|p| p.starts_with("/yahoo")).count(),
            usize::from(sip_fails)
        );
    }

    #[tokio::test]
    async fn sip_uses_distinct_endpoint_parameters_and_is_cached() {
        supplemental_http_case(false).await;
    }

    #[tokio::test]
    async fn yahoo_fills_extended_history_when_sip_fails_without_changing_iex() {
        supplemental_http_case(true).await;
    }

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
        let q = quote_from_snapshot("AAPL", &snap, true).unwrap();
        assert_eq!(q.price, 214.53);
        assert_eq!(q.prev_close, Some(210.4));
        assert_eq!(q.currency.as_deref(), Some("USD"));
        assert_eq!(q.symbol, "AAPL");
    }

    /// A print after the closing bell is the extended one, not the price.
    /// Quoting the last trade whenever it happened made this source read
    /// differently from yahoo after hours, and hid the fact that the
    /// regular session had already closed lower.
    #[test]
    fn a_late_print_becomes_the_extended_quote() {
        let raw = r#"{
          "symbol": "AAPL",
          "latestTrade": {"t": "2026-07-10T22:30:00Z", "p": 216.9},
          "dailyBar": {"t": "2026-07-10T04:00:00Z", "o": 212.0, "h": 215.0, "l": 211.5, "c": 214.5, "v": 5000000},
          "prevDailyBar": {"t": "2026-07-09T04:00:00Z", "o": 210.0, "h": 212.5, "l": 209.0, "c": 210.4, "v": 4800000}
        }"#;
        let snap: Snapshot = serde_json::from_str(raw).unwrap();
        let q = quote_from_snapshot("AAPL", &snap, true).unwrap();
        // 22:30 UTC is 18:30 in New York: inside the post session.
        assert_eq!(q.price, 214.5, "headline price stays the regular close");
        assert_eq!(q.extended, Some(216.9));
        let at = chrono::DateTime::parse_from_rfc3339("2026-07-10T22:31:00Z")
            .unwrap()
            .to_utc();
        assert!((q.extended_price_at(at).unwrap() - q.extended_reference() - 2.4).abs() < 1e-9);
    }

    /// One venue's share count is not the market's, and IEX carries a few
    /// percent of the tape.
    #[test]
    fn single_venue_volume_is_withheld() {
        let snap: Snapshot = serde_json::from_str(SNAPSHOT_FULL).unwrap();
        assert_eq!(
            quote_from_snapshot("AAPL", &snap, false).unwrap().volume,
            None
        );
        assert_eq!(
            quote_from_snapshot("AAPL", &snap, true).unwrap().volume,
            Some(5_000_000.0)
        );
    }

    #[test]
    fn quote_falls_back_to_daily_bar() {
        let raw = r#"{
          "symbol": "THIN",
          "dailyBar": {"t": "2026-07-10T04:00:00Z", "o": 9.0, "h": 10.0, "l": 8.5, "c": 9.8, "v": 300}
        }"#;
        let snap: Snapshot = serde_json::from_str(raw).unwrap();
        let q = quote_from_snapshot("THIN", &snap, true).unwrap();
        assert_eq!(q.price, 9.8);
        assert_eq!(q.prev_close, None);
    }

    #[test]
    fn empty_snapshot_is_an_error() {
        let snap: Snapshot = serde_json::from_str("{}").unwrap();
        let err = quote_from_snapshot("NOPE", &snap, true).unwrap_err();
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
        assert!(
            candles.windows(2).all(|w| w[0].ts < w[1].ts),
            "not ascending"
        );
        assert_eq!(candles[0].close, 214.1);
        assert_eq!(candles[0].volume, None);
        assert_eq!(candles[2].close, 214.5);
        assert_eq!(candles[2].volume, Some(900.0));
    }

    #[test]
    fn bars_without_close_or_timestamp_are_dropped() {
        let bars = vec![
            AlpacaBar {
                t: "2026-07-10T15:55:00Z".into(),
                c: Some(1.0),
                ..Default::default()
            },
            AlpacaBar {
                t: "2026-07-10T15:50:00Z".into(),
                c: None,
                ..Default::default()
            },
            AlpacaBar {
                t: "not-a-date".into(),
                c: Some(2.0),
                ..Default::default()
            },
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
        let snap = parsed
            .snapshots
            .as_mut()
            .unwrap()
            .remove("BTC/USD")
            .unwrap();
        let q = quote_from_snapshot("BTC-USD", &snap, false).unwrap();
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
        let html = error_message(
            StatusCode::UNAUTHORIZED,
            "<html>401 Authorization Required</html>",
        );
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
            let data = client
                .fetch(symbol, Range::D5, Interval::M15, Sessions::Regular)
                .await
                .unwrap();
            assert!(data.quote.price > 0.0, "{symbol}: no price");
            assert!(!data.candles.is_empty(), "{symbol}: no candles");
            assert!(
                data.candles.windows(2).all(|w| w[0].ts <= w[1].ts),
                "{symbol}: candles not ascending"
            );
        }
    }
}
