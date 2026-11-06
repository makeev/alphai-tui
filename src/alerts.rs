//! Session-only price crossings. No I/O: accepted quotes drive rules;
//! the runtime consumes the notification clock and the bell effect.
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::domain::{PriceFeed, Quote};

const BANNER_TIME: Duration = Duration::from_secs(12);
const BELL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

#[derive(Clone, Debug)]
pub struct Level {
    pub direction: Direction,
    pub price: f64,
    pub label: String,
}

impl Level {
    pub fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        let (direction, rest) = if let Some(rest) = input.strip_prefix('>') {
            (Direction::Up, rest)
        } else if let Some(rest) = input.strip_prefix('<') {
            (Direction::Down, rest)
        } else {
            return Err("Use >= 150 or <= 140".into());
        };
        let value = rest.strip_prefix('=').unwrap_or(rest).trim();
        let mut parts = value.split('.');
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next();
        if whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || fraction.is_some_and(|f| f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()))
            || parts.next().is_some()
            || input.len() > 32
        {
            return Err("Use a positive decimal price, e.g. >= 150.25".into());
        }
        let price = value.parse::<f64>().unwrap_or(f64::NAN);
        if !price.is_finite() || price <= 0.0 {
            return Err("Price must be finite and greater than zero".into());
        }
        let whole = whole.trim_start_matches('0');
        let whole = if whole.is_empty() { "0" } else { whole };
        let amount = fraction.map_or_else(|| whole.to_string(), |f| format!("{whole}.{f}"));
        let operator = match direction {
            Direction::Up => ">=",
            Direction::Down => "<=",
        };
        let label = format!("{operator} {amount}");
        // The saved expression is also the next prompt's input: an alias
        // must not normalize into a line that the prompt cannot rearm.
        if label.len() > 32 {
            return Err("Price is too long".into());
        }
        Ok(Self {
            direction,
            price,
            label,
        })
    }

    pub fn reached(&self, price: f64) -> bool {
        match self.direction {
            Direction::Up => price >= self.price,
            Direction::Down => price <= self.price,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub price: f64,
    pub timestamp: i64,
    pub feed: PriceFeed,
    pub currency: Option<String>,
}

/// Use the newest timestamped price, never the chart's historical OHLC.
/// A missing regular timestamp is normal for an Alpaca overnight close.
pub fn quote_sample(quote: &Quote, now: DateTime<Utc>) -> Option<Sample> {
    let candidate = |price: f64, time: Option<i64>, feed| {
        let timestamp = time.filter(|t| *t > 0 && *t <= now.timestamp())?;
        (price.is_finite() && price > 0.0).then(|| Sample {
            price,
            timestamp,
            feed,
            currency: quote
                .currency
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_ascii_uppercase),
        })
    };
    let regular = candidate(quote.price, quote.timing.regular, quote.timing.regular_feed);
    let extended = quote
        .extended_price_at(now)
        .and_then(|price| candidate(price, quote.timing.extended, quote.timing.extended_feed));
    match (regular, extended) {
        (Some(r), Some(e)) => Some(if e.timestamp > r.timestamp { e } else { r }),
        (r, e) => r.or(e),
    }
}

#[derive(Clone, Debug)]
pub struct Trigger {
    pub sample: Sample,
    pub detected_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub level: Level,
    pub currency: Option<String>,
    pub previous: Option<Sample>,
    pub fired: Option<Trigger>,
    pub currency_paused: bool,
    latest_timestamp: Option<i64>,
}

impl Rule {
    pub fn new(level: Level, previous: Option<Sample>) -> Self {
        Self {
            currency: previous.as_ref().and_then(|s| s.currency.clone()),
            latest_timestamp: previous.as_ref().map(|s| s.timestamp),
            level,
            previous,
            fired: None,
            currency_paused: false,
        }
    }

    fn observe(&mut self, sample: Sample, detected_at: DateTime<Utc>) -> Option<Trigger> {
        if self.fired.is_some() || self.latest_timestamp.is_some_and(|t| sample.timestamp < t) {
            return None;
        }
        self.latest_timestamp = Some(sample.timestamp);
        if matches!((&self.currency, &sample.currency), (Some(a), Some(b)) if a != b) {
            self.currency_paused = true;
            self.previous = None;
            return None;
        }
        if self.currency.is_none() {
            self.currency = sample.currency.clone();
        }
        // Returning from a currency mismatch establishes a new baseline.
        // Missing currency is compatible with a known one (e.g. Finnhub).
        self.currency_paused = false;
        let crossed = self.previous.as_ref().is_some_and(|previous| {
            !self.level.reached(previous.price) && self.level.reached(sample.price)
        });
        self.previous = Some(sample.clone());
        if crossed {
            let trigger = Trigger {
                sample,
                detected_at,
            };
            self.fired = Some(trigger.clone());
            Some(trigger)
        } else {
            None
        }
    }

    pub fn marker(&self) -> char {
        if self.fired.is_some() {
            '!'
        } else if self.currency_paused || self.previous.is_none() {
            '?'
        } else {
            match self.level.direction {
                Direction::Up => '↑',
                Direction::Down => '↓',
            }
        }
    }

    pub fn status(&self) -> &'static str {
        if self.fired.is_some() {
            "Fired; Enter arms again"
        } else if self.currency_paused {
            "Paused: currency changed; Enter accepts it"
        } else if let Some(previous) = &self.previous {
            if self.level.reached(previous.price) {
                match self.level.direction {
                    Direction::Up => "Waiting for a move below, then back to the level",
                    Direction::Down => "Waiting for a move above, then back to the level",
                }
            } else {
                "Armed; waiting for the crossing"
            }
        } else {
            "Waiting for a timestamped quote"
        }
    }
}

#[derive(Clone, Debug)]
pub struct Notification {
    pub symbol: String,
    pub level: String,
    pub trigger: Trigger,
    needs_bell: bool,
}

#[derive(Default)]
pub struct Alerts {
    rules: HashMap<String, Rule>,
    queue: VecDeque<Notification>,
    shown_since: Option<Instant>,
    last_bell: Option<Instant>,
}

impl Alerts {
    pub fn get(&self, symbol: &str) -> Option<&Rule> {
        self.rules.get(symbol)
    }

    pub fn arm(&mut self, symbol: String, level: Level, previous: Option<Sample>) {
        self.remove(&symbol);
        self.rules.insert(symbol, Rule::new(level, previous));
    }

    pub fn remove(&mut self, symbol: &str) {
        self.rules.remove(symbol);
        if self.queue.front().is_some_and(|n| n.symbol == symbol) {
            self.shown_since = None;
        }
        self.queue.retain(|n| n.symbol != symbol);
    }

    pub fn retain_symbols(&mut self, symbols: &[String]) {
        self.rules.retain(|s, _| symbols.contains(s));
        if self
            .queue
            .front()
            .is_some_and(|n| !symbols.contains(&n.symbol))
        {
            self.shown_since = None;
        }
        self.queue.retain(|n| symbols.contains(&n.symbol));
    }

    pub fn observe(&mut self, symbol: &str, sample: Sample, now: DateTime<Utc>) {
        let Some(rule) = self.rules.get_mut(symbol) else {
            return;
        };
        if let Some(trigger) = rule.observe(sample, now) {
            self.queue.push_back(Notification {
                symbol: symbol.into(),
                level: rule.level.label.clone(),
                trigger,
                needs_bell: true,
            });
        }
    }

    /// Called immediately before a frame. Returns an effect, never writes
    /// to the terminal; closely spaced events share a single bell.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self
            .shown_since
            .is_some_and(|at| now.saturating_duration_since(at) >= BANNER_TIME)
        {
            self.queue.pop_front();
            self.shown_since = None;
        }
        if !self.queue.is_empty() && self.shown_since.is_none() {
            self.shown_since = Some(now);
        }
        let pending = self.queue.iter().any(|n| n.needs_bell);
        for notice in &mut self.queue {
            notice.needs_bell = false;
        }
        let bell = pending
            && self
                .last_bell
                .is_none_or(|at| now.saturating_duration_since(at) >= BELL_INTERVAL);
        if bell {
            self.last_bell = Some(now);
        }
        bell
    }

    pub fn notification(&self) -> Option<&Notification> {
        self.queue.front()
    }

    pub fn waiting_count(&self) -> usize {
        self.queue.len().saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::QuoteTiming;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 6, 21, 10, 0).unwrap()
    }
    fn sample(price: f64, ts: i64) -> Sample {
        Sample {
            price,
            timestamp: ts,
            feed: PriceFeed::Iex,
            currency: Some("USD".into()),
        }
    }

    #[test]
    fn grammar_keeps_precision_and_normalizes_aliases() {
        assert_eq!(
            Level::parse(" > 000.0000100 ").unwrap().label,
            ">= 0.0000100"
        );
        assert_eq!(Level::parse("<= 0140.50").unwrap().label, "<= 140.50");
        let long = format!(">{}", "1".repeat(31));
        assert!(Level::parse(&long).is_err());
        let limit = Level::parse(&format!(">{}", "1".repeat(29))).unwrap();
        assert_eq!(Level::parse(&limit.label).unwrap().label, limit.label);
        for bad in [
            "150", "=150", ">=NaN", ">=inf", ">=0", "<-1", ">1e2", ">1,200", ">1.2.3", ">.5", ">1.",
        ] {
            assert!(Level::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn crossings_are_inclusive_one_shot_and_require_a_baseline() {
        for (level, before, at, beyond) in [(">150", 149., 150., 151.), ("<150", 151., 150., 149.)]
        {
            let mut r = Rule::new(Level::parse(level).unwrap(), None);
            assert!(r.observe(sample(before, 1), now()).is_none());
            assert!(r.observe(sample(at, 2), now()).is_some());
            assert!(r.observe(sample(beyond, 3), now()).is_none());
            assert_eq!(r.fired.as_ref().unwrap().sample.price, at);
        }
        let mut r = Rule::new(Level::parse(">150").unwrap(), Some(sample(151., 1)));
        assert!(r.observe(sample(152., 2), now()).is_none());
        assert!(r.observe(sample(149., 3), now()).is_none());
        assert!(r.observe(sample(150., 4), now()).is_some());
    }

    #[test]
    fn timestamp_order_survives_feed_changes_and_open_bar_updates() {
        let mut r = Rule::new(Level::parse(">150").unwrap(), Some(sample(149., 10)));
        assert!(r.observe(sample(160., 9), now()).is_none());
        assert!(r.observe(sample(149., 10), now()).is_none());
        let mut next = sample(150., 10);
        next.feed = PriceFeed::DelayedSip;
        assert!(r.observe(next, now()).is_some());
    }

    #[test]
    fn missing_currency_is_compatible_but_different_known_currency_pauses() {
        let mut r = Rule::new(Level::parse(">150").unwrap(), Some(sample(149., 1)));
        let mut next = sample(151., 2);
        next.currency = None;
        next.feed = PriceFeed::Finnhub;
        assert!(r.observe(next, now()).is_some());
        let mut r = Rule::new(Level::parse(">150").unwrap(), Some(sample(149., 1)));
        let mut next = sample(151., 2);
        next.currency = Some("EUR".into());
        assert!(r.observe(next, now()).is_none());
        assert!(r.currency_paused);
        assert!(r.observe(sample(151., 3), now()).is_none());
        assert!(!r.currency_paused);
        assert!(r.observe(sample(149., 4), now()).is_none());
        assert!(r.observe(sample(150., 5), now()).is_some());
    }

    #[test]
    fn samples_use_timestamped_prices_with_source_specific_gaps() {
        let at = now(); // US after hours; today's close precedes AH.
        let mut q = Quote {
            price: 149.,
            extended: Some(151.),
            timing: QuoteTiming {
                regular: Some(at.timestamp() - 600),
                regular_feed: PriceFeed::Yahoo,
                extended: Some(at.timestamp() - 1),
                extended_feed: PriceFeed::Yahoo,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(quote_sample(&q, at).unwrap().price, 151.);
        q.timing.regular = None; // Alpaca: regular close has no trade time.
        assert_eq!(quote_sample(&q, at).unwrap().price, 151.);
        q.extended = None; // Tiingo before open, without daily history.
        assert!(quote_sample(&q, at).is_none());
        q.timing.regular = Some(at.timestamp());
        q.timing.regular_feed = PriceFeed::Finnhub;
        assert_eq!(quote_sample(&q, at).unwrap().currency, None);
        for price in [0., -1., f64::NAN, f64::INFINITY] {
            q.price = price;
            assert!(quote_sample(&q, at).is_none());
        }
        q.price = 149.;
        q.timing.regular = Some(at.timestamp() + 1);
        assert!(quote_sample(&q, at).is_none());
    }

    #[test]
    fn queue_coalesces_bells_times_each_banner_and_prunes_rearmed_rules() {
        let mut alerts = Alerts::default();
        for s in ["AAPL", "MSFT"] {
            alerts.arm(
                s.into(),
                Level::parse(">150").unwrap(),
                Some(sample(149., 1)),
            );
            alerts.observe(s, sample(150., 2), now());
        }
        let clock = Instant::now();
        assert!(alerts.tick(clock));
        assert!(!alerts.tick(clock + Duration::from_secs(1)));
        assert_eq!(alerts.notification().unwrap().symbol, "AAPL");
        assert!(!alerts.tick(clock + BANNER_TIME));
        assert_eq!(alerts.notification().unwrap().symbol, "MSFT");
        alerts.arm(
            "MSFT".into(),
            Level::parse(">160").unwrap(),
            Some(sample(150., 2)),
        );
        assert!(alerts.notification().is_none());
        assert!(alerts.get("AAPL").unwrap().fired.is_some());
        alerts.retain_symbols(&["MSFT".into()]);
        assert!(alerts.get("AAPL").is_none());
    }

    #[test]
    fn samples_cover_premarket_night_crypto_and_equal_timestamps() {
        let pre = Utc.with_ymd_and_hms(2026, 11, 6, 13, 0, 0).unwrap();
        let mut q = Quote {
            symbol: "AAPL".into(),
            price: 149.,
            extended: Some(151.),
            timing: QuoteTiming {
                regular: Some(pre.timestamp() - 16 * 3600),
                regular_feed: PriceFeed::Yahoo,
                extended: Some(pre.timestamp() - 60),
                extended_feed: PriceFeed::Yahoo,
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(quote_sample(&q, pre).unwrap().price, 151.);
        // A stale extended price must not outrank a newer regular print.
        q.timing.regular = Some(pre.timestamp());
        assert_eq!(quote_sample(&q, pre).unwrap().price, 149.);
        q.timing.extended = q.timing.regular;
        assert_eq!(quote_sample(&q, pre).unwrap().price, 149.);
        let night = Utc.with_ymd_and_hms(2026, 11, 7, 2, 0, 0).unwrap();
        q.timing.regular = None;
        // Last AH print remains valid overnight; a PRE from this morning does not.
        assert!(quote_sample(&q, night).is_none());
        q.timing.extended = Some(now().timestamp());
        assert_eq!(quote_sample(&q, night).unwrap().price, 151.);
        let monday_pre = Utc.with_ymd_and_hms(2026, 11, 9, 13, 0, 0).unwrap();
        assert!(quote_sample(&q, monday_pre).is_none());
        q.symbol = "BTC-USD".into();
        q.extended = None;
        q.timing.regular = Some(night.timestamp());
        q.timing.regular_feed = PriceFeed::Crypto;
        assert_eq!(quote_sample(&q, night).unwrap().feed, PriceFeed::Crypto);
    }

    #[test]
    fn banner_timer_starts_on_tick_and_a_suppressed_bell_never_replays() {
        let mut alerts = Alerts::default();
        let clock = Instant::now();
        for s in ["AAPL", "MSFT", "NVDA"] {
            alerts.arm(
                s.into(),
                Level::parse(">150").unwrap(),
                Some(sample(149., 1)),
            );
        }
        alerts.observe("AAPL", sample(150., 2), now());
        assert!(alerts.tick(clock));
        alerts.observe("MSFT", sample(150., 2), now());
        assert!(!alerts.tick(clock + Duration::from_millis(30)));
        assert!(
            !alerts.tick(clock + Duration::from_secs(1)),
            "suppressed sound replayed"
        );
        alerts.observe("NVDA", sample(150., 2), now());
        assert!(alerts.tick(clock + Duration::from_secs(2)));
        assert_eq!(alerts.waiting_count(), 2);
        // Even after a long idle interval, the next banner gets its full time.
        assert!(!alerts.tick(clock + Duration::from_secs(60)));
        assert_eq!(alerts.notification().unwrap().symbol, "MSFT");
        alerts.tick(clock + Duration::from_secs(71));
        assert_eq!(alerts.notification().unwrap().symbol, "MSFT");
        alerts.tick(clock + Duration::from_secs(72));
        assert_eq!(alerts.notification().unwrap().symbol, "NVDA");
        alerts.tick(clock + Duration::from_secs(84));
        assert!(alerts.notification().is_none());
        assert!(alerts.get("MSFT").unwrap().fired.is_some());
    }
}
