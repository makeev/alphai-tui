pub mod alpaca;
mod extended;
pub mod finnhub;
pub mod http;
pub mod registry;
pub mod yahoo;

use std::sync::Arc;

use anyhow::{Result, anyhow};
use async_trait::async_trait;

use crate::config::Config;
use crate::domain::{Candle, Interval, Range, Sessions, TickerData};

/// A pluggable market-data provider.
///
/// To add a backend (IBKR, Alpha Vantage, ...): implement this trait in a
/// new module and append one entry to `registry::SOURCES` — the CLI, config
/// keys, settings screen and error messages all derive from that table.
/// Streaming sources will get an optional `watch()` method later; polling
/// via `fetch` is the baseline every source must support.
#[async_trait]
pub trait DataSource: Send + Sync {
    fn name(&self) -> &'static str;

    /// Latest quote plus recent candles for one symbol.
    async fn fetch(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        sessions: Sessions,
    ) -> Result<TickerData>;

    /// How stale this source's prices are, for the quote rail's badge.
    /// None means real time. It is a method rather than a registry field
    /// because a source can be configured either way at runtime (an Alpaca
    /// key on `ALPACA_FEED=delayed_sip`, say), and only the source knows.
    fn delay_note(&self) -> Option<&'static str> {
        None
    }
}

/// Build a source by name (registry id or alias). Credentials resolve
/// env-over-file via `Config::key_value`; a missing one turns into the
/// registry's how-to-fix message.
pub fn make_source(name: &str, cfg: &Config) -> Result<Arc<dyn DataSource>> {
    let info = registry::find(name).ok_or_else(|| {
        anyhow!(
            "unknown data source '{name}' (available: {})",
            registry::ids().join(", ")
        )
    })?;
    let keys = info
        .key_fields
        .iter()
        .map(|field| {
            cfg.key_value(field)
                .ok_or_else(|| anyhow!(registry::missing_keys_msg(info)))
        })
        .collect::<Result<Vec<_>>>()?;
    (info.make)(&keys)
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
        feed: Default::default(),
    })
}

/// The UI assumes ascending timestamps; feeds that page newest-first
/// deliver descending bars.
pub fn sort_ascending(candles: &mut [Candle]) {
    candles.sort_by_key(|c| c.ts);
}

/// Filter US intraday bars and align larger buckets to each session's own
/// opening. Callers fetch 30m rather than 60m to preserve the 09:30 boundary.
pub fn normalize_sessions(
    candles: Vec<Candle>,
    symbol: &str,
    interval: Interval,
    sessions: Sessions,
) -> Vec<Candle> {
    use crate::market::{self, Session};
    if interval == Interval::D1 || !market::is_us_equity(symbol) {
        return candles;
    }
    let mut out: Vec<Candle> = Vec::new();
    for mut c in candles {
        let Some(w) = market::window_at(c.ts) else {
            continue;
        };
        if sessions == Sessions::Regular && w.session != Session::Open {
            continue;
        }
        c.ts = w.start + (c.ts - w.start) / interval.secs() * interval.secs();
        if let Some(last) = out.last_mut()
            && last.ts == c.ts
            && last.feed == c.feed
        {
            last.high = last.high.max(c.high);
            last.low = last.low.min(c.low);
            last.close = c.close;
            last.volume = match (last.volume, c.volume) {
                (None, None) => None,
                (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
            };
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hourly_bars_do_not_mix_premarket_with_the_open() {
        let ts = chrono::DateTime::parse_from_rfc3339("2026-09-14T13:00:00Z")
            .unwrap()
            .timestamp();
        let bars: Vec<_> = (0..4)
            .map(|i| Candle {
                ts: ts + i * 1800,
                open: 100.0 + i as f64,
                high: 101.0 + i as f64,
                low: 99.0 + i as f64,
                close: 100.5 + i as f64,
                volume: Some(10.0),
                ..Default::default()
            })
            .collect();
        let extended = normalize_sessions(bars.clone(), "AAPL", Interval::M60, Sessions::Extended);
        assert_eq!(extended.len(), 3);
        assert_eq!(extended[0].volume, Some(10.0));
        assert_eq!(extended[1].ts, ts + 1800);
        assert_eq!(extended[1].open, 101.0);
        assert_eq!(extended[1].close, 102.5);
        assert_eq!(extended[1].volume, Some(20.0));
        let regular = normalize_sessions(bars.clone(), "AAPL", Interval::M60, Sessions::Regular);
        assert_eq!(regular.len(), 2);
        assert_eq!(regular[0].ts, ts + 1800, "09:30 must not be dropped");
        assert_eq!(
            normalize_sessions(bars, "BTC-USD", Interval::M60, Sessions::Regular).len(),
            4
        );
    }

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
        assert_eq!(
            candles.iter().map(|c| c.ts).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }
}
