use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::domain::{Candle, Interval, Quote, Range, TickerData};
use crate::source::DataSource;

/// Yahoo Finance v8 chart endpoint: no API key, ~15 min delayed quotes.
/// One request per symbol returns both the latest price and the candle
/// history, so polling stays at one HTTP call per ticker per cycle.
pub struct Yahoo {
    client: reqwest::Client,
}

impl Yahoo {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
            )
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }
}

#[async_trait]
impl DataSource for Yahoo {
    fn name(&self) -> &'static str {
        "yahoo"
    }

    async fn fetch(&self, symbol: &str, range: Range, interval: Interval) -> Result<TickerData> {
        let url = format!("https://query1.finance.yahoo.com/v8/finance/chart/{symbol}");
        let resp = self
            .client
            .get(&url)
            .query(&[("range", range.as_str()), ("interval", interval.as_str())])
            .send()
            .await
            .context("request failed")?
            .error_for_status()
            .context("yahoo returned an error status")?;

        let body: ChartResponse = resp.json().await.context("bad JSON from yahoo")?;

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
            .and_then(|mut r| if r.is_empty() { None } else { Some(r.remove(0)) })
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
            // Halted/empty minutes come back as nulls; a candle without a
            // close is useless for us, so drop it.
            let close = series(&quote.close, i)?;
            Some(Candle {
                ts,
                open: series(&quote.open, i).unwrap_or(close),
                high: series(&quote.high, i).unwrap_or(close),
                low: series(&quote.low, i).unwrap_or(close),
                close,
                volume: series(&quote.volume, i),
            })
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
