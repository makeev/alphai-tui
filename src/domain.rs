use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// History window requested from a data source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Range {
    #[value(name = "1d")]
    D1,
    #[value(name = "5d")]
    D5,
    #[value(name = "1mo")]
    Mo1,
    #[value(name = "3mo")]
    Mo3,
    #[value(name = "6mo")]
    Mo6,
    #[value(name = "1y")]
    Y1,
    #[value(name = "2y")]
    Y2,
}

/// All ranges, shortest first; `fetch_range` picks the smallest that fits.
const RANGES: [Range; 7] = [
    Range::D1,
    Range::D5,
    Range::Mo1,
    Range::Mo3,
    Range::Mo6,
    Range::Y1,
    Range::Y2,
];

impl Range {
    pub fn as_str(&self) -> &'static str {
        match self {
            Range::D1 => "1d",
            Range::D5 => "5d",
            Range::Mo1 => "1mo",
            Range::Mo3 => "3mo",
            Range::Mo6 => "6mo",
            Range::Y1 => "1y",
            Range::Y2 => "2y",
        }
    }

    /// Calendar seconds the range covers. `5d` means 5 trading days (Yahoo's
    /// semantics), which is 7 calendar days.
    pub fn secs(&self) -> i64 {
        const DAY: i64 = 86_400;
        match self {
            Range::D1 => DAY,
            Range::D5 => 7 * DAY,
            Range::Mo1 => 30 * DAY,
            Range::Mo3 => 90 * DAY,
            Range::Mo6 => 180 * DAY,
            Range::Y1 => 365 * DAY,
            Range::Y2 => 730 * DAY,
        }
    }
}

/// Candle granularity requested from a data source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Interval {
    #[value(name = "1m")]
    M1,
    #[value(name = "2m")]
    M2,
    #[value(name = "5m")]
    M5,
    #[value(name = "15m")]
    M15,
    #[value(name = "30m")]
    M30,
    #[value(name = "60m")]
    M60,
    #[value(name = "1d")]
    D1,
}

impl Interval {
    pub fn as_str(&self) -> &'static str {
        match self {
            Interval::M1 => "1m",
            Interval::M2 => "2m",
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::M30 => "30m",
            Interval::M60 => "60m",
            Interval::D1 => "1d",
        }
    }

    pub fn secs(&self) -> i64 {
        match self {
            Interval::M1 => 60,
            Interval::M2 => 120,
            Interval::M5 => 300,
            Interval::M15 => 900,
            Interval::M30 => 1_800,
            Interval::M60 => 3_600,
            Interval::D1 => 86_400,
        }
    }
}

/// The range to actually request from a data source so that the slowest
/// indicator (the SMA of `slow_bars` periods, configurable via `[chart]`)
/// has a full lookback window behind every candle of the visible `display`
/// range. Candles only exist while the market trades, so the warm-up is
/// scaled from market time to calendar time: ~6.5 trading hours per weekday
/// for intraday bars (factor 6 with margin for holidays), 5 trading days
/// per week for daily bars (factor 1.5). Picks the smallest range that
/// covers display + warm-up; the UI trims rendering back to `display` (see
/// `ui::chart`). Combos too big to warm up fully (e.g. 2y/1d) fall back to
/// the largest range and degrade to an indented SMA, exactly like before.
pub fn fetch_range(display: Range, interval: Interval, slow_bars: usize) -> Range {
    let bars = slow_bars as i64;
    let warmup = match interval {
        Interval::D1 => bars * interval.secs() * 3 / 2,
        _ => bars * interval.secs() * 6,
    };
    let need = display.secs() + warmup;
    RANGES
        .into_iter()
        .find(|r| r.secs() >= need)
        .unwrap_or(Range::Y2)
}

/// Which trading sessions a fetch should bring back candles for.
///
/// Yahoo and Alpaca support both; Finnhub synthesizes its history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sessions {
    #[default]
    Regular,
    Extended,
}

impl Sessions {
    pub fn toggled(self) -> Self {
        match self {
            Sessions::Regular => Sessions::Extended,
            Sessions::Extended => Sessions::Regular,
        }
    }
}

/// Provenance travels with a price/bar, including through caches and mixed
/// feeds. Unknown is reserved for old caches and synthetic test fixtures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceFeed {
    #[default]
    Unknown,
    Iex,
    Sip,
    DelayedSip,
    Yahoo,
    Finnhub,
    Crypto,
}

impl PriceFeed {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "",
            Self::Iex => "IEX",
            Self::Sip => "SIP",
            Self::DelayedSip => "SIP · delayed 15m",
            Self::Yahoo => "Yahoo",
            Self::Finnhub => "Finnhub",
            Self::Crypto => "Alpaca crypto",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct QuoteTiming {
    pub regular: Option<i64>,
    pub regular_feed: PriceFeed,
    /// Provider timestamp; the bar start when only an OHLC close is known.
    pub extended: Option<i64>,
    pub extended_feed: PriceFeed,
    /// The extended source's regular close: IEX and consolidated closes
    /// can differ, so a hybrid must not manufacture a percentage at the join.
    pub extended_reference: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Quote {
    pub symbol: String,
    /// The regular session's price: the last trade while the exchange was
    /// open, which is what every view has always shown.
    pub price: f64,
    pub prev_close: Option<f64>,
    pub currency: Option<String>,
    /// Last trade outside the regular session, when the source reports one.
    /// Sources differ in what they can answer here, so it stays optional
    /// everywhere and a view omits its zone when it is None.
    pub extended: Option<f64>,
    #[serde(default)]
    pub timing: QuoteTiming,
    /// 52 week range as (low, high).
    pub fifty_two_week: Option<(f64, f64)>,
    /// The regular session's own (low, high), when the source states it.
    /// Derived from candles otherwise, which stops being the same number
    /// once extended-hours candles are drawn.
    pub day_range: Option<(f64, f64)>,
    /// Regular session volume, in shares.
    pub volume: Option<f64>,
}

impl Quote {
    pub fn change(&self) -> Option<f64> {
        self.prev_close.map(|pc| self.price - pc)
    }

    pub fn change_pct(&self) -> Option<f64> {
        self.prev_close
            .filter(|pc| *pc != 0.0)
            .map(|pc| (self.price - pc) / pc * 100.0)
    }

    /// A timestamped extended trade remains valid at 0%. Older cache rows
    /// without timing keep their legacy interpretation until a fresh fetch.
    pub fn extended_price(&self) -> Option<f64> {
        self.extended_price_at(chrono::Utc::now())
    }

    pub fn extended_price_at(&self, now: chrono::DateTime<chrono::Utc>) -> Option<f64> {
        let price = self.extended.filter(|p| p.is_finite() && *p > 0.0)?;
        match self.timing.extended {
            Some(ts) => crate::market::extended_window(now)
                .or_else(|| {
                    // During SIP's 15-minute lag after the opening bell,
                    // its latest trade can still belong to today's PRE.
                    let open = crate::market::window_at(now.timestamp())?;
                    if self.timing.extended_feed != PriceFeed::DelayedSip
                        || open.session != crate::market::Session::Open
                        || now.timestamp() >= open.start + 15 * 60
                        || self.timing.regular.is_some_and(|t| t >= open.start)
                    {
                        return None;
                    }
                    crate::market::windows(crate::market::et_date(ts)?)
                        .into_iter()
                        .find(|w| w.session == crate::market::Session::Pre && w.end == open.start)
                })
                .filter(|w| w.start <= ts && ts < w.end && ts <= now.timestamp())
                .map(|_| price),
            None => (price != self.price).then_some(price),
        }
    }

    pub fn extended_reference(&self) -> f64 {
        self.timing.extended_reference.unwrap_or(self.price)
    }

    /// Extended move measured against the regular close, not the previous
    /// one: after hours a reader is asking what the stock did *since* the
    /// bell, which is the convention every broker screen follows.
    pub fn extended_change(&self) -> Option<f64> {
        self.extended_price().map(|e| e - self.extended_reference())
    }

    pub fn extended_change_pct(&self) -> Option<f64> {
        (self.extended_reference() != 0.0)
            .then(|| {
                self.extended_change()
                    .map(|c| c / self.extended_reference() * 100.0)
            })
            .flatten()
    }
}

/// One OHLCV bar; `ts` is epoch seconds.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Candle {
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: Option<f64>,
    #[serde(default)]
    pub feed: PriceFeed,
}

/// Everything the UI knows about one ticker: latest quote + recent candles.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TickerData {
    pub quote: Quote,
    pub candles: Vec<Candle>,
}

impl TickerData {
    /// Only a timestamped trade from the bar's own feed and interval may
    /// update it. A regular close must never overwrite a PRE/AH bar, or a
    /// current IEX quote rewrite a delayed consolidated bar.
    pub fn update_current_bar(&mut self, interval: Interval) {
        let Some(bar) = self.candles.last_mut() else {
            return;
        };
        let q = &self.quote;
        let extended = crate::market::window_at(bar.ts)
            .is_some_and(|w| w.session != crate::market::Session::Open)
            && crate::market::is_us_equity(&q.symbol)
            && interval != Interval::D1;
        let (price, ts, feed) = if extended {
            (q.extended, q.timing.extended, q.timing.extended_feed)
        } else {
            (Some(q.price), q.timing.regular, q.timing.regular_feed)
        };
        let (Some(price), Some(ts)) = (price, ts) else {
            return;
        };
        if feed == PriceFeed::Unknown || feed != bar.feed || !price.is_finite() {
            return;
        }
        let same_bar = if interval == Interval::D1 {
            crate::market::et_date(ts) == crate::market::et_date(bar.ts)
                && crate::market::window_at(ts)
                    .is_some_and(|w| w.session == crate::market::Session::Open)
        } else {
            ts >= bar.ts
                && ts < bar.ts + interval.secs()
                && (!crate::market::is_us_equity(&q.symbol)
                    || crate::market::window_at(ts) == crate::market::window_at(bar.ts))
        };
        if same_bar {
            bar.close = price;
            bar.high = bar.high.max(price);
            bar.low = bar.low.min(price);
        }
    }
}

pub fn fmt_price(p: f64) -> String {
    if p.abs() >= 1.0 {
        format!("{p:.2}")
    } else {
        format!("{p:.4}")
    }
}

/// Share counts get long fast, and the volume panel has a price gutter's
/// worth of room: 12.4M, 1.24B, 934K.
pub fn fmt_volume(v: f64) -> String {
    for (unit, scale) in [("B", 1e9), ("M", 1e6), ("K", 1e3)] {
        if v.abs() >= scale {
            let scaled = v / scale;
            return if scaled.abs() >= 100.0 {
                format!("{scaled:.0}{unit}")
            } else {
                format!("{scaled:.1}{unit}")
            };
        }
    }
    format!("{v:.0}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::SMA_SLOW;

    fn instant(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }

    #[test]
    fn timestamped_zero_move_is_valid_and_last_friday_is_not_monday_premarket() {
        let mut q = Quote {
            symbol: "AAPL".into(),
            price: 100.0,
            extended: Some(100.0),
            ..Default::default()
        };
        q.timing.extended = Some(instant("2026-09-11T20:41:00Z").timestamp());
        assert_eq!(
            q.extended_price_at(instant("2026-09-12T10:00:00Z")),
            Some(100.0)
        );
        assert_eq!(q.extended_price_at(instant("2026-09-14T08:05:00Z")), None);
        q.timing.extended = Some(instant("2026-09-14T08:01:00Z").timestamp());
        assert_eq!(
            q.extended_price_at(instant("2026-09-14T08:05:00Z")),
            Some(100.0)
        );
        assert_eq!(q.extended_price_at(instant("2026-09-14T13:30:00Z")), None);
        assert_eq!(q.extended_price_at(instant("2026-09-14T08:00:00Z")), None);
    }

    #[test]
    fn delayed_premarket_survives_the_bell_until_regular_data_arrives() {
        let mut q = Quote {
            symbol: "AAPL".into(),
            price: 100.0,
            extended: Some(102.0),
            timing: QuoteTiming {
                extended: Some(instant("2026-09-14T13:20:00Z").timestamp()),
                extended_feed: PriceFeed::DelayedSip,
                ..Default::default()
            },
            ..Default::default()
        };
        let now = instant("2026-09-14T13:35:00Z");
        assert_eq!(q.extended_price_at(now), Some(102.0));
        assert_eq!(q.extended_price_at(instant("2026-09-14T13:45:00Z")), None);
        q.timing.regular = Some(instant("2026-09-14T13:30:01Z").timestamp());
        assert_eq!(q.extended_price_at(now), None);
        q.timing.regular = None;
        q.timing.extended = Some(instant("2026-09-11T13:20:00Z").timestamp());
        assert_eq!(q.extended_price_at(now), None);
    }

    #[test]
    fn only_same_feed_and_same_interval_trades_update_a_bar() {
        let ts = instant("2026-09-14T08:00:00Z").timestamp();
        let mut data = TickerData {
            quote: Quote {
                symbol: "AAPL".into(),
                price: 110.0,
                extended: Some(102.0),
                timing: QuoteTiming {
                    regular: Some(ts - 3 * 86400),
                    extended: Some(ts + 30),
                    regular_feed: PriceFeed::Iex,
                    extended_feed: PriceFeed::Iex,
                    ..Default::default()
                },
                ..Default::default()
            },
            candles: vec![Candle {
                ts,
                open: 100.0,
                high: 101.0,
                low: 99.0,
                close: 100.5,
                feed: PriceFeed::DelayedSip,
                volume: Some(100.0),
            }],
        };
        data.update_current_bar(Interval::M5);
        assert_eq!(data.candles[0].close, 100.5, "IEX must not rewrite SIP");
        data.quote.timing.extended_feed = PriceFeed::DelayedSip;
        data.update_current_bar(Interval::M5);
        assert_eq!(
            data.candles[0].close, 102.0,
            "the PRE trade updates PRE, not the regular close"
        );
        data.quote.extended = Some(105.0);
        data.quote.timing.extended = Some(ts + 301);
        data.update_current_bar(Interval::M5);
        assert_eq!(
            data.candles[0].close, 102.0,
            "a newer bucket must not revise the old one"
        );
    }

    /// Short enough for the price gutter at every magnitude a share count
    /// reaches, and never wider than a price label.
    #[test]
    fn fmt_volume_scales_by_magnitude() {
        assert_eq!(fmt_volume(0.0), "0");
        assert_eq!(fmt_volume(934.0), "934");
        assert_eq!(fmt_volume(12_400.0), "12.4K");
        assert_eq!(fmt_volume(934_000.0), "934K");
        assert_eq!(fmt_volume(12_400_000.0), "12.4M");
        assert_eq!(fmt_volume(1_240_000_000.0), "1.2B");
        assert!(fmt_volume(999_999_999_999.0).chars().count() <= 5);
    }

    /// Every `t`-cycle preset must fetch enough history for a full SMA100
    /// warm-up behind its visible window.
    #[test]
    fn fetch_range_covers_presets() {
        assert_eq!(fetch_range(Range::D1, Interval::M5, SMA_SLOW), Range::D5);
        assert_eq!(fetch_range(Range::D5, Interval::M15, SMA_SLOW), Range::Mo1);
        assert_eq!(fetch_range(Range::Mo1, Interval::M60, SMA_SLOW), Range::Mo3);
        assert_eq!(fetch_range(Range::Mo6, Interval::D1, SMA_SLOW), Range::Y1);
        assert_eq!(fetch_range(Range::Y1, Interval::D1, SMA_SLOW), Range::Y2);
    }

    #[test]
    fn fetch_range_odd_combos() {
        // CLI-only combos still get the smallest range that fits.
        assert_eq!(fetch_range(Range::D1, Interval::M1, SMA_SLOW), Range::D5);
        assert_eq!(fetch_range(Range::Mo3, Interval::M5, SMA_SLOW), Range::Mo6);
        // Nothing bigger than 2y exists: degrade gracefully.
        assert_eq!(fetch_range(Range::Y2, Interval::D1, SMA_SLOW), Range::Y2);
    }

    /// A configured slow period scales the warm-up: a slower SMA widens the
    /// over-fetch for the same visible window.
    #[test]
    fn fetch_range_scales_with_the_slow_period() {
        assert_eq!(fetch_range(Range::D1, Interval::M5, 400), Range::Mo1);
        assert_eq!(fetch_range(Range::Mo6, Interval::D1, 200), Range::Y2);
    }

    #[test]
    fn range_secs_ascending() {
        assert!(RANGES.windows(2).all(|w| w[0].secs() < w[1].secs()));
    }
}
