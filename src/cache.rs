//! Last-good quotes on disk, so a throttled or blocked source does not
//! leave the app staring at an empty screen.
//!
//! Yahoo, the keyless default, rate limits by IP: measured 2026-09-10, a
//! block arrived after about ten requests and held for 19 minutes, and the
//! second one that day held for over an hour. Nothing in the client can
//! shorten that, so the answer is to have something to show meanwhile. The
//! rail labels what came from here, and the first real poll replaces it.
//!
//! Deliberately not a request cache: entries are only ever read at startup,
//! never to answer a fetch, so this can never stand between the user and a
//! live price.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::domain::{Interval, Range, Sessions, TickerData};

/// Entries older than this are dropped on load. A week-old session is still
/// honest history when it is labelled, but past that the chart under it is
/// telling a story about a different market.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

/// How often the store is allowed to hit the disk. The poller writes after
/// a cycle, and cycles can be two seconds apart; the file only has to be
/// good enough for the next start.
const WRITE_EVERY: Duration = Duration::from_secs(60);

/// Bumped when the entry shape changes; a file from another version is
/// dropped rather than migrated.
const VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// Unix seconds of the fetch behind this entry.
    pub fetched: i64,
    /// The window this was fetched for (`params_key`). An entry taken with
    /// other parameters is not comparable data, so it is never seeded.
    pub params: String,
    pub data: TickerData,
}

#[derive(Default, Deserialize, Serialize)]
struct File {
    version: u32,
    entries: HashMap<String, Entry>,
}

/// The store the poller owns: entries in memory, written back on a timer.
pub struct Store {
    path: Option<PathBuf>,
    entries: HashMap<String, Entry>,
    dirty: bool,
    last_write: Option<Instant>,
}

impl Store {
    /// Loads the cache file, or an empty store when there is none, when it
    /// is unreadable, or when it was written by another version. A cache is
    /// never worth an error message: every caller can work without it.
    pub fn load() -> Self {
        Self::load_at(default_path())
    }

    pub fn load_at(path: Option<PathBuf>) -> Self {
        let entries = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|raw| serde_json::from_str::<File>(&raw).ok())
            .filter(|file| file.version == VERSION)
            .map(|file| file.entries)
            .unwrap_or_default();
        let now = unix_now();
        let max_age = MAX_AGE.as_secs() as i64;
        let entries = entries
            .into_iter()
            .filter(|(_, e)| now - e.fetched <= max_age)
            .collect();
        Self {
            path,
            entries,
            dirty: false,
            last_write: None,
        }
    }

    /// The cached ticker for this source and window, if there is one. The
    /// window has to match: a 1d/5m series seeded into a 1y/1d chart would
    /// be a different picture drawn on the same axes.
    pub fn get(&self, source: &str, symbol: &str, params: &str) -> Option<&Entry> {
        self.entries
            .get(&key(source, symbol))
            .filter(|e| e.params == params)
    }

    /// Records a successful fetch. Nothing is written here: `flush_due`
    /// owns the disk, so a two second poll interval does not mean two
    /// second writes.
    pub fn put(&mut self, source: &str, symbol: &str, params: &str, data: &TickerData) {
        self.entries.insert(
            key(source, symbol),
            Entry {
                fetched: unix_now(),
                params: params.to_string(),
                data: data.clone(),
            },
        );
        self.dirty = true;
    }

    /// Writes the first successful cycle immediately, then at most once
    /// per write interval. Returns whether it wrote; failures are
    /// swallowed, since a cache that cannot be written is not an error the
    /// user can act on.
    pub fn flush_due(&mut self) -> bool {
        if !self.dirty || self.last_write.is_some_and(|at| at.elapsed() < WRITE_EVERY) {
            return false;
        }
        self.flush()
    }

    /// Writes the store back regardless of the timer.
    pub fn flush(&mut self) -> bool {
        // Failed writes get the same backoff as successful ones.
        self.last_write = Some(Instant::now());
        let Some(path) = self.path.clone() else {
            return false;
        };
        let file = File {
            version: VERSION,
            entries: self.entries.clone(),
        };
        let Ok(raw) = serde_json::to_string(&file) else {
            return false;
        };
        if let Some(dir) = path.parent()
            && std::fs::create_dir_all(dir).is_err()
        {
            return false;
        }
        // Through a temp file: a half-written cache would be dropped on the
        // next load, which is exactly the start where it was wanted.
        let tmp = path.with_extension("tmp");
        let written = std::fs::write(&tmp, raw).is_ok() && std::fs::rename(&tmp, &path).is_ok();
        if written {
            self.dirty = false;
        }
        written
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Runtime shutdown drops the poller even while it is sleeping or
        // fetching. Preserve the final updates of short sessions too.
        if self.dirty {
            self.flush();
        }
    }
}

/// `<cache dir>/alphai-tui/quotes.json`, or None on a platform with no
/// cache directory (the store then works in memory and writes nothing).
fn default_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("alphai-tui").join("quotes.json"))
}

fn key(source: &str, symbol: &str) -> String {
    format!("{source}/{symbol}")
}

/// The fetch window as one comparable string. Kept as text rather than the
/// enums so the file survives a rename or a new variant: an entry it can no
/// longer match is simply not seeded.
pub fn params_key(range: Range, interval: Interval, sessions: Sessions) -> String {
    let sessions = match sessions {
        Sessions::Regular => "regular",
        Sessions::Extended => "extended",
    };
    format!("{}/{}/{sessions}", range.as_str(), interval.as_str())
}

/// Compact age of a cached entry, for the rail's badge: "4m", "2h", "3d".
pub fn age_label(fetched: i64) -> String {
    let mins = (unix_now() - fetched).max(0) / 60;
    match mins {
        0..=59 => format!("{mins}m"),
        60..=1439 => format!("{}h", mins / 60),
        _ => format!("{}d", mins / 1440),
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Candle, Quote};

    fn data(price: f64) -> TickerData {
        TickerData {
            quote: Quote {
                symbol: "AAPL".into(),
                price,
                prev_close: Some(1.0),
                currency: Some("USD".into()),
                extended: None,
                fifty_two_week: None,
                day_range: None,
                volume: None,
            },
            candles: vec![Candle {
                ts: 1_700_000_000,
                open: 1.0,
                high: 2.0,
                low: 0.5,
                close: price,
                volume: None,
            }],
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("alphai-tui-test-{name}-{}", std::process::id()))
            .join("quotes.json")
    }

    #[test]
    fn a_written_store_comes_back_on_the_next_load() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let params = params_key(Range::D1, Interval::M5, Sessions::Regular);

        let mut store = Store::load_at(Some(path.clone()));
        assert!(store.get("yahoo", "AAPL", &params).is_none());
        store.put("yahoo", "AAPL", &params, &data(10.0));
        assert!(store.flush(), "write failed");

        let store = Store::load_at(Some(path.clone()));
        let entry = store.get("yahoo", "AAPL", &params).expect("entry");
        assert_eq!(entry.data.quote.price, 10.0);
        assert_eq!(entry.data.candles.len(), 1);
        // Another source, or another window, is not this entry.
        assert!(store.get("alpaca", "AAPL", &params).is_none());
        let other = params_key(Range::Y1, Interval::D1, Sessions::Regular);
        assert!(store.get("yahoo", "AAPL", &other).is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn stale_entries_are_dropped_on_load() {
        let path = tmp_path("stale");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let params = params_key(Range::D1, Interval::M5, Sessions::Regular);
        let mut store = Store::load_at(Some(path.clone()));
        store.put("yahoo", "AAPL", &params, &data(10.0));
        // Backdate it past the horizon, the way a week off would.
        let entry = store.entries.get_mut(&key("yahoo", "AAPL")).unwrap();
        entry.fetched -= MAX_AGE.as_secs() as i64 + 60;
        assert!(store.flush());

        let store = Store::load_at(Some(path.clone()));
        assert!(store.get("yahoo", "AAPL", &params).is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_short_session_keeps_its_last_successful_fetch() {
        let path = tmp_path("short-session");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let params = params_key(Range::D1, Interval::M5, Sessions::Regular);
        {
            let mut store = Store::load_at(Some(path.clone()));
            store.put("yahoo", "AAPL", &params, &data(10.0));
            assert!(store.flush_due(), "the first cycle should reach disk");
            store.put("yahoo", "AAPL", &params, &data(11.0));
            assert!(!store.flush_due(), "subsequent cycles should be throttled");
        }
        let store = Store::load_at(Some(path.clone()));
        assert_eq!(
            store
                .get("yahoo", "AAPL", &params)
                .unwrap()
                .data
                .quote
                .price,
            11.0
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A file from another version, or a truncated one, must not stop the
    /// app: it is a cache, and an empty store is a correct answer.
    #[test]
    fn an_unreadable_file_loads_as_an_empty_store() {
        let path = tmp_path("broken");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"version\": 99, \"entries\": {}}").unwrap();
        let params = params_key(Range::D1, Interval::M5, Sessions::Regular);
        assert!(
            Store::load_at(Some(path.clone()))
                .get("yahoo", "AAPL", &params)
                .is_none()
        );
        std::fs::write(&path, "{ not json").unwrap();
        assert!(
            Store::load_at(Some(path.clone()))
                .get("yahoo", "AAPL", &params)
                .is_none()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The disk is touched on a timer, not on every poll.
    #[test]
    fn flush_due_waits_for_its_interval() {
        let path = tmp_path("timer");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let params = params_key(Range::D1, Interval::M5, Sessions::Regular);
        let mut store = Store::load_at(Some(path.clone()));
        store.put("yahoo", "AAPL", &params, &data(10.0));
        assert!(store.flush_due(), "did not write the first cycle");
        store.put("yahoo", "AAPL", &params, &data(11.0));
        assert!(!store.flush_due(), "wrote inside the interval");
        store.last_write = Some(Instant::now() - WRITE_EVERY - Duration::from_secs(1));
        assert!(store.flush_due(), "did not write after the interval");
        assert!(!store.flush_due(), "wrote again with nothing new");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn age_label_reads_in_the_units_a_reader_thinks_in() {
        let now = unix_now();
        assert_eq!(age_label(now), "0m");
        assert_eq!(age_label(now - 300), "5m");
        assert_eq!(age_label(now - 7_200), "2h");
        assert_eq!(age_label(now - 3 * 86_400), "3d");
        // A clock that moved backwards is not a negative age.
        assert_eq!(age_label(now + 600), "0m");
    }

    #[test]
    fn params_key_separates_the_windows_it_has_to() {
        let d1 = params_key(Range::D1, Interval::M5, Sessions::Regular);
        assert_eq!(d1, "1d/5m/regular");
        assert_ne!(d1, params_key(Range::D1, Interval::M15, Sessions::Regular));
        assert_ne!(d1, params_key(Range::D5, Interval::M5, Sessions::Regular));
        assert_ne!(d1, params_key(Range::D1, Interval::M5, Sessions::Extended));
    }
}
