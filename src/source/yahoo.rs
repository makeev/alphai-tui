use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::domain::{Candle, Interval, PriceFeed, Quote, QuoteTiming, Range, Sessions, TickerData};
use crate::market;
use crate::source::{DataSource, candle_from_ohlc, http, normalize_sessions};

/// Yahoo blocks obvious non-browser agents, so this client masquerades as
/// Chrome. Do not swap in `http::APP_UA`.
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// Yahoo Finance v8 chart endpoint: no API key; timing varies by exchange.
/// One request per symbol returns both the latest price and the candle
/// history. Daily charts may use a cached intraday request to time PRE/AH.
pub struct Yahoo {
    client: reqwest::Client,
    base: String,
    extended: Mutex<HashMap<String, CachedPrint>>,
}

#[derive(Clone, Copy)]
struct CachedPrint {
    until: Instant,
    value: Option<(f64, i64)>,
}

impl Yahoo {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: http::client_with(BROWSER_UA, None)?,
            base: "https://query1.finance.yahoo.com/v8/finance/chart".into(),
            extended: Mutex::new(HashMap::new()),
        })
    }
}

/// What a 429 from the public chart feed actually means, in the terms a
/// reader can act on.
///
/// It is not the usual "slow down": the feed throttles by IP and holds the
/// block for a fixed stretch however quietly the client waits. Measured
/// 2026-09-10 from one address: the first block arrived after about ten
/// requests and held for 19 minutes, and the second came after eight and
/// held for over an hour. Nothing the client does shortens it, so the
/// message points at the one thing that does work, which is a different
/// source. This is the failure behind the "it just stops working" reports
/// on every other terminal stock tool.
pub(crate) const THROTTLE_MSG: &str = "yahoo is rate limiting this IP and blocks last tens of minutes.      Press s to switch source, or wait it out";

fn error_message(status: reqwest::StatusCode, body: &str) -> String {
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return THROTTLE_MSG.to_string();
    }
    match http::body_message(body) {
        Some(msg) => format!("yahoo API {status}: {msg}"),
        None => format!("yahoo API {status}"),
    }
}

#[async_trait]
impl DataSource for Yahoo {
    fn name(&self) -> &'static str {
        "yahoo"
    }

    fn delay_note(&self) -> Option<&'static str> {
        Some("timing varies")
    }

    async fn fetch(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        sessions: Sessions,
    ) -> Result<TickerData> {
        let us = market::is_us_equity(symbol);
        let raw_interval = if us && interval == Interval::M60 {
            Interval::M30
        } else {
            interval
        };
        // Intraday extended bars also timestamp the PRE/AH quote when Yahoo
        // omits the optional metadata timestamps. E controls local filtering.
        let result = self
            .chart(
                symbol,
                range,
                raw_interval,
                us || sessions == Sessions::Extended,
            )
            .await?;
        let mut candles = build_candles(&result);
        let now = chrono::Utc::now();
        let mut extended = extended_print(&result, &candles, now);
        if us
            && interval == Interval::D1
            && market::extended_window(now).is_some()
            && extended.is_none()
        {
            let cached = self.extended.lock().unwrap().get(symbol).copied();
            if let Some(cached) = cached.filter(|cached| Instant::now() < cached.until) {
                extended = cached.value;
            } else {
                let value = match self.chart(symbol, Range::D1, Interval::M5, true).await {
                    Ok(short) => extended_print(&short, &build_candles(&short), now),
                    Err(_) => cached.and_then(|cached| cached.value),
                };
                let mut cache = self.extended.lock().unwrap();
                if cache.len() > 128 {
                    cache.clear();
                }
                cache.insert(
                    symbol.into(),
                    CachedPrint {
                        until: Instant::now() + Duration::from_secs(60),
                        value,
                    },
                );
                extended = value;
            }
        }
        let meta = &result.meta;
        let price = meta
            .regular_market_price
            .or_else(|| {
                candles
                    .iter()
                    .rev()
                    .find(|c| {
                        !us || interval == Interval::D1
                            || market::window_at(c.ts)
                                .is_some_and(|w| w.session == market::Session::Open)
                    })
                    .map(|c| c.close)
            })
            .ok_or_else(|| anyhow!("no regular price data"))?;
        let daily_prev = (interval == Interval::D1 && candles.len() >= 2)
            .then(|| candles[candles.len() - 2].close);
        candles = normalize_sessions(candles, symbol, interval, sessions);
        Ok(TickerData {
            quote: Quote {
                symbol: meta.symbol.clone(),
                price,
                prev_close: meta
                    .previous_close
                    .or(daily_prev)
                    .or(meta.chart_previous_close),
                currency: meta.currency.clone(),
                extended: extended.map(|(price, _)| price),
                timing: QuoteTiming {
                    regular: meta.regular_market_time,
                    regular_feed: PriceFeed::Yahoo,
                    extended: extended.map(|(_, ts)| ts),
                    extended_feed: PriceFeed::Yahoo,
                    extended_reference: Some(price),
                },
                fifty_two_week: meta.fifty_two_week_low.zip(meta.fifty_two_week_high),
                volume: meta.regular_market_volume,
                day_range: meta
                    .regular_market_day_low
                    .zip(meta.regular_market_day_high),
            },
            candles,
        })
    }
}

impl Yahoo {
    #[cfg(test)]
    pub(super) fn test_at(base: String) -> Self {
        let mut yahoo = Self::new().unwrap();
        yahoo.base = base;
        yahoo
    }
    async fn chart(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        pre_post: bool,
    ) -> Result<ChartResult> {
        let url = format!("{}/{symbol}", self.base);
        let body: ChartResponse = http::get_json_retrying(
            &self.client,
            "yahoo",
            &url,
            &[
                ("range", range.as_str()),
                ("interval", interval.as_str()),
                ("includePrePost", if pre_post { "true" } else { "false" }),
            ],
            error_message,
            http::Retry::GatewayOnly,
        )
        .await?;
        if let Some(err) = body.chart.error {
            bail!(
                "{}: {}",
                err.code.unwrap_or_default(),
                err.description.unwrap_or_default()
            );
        }
        body.chart
            .result
            .and_then(|r| r.into_iter().next())
            .ok_or_else(|| anyhow!("empty chart result"))
    }
}

fn extended_print(
    result: &ChartResult,
    candles: &[Candle],
    now: chrono::DateTime<chrono::Utc>,
) -> Option<(f64, i64)> {
    if !market::is_us_equity(&result.meta.symbol) {
        return None;
    }
    let w = market::extended_window(now)?;
    let m = &result.meta;
    let explicit = [
        m.pre_market_price.zip(m.pre_market_time),
        m.post_market_price.zip(m.post_market_time),
        m.fullday_price
            .zip(m.fullday_market_time.or(m.fullday_time)),
    ];
    explicit
        .into_iter()
        .flatten()
        .chain(candles.iter().map(|c| (c.close, c.ts)))
        .filter(|(p, ts)| {
            p.is_finite() && *p > 0.0 && *ts >= w.start && *ts < w.end && *ts <= now.timestamp()
        })
        .max_by_key(|(_, ts)| *ts)
}

fn build_candles(result: &ChartResult) -> Vec<Candle> {
    let timestamps = match &result.timestamp {
        Some(ts) => ts,
        None => return Vec::new(),
    };
    let quote = match result.indicators.quote.first() {
        Some(q) => q,
        None => return Vec::new(),
    };
    let series = |v: &Option<Vec<Option<f64>>>, i: usize| -> Option<f64> {
        v.as_ref().and_then(|v| v.get(i).copied().flatten())
    };

    timestamps
        .iter()
        .enumerate()
        .filter_map(|(i, &ts)| {
            // Halted/empty minutes come back as null closes and are dropped.
            let mut candle = candle_from_ohlc(
                ts,
                series(&quote.open, i),
                series(&quote.high, i),
                series(&quote.low, i),
                series(&quote.close, i),
                series(&quote.volume, i),
            )?;
            candle.feed = PriceFeed::Yahoo;
            Some(candle)
        })
        .collect()
}

#[derive(Deserialize)]
struct ChartResponse {
    chart: Chart,
}

#[derive(Deserialize)]
struct Chart {
    result: Option<Vec<ChartResult>>,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    code: Option<String>,
    description: Option<String>,
}

#[derive(Deserialize)]
struct ChartResult {
    meta: Meta,
    timestamp: Option<Vec<i64>>,
    indicators: Indicators,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    symbol: String,
    currency: Option<String>,
    regular_market_price: Option<f64>,
    regular_market_time: Option<i64>,
    pre_market_price: Option<f64>,
    pre_market_time: Option<i64>,
    post_market_price: Option<f64>,
    post_market_time: Option<i64>,
    fullday_market_time: Option<i64>,
    fullday_time: Option<i64>,
    previous_close: Option<f64>,
    chart_previous_close: Option<f64>,
    /// Last trade of the *whole* day, extended sessions included. Verified
    /// 2026-09-10: it rides along without `includePrePost`, so reading it
    /// costs no extra request and leaves the candle set alone. During the
    /// regular session it equals `regularMarketPrice`, and `Quote` filters
    /// that case out rather than showing a zero move.
    fullday_price: Option<f64>,
    fifty_two_week_high: Option<f64>,
    fifty_two_week_low: Option<f64>,
    regular_market_volume: Option<f64>,
    regular_market_day_high: Option<f64>,
    regular_market_day_low: Option<f64>,
}

#[derive(Deserialize)]
struct Indicators {
    quote: Vec<QuoteBlock>,
}

#[derive(Deserialize)]
struct QuoteBlock {
    open: Option<Vec<Option<f64>>>,
    high: Option<Vec<Option<f64>>>,
    low: Option<Vec<Option<f64>>>,
    close: Option<Vec<Option<f64>>>,
    volume: Option<Vec<Option<f64>>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unstamped_fullday_price_cannot_become_todays_premarket() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-14T09:00:00Z")
            .unwrap()
            .to_utc();
        let result = parse(CHART_AFTER_HOURS);
        assert_eq!(extended_print(&result, &build_candles(&result), now), None);
        let bars = vec![Candle {
            ts: now.timestamp() - 300,
            close: 315.34,
            ..Default::default()
        }];
        assert_eq!(
            extended_print(&result, &bars, now),
            Some((315.34, now.timestamp() - 300)),
            "zero movement is a valid timestamped quote"
        );
    }

    /// Trimmed from a live response (AAPL, 2026-09-10, after the bell). The
    /// meta values are verbatim: `fulldayPrice` above `regularMarketPrice`
    /// is the after-hours print, and it arrives without asking for
    /// `includePrePost`, which is why reading it costs no extra request.
    const CHART_AFTER_HOURS: &str = r#"{
      "chart": {"result": [{
        "meta": {
          "symbol": "AAPL",
          "currency": "USD",
          "regularMarketPrice": 315.34,
          "previousClose": 316.22,
          "chartPreviousClose": 316.22,
          "fulldayPrice": 317.47,
          "fiftyTwoWeekHigh": 344.57,
          "fiftyTwoWeekLow": 225.95,
          "regularMarketVolume": 64902991
        },
        "timestamp": [1757520000, 1757520300],
        "indicators": {"quote": [{
          "open": [315.1, 315.2], "high": [315.6, 315.5],
          "low": [314.9, 315.0], "close": [315.2, 315.34],
          "volume": [120000, 98000]
        }]}
      }], "error": null}
    }"#;

    fn parse(raw: &str) -> ChartResult {
        let body: ChartResponse = serde_json::from_str(raw).unwrap();
        body.chart.result.unwrap().remove(0)
    }

    #[test]
    fn after_hours_fields_come_off_the_meta_block() {
        let meta = parse(CHART_AFTER_HOURS).meta;
        assert_eq!(meta.regular_market_price, Some(315.34));
        assert_eq!(meta.fullday_price, Some(317.47));
        assert_eq!(meta.fifty_two_week_low, Some(225.95));
        assert_eq!(meta.fifty_two_week_high, Some(344.57));
        assert_eq!(meta.regular_market_volume, Some(64_902_991.0));
    }

    /// The headline price stays the regular close while the late print goes
    /// to its own field, so the day still reads as down 0.28% and the
    /// after-hours move as up 0.68% at the same time.
    #[test]
    fn the_quote_keeps_the_two_prices_apart() {
        let meta = parse(CHART_AFTER_HOURS).meta;
        let quote = Quote {
            timing: Default::default(),
            symbol: meta.symbol.clone(),
            price: meta.regular_market_price.unwrap(),
            prev_close: meta.previous_close,
            currency: meta.currency.clone(),
            extended: meta.regular_market_price.and(meta.fullday_price),
            fifty_two_week: meta.fifty_two_week_low.zip(meta.fifty_two_week_high),
            day_range: meta
                .regular_market_day_low
                .zip(meta.regular_market_day_high),
            volume: meta.regular_market_volume,
        };
        assert!((quote.change_pct().unwrap() - -0.278_29).abs() < 1e-4);
        assert!((quote.extended_change_pct().unwrap() - 0.675_43).abs() < 1e-4);
        assert_eq!(quote.extended_price(), Some(317.47));
    }

    /// Symbols with no extended session report the same number twice, and
    /// that must not read as a zero move.
    #[test]
    fn a_matching_fullday_price_is_not_an_extended_print() {
        let raw = CHART_AFTER_HOURS.replace("\"fulldayPrice\": 317.47", "\"fulldayPrice\": 315.34");
        let meta = parse(&raw).meta;
        let quote = Quote {
            timing: Default::default(),
            symbol: meta.symbol.clone(),
            price: meta.regular_market_price.unwrap(),
            prev_close: meta.previous_close,
            currency: None,
            extended: meta.regular_market_price.and(meta.fullday_price),
            fifty_two_week: None,
            day_range: None,
            volume: None,
        };
        assert_eq!(quote.extended_price(), None);
        assert_eq!(quote.extended_change_pct(), None);
    }

    /// A 429 here is not "slow down": the feed blocks by IP for tens of
    /// minutes whatever the client does, so the message names the one
    /// thing that fixes it now. The generic path keeps the API's own text.
    #[test]
    fn a_429_says_it_is_an_ip_block_and_what_to_do() {
        use reqwest::StatusCode;
        let msg = error_message(StatusCode::TOO_MANY_REQUESTS, "Too Many Requests");
        assert_eq!(msg, THROTTLE_MSG);
        assert!(msg.contains("Press s to switch source"), "{msg}");
        assert_eq!(
            error_message(StatusCode::NOT_FOUND, r#"{"message":"No data found"}"#),
            "yahoo API 404 Not Found: No data found"
        );
        assert_eq!(
            error_message(StatusCode::NOT_FOUND, "<html>nope</html>"),
            "yahoo API 404 Not Found"
        );
    }
}
