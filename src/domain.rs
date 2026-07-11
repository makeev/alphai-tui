use clap::ValueEnum;

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

#[derive(Clone, Debug)]
pub struct Quote {
    pub symbol: String,
    pub price: f64,
    pub prev_close: Option<f64>,
    pub currency: Option<String>,
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
}

/// One OHLCV bar; `ts` is epoch seconds.
#[derive(Clone, Copy, Debug)]
pub struct Candle {
    pub ts: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: Option<f64>,
}

/// Everything the UI knows about one ticker: latest quote + recent candles.
#[derive(Clone, Debug)]
pub struct TickerData {
    pub quote: Quote,
    pub candles: Vec<Candle>,
}

pub fn fmt_price(p: f64) -> String {
    if p.abs() >= 1.0 {
        format!("{p:.2}")
    } else {
        format!("{p:.4}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indicators::SMA_SLOW;

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
