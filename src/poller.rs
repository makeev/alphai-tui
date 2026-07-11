use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use crate::alphai;
use crate::domain::{Interval, Range, TickerData, fetch_range};
use crate::source::DataSource;

pub enum SourceEvent {
    Data { symbol: String, data: TickerData },
    Error { symbol: String, error: String },
    /// News / insider / sentiment results from the AlphaAI task.
    Alphai(alphai::Event),
}

/// The active price source, swappable at runtime from the settings screen.
/// The poller re-reads it at the top of every cycle.
pub type SharedSource = Arc<RwLock<Arc<dyn DataSource>>>;

/// The history window and granularity, swappable at runtime with the [ and ]
/// keys. Same pattern as `SharedSource`: re-read at the top of every cycle.
pub type SharedParams = Arc<RwLock<(Range, Interval)>>;

/// Polls every symbol concurrently, then sleeps until the next cycle or a
/// manual refresh. Streaming sources will bypass this and push straight into
/// the same channel. `slow_bars` is the slowest indicator period, sizing
/// the warm-up over-fetch.
pub async fn run(
    source: SharedSource,
    symbols: Vec<String>,
    params: SharedParams,
    every: Duration,
    slow_bars: usize,
    tx: UnboundedSender<SourceEvent>,
    refresh: Arc<Notify>,
) {
    loop {
        let current = source.read().unwrap().clone();
        let (range, interval) = *params.read().unwrap();
        // Fetch wider than the visible range so indicators have their warm-up
        // history; the chart trims rendering back to `range`.
        let range = fetch_range(range, interval, slow_bars);
        let mut set = JoinSet::new();
        for symbol in &symbols {
            let source = current.clone();
            let symbol = symbol.clone();
            set.spawn(async move {
                let res = source.fetch(&symbol, range, interval).await;
                (symbol, res)
            });
        }
        while let Some(joined) = set.join_next().await {
            let Ok((symbol, res)) = joined else { continue };
            let event = match res {
                Ok(data) => SourceEvent::Data { symbol, data },
                Err(e) => SourceEvent::Error {
                    symbol,
                    error: format!("{e:#}"),
                },
            };
            if tx.send(event).is_err() {
                return; // UI is gone
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(every) => {}
            _ = refresh.notified() => {}
        }
    }
}
