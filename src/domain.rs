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
}

impl Range {
    pub fn as_str(&self) -> &'static str {
        match self {
            Range::D1 => "1d",
            Range::D5 => "5d",
            Range::Mo1 => "1mo",
            Range::Mo3 => "3mo",
            Range::Mo6 => "6mo",
            Range::Y1 => "1y",
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
