pub mod alpaca;
pub mod finnhub;
pub mod yahoo;

use std::sync::Arc;

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::domain::{Interval, Range, TickerData};

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
