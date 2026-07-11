pub mod alpaca;
pub mod finnhub;
pub mod http;
pub mod yahoo;

use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::domain::{Candle, Interval, Range, TickerData};

/// A pluggable market-data provider.
///
/// To add a backend (IBKR, Alpha Vantage, ...): implement this trait in a new
/// module and register it in `make_source` below. Streaming sources will get
/// an optional `watch()` method later; polling via `fetch` is the baseline
/// every source must support.
#[async_trait]
pub trait DataSource: Send + Sync {
    fn name(&self) -> &'static str;

    /// Latest quote plus recent candles for one symbol.
    async fn fetch(&self, symbol: &str, range: Range, interval: Interval) -> Result<TickerData>;
}

/// `finnhub_key` and `alpaca_keys` (key id, secret) are already resolved
/// (env var wins over the config file; the caller does that layering via
/// `Config::finnhub_key` / `Config::alpaca_keys`).
pub fn make_source(
    name: &str,
    finnhub_key: Option<&str>,
    alpaca_keys: Option<(String, String)>,
) -> Result<Arc<dyn DataSource>> {
    match name.to_lowercase().as_str() {
        "yahoo" | "yf" | "yfinance" => Ok(Arc::new(yahoo::Yahoo::new()?)),
        "finnhub" | "fh" => {
            let token = finnhub_key.map(str::to_string).filter(|t| !t.is_empty()).ok_or_else(
                || {
                    anyhow::anyhow!(
                        "finnhub needs an API key: set it in the settings screen (s) \
                         or export FINNHUB_API_KEY=<key>"
                    )
                },
            )?;
            Ok(Arc::new(finnhub::Finnhub::new(token)?))
        }
        "alpaca" | "alpc" => {
            let (id, secret) = alpaca_keys.ok_or_else(|| {
                anyhow::anyhow!(
                    "alpaca needs an API key id and secret: set them in the settings \
                     screen (s) or export APCA_API_KEY_ID / APCA_API_SECRET_KEY"
                )
            })?;
            Ok(Arc::new(alpaca::Alpaca::new(id, secret)?))
        }
        other => bail!("unknown data source '{other}' (available: yahoo, finnhub, alpaca)"),
    }
}

/// One candle from one bar of a feed. A bar without a close is useless to
/// every view, so it yields None; thin feeds omit O/H/L, which fall back to
/// the close.
pub fn candle_from_ohlc(
    ts: i64,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    volume: Option<f64>,
) -> Option<Candle> {
    let close = close?;
    Some(Candle {
        ts,
        open: open.unwrap_or(close),
        high: high.unwrap_or(close),
        low: low.unwrap_or(close),
        close,
        volume,
    })
}

/// The UI assumes ascending timestamps; feeds that page newest-first
/// deliver descending bars.
pub fn sort_ascending(candles: &mut [Candle]) {
    candles.sort_by_key(|c| c.ts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candle_requires_close_and_fills_ohlc_from_it() {
        assert!(candle_from_ohlc(1, Some(1.0), Some(2.0), Some(0.5), None, None).is_none());
        let c = candle_from_ohlc(1, None, None, None, Some(2.0), Some(30.0)).unwrap();
        assert_eq!((c.open, c.high, c.low, c.close), (2.0, 2.0, 2.0, 2.0));
        assert_eq!(c.volume, Some(30.0));
        let full = candle_from_ohlc(2, Some(1.0), Some(3.0), Some(0.5), Some(2.0), None).unwrap();
        assert_eq!((full.open, full.high, full.low), (1.0, 3.0, 0.5));
    }

    #[test]
    fn sort_ascending_orders_by_timestamp() {
        let mk = |ts| candle_from_ohlc(ts, None, None, None, Some(1.0), None).unwrap();
        let mut candles = vec![mk(3), mk(1), mk(2)];
        sort_ascending(&mut candles);
        assert_eq!(candles.iter().map(|c| c.ts).collect::<Vec<_>>(), vec![1, 2, 3]);
    }
}
