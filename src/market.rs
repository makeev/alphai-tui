//! The US equity session clock: what the market is doing right now.
//!
//! Sources stamp their data in UTC and the header shows local time, but
//! "is the market open" is a New York question. Rather than pull a whole
//! timezone database in for one badge, the offset comes from the US
//! daylight-saving rule (second Sunday of March to first Sunday of
//! November) and the closures from the NYSE holiday rules, Good Friday
//! included. Half days are deliberately not modelled: on the three
//! afternoons a year the exchange closes at 13:00 (the day after
//! Thanksgiving, and the eves of Independence Day and Christmas) the badge
//! reads "live" until 16:00.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, Utc, Weekday};

/// What the US equity market is doing at a given moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// 04:00 to 09:30 ET.
    Pre,
    /// 09:30 to 16:00 ET, the regular session.
    Open,
    /// 16:00 to 20:00 ET.
    Post,
    /// Overnight, weekends and holidays.
    Closed,
}

impl Session {
    /// Badge glyph and word for the quote rail.
    pub fn label(self) -> &'static str {
        match self {
            Session::Pre => "◐ pre",
            Session::Open => "● live",
            Session::Post => "◑ post",
            Session::Closed => "○ closed",
        }
    }
}

/// The session plus how long until it changes: time to the closing bell
/// while the regular session runs, time to the next opening bell otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketClock {
    pub session: Session,
    pub until: Duration,
}

impl MarketClock {
    /// "closes in 2h14m" / "opens in 12h40m", short enough for the rail.
    pub fn countdown(&self) -> String {
        let verb = if self.session == Session::Open {
            "closes"
        } else {
            "opens"
        };
        let mins = self.until.num_minutes().max(0);
        if mins >= 60 {
            format!("{verb} in {}h{:02}m", mins / 60, mins % 60)
        } else {
            format!("{verb} in {mins}m")
        }
    }
}

/// Session boundaries as minutes since ET midnight.
const PRE_OPEN: i64 = 4 * 60;
const OPEN: i64 = 9 * 60 + 30;
const CLOSE: i64 = 16 * 60;
const POST_CLOSE: i64 = 20 * 60;

/// The New York wall clock at a UTC instant.
pub fn et_time(now: DateTime<Utc>) -> NaiveDateTime {
    let utc = now.naive_utc();
    utc - Duration::hours(et_offset_hours(utc))
}

/// The ET calendar date of an epoch-second timestamp, i.e. which session a
/// candle belongs to.
pub fn et_date(ts: i64) -> Option<NaiveDate> {
    DateTime::from_timestamp(ts, 0).map(|t| et_time(t).date())
}

/// Session and countdown at a UTC instant.
pub fn clock_at(now: DateTime<Utc>) -> MarketClock {
    let et = et_time(now);
    let date = et.date();
    let minute = et.time().signed_duration_since(chrono::NaiveTime::MIN);
    let mins = minute.num_minutes();

    if trading_day(date) {
        let (session, until_mins) = match mins {
            m if m < PRE_OPEN => (Session::Closed, Some(OPEN - m)),
            m if m < OPEN => (Session::Pre, Some(OPEN - m)),
            m if m < CLOSE => (Session::Open, Some(CLOSE - m)),
            m if m < POST_CLOSE => (Session::Post, None),
            _ => (Session::Closed, None),
        };
        if let Some(mins) = until_mins {
            // Minute resolution would round 30 seconds off the countdown;
            // subtracting the wall clock keeps it honest.
            let target = date
                .and_hms_opt(0, 0, 0)
                .expect("midnight exists")
                .checked_add_signed(Duration::minutes(mins) + minute)
                .expect("session boundary is in range");
            return MarketClock {
                session,
                until: target.signed_duration_since(et),
            };
        }
        return MarketClock {
            session,
            until: until_next_open(et, date),
        };
    }
    MarketClock {
        session: Session::Closed,
        until: until_next_open(et, date),
    }
}

/// Time from `et` to the next opening bell. Across a DST change the answer
/// is an hour off, which no countdown shown in whole minutes is worth a
/// timezone database to fix.
fn until_next_open(et: NaiveDateTime, from: NaiveDate) -> Duration {
    let mut day = from;
    for _ in 0..10 {
        day = day.succ_opt().unwrap_or(day);
        if trading_day(day) {
            break;
        }
    }
    let open = day
        .and_hms_opt(9, 30, 0)
        .expect("09:30 exists")
        .signed_duration_since(et);
    open.max(Duration::zero())
}

/// A weekday the exchange actually trades.
fn trading_day(d: NaiveDate) -> bool {
    !matches!(d.weekday(), Weekday::Sat | Weekday::Sun) && !is_holiday(d)
}

/// Hours ET runs behind UTC: 4 on daylight time, 5 on standard time. The
/// transitions are compared in UTC (02:00 local is 07:00 UTC entering DST
/// and 06:00 UTC leaving it), so the ambiguous local hour never comes up.
fn et_offset_hours(utc: NaiveDateTime) -> i64 {
    let year = utc.year();
    let starts = nth_weekday(year, 3, Weekday::Sun, 2)
        .and_hms_opt(7, 0, 0)
        .expect("07:00 exists");
    let ends = nth_weekday(year, 11, Weekday::Sun, 1)
        .and_hms_opt(6, 0, 0)
        .expect("06:00 exists");
    if utc >= starts && utc < ends { 4 } else { 5 }
}

/// A day the NYSE is closed for a holiday.
pub fn is_holiday(d: NaiveDate) -> bool {
    holidays(d.year()).contains(&d)
}

/// The NYSE closures of one year, by rule rather than by table so the badge
/// stays right without a yearly edit.
fn holidays(year: i32) -> Vec<NaiveDate> {
    [
        // A Saturday New Year's Day closes nothing: the Friday it would
        // roll back to belongs to the previous year, and the exchange
        // trades it.
        observed(ymd(year, 1, 1), false),
        Some(nth_weekday(year, 1, Weekday::Mon, 3)), // Martin Luther King Jr. Day
        Some(nth_weekday(year, 2, Weekday::Mon, 3)), // Washington's Birthday
        Some(easter(year) - Duration::days(2)),      // Good Friday
        Some(last_weekday(year, 5, Weekday::Mon)),   // Memorial Day
        observed(ymd(year, 6, 19), true),            // Juneteenth
        observed(ymd(year, 7, 4), true),             // Independence Day
        Some(nth_weekday(year, 9, Weekday::Mon, 1)), // Labor Day
        Some(nth_weekday(year, 11, Weekday::Thu, 4)), // Thanksgiving
        observed(ymd(year, 12, 25), true),           // Christmas
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Where a fixed-date holiday lands: Saturday is observed on the Friday
/// before, Sunday on the Monday after. `rollback` turns the Friday half of
/// that rule off.
fn observed(d: NaiveDate, rollback: bool) -> Option<NaiveDate> {
    match d.weekday() {
        Weekday::Sat if rollback => Some(d - Duration::days(1)),
        Weekday::Sat => None,
        Weekday::Sun => Some(d + Duration::days(1)),
        _ => Some(d),
    }
}

/// Easter Sunday by the anonymous Gregorian computus; Good Friday is two
/// days before it, and it is the one NYSE closure with no fixed date.
fn easter(year: i32) -> NaiveDate {
    let a = year % 19;
    let b = year / 100;
    let c = year % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    ymd(year, month as u32, day as u32)
}

/// The `n`th `weekday` of a month, 1-based.
fn nth_weekday(year: i32, month: u32, weekday: Weekday, n: u32) -> NaiveDate {
    let first = ymd(year, month, 1);
    let shift = (7 + weekday.num_days_from_monday() - first.weekday().num_days_from_monday()) % 7;
    first + Duration::days((shift + (n - 1) * 7) as i64)
}

/// The last `weekday` of a month (Memorial Day).
fn last_weekday(year: i32, month: u32, weekday: Weekday) -> NaiveDate {
    let next_month = if month == 12 {
        ymd(year + 1, 1, 1)
    } else {
        ymd(year, month + 1, 1)
    };
    let mut d = next_month - Duration::days(1);
    while d.weekday() != weekday {
        d -= Duration::days(1);
    }
    d
}

/// Every call site passes a date that exists; a bad one is a bug, not input.
fn ymd(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("valid calendar date")
}

/// Crypto pairs trade around the clock, so the session badge does not
/// apply to them. Matches the app's Yahoo-style convention (`BTC-USD`) and
/// the exchange-prefixed forms Finnhub takes (`BINANCE:BTCUSDT`).
pub fn is_crypto(symbol: &str) -> bool {
    symbol.contains(':')
        || ["-USD", "-USDT", "-EUR", "-GBP"]
            .iter()
            .any(|suffix| symbol.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, m: u32, d: u32, hh: u32, mm: u32) -> DateTime<Utc> {
        ymd(y, m, d).and_hms_opt(hh, mm, 0).unwrap().and_utc()
    }

    #[test]
    fn dst_moves_the_new_york_offset() {
        // 2026: DST runs 8 March to 1 November.
        assert_eq!(et_offset_hours(utc(2026, 1, 15, 12, 0).naive_utc()), 5);
        assert_eq!(et_offset_hours(utc(2026, 7, 15, 12, 0).naive_utc()), 4);
        // Right at the boundaries, compared in UTC.
        assert_eq!(et_offset_hours(utc(2026, 3, 8, 6, 59).naive_utc()), 5);
        assert_eq!(et_offset_hours(utc(2026, 3, 8, 7, 0).naive_utc()), 4);
        assert_eq!(et_offset_hours(utc(2026, 11, 1, 5, 59).naive_utc()), 4);
        assert_eq!(et_offset_hours(utc(2026, 11, 1, 6, 0).naive_utc()), 5);
    }

    #[test]
    fn sessions_follow_the_new_york_clock() {
        // Thursday 10 September 2026, EDT (UTC-4).
        let at = |hh, mm| clock_at(utc(2026, 9, 10, hh, mm)).session;
        assert_eq!(at(7, 0), Session::Closed); // 03:00 ET
        assert_eq!(at(8, 30), Session::Pre); // 04:30 ET
        assert_eq!(at(13, 29), Session::Pre); // 09:29 ET
        assert_eq!(at(13, 30), Session::Open); // 09:30 ET
        assert_eq!(at(19, 59), Session::Open); // 15:59 ET
        assert_eq!(at(20, 0), Session::Post); // 16:00 ET
        assert_eq!(at(23, 59), Session::Post); // 19:59 ET
        assert_eq!(at(0, 30), Session::Closed); // 20:30 ET the day before
    }

    #[test]
    fn weekends_and_holidays_are_closed() {
        // Saturday 12 September 2026, mid-session hour.
        assert_eq!(clock_at(utc(2026, 9, 12, 15, 0)).session, Session::Closed);
        // Thanksgiving 2026 falls on 26 November.
        assert_eq!(clock_at(utc(2026, 11, 26, 15, 0)).session, Session::Closed);
        // The Friday after it trades (as a half day, which we do not model).
        assert_eq!(clock_at(utc(2026, 11, 27, 15, 0)).session, Session::Open);
    }

    #[test]
    fn holiday_rules_land_on_the_right_dates() {
        // Good Friday: 3 April 2026, 26 March 2027.
        assert!(is_holiday(ymd(2026, 4, 3)));
        assert!(is_holiday(ymd(2027, 3, 26)));
        // Third Monday of January and February 2026.
        assert!(is_holiday(ymd(2026, 1, 19)));
        assert!(is_holiday(ymd(2026, 2, 16)));
        // Last Monday of May, first Monday of September.
        assert!(is_holiday(ymd(2026, 5, 25)));
        assert!(is_holiday(ymd(2026, 9, 7)));
        // Independence Day 2026 is a Saturday: observed on Friday the 3rd.
        assert!(is_holiday(ymd(2026, 7, 3)));
        assert!(!is_holiday(ymd(2026, 7, 6)));
        // Christmas 2027 is a Saturday: observed on Friday the 24th.
        assert!(is_holiday(ymd(2027, 12, 24)));
        // A Saturday New Year's Day closes nothing at either end.
        assert!(!is_holiday(ymd(2027, 12, 31))); // 1 Jan 2028 is a Saturday
        assert!(!is_holiday(ymd(2028, 1, 3)));
        // A Sunday one is observed on the Monday.
        assert!(is_holiday(ymd(2034, 1, 2))); // 1 Jan 2034 is a Sunday
        // An ordinary trading day is not a holiday.
        assert!(!is_holiday(ymd(2026, 9, 10)));
    }

    #[test]
    fn countdown_points_at_the_next_bell() {
        // Wednesday 15:00 ET: 1 hour to the close.
        let open = clock_at(utc(2026, 9, 9, 19, 0));
        assert_eq!(open.session, Session::Open);
        assert_eq!(open.countdown(), "closes in 1h00m");
        // Friday 21:00 ET skips the weekend to Monday's opening bell.
        let weekend = clock_at(utc(2026, 9, 12, 1, 0));
        assert_eq!(weekend.session, Session::Closed);
        assert_eq!(weekend.until.num_hours(), 60); // Fri 21:00 -> Mon 09:30
        // Under an hour drops the hours part.
        let pre = clock_at(utc(2026, 9, 10, 13, 0));
        assert_eq!(pre.session, Session::Pre);
        assert_eq!(pre.countdown(), "opens in 30m");
    }

    #[test]
    fn et_date_buckets_a_candle_into_its_session() {
        // 20:30 UTC on 10 September 2026 is 16:30 ET the same day.
        let ts = utc(2026, 9, 10, 20, 30).timestamp();
        assert_eq!(et_date(ts), Some(ymd(2026, 9, 10)));
        // 01:00 UTC the next day is still the 10th in New York.
        let overnight = utc(2026, 9, 11, 1, 0).timestamp();
        assert_eq!(et_date(overnight), Some(ymd(2026, 9, 10)));
    }

    #[test]
    fn crypto_is_recognised_without_catching_share_classes() {
        assert!(is_crypto("BTC-USD"));
        assert!(is_crypto("ETH-USDT"));
        assert!(is_crypto("BINANCE:BTCUSDT"));
        assert!(!is_crypto("AAPL"));
        assert!(!is_crypto("BRK-B"));
        assert!(!is_crypto("VOD.L"));
    }
}
