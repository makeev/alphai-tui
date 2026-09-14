//! Positions and the arithmetic over them.
//!
//! One line per ticker: how many units and what they cost on average.
//! Lots would be more faithful to a brokerage statement, but a single
//! average is what the config can hold without becoming a ledger, and it
//! answers the two questions a watchlist cannot: what is this worth, and
//! am I up on it.
//!
//! Deliberately free of I/O and of `App`: the views, the rail, the JSON
//! output and the tests all price the same numbers through here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::Quote;
use crate::market::{self, Session};

/// Price used to value holdings everywhere, independent of the chart's
/// extended-candle toggle. A missing or invalid extended print falls back
/// to the regular-session quote.
pub fn price(quote: &Quote) -> f64 {
    price_at(quote, Utc::now())
}

pub fn price_at(quote: &Quote, now: DateTime<Utc>) -> f64 {
    quote.extended_price_at(now).unwrap_or(quote.price)
}

/// During premarket the regular quote is yesterday's close; the source's
/// previous close still belongs to the session before that. After hours,
/// include both the regular day's move and the extended move.
pub fn day_change(quote: &Quote, now: DateTime<Utc>) -> Option<f64> {
    let extended = quote.extended_price_at(now);
    let previous = if extended.is_some()
        && !market::is_crypto(&quote.symbol)
        && quote
            .timing
            .extended
            .and_then(market::window_at)
            .map(|window| window.session)
            .unwrap_or_else(|| market::clock_at(now).session)
            == Session::Pre
    {
        Some(quote.extended_reference())
    } else {
        quote.prev_close
    };
    previous.map(|previous| extended.unwrap_or(quote.price) - previous)
}

/// One holding. `qty` may be negative (a short), in which case a rising
/// price loses money and the percentages below still read the way a
/// statement reads, because they measure against the money at risk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub symbol: String,
    pub qty: f64,
    pub avg_price: f64,
}

impl Position {
    /// What the units cost in total.
    pub fn cost(&self) -> f64 {
        self.qty * self.avg_price
    }

    pub fn value(&self, price: f64) -> f64 {
        self.qty * price
    }

    pub fn pnl(&self, price: f64) -> f64 {
        self.value(price) - self.cost()
    }

    /// Against the absolute cost, so a short that gains reads positive.
    /// None when the basis is zero: a percentage of nothing is not zero,
    /// it is unanswerable.
    pub fn pnl_pct(&self, price: f64) -> Option<f64> {
        let cost = self.cost();
        (cost != 0.0).then(|| self.pnl(price) / cost.abs() * 100.0)
    }

    /// Today's move on this holding, including extended trading when the
    /// source reports it. None when there is no reference close.
    pub fn day_pnl(&self, quote: &Quote, now: DateTime<Utc>) -> Option<f64> {
        day_change(quote, now).map(|c| c * self.qty)
    }
}

/// The bottom line of the portfolio view.
///
/// `value` and `pnl` cover only the rows that have a price: a holding
/// still waiting for its first poll must not be summed in as zero, and
/// `priced`/`total` is what lets the view say so out loud.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Totals {
    pub value: f64,
    pub cost: f64,
    pub pnl: f64,
    /// None when not one priced row stated a previous close.
    pub day_pnl: Option<f64>,
    pub priced: usize,
    pub total: usize,
}

impl Totals {
    pub fn pnl_pct(&self) -> Option<f64> {
        (self.cost != 0.0).then(|| self.pnl / self.cost.abs() * 100.0)
    }
}

/// Sum the rows the view is about to draw. Pass `None` for a position
/// whose quote has not arrived (or whose fetch failed).
pub fn totals<'a>(
    rows: impl Iterator<Item = (&'a Position, Option<&'a Quote>)>,
    now: DateTime<Utc>,
) -> Totals {
    let mut t = Totals::default();
    for (pos, quote) in rows {
        t.total += 1;
        let Some(quote) = quote else { continue };
        t.priced += 1;
        let price = price_at(quote, now);
        t.value += pos.value(price);
        t.cost += pos.cost();
        t.pnl += pos.pnl(price);
        if let Some(day) = pos.day_pnl(quote, now) {
            *t.day_pnl.get_or_insert(0.0) += day;
        }
    }
    t
}

/// A row's share of the portfolio, in percent. None when there is nothing
/// to take a share of.
pub fn weight(value: f64, total_value: f64) -> Option<f64> {
    (total_value != 0.0).then(|| value / total_value * 100.0)
}

/// The symbols worth polling: the watchlist first, in its own order, then
/// anything held that is not on it. Both `main` (at startup) and `App`
/// (after an edit) build the poller's list through here.
pub fn polled_symbols(watchlist: &[String], positions: &[Position]) -> Vec<String> {
    let mut out = watchlist.to_vec();
    for p in positions {
        if !out.contains(&p.symbol) {
            out.push(p.symbol.clone());
        }
    }
    out
}

/// What one line typed into the position prompt asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Set {
        symbol: String,
        qty: f64,
        avg_price: f64,
    },
    Clear {
        symbol: String,
    },
}

/// Parse the prompt line. `target` is the ticker the cursor is on, used
/// when the line names none.
///
/// Accepted: `12 182.31`, `12 @ 182.31`, `AAPL 12 182.31`, `AAPL` (clears
/// it), and an empty line (clears the target). `$` and thousands commas
/// are noise and are dropped. A zero quantity is a clear rather than a
/// holding of nothing.
pub fn parse_entry(input: &str, target: &str) -> Result<Entry, String> {
    let cleaned = input.replace('@', " ").replace(['$', ','], "");
    let words: Vec<&str> = cleaned.split_whitespace().collect();

    let (symbol, qty, price) = match words.as_slice() {
        [] => return clear(target),
        // A lone word that is not a number names a ticker to forget.
        [one] if one.parse::<f64>().is_err() => return clear(one),
        [_] => return Err("give a quantity and an average price".to_string()),
        [qty, price] => (target, *qty, *price),
        [symbol, qty, price] => (*symbol, *qty, *price),
        _ => return Err("too many words: SYMBOL QTY AVERAGE".to_string()),
    };

    let symbol = normalize_symbol(symbol)?;
    let qty = number(qty, "quantity")?;
    let avg_price = number(price, "average price")?;
    // Nothing held is not a holding of nothing: it is a line to drop.
    if qty == 0.0 {
        return Ok(Entry::Clear { symbol });
    }
    if avg_price < 0.0 {
        return Err("the average price cannot be negative".to_string());
    }
    Ok(Entry::Set {
        symbol,
        qty,
        avg_price,
    })
}

fn clear(symbol: &str) -> Result<Entry, String> {
    Ok(Entry::Clear {
        symbol: normalize_symbol(symbol)?,
    })
}

fn normalize_symbol(raw: &str) -> Result<String, String> {
    let symbol = raw.trim().to_uppercase();
    if symbol.is_empty() {
        return Err("name a ticker".to_string());
    }
    if symbol.parse::<f64>().is_ok() {
        return Err(format!("{symbol} is not a ticker"));
    }
    Ok(symbol)
}

fn number(raw: &str, what: &str) -> Result<f64, String> {
    match raw.parse::<f64>() {
        Ok(v) if v.is_finite() => Ok(v),
        _ => Err(format!("{what}: {raw} is not a number")),
    }
}

/// Money with thousands separators: a position is worth four or five
/// digits more often than a share price is, and `12430.00` is a number
/// nobody reads at a glance. Prices themselves keep `domain::fmt_price`.
pub fn fmt_money(v: f64) -> String {
    let sign = if v < 0.0 { "-" } else { "" };
    let raw = format!("{:.2}", v.abs());
    let (int, frac) = raw.split_once('.').unwrap_or((raw.as_str(), "00"));
    let mut grouped = String::with_capacity(int.len() + int.len() / 3);
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{sign}{grouped}.{frac}")
}

/// The same, with the sign always written: a P&L of zero is `+0.00`, the
/// way every change column in this app already reads.
pub fn fmt_signed(v: f64) -> String {
    format!("{}{}", if v < 0.0 { "" } else { "+" }, fmt_money(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(2026, 9, 11)
            .unwrap()
            .and_hms_opt(hour, 30, 0)
            .unwrap()
            .and_utc()
    }

    fn quote(price: f64, prev_close: Option<f64>) -> Quote {
        Quote {
            timing: Default::default(),
            symbol: "AAPL".to_string(),
            price,
            prev_close,
            currency: Some("USD".to_string()),
            extended: None,
            fifty_two_week: None,
            day_range: None,
            volume: None,
        }
    }

    fn pos(qty: f64, avg_price: f64) -> Position {
        Position {
            symbol: "AAPL".to_string(),
            qty,
            avg_price,
        }
    }

    #[test]
    fn a_long_position_values_and_profits() {
        let p = pos(10.0, 100.0);
        assert_eq!(p.cost(), 1000.0);
        assert_eq!(p.value(110.0), 1100.0);
        assert_eq!(p.pnl(110.0), 100.0);
        assert_eq!(p.pnl_pct(110.0), Some(10.0));
    }

    #[test]
    fn a_short_gains_when_the_price_falls() {
        let p = pos(-10.0, 100.0);
        assert_eq!(p.pnl(90.0), 100.0);
        // Measured against the money at risk, so the gain reads positive.
        assert_eq!(p.pnl_pct(90.0), Some(10.0));
        assert_eq!(p.pnl(110.0), -100.0);
    }

    #[test]
    fn a_zero_basis_has_no_percentage() {
        assert_eq!(pos(10.0, 0.0).pnl_pct(110.0), None);
        assert_eq!(pos(10.0, 0.0).pnl(110.0), 1100.0);
    }

    #[test]
    fn the_day_move_needs_a_previous_close() {
        let p = pos(10.0, 100.0);
        assert_eq!(p.day_pnl(&quote(110.0, Some(109.0)), at(15)), Some(10.0));
        assert_eq!(p.day_pnl(&quote(110.0, None), at(15)), None);
    }

    #[test]
    fn totals_skip_the_rows_without_a_price() {
        let a = pos(10.0, 100.0);
        let b = Position {
            symbol: "MSFT".to_string(),
            qty: 5.0,
            avg_price: 400.0,
        };
        let qa = quote(110.0, Some(109.0));
        let t = totals([(&a, Some(&qa)), (&b, None)].into_iter(), at(15));
        assert_eq!(t.priced, 1);
        assert_eq!(t.total, 2);
        // The unpriced holding contributes neither value nor basis, so the
        // percentage stays honest instead of counting 2000 as a 100% loss.
        assert_eq!(t.value, 1100.0);
        assert_eq!(t.cost, 1000.0);
        assert_eq!(t.pnl, 100.0);
        assert_eq!(t.pnl_pct(), Some(10.0));
        assert_eq!(t.day_pnl, Some(10.0));
    }

    #[test]
    fn totals_have_no_day_move_when_no_row_states_one() {
        let a = pos(10.0, 100.0);
        let qa = quote(110.0, None);
        assert_eq!(totals([(&a, Some(&qa))].into_iter(), at(15)).day_pnl, None);
    }

    #[test]
    fn extended_prices_value_longs_shorts_and_totals() {
        let mut q = quote(89.12, Some(94.94));
        q.extended = Some(91.28);
        let long = pos(300.0, 90.26);
        let short = pos(-100.0, 90.26);
        let near = |actual: f64, expected: f64| {
            assert!((actual - expected).abs() < 1e-8, "{actual} != {expected}")
        };
        near(long.pnl(price(&q)), 306.0);
        near(short.pnl(price(&q)), -102.0);
        let t = totals([(&long, Some(&q)), (&short, Some(&q))].into_iter(), at(10));
        near(t.value, 18_256.0);
        near(t.pnl, 204.0);
        near(t.day_pnl.unwrap(), 432.0);
    }

    #[test]
    fn premarket_resets_the_day_reference_but_after_hours_keeps_the_day() {
        let mut q = quote(89.12, Some(94.94));
        q.extended = Some(91.28);
        let p = pos(300.0, 90.26);
        assert!((p.day_pnl(&q, at(10)).unwrap() - 648.0).abs() < 1e-8);
        assert!((p.day_pnl(&q, at(21)).unwrap() + 1098.0).abs() < 1e-8);
        q.prev_close = None;
        assert!((p.day_pnl(&q, at(10)).unwrap() - 648.0).abs() < 1e-8);
        assert_eq!(p.day_pnl(&q, at(21)), None);
        q.extended = Some(q.price);
        q.timing.extended = Some(at(10).timestamp());
        assert_eq!(p.day_pnl(&q, at(10)), Some(0.0));
    }

    #[test]
    fn missing_or_invalid_extended_prices_fall_back_to_regular() {
        let mut q = quote(110.0, Some(109.0));
        for extended in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            q.extended = extended;
            assert_eq!(price(&q), 110.0);
            assert_eq!(day_change(&q, at(10)), Some(1.0));
        }
        q.extended = Some(112.0);
        assert_eq!(price(&q), 112.0);
        q.extended = None; // the next regular-session response
        assert_eq!(price(&q), 110.0);
    }

    #[test]
    fn crypto_does_not_reset_its_day_at_the_us_premarket() {
        let mut q = quote(100.0, Some(95.0));
        q.symbol = "BTC-USD".into();
        q.extended = Some(101.0);
        assert_eq!(day_change(&q, at(10)), Some(6.0));
    }

    #[test]
    fn weights_need_something_to_divide() {
        assert_eq!(weight(250.0, 1000.0), Some(25.0));
        assert_eq!(weight(0.0, 0.0), None);
    }

    #[test]
    fn the_prompt_takes_every_shape_of_the_line() {
        let set = |symbol: &str, qty: f64, avg_price: f64| Entry::Set {
            symbol: symbol.to_string(),
            qty,
            avg_price,
        };
        assert_eq!(
            parse_entry("12 182.31", "AAPL"),
            Ok(set("AAPL", 12.0, 182.31))
        );
        assert_eq!(
            parse_entry("12 @ 182.31", "AAPL"),
            Ok(set("AAPL", 12.0, 182.31))
        );
        assert_eq!(
            parse_entry("nvda 3 1,204.50", "AAPL"),
            Ok(set("NVDA", 3.0, 1204.5))
        );
        assert_eq!(parse_entry("  4 $99 ", "AAPL"), Ok(set("AAPL", 4.0, 99.0)));
        assert_eq!(
            parse_entry("0.25 64000", "BTC-USD"),
            Ok(set("BTC-USD", 0.25, 64000.0))
        );
        assert_eq!(
            parse_entry("-10 100", "AAPL"),
            Ok(set("AAPL", -10.0, 100.0))
        );
    }

    #[test]
    fn the_prompt_clears_on_an_empty_line_a_bare_ticker_or_a_zero() {
        let clear = |symbol: &str| {
            Ok(Entry::Clear {
                symbol: symbol.to_string(),
            })
        };
        assert_eq!(parse_entry("", "AAPL"), clear("AAPL"));
        assert_eq!(parse_entry("   ", "AAPL"), clear("AAPL"));
        assert_eq!(parse_entry("msft", "AAPL"), clear("MSFT"));
        assert_eq!(parse_entry("0 182.31", "AAPL"), clear("AAPL"));
    }

    #[test]
    fn the_prompt_says_what_is_wrong() {
        assert!(parse_entry("12", "AAPL").is_err());
        assert!(parse_entry("abc 12", "AAPL").is_err());
        assert!(parse_entry("12 -5", "AAPL").is_err());
        assert!(parse_entry("AAPL 12 182.31 extra", "AAPL").is_err());
        assert!(parse_entry("12 182.31", "").is_err());
    }

    #[test]
    fn money_is_grouped_and_signed() {
        assert_eq!(fmt_money(12430.0), "12,430.00");
        assert_eq!(fmt_money(1234567.891), "1,234,567.89");
        assert_eq!(fmt_money(-142.3), "-142.30");
        assert_eq!(fmt_money(99.0), "99.00");
        assert_eq!(fmt_signed(142.3), "+142.30");
        assert_eq!(fmt_signed(-142.3), "-142.30");
        assert_eq!(fmt_signed(0.0), "+0.00");
    }
}
