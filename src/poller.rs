use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use crate::alphai;
use crate::domain::{Interval, Range, TickerData};
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

/// Polls every symbol concurrently, then sleeps until the next cycle or a
/// manual refresh. Streaming sources will bypass this and push straight into
/// the same channel.
pub async fn run(
    source: SharedSource,
    symbols: Vec<String>,
    range: Range,
    interval: Interval,
    every: Duration,
    tx: UnboundedSender<SourceEvent>,
    refresh: Arc<Notify>,
) {
    loop {
        let current = source.read().unwrap().clone();
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
