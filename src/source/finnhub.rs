use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::domain::{Candle, Interval, Quote, Range, TickerData};
use crate::source::DataSource;

/// Cap on the per-symbol synthetic history (~2.5h at 15s polls).
const MAX_HISTORY: usize = 600;

/// Finnhub free tier: real-time-ish `/quote` (60 req/min), but historical
/// candles are a premium endpoint. So this source synthesizes history by
/// accumulating one candle per poll — charts grow over the session and reset
/// on restart. `range`/`interval` args are therefore ignored.
pub struct Finnhub {
    client: reqwest::Client,
    token: String,
    history: Mutex<HashMap<String, Vec<Candle>>>,
}

impl Finnhub {
    pub fn new(token: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            token,
            history: Mutex::new(HashMap::new()),
        })
    }
}

#[async_trait]
impl DataSource for Finnhub {
    fn name(&self) -> &'static str {
        "finnhub"
    }

    async fn fetch(&self, symbol: &str, _range: Range, _interval: Interval) -> Result<TickerData> {
        let resp = self
            .client
            .get("https://finnhub.io/api/v1/quote")
            .query(&[("symbol", symbol), ("token", &self.token)])
            .send()
            .await
            .context("request failed")?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            bail!("finnhub rate limit hit (60 req/min free tier), raise --every or drop tickers");
        }
        let resp = resp.error_for_status().context("finnhub returned an error status")?;
        let q: FhQuote = resp.json().await.context("bad JSON from finnhub")?;

        // Finnhub answers unknown symbols with HTTP 200 and all-zero fields.
        if q.c == 0.0 && q.pc == 0.0 && q.t == 0 {
            bail!("no data for '{symbol}' (unknown symbol? crypto needs EXCHANGE:PAIR, e.g. BINANCE:BTCUSDT)");
        }

        let candle = Candle {
            ts: q.t,
            open: q.c,
            high: q.c,
            low: q.c,
            close: q.c,
            volume: None,
        };
        let candles = {
            let mut history = self.history.lock().unwrap();
            let series = history.entry(symbol.to_string()).or_default();
            push_tick(series, candle);
            series.clone()
        };

        Ok(TickerData {
            quote: Quote {
                symbol: symbol.to_string(),
                price: q.c,
                prev_close: Some(q.pc),
                currency: None, // /quote does not report currency (US listings: USD)
            },
            candles,
        })
    }
}

/// Append a synthetic tick-candle: same timestamp updates the last candle
/// instead of duplicating it; the series is capped at MAX_HISTORY.
fn push_tick(series: &mut Vec<Candle>, candle: Candle) {
    match series.last_mut() {
        Some(last) if last.ts == candle.ts => *last = candle,
        _ => series.push(candle),
    }
    if series.len() > MAX_HISTORY {
        let excess = series.len() - MAX_HISTORY;
        series.drain(..excess);
    }
}

#[derive(Deserialize)]
struct FhQuote {
    /// current price
    #[serde(default)]
    c: f64,
    /// previous close
    #[serde(default)]
    pc: f64,
    /// quote timestamp, epoch seconds
    #[serde(default)]
    t: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(ts: i64, close: f64) -> Candle {
        Candle {
            ts,
            open: close,
            high: close,
            low: close,
            close,
            volume: None,
        }
    }

    #[test]
    fn same_timestamp_updates_last_candle() {
        let mut series = vec![];
        push_tick(&mut series, tick(100, 1.0));
        push_tick(&mut series, tick(100, 2.0));
        push_tick(&mut series, tick(101, 3.0));
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].close, 2.0);
        assert_eq!(series[1].close, 3.0);
    }

    #[test]
    fn history_is_capped() {
        let mut series = vec![];
        for i in 0..(MAX_HISTORY as i64 + 50) {
            push_tick(&mut series, tick(i, i as f64));
        }
        assert_eq!(series.len(), MAX_HISTORY);
        assert_eq!(series[0].ts, 50); // oldest dropped
    }
}
