use std::sync::{Arc, RwLock};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinSet;

use crate::alphai;
use crate::cache;
use crate::domain::{Interval, Range, Sessions, TickerData, fetch_range};
use crate::source::DataSource;

pub enum SourceEvent {
    Data {
        source: Arc<dyn DataSource>,
        symbol: String,
        data: TickerData,
    },
    Error {
        source: Arc<dyn DataSource>,
        symbol: String,
        error: String,
    },
    /// News / insider / sentiment results from the AlphaAI task.
    Alphai(alphai::Event),
}

/// The active price source, swappable at runtime from the settings screen.
/// The poller re-reads it at the top of every cycle.
pub type SharedSource = Arc<RwLock<Arc<dyn DataSource>>>;

/// The history window, granularity and which sessions to draw, all
/// swappable at runtime with the preset and extended-hours keys. Same pattern as `SharedSource`: re-read at the top of every cycle.
pub type SharedParams = Arc<RwLock<(Range, Interval, Sessions)>>;

/// The poll interval, editable at runtime from the settings screen. Same
/// pattern as `SharedSource`: re-read before every sleep.
pub type SharedEvery = Arc<RwLock<Duration>>;

/// The watchlist, editable at runtime with the add and remove keys. Same
/// pattern again: re-read at the top of every cycle, so a ticker added
/// mid-session is polled from the next tick without restarting the task.
pub type SharedSymbols = Arc<RwLock<Vec<String>>>;

/// Everything the poll loop reads: the shared state the UI edits live, the
/// channel it reports on, and the on-disk cache it keeps current. One
/// struct rather than eight arguments, like `AppInit`.
pub struct Poller {
    pub source: SharedSource,
    pub symbols: SharedSymbols,
    pub params: SharedParams,
    pub every: SharedEvery,
    /// The slowest indicator period, sizing the warm-up over-fetch.
    pub slow_bars: usize,
    pub tx: UnboundedSender<SourceEvent>,
    pub refresh: Arc<Notify>,
    pub cache: cache::Store,
}

/// Polls every symbol concurrently, then sleeps until the next cycle or a
/// manual refresh. Streaming sources will bypass this and push straight into
/// the same channel. `slow_bars` is the slowest indicator period, sizing
/// the warm-up over-fetch.
pub async fn run(poller: Poller) {
    let Poller {
        source,
        symbols,
        params,
        every,
        slow_bars,
        tx,
        refresh,
        mut cache,
    } = poller;
    loop {
        let current = source.read().unwrap().clone();
        let symbols = symbols.read().unwrap().clone();
        let (range, interval, sessions) = *params.read().unwrap();
        // Fetch wider than the visible range so indicators have their warm-up
        // history; the chart trims rendering back to `range`.
        let range = fetch_range(range, interval, slow_bars);
        // What the next start would have to match to reuse these rows.
        let window = cache::params_key(range, interval, sessions);
        let source_name = current.name();
        let mut set = JoinSet::new();
        for symbol in &symbols {
            let source = current.clone();
            let symbol = symbol.clone();
            set.spawn(async move {
                let res = source.fetch(&symbol, range, interval, sessions).await;
                (symbol, res)
            });
        }
        while let Some(joined) = set.join_next().await {
            let Ok((symbol, res)) = joined else { continue };
            let event = match res {
                Ok(data) => {
                    // Kept for the next start, not for this session: the
                    // store is never read to answer a fetch.
                    cache.put(source_name, &symbol, &window, &data);
                    SourceEvent::Data {
                        source: current.clone(),
                        symbol,
                        data,
                    }
                }
                Err(e) => SourceEvent::Error {
                    source: current.clone(),
                    symbol,
                    error: format!("{e:#}"),
                },
            };
            if tx.send(event).is_err() {
                return; // UI is gone
            }
        }
        // Once a cycle at most, and the store itself holds the disk down
        // to one write a minute.
        cache.flush_due();
        let sleep = *every.read().unwrap();
        tokio::select! {
            _ = tokio::time::sleep(sleep) => {}
            _ = refresh.notified() => {}
        }
    }
}
