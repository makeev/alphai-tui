//! Supplemental extended-hours data. Cache by symbol and candle window;
//! failures retain the last successful session and never fail an IEX poll.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use crate::domain::{Candle, Quote, TickerData};
use crate::market::{self, Session};

pub const REFRESH: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub struct Supplement {
    pub quote: Option<Quote>,
    pub candles: Vec<Candle>,
}

#[derive(Default)]
pub struct Cache {
    entries: HashMap<String, (Instant, Supplement)>,
    yahoo_after: Option<Instant>,
}

impl Cache {
    /// Reserve a refresh before releasing the lock. Concurrent requests for
    /// the same key reuse the old value instead of starting another fetch.
    pub fn begin(&mut self, key: &str, now: Instant) -> (Supplement, bool) {
        if let Some((after, data)) = self.entries.get_mut(key) {
            let due = now >= *after;
            if due {
                *after = now + REFRESH;
            }
            return (data.clone(), due);
        }
        // Preset/ticker changes should not make a long-running terminal's
        // request cache unbounded. The on-disk startup cache is separate.
        if self.entries.len() >= 128 {
            self.entries.retain(|_, (after, _)| {
                now.saturating_duration_since(*after) < Duration::from_secs(3600)
            });
            if self.entries.len() >= 128 {
                self.entries.clear();
            }
        }
        self.entries
            .insert(key.into(), (now + REFRESH, Supplement::default()));
        (Supplement::default(), true)
    }

    pub fn finish(&mut self, key: &str, data: Supplement) {
        if let Some((_, old)) = self.entries.get_mut(key) {
            *old = data;
        }
    }

    /// One shared Yahoo gate across all tickers: a block on this IP must
    /// not produce one more failing request per watchlist row every poll.
    pub fn claim_yahoo(&mut self, now: Instant) -> bool {
        if self.yahoo_after.is_some_and(|after| now < after) {
            return false;
        }
        self.yahoo_after = Some(now + Duration::from_secs(5));
        true
    }

    pub fn yahoo_failed(&mut self, now: Instant, blocked: bool) {
        self.yahoo_after = Some(now + Duration::from_secs(if blocked { 30 * 60 } else { 120 }));
    }
}

fn session_key(c: &Candle) -> Option<i64> {
    market::window_at(c.ts)
        .filter(|w| w.session != Session::Open)
        .map(|w| w.start)
}

/// Replace a whole extended session when changing provider. Same-provider
/// refreshes merge by timestamp, so a partial/empty response does not erase
/// earlier bars. A narrower alternate never destroys better cached history.
pub fn merge_history(old: &[Candle], incoming: &[Candle]) -> Vec<Candle> {
    let group = |candles: &[Candle]| {
        let mut groups: BTreeMap<i64, Vec<Candle>> = BTreeMap::new();
        for c in candles {
            if let Some(key) = session_key(c) {
                groups.entry(key).or_default().push(*c);
            }
        }
        groups
    };
    let mut groups = group(old);
    for (key, new) in group(incoming) {
        let previous = groups.entry(key).or_default();
        if previous.is_empty() || previous[0].feed == new[0].feed {
            let mut bars: BTreeMap<i64, Candle> =
                previous.iter().chain(&new).map(|c| (c.ts, *c)).collect();
            *previous = std::mem::take(&mut bars).into_values().collect();
        } else if new.first().unwrap().ts <= previous.first().unwrap().ts
            && (new.last().unwrap().ts >= previous.last().unwrap().ts
                || (previous[0].feed == crate::domain::PriceFeed::Iex
                    && new.len() > previous.len()))
        {
            *previous = new;
        }
    }
    groups.into_values().flatten().collect()
}

pub fn merge_quote(old: &mut Option<Quote>, incoming: Option<Quote>) {
    let Some(new) = incoming else { return };
    if new.timing.extended.is_some()
        && old.as_ref().and_then(|q| q.timing.extended) <= new.timing.extended
    {
        *old = Some(new);
    }
}

pub fn apply(data: &mut TickerData, extra: &Supplement, now: chrono::DateTime<chrono::Utc>) {
    if let Some(q) = &extra.quote
        && q.extended_price_at(now).is_some()
        && (data.quote.extended_price_at(now).is_none()
            || q.timing.extended > data.quote.timing.extended)
    {
        data.quote.extended = q.extended;
        data.quote.timing.extended = q.timing.extended;
        data.quote.timing.extended_feed = q.timing.extended_feed;
        data.quote.timing.extended_reference = Some(q.extended_reference());
    }
    let ext = merge_history(&data.candles, &extra.candles);
    data.candles.retain(|c| session_key(c).is_none());
    data.candles.extend(ext);
    data.candles.sort_by_key(|c| c.ts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{PriceFeed, QuoteTiming};

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }
    fn bar(ts: i64, feed: PriceFeed, price: f64) -> Candle {
        Candle {
            ts,
            open: price,
            high: price,
            low: price,
            close: price,
            feed,
            volume: Some(10.0),
        }
    }

    #[test]
    fn sparse_iex_is_replaced_as_a_session_and_partial_refreshes_preserve_history() {
        let ts = at("2026-09-14T08:00:00Z").timestamp();
        let iex = vec![bar(ts + 4 * 3600, PriceFeed::Iex, 100.0)];
        let sip = vec![
            bar(ts, PriceFeed::DelayedSip, 99.0),
            bar(ts + 300, PriceFeed::DelayedSip, 101.0),
        ];
        let merged = merge_history(&iex, &sip);
        assert_eq!(merged.len(), 2);
        assert!(merged.iter().all(|c| c.feed == PriceFeed::DelayedSip));
        let partial = merge_history(&merged, &[bar(ts + 300, PriceFeed::DelayedSip, 102.0)]);
        assert_eq!(partial.len(), 2);
        assert_eq!(partial[1].close, 102.0);
        assert_eq!(
            partial[1].volume,
            Some(10.0),
            "refreshes must not add volume twice"
        );
        assert_eq!(merge_history(&partial, &[]).len(), 2);
        let narrower = merge_history(&partial, &[bar(ts + 300, PriceFeed::Yahoo, 103.0)]);
        assert!(narrower.iter().all(|c| c.feed == PriceFeed::DelayedSip));
    }

    #[test]
    fn mixed_quote_uses_its_own_reference_and_does_not_replace_the_headline() {
        let now = at("2026-09-14T09:00:00Z");
        let mut primary = TickerData {
            quote: Quote {
                symbol: "AAPL".into(),
                price: 100.0,
                ..Default::default()
            },
            candles: vec![],
        };
        let extra = Supplement {
            quote: Some(Quote {
                symbol: "AAPL".into(),
                price: 99.9,
                extended: Some(101.0),
                timing: QuoteTiming {
                    extended: Some(now.timestamp() - 900),
                    extended_feed: PriceFeed::DelayedSip,
                    extended_reference: Some(99.9),
                    ..Default::default()
                },
                ..Default::default()
            }),
            candles: vec![],
        };
        apply(&mut primary, &extra, now);
        assert_eq!(primary.quote.price, 100.0);
        assert_eq!(primary.quote.extended_price_at(now), Some(101.0));
        assert_eq!(primary.quote.extended_reference(), 99.9);
        assert_eq!(primary.quote.timing.extended_feed, PriceFeed::DelayedSip);
    }

    #[test]
    fn refresh_cache_and_yahoo_cooldown_are_bounded_across_symbols() {
        let now = Instant::now();
        let mut cache = Cache::default();
        assert!(cache.begin("AAPL:5d:5m", now).1);
        assert!(!cache.begin("AAPL:5d:5m", now + Duration::from_secs(5)).1);
        assert!(cache.begin("AAPL:5d:5m", now + REFRESH).1);
        assert!(cache.begin("AAPL:5d:15m", now).1);
        assert!(cache.claim_yahoo(now));
        cache.yahoo_failed(now, true);
        assert!(!cache.claim_yahoo(now + Duration::from_secs(1799)));
        assert!(cache.claim_yahoo(now + Duration::from_secs(1800)));
    }
}
