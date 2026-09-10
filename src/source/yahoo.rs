use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::domain::{Candle, Interval, Quote, Range, TickerData};
use crate::source::{DataSource, candle_from_ohlc, http};

/// Yahoo blocks obvious non-browser agents, so this client masquerades as
/// Chrome. Do not swap in `http::APP_UA`.
const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/124.0 Safari/537.36";

/// Yahoo Finance v8 chart endpoint: no API key, ~15 min delayed quotes.
/// One request per symbol returns both the latest price and the candle
/// history, so polling stays at one HTTP call per ticker per cycle.
pub struct Yahoo {
    client: reqwest::Client,
}

impl Yahoo {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: http::client_with(BROWSER_UA, None)?,
        })
    }
}

#[async_trait]
impl DataSource for Yahoo {
    fn name(&self) -> &'static str {
        "yahoo"
    }

    /// The public chart feed runs on the exchanges' free delayed data.
    fn delay_note(&self) -> Option<&'static str> {
        Some("delayed 15m")
    }

    async fn fetch(&self, symbol: &str, range: Range, interval: Interval) -> Result<TickerData> {
        let url = format!("https://query1.finance.yahoo.com/v8/finance/chart/{symbol}");
        let body: ChartResponse = http::get_json(
            &self.client,
            "yahoo",
            &url,
            &[("range", range.as_str()), ("interval", interval.as_str())],
            |status, body| match http::body_message(body) {
                Some(msg) => format!("yahoo API {status}: {msg}"),
                None => format!("yahoo API {status}"),
            },
        )
        .await?;

        if let Some(err) = body.chart.error {
            bail!(
                "{}: {}",
                err.code.unwrap_or_default(),
                err.description.unwrap_or_default()
            );
        }
        let result = body
            .chart
            .result
            .and_then(|mut r| {
                if r.is_empty() {
                    None
                } else {
                    Some(r.remove(0))
                }
            })
            .ok_or_else(|| anyhow!("empty chart result"))?;

        let candles = build_candles(&result);
        let last_close = candles.last().map(|c| c.close);
        let price = result
            .meta
            .regular_market_price
            .or(last_close)
            .ok_or_else(|| anyhow!("no price data"))?;

        // Daily responses never carry `previousClose`, and the
        // `chartPreviousClose` fallback is the close before the fetched
        // window, which the indicator warm-up over-fetch pushes months into
        // the past. The bar before the last one is the real previous close.
        let daily_prev = (interval == Interval::D1 && candles.len() >= 2)
            .then(|| candles[candles.len() - 2].close);

        Ok(TickerData {
            quote: Quote {
                symbol: result.meta.symbol.clone(),
                price,
                prev_close: result
                    .meta
                    .previous_close
                    .or(daily_prev)
                    .or(result.meta.chart_previous_close),
                currency: result.meta.currency.clone(),
                // Only when the regular price came from the meta block: the
                // pair means "regular versus extended" solely when both
                // sides are the API's own numbers. Fall back to a candle
                // close for one side and the difference stops being a
                // session boundary and starts being a rounding artefact.
                extended: result
                    .meta
                    .regular_market_price
                    .and(result.meta.fullday_price),
                fifty_two_week: result
                    .meta
                    .fifty_two_week_low
                    .zip(result.meta.fifty_two_week_high),
                volume: result.meta.regular_market_volume,
            },
            candles,
        })
    }
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
            candle_from_ohlc(
                ts,
                series(&quote.open, i),
                series(&quote.high, i),
                series(&quote.low, i),
                series(&quote.close, i),
                series(&quote.volume, i),
            )
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
            symbol: meta.symbol.clone(),
            price: meta.regular_market_price.unwrap(),
            prev_close: meta.previous_close,
            currency: meta.currency.clone(),
            extended: meta.regular_market_price.and(meta.fullday_price),
            fifty_two_week: meta.fifty_two_week_low.zip(meta.fifty_two_week_high),
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
            symbol: meta.symbol.clone(),
            price: meta.regular_market_price.unwrap(),
            prev_close: meta.previous_close,
            currency: None,
            extended: meta.regular_market_price.and(meta.fullday_price),
            fifty_two_week: None,
            volume: None,
        };
        assert_eq!(quote.extended_price(), None);
        assert_eq!(quote.extended_change_pct(), None);
    }
}
