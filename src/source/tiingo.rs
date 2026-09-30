use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::domain::{Candle, Interval, PriceFeed, Quote, QuoteTiming, Range, Sessions, TickerData};
use crate::market::{self, Session};
use crate::source::{DataSource, candle_from_ohlc, http, normalize_sessions, sort_ascending};

pub const DEFAULT_API_URL: &str = "https://api.tiingo.com";

/// What the symbol error adds when a ticker is outside Tiingo's coverage.
const HINT: &str = "tiingo covers US stocks, ETFs and funds, and crypto as BTC-USD";

/// How long intraday bars are served from memory before the next request.
/// Between refreshes the live price is spliced into them (see `splice`).
const BARS_TTL: Duration = Duration::from_secs(60);

/// Daily history changes once a day; it also supplies the 52 week range
/// and the close that premarket prints are measured against.
const DAILY_TTL: Duration = Duration::from_secs(15 * 60);

/// A failed refresh is not tried again before this, so a dead symbol or a
/// used-up allowance costs one request a minute rather than one a poll.
/// The quote request waits it out only after a refusal no retry can win
/// (see `lasting`): anything else is tried again on the next poll, since
/// a minute of frozen prices is worse than one more request.
const RETRY: Duration = Duration::from_secs(60);

/// A session's close does not change once it is known.
const CLOSE_TTL: Duration = Duration::from_secs(12 * 3_600);

/// Cached windows nobody asked for in this long are dropped.
const EVICT: Duration = Duration::from_secs(10 * 60);

/// A quote batch answers the `fetch` calls of its own poll cycle. The cap
/// only matters to a caller that never starts a cycle.
const BATCH_REUSE: Duration = Duration::from_secs(10);

/// Two years of daily bars and a margin: the longest window a chart asks
/// for, fetched whole so every range reuses one response.
const DAILY_DAYS: i64 = 740;

/// The crypto bars endpoint stops at 5,000 rows and keeps the OLDEST ones
/// (measured 2026-09-30: 5m bars asked from 30 June ended on 17 July), so
/// a window longer than that loses its newest bars rather than its oldest.
/// The start is moved forward instead; the date granularity of `startDate`
/// can add up to a day on top, which the margin covers.
const CRYPTO_MAX_ROWS: i64 = 4_900;

/// Tiingo: IEX prices and intraday bars in real time, consolidated daily
/// history and crypto, on one key.
///
/// Measured against the live API 2026-09-29/30:
/// - `/iex/?tickers=` quotes a whole watchlist in one request, which is
///   how this source polls: one request per cycle rather than one per
///   symbol. Since IEX changed its data policy on 1 February 2025 the
///   last trade, bid and ask need an exchange agreement, so the price is
///   `tngoLast`, Tiingo's reference price, within a few cents of the IEX
///   last trade. During the day its volume is IEX's alone. After the
///   session Tiingo swaps the row for the official daily bar, stamped
///   16:00 ET exactly, with the consolidated close and volume.
/// - `/iex/{t}/prices` has intraday bars from 08:00 to 17:30 ET with
///   `afterHours`, newest 10,000 rows per request.
/// - `/tiingo/daily/{t}/prices` is consolidated end-of-day history.
/// - `/tiingo/crypto/*` covers pairs, with a real last trade.
///
/// The free plan allows 50 requests an hour and 1,000 a day, Power
/// 10,000 and 100,000. Nothing in a response says which one a key is on,
/// so the budget is spent frugally for both: bars and daily history are
/// cached, and only the quote batch goes out every poll.
pub struct Tiingo {
    client: reqwest::Client,
    base: String,
    /// Bumped by `begin_cycle`; a batch belongs to the cycle it was
    /// fetched in.
    cycle: AtomicU64,
    /// What the current cycle will ask for, so its first `fetch` can
    /// quote all of it.
    wanted: Mutex<Vec<Asset>>,
    /// Held across the batch request, so the other symbols of the cycle
    /// wait for it instead of sending their own.
    batch: tokio::sync::Mutex<Batch>,
    bars: Memo<Vec<Candle>>,
    daily: Memo<Vec<Candle>>,
    /// Provisional regular closes by symbol and day (see `session_close`).
    closes: Memo<f64>,
}

impl Tiingo {
    pub fn new(key: String) -> Result<Self> {
        let base = std::env::var("TIINGO_API_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_URL.to_string());
        Self::at(key, base)
    }

    fn at(key: String, base: String) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut auth =
            HeaderValue::from_str(&format!("Token {key}")).context("invalid Tiingo key")?;
        auth.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, auth);
        Ok(Self {
            client: http::client_with(http::APP_UA, Some(headers))?,
            base,
            cycle: AtomicU64::new(0),
            wanted: Mutex::new(Vec::new()),
            batch: tokio::sync::Mutex::new(Batch::default()),
            bars: Memo::default(),
            daily: Memo::default(),
            closes: Memo::default(),
        })
    }

    /// GET and parse. The allowance a 429 reports is an hourly or daily
    /// window, not a burst, so a retry cannot win it and is not made.
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
        symbol: Option<&str>,
    ) -> Result<T> {
        let url = format!("{}{path}", self.base);
        let value: serde_json::Value = http::get_json_retrying(
            &self.client,
            "tiingo",
            &url,
            query,
            |status, body| error_message(status, body, symbol),
            http::Retry::GatewayOnly,
        )
        .await?;
        // Refusals are `{"detail": "..."}`; one arriving with a 200 where
        // a list was expected would otherwise read as broken JSON.
        if let Some(detail) = value.get("detail").and_then(|d| d.as_str()) {
            bail!("{}", describe(None, detail, symbol));
        }
        serde_json::from_value(value).context("bad JSON from tiingo")
    }

    /// The cycle's quote batch, fetched if this cycle has none covering
    /// `asset` yet. Every `fetch` of a cycle goes through here, and the
    /// lock is held across the request: the first caller quotes the whole
    /// cycle, the rest wait and read its answer, errors included.
    async fn batch(&self, asset: &Asset) -> tokio::sync::MutexGuard<'_, Batch> {
        let cycle = self.cycle.load(Ordering::Relaxed);
        let mut batch = self.batch.lock().await;
        let current = batch.cycle == cycle && batch.at.is_some_and(|at| at.elapsed() < BATCH_REUSE);
        let held = batch.held_until.is_some_and(|until| Instant::now() < until);
        let reusable = (current || held) && batch.requested.contains(asset);
        if !reusable {
            let mut assets = self.wanted.lock().unwrap().clone();
            if !assets.contains(asset) {
                assets.push(asset.clone());
            }
            *batch = self.fetch_batch(assets, cycle).await;
        }
        batch
    }

    async fn fetch_batch(&self, assets: Vec<Asset>, cycle: u64) -> Batch {
        let join = |kind: fn(&Asset) -> Option<&str>| {
            let mut tickers: Vec<&str> = assets.iter().filter_map(kind).collect();
            tickers.sort_unstable();
            tickers.dedup();
            tickers.join(",")
        };
        let (stocks, pairs) = (join(Asset::stock), join(Asset::crypto));
        let (stocks, crypto) = tokio::join!(
            self.quote_rows("/iex/", &stocks, |row: IexTop| {
                Some((row.ticker.to_uppercase(), row))
            }),
            self.quote_rows("/tiingo/crypto/top", &pairs, |row: CryptoTop| {
                let book = row.top_of_book_data.into_iter().next()?;
                Some((row.ticker.to_lowercase(), book))
            }),
        );
        let refused = [
            stocks.as_ref().and_then(|r| r.as_ref().err()),
            crypto.as_ref().and_then(|r| r.as_ref().err()),
        ]
        .into_iter()
        .flatten()
        .any(|e| lasting(e));
        let now = Instant::now();
        Batch {
            cycle,
            at: Some(now),
            held_until: refused.then(|| now + RETRY),
            requested: assets.into_iter().collect(),
            stocks,
            crypto,
        }
    }

    /// One quote request for `tickers`, keyed by `keyed`; None when there
    /// is nothing of that kind to ask for.
    async fn quote_rows<R: DeserializeOwned, T>(
        &self,
        path: &str,
        tickers: &str,
        keyed: impl Fn(R) -> Option<(String, T)>,
    ) -> Option<Quoted<T>> {
        if tickers.is_empty() {
            return None;
        }
        let rows = self
            .get::<Vec<R>>(path, &[("tickers", tickers)], None)
            .await;
        Some(
            rows.map(|rows| rows.into_iter().filter_map(keyed).collect())
                .map_err(|e| format!("{e:#}")),
        )
    }

    async fn fetch_stock(
        &self,
        symbol: &str,
        ticker: &str,
        range: Range,
        interval: Interval,
        sessions: Sessions,
    ) -> Result<TickerData> {
        let asset = Asset::Stock(ticker.to_string());
        let intraday = interval != Interval::D1;
        let bars_key = format!("{symbol}:{}:{}", range.as_str(), interval.as_str());
        let (top, daily, bars) = tokio::join!(
            async { quoted(&self.batch(&asset).await.stocks, ticker, symbol) },
            self.daily(symbol, ticker),
            async {
                if intraday {
                    Some(
                        self.intraday(symbol, ticker, range, interval, &bars_key)
                            .await,
                    )
                } else {
                    None
                }
            },
        );
        let top = top?;
        let unknown = || http::unknown_symbol(symbol, Some(HINT));
        let ts = top
            .timestamp
            .as_deref()
            .and_then(parse_ts)
            .ok_or_else(unknown)?;
        let price = top
            .tngo_last
            .filter(|p| p.is_finite() && *p > 0.0)
            .ok_or_else(unknown)?;
        let stamp = stamp(ts);
        let history: &[Candle] = daily.as_deref().unwrap_or(&[]);

        // After the bell and before the official row the headline needs
        // today's close, which the daily history does not carry yet.
        let mut close_today = None;
        if let Stamp::Live(Session::Post, day) = stamp
            && !history.iter().any(|c| market::et_date(c.ts) == Some(day))
        {
            close_today = bars
                .as_ref()
                .and_then(|b| b.as_ref().ok())
                .and_then(|bars| last_regular_close(bars, day));
            if close_today.is_none() {
                close_today = self.session_close(symbol, ticker, day).await;
            }
        }
        let mut quote = stock_quote(
            symbol,
            &top,
            (ts, price),
            stamp,
            history,
            close_today,
            Utc::now(),
        );

        let candles = match bars {
            Some(bars) => {
                let mut candles = bars?;
                if let Stamp::Live(..) = stamp {
                    let secs = fetch_interval(interval).secs();
                    if let Some(spliced) = self.bars.update(&bars_key, |bars| {
                        splice(bars, symbol, ts, price, secs, PriceFeed::Iex)
                    }) {
                        candles = spliced;
                    }
                }
                if let Stamp::Official(day) = stamp {
                    late_print(&mut quote, &candles, day);
                }
                normalize_sessions(candles, symbol, interval, sessions)
            }
            None => {
                let mut candles = daily?;
                let today = today_bar(&top, stamp, &quote, &candles);
                candles.extend(today);
                candles
            }
        };
        Ok(TickerData { quote, candles })
    }

    /// Intraday IEX bars, pre and post market included; `normalize_sessions`
    /// drops those when `E` is off. Hours come as 30 minute halves, which
    /// the session code joins so the 09:30 open keeps its own bar.
    async fn intraday(
        &self,
        symbol: &str,
        ticker: &str,
        range: Range,
        interval: Interval,
        key: &str,
    ) -> Result<Vec<Candle>> {
        if let Some(hit) = self.bars.fresh(key) {
            return hit;
        }
        let start = start_date(range.secs());
        let fetched = self
            .get::<Vec<Bar>>(
                &format!("/iex/{ticker}/prices"),
                &[
                    ("startDate", start.as_str()),
                    ("resampleFreq", resample(fetch_interval(interval))),
                    ("afterHours", "true"),
                    ("columns", "open,high,low,close,volume"),
                ],
                Some(symbol),
            )
            .await
            .map(|rows| candles(rows, PriceFeed::Iex));
        self.bars.store(key, fetched, BARS_TTL)
    }

    /// Consolidated daily bars, split adjusted, two years of them.
    async fn daily(&self, symbol: &str, ticker: &str) -> Result<Vec<Candle>> {
        if let Some(hit) = self.daily.fresh(symbol) {
            return hit;
        }
        let start = start_date(DAILY_DAYS * 86_400);
        let fetched = self
            .get::<Vec<DailyBar>>(
                &format!("/tiingo/daily/{ticker}/prices"),
                &[("startDate", start.as_str())],
                Some(symbol),
            )
            .await
            .map(split_adjusted);
        self.daily.store(symbol, fetched, DAILY_TTL)
    }

    /// The last regular IEX minute of `day`, as a stand-in for the close
    /// until the official row replaces it (measured 2026-09-29: 329.57
    /// against an official 329.40 for AAPL). One request per symbol a day;
    /// a failure is not retried for a minute.
    async fn session_close(&self, symbol: &str, ticker: &str, day: NaiveDate) -> Option<f64> {
        let key = format!("{symbol}:{day}");
        if let Some(hit) = self.closes.fresh(&key) {
            return hit.ok();
        }
        let date = day.format("%Y-%m-%d").to_string();
        let fetched = self
            .get::<Vec<Bar>>(
                &format!("/iex/{ticker}/prices"),
                &[
                    ("startDate", date.as_str()),
                    ("endDate", date.as_str()),
                    ("resampleFreq", "1min"),
                    ("columns", "close"),
                ],
                Some(symbol),
            )
            .await
            .and_then(|rows| {
                rows.iter()
                    .rev()
                    .find_map(|b| b.close)
                    .ok_or_else(|| anyhow!("no regular session bars on {day}"))
            });
        self.closes.store(&key, fetched, CLOSE_TTL).ok()
    }

    async fn fetch_crypto(
        &self,
        symbol: &str,
        pair: &str,
        range: Range,
        interval: Interval,
    ) -> Result<TickerData> {
        let asset = Asset::Crypto(pair.to_string());
        let key = format!("{symbol}:{}:{}", range.as_str(), interval.as_str());
        let (book, bars) = tokio::join!(
            async { quoted(&self.batch(&asset).await.crypto, pair, symbol) },
            self.crypto_bars(symbol, pair, range, interval, &key),
        );
        let book = book?;
        let price = book
            .last_price
            .filter(|p| p.is_finite() && *p > 0.0)
            .ok_or_else(|| http::unknown_symbol(symbol, Some(HINT)))?;
        let ts = book
            .last_sale_timestamp
            .as_deref()
            .or(book.quote_timestamp.as_deref())
            .and_then(parse_ts);
        let mut candles = bars?;
        if let Some(ts) = ts
            && let Some(spliced) = self.bars.update(&key, |bars| {
                splice(bars, symbol, ts, price, interval.secs(), PriceFeed::Tiingo)
            })
        {
            candles = spliced;
        }
        // Crypto has no close; like Yahoo, the day turns at 00:00 UTC.
        let midnight = Utc::now().date_naive().and_time(chrono::NaiveTime::MIN);
        let midnight = midnight.and_utc().timestamp();
        let prev_close = candles
            .iter()
            .rev()
            .find(|c| c.ts < midnight)
            .map(|c| c.close);
        let today = candles.iter().filter(|c| c.ts >= midnight);
        let day_range = today.fold(None, |range: Option<(f64, f64)>, c| {
            Some(match range {
                Some((lo, hi)) => (lo.min(c.low), hi.max(c.high)),
                None => (c.low, c.high),
            })
        });
        Ok(TickerData {
            quote: Quote {
                symbol: symbol.to_string(),
                price,
                prev_close,
                currency: symbol.rsplit_once('-').map(|(_, q)| q.to_string()),
                extended: None,
                timing: QuoteTiming {
                    regular: ts,
                    regular_feed: PriceFeed::Tiingo,
                    ..Default::default()
                },
                fifty_two_week: None,
                day_range,
                // Tiingo sums the venues it follows, not the whole market.
                volume: None,
            },
            candles,
        })
    }

    async fn crypto_bars(
        &self,
        symbol: &str,
        pair: &str,
        range: Range,
        interval: Interval,
        key: &str,
    ) -> Result<Vec<Candle>> {
        if let Some(hit) = self.bars.fresh(key) {
            return hit;
        }
        let span = range
            .secs()
            .min(CRYPTO_MAX_ROWS * interval.secs() - 86_400)
            .max(86_400);
        let start = start_date(span);
        let fetched = self
            .get::<Vec<CryptoPrices>>(
                "/tiingo/crypto/prices",
                &[
                    ("tickers", pair),
                    ("startDate", start.as_str()),
                    ("resampleFreq", resample(interval)),
                ],
                Some(symbol),
            )
            .await
            .map(|rows| {
                let rows = rows
                    .into_iter()
                    .find(|r| r.ticker.eq_ignore_ascii_case(pair))
                    .map(|r| r.price_data)
                    .unwrap_or_default();
                candles(rows, PriceFeed::Tiingo)
            });
        self.bars.store(key, fetched, BARS_TTL)
    }
}

#[async_trait]
impl DataSource for Tiingo {
    fn name(&self) -> &'static str {
        "tiingo"
    }

    fn begin_cycle(&self, symbols: &[String]) {
        *self.wanted.lock().unwrap() = symbols.iter().filter_map(|s| asset(s)).collect();
        self.cycle.fetch_add(1, Ordering::Relaxed);
    }

    async fn fetch(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        sessions: Sessions,
    ) -> Result<TickerData> {
        match asset(symbol) {
            Some(Asset::Stock(ticker)) => {
                self.fetch_stock(symbol, &ticker, range, interval, sessions)
                    .await
            }
            Some(Asset::Crypto(pair)) => self.fetch_crypto(symbol, &pair, range, interval).await,
            None => Err(http::unknown_symbol(symbol, Some(HINT))),
        }
    }
}

/// What a watchlist symbol is to Tiingo.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Asset {
    /// A US listing, in Tiingo's spelling: share classes take a dash, so
    /// `BRK.B` and `BRK-B` are both `BRK-B`.
    Stock(String),
    /// A crypto pair, lowercase and joined: `BTC-USD` is `btcusd`.
    Crypto(String),
}

impl Asset {
    fn stock(&self) -> Option<&str> {
        match self {
            Asset::Stock(t) => Some(t),
            Asset::Crypto(_) => None,
        }
    }

    fn crypto(&self) -> Option<&str> {
        match self {
            Asset::Crypto(p) => Some(p),
            Asset::Stock(_) => None,
        }
    }
}

/// None for what Tiingo has no answer to: exchange-prefixed crypto
/// (`BINANCE:BTCUSDT`), indices, FX and listings outside the US. Those
/// are never put in a batch, so they cannot cost one.
fn asset(symbol: &str) -> Option<Asset> {
    if market::is_crypto(symbol) {
        let (base, quote) = symbol.rsplit_once('-')?;
        if base.is_empty() || symbol.contains(':') {
            return None;
        }
        return Some(Asset::Crypto(format!("{base}{quote}").to_lowercase()));
    }
    market::is_us_equity(symbol).then(|| Asset::Stock(symbol.to_uppercase().replace('.', "-")))
}

/// One quote request's answer: rows by Tiingo ticker, or the error that
/// every symbol of the request gets.
type Quoted<T> = Result<HashMap<String, T>, String>;

/// The quote requests of one cycle, shared by its `fetch` calls.
#[derive(Default)]
struct Batch {
    cycle: u64,
    at: Option<Instant>,
    /// Set by a refusal that lasts (see `lasting`): until then every cycle
    /// reads this answer instead of asking again.
    held_until: Option<Instant>,
    requested: HashSet<Asset>,
    stocks: Option<Quoted<IexTop>>,
    crypto: Option<Quoted<CryptoBook>>,
}

/// `symbol`'s row in a quote answer. A ticker Tiingo does not know is
/// simply missing from it.
fn quoted<T: Clone>(answer: &Option<Quoted<T>>, key: &str, symbol: &str) -> Result<T> {
    match answer {
        Some(Ok(rows)) => rows
            .get(key)
            .cloned()
            .ok_or_else(|| http::unknown_symbol(symbol, Some(HINT))),
        Some(Err(e)) => Err(anyhow!("{e}")),
        None => Err(anyhow!("tiingo: {symbol} was not quoted")),
    }
}

/// Responses kept per key until they are due again. A failed refresh keeps
/// serving the last good answer and tries again after `RETRY`; with no
/// good answer the error itself is kept that long.
struct Memo<T> {
    slots: Mutex<HashMap<String, Slot<T>>>,
}

struct Slot<T> {
    due: Instant,
    value: Result<T, String>,
}

impl<T> Default for Memo<T> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
        }
    }
}

impl<T: Clone> Memo<T> {
    fn fresh(&self, key: &str) -> Option<Result<T>> {
        let slots = self.slots.lock().unwrap();
        let slot = slots.get(key).filter(|s| Instant::now() < s.due)?;
        Some(slot.value.clone().map_err(|e| anyhow!(e)))
    }

    fn store(&self, key: &str, fetched: Result<T>, ttl: Duration) -> Result<T> {
        let now = Instant::now();
        let mut slots = self.slots.lock().unwrap();
        slots.retain(|_, s| now < s.due + EVICT);
        let slot = match (fetched, slots.remove(key)) {
            (Ok(value), _) => Slot {
                due: now + ttl,
                value: Ok(value),
            },
            (Err(_), Some(Slot { value: Ok(old), .. })) => Slot {
                due: now + RETRY,
                value: Ok(old),
            },
            (Err(e), _) => Slot {
                due: now + RETRY,
                value: Err(format!("{e:#}")),
            },
        };
        let out = slot.value.clone().map_err(|e| anyhow!(e));
        slots.insert(key.to_string(), slot);
        out
    }

    /// Edit a kept answer in place and return the result; None when there
    /// is no good answer under `key`.
    fn update(&self, key: &str, edit: impl FnOnce(&mut T)) -> Option<T> {
        let mut slots = self.slots.lock().unwrap();
        let value = slots.get_mut(key)?.value.as_mut().ok()?;
        edit(value);
        Some(value.clone())
    }
}

/// What a `/iex/` row is, read off its timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stamp {
    /// The official daily bar that replaces the live row after a session,
    /// stamped at 16:00 ET to the second (and at the bell on a half day).
    Official(NaiveDate),
    /// A live reference price inside one of the day's session windows.
    Live(Session, NaiveDate),
    /// Anything else, treated as a plain last price.
    Other,
}

fn stamp(ts: i64) -> Stamp {
    let Some(day) = market::et_date(ts) else {
        return Stamp::Other;
    };
    if let Some(open) = market::windows(day)
        .into_iter()
        .find(|w| w.session == Session::Open)
        // 09:30 plus six and a half hours is 16:00 on a half day too.
        && (ts == open.end || ts == open.start + 6 * 3_600 + 1_800)
    {
        return Stamp::Official(day);
    }
    match market::window_at(ts) {
        Some(w) => Stamp::Live(w.session, day),
        None => Stamp::Other,
    }
}

/// The quote for a stock, from its `/iex/` row read against the daily
/// history.
///
/// Before and after the regular session the IEX price is an extended
/// print, and the headline stays the session's close, the way broker
/// screens read: premarket, that is the last close in the daily history;
/// after the bell, today's close from the history or, until Tiingo has it,
/// `close_today`. Volume is only ever the official row's or the daily
/// history's: during the day the row counts IEX alone, a few percent of
/// the tape.
fn stock_quote(
    symbol: &str,
    top: &IexTop,
    (ts, price): (i64, f64),
    stamp: Stamp,
    daily: &[Candle],
    close_today: Option<f64>,
    now: DateTime<Utc>,
) -> Quote {
    let top_range = top
        .low
        .zip(top.high)
        .filter(|(lo, hi)| lo.is_finite() && hi.is_finite() && *lo > 0.0 && lo <= hi);
    let mut q = Quote {
        symbol: symbol.to_string(),
        price,
        prev_close: top.prev_close.filter(|p| p.is_finite() && *p > 0.0),
        currency: Some("USD".into()),
        extended: None,
        timing: QuoteTiming {
            regular: Some(ts),
            regular_feed: PriceFeed::Iex,
            ..Default::default()
        },
        fifty_two_week: None,
        day_range: top_range,
        volume: None,
    };
    let before = |day: NaiveDate| {
        daily
            .iter()
            .filter(move |c| market::et_date(c.ts).is_some_and(|d| d < day))
    };
    let extended = |q: &mut Quote| {
        q.extended = Some(price);
        q.timing.extended = Some(ts);
        q.timing.extended_feed = PriceFeed::Iex;
        q.timing.extended_reference = Some(q.price);
    };
    match stamp {
        Stamp::Official(_) => {
            q.timing.regular_feed = PriceFeed::Tiingo;
            q.volume = top.volume.filter(|v| v.is_finite() && *v > 0.0);
        }
        Stamp::Live(Session::Pre, day) => {
            let prior: Vec<&Candle> = before(day).collect();
            if let Some(last) = prior.last() {
                q.price = last.close;
                q.prev_close = prior.len().checked_sub(2).map(|i| prior[i].close);
                q.day_range = Some((last.low, last.high));
                q.volume = last.volume;
                q.timing.regular = market::et_date(last.ts).and_then(close_ts);
                q.timing.regular_feed = PriceFeed::Tiingo;
                extended(&mut q);
            } else if let Some(prev) = q.prev_close {
                q.price = prev;
                q.prev_close = None;
                q.day_range = None;
                q.timing.regular = None;
                q.timing.regular_feed = PriceFeed::Tiingo;
                extended(&mut q);
            }
        }
        Stamp::Live(Session::Post, day) => {
            let bar = daily.iter().find(|c| market::et_date(c.ts) == Some(day));
            if let Some(close) = bar.map(|b| b.close).or(close_today) {
                q.price = close;
                q.timing.regular = close_ts(day);
                if let Some(bar) = bar {
                    q.day_range = Some((bar.low, bar.high));
                    q.volume = bar.volume;
                    q.timing.regular_feed = PriceFeed::Tiingo;
                }
                extended(&mut q);
            }
            if q.prev_close.is_none() {
                q.prev_close = before(day).next_back().map(|c| c.close);
            }
        }
        Stamp::Live(..) | Stamp::Other => {}
    }
    let year_ago = now.timestamp() - 365 * 86_400;
    q.fifty_two_week = daily
        .iter()
        .filter(|c| c.ts >= year_ago)
        .map(|c| (c.low, c.high))
        .chain(q.day_range)
        .reduce(|(lo, hi), (l, h)| (lo.min(l), hi.max(h)));
    q
}

/// The after-hours print behind an official row. The row replaces the
/// live one after the session and says nothing of what traded after the
/// bell, which the IEX bars still carry until 17:30 ET: the last of them
/// is the extended print, stamped with its bar's start like Yahoo's, so
/// the rail keeps the late move overnight as it does on the other
/// sources.
fn late_print(quote: &mut Quote, bars: &[Candle], day: NaiveDate) {
    let Some(bar) = bars.iter().rev().find(|c| {
        c.feed == PriceFeed::Iex
            && market::et_date(c.ts) == Some(day)
            && market::window_at(c.ts).is_some_and(|w| w.session == Session::Post)
    }) else {
        return;
    };
    quote.extended = Some(bar.close);
    quote.timing.extended = Some(bar.ts);
    quote.timing.extended_feed = PriceFeed::Iex;
    quote.timing.extended_reference = Some(quote.price);
}

/// Today's bar on a daily chart, while the daily history does not have it
/// yet: the official row once it is in, the live IEX session until then.
fn today_bar(top: &IexTop, stamp: Stamp, quote: &Quote, daily: &[Candle]) -> Option<Candle> {
    let (day, feed, volume) = match stamp {
        Stamp::Official(day) => (
            day,
            PriceFeed::Tiingo,
            top.volume.filter(|v| v.is_finite() && *v > 0.0),
        ),
        Stamp::Live(Session::Open, day) => (day, PriceFeed::Iex, None),
        // Only once a close was found: otherwise the quote's price is the
        // after-hours one, and it would pass for the day's close.
        Stamp::Live(Session::Post, day) if quote.extended.is_some() => (day, PriceFeed::Iex, None),
        _ => return None,
    };
    if daily.iter().any(|c| market::et_date(c.ts) == Some(day)) {
        return None;
    }
    let close = quote.price;
    let mut bar = candle_from_ohlc(
        day_ts(day),
        top.open,
        top.high,
        top.low,
        Some(close),
        volume,
    )?;
    // IEX's own high and low need not bracket a close taken elsewhere.
    bar.high = bar.high.max(close);
    bar.low = bar.low.min(close);
    bar.feed = feed;
    Some(bar)
}

/// Carry a live price into cached bars between refreshes: it moves the last
/// bar while it falls in that bar's bucket and opens the next bucket once
/// it does not, so the chart keeps pace with the quote, and a bar's high
/// and low keep what earlier polls saw. The next refresh replaces both with
/// the feed's own bars. A new bucket has no volume until then.
fn splice(
    candles: &mut Vec<Candle>,
    symbol: &str,
    ts: i64,
    price: f64,
    secs: i64,
    feed: PriceFeed,
) {
    if !price.is_finite() || price <= 0.0 || secs <= 0 {
        return;
    }
    let start = if market::is_us_equity(symbol) {
        let Some(w) = market::window_at(ts) else {
            return;
        };
        w.start + (ts - w.start) / secs * secs
    } else {
        ts - ts.rem_euclid(secs)
    };
    match candles.last_mut() {
        Some(last) if start < last.ts => {}
        Some(last) if start == last.ts => {
            if last.feed == feed {
                last.close = price;
                last.high = last.high.max(price);
                last.low = last.low.min(price);
            }
        }
        _ => candles.push(Candle {
            ts: start,
            open: price,
            high: price,
            low: price,
            close: price,
            volume: None,
            feed,
        }),
    }
}

/// Tiingo's daily rows carry raw prices plus the split that took effect on
/// each date (`splitFactor` 15 on O'Reilly's 10 June 2025 row). Earlier
/// rows are divided by every later split and their volume multiplied,
/// which is split-only adjustment. The `adj*` columns fold dividends in as
/// well, and prices adjusted for those no longer meet the live quote.
fn split_adjusted(rows: Vec<DailyBar>) -> Vec<Candle> {
    let mut out = Vec::with_capacity(rows.len());
    let mut factor = 1.0;
    for row in rows.into_iter().rev() {
        if let Some(day) = row.date.get(..10).and_then(|d| d.parse::<NaiveDate>().ok()) {
            let scale = |p: Option<f64>| p.map(|p| p / factor);
            if let Some(mut c) = candle_from_ohlc(
                day_ts(day),
                scale(row.open),
                scale(row.high),
                scale(row.low),
                scale(row.close),
                row.volume.map(|v| v * factor),
            ) {
                c.feed = PriceFeed::Tiingo;
                out.push(c);
            }
        }
        if let Some(split) = row.split_factor.filter(|f| f.is_finite() && *f > 0.0) {
            factor *= split;
        }
    }
    out.reverse();
    out
}

/// Intraday and crypto rows; unparseable ones are dropped.
fn candles(rows: Vec<Bar>, feed: PriceFeed) -> Vec<Candle> {
    let mut out: Vec<Candle> = rows
        .into_iter()
        .filter_map(|b| {
            let mut c =
                candle_from_ohlc(parse_ts(&b.date)?, b.open, b.high, b.low, b.close, b.volume)?;
            c.feed = feed;
            Some(c)
        })
        .collect();
    sort_ascending(&mut out);
    out
}

/// The regular close of `day` from intraday bars, when they reach it.
fn last_regular_close(bars: &[Candle], day: NaiveDate) -> Option<f64> {
    bars.iter()
        .rev()
        .find(|c| {
            market::et_date(c.ts) == Some(day)
                && market::window_at(c.ts).is_some_and(|w| w.session == Session::Open)
        })
        .map(|c| c.close)
}

/// A daily bar's timestamp: the opening bell of its day, so its ET date is
/// the trading date whichever offset is in force. Tiingo stamps the date
/// at 00:00 UTC, which is still the previous evening in New York.
fn day_ts(day: NaiveDate) -> i64 {
    market::windows(day)
        .into_iter()
        .find(|w| w.session == Session::Open)
        .map_or_else(
            || day.and_hms_opt(16, 0, 0).unwrap().and_utc().timestamp(),
            |w| w.start,
        )
}

/// When the regular session of `day` closed.
fn close_ts(day: NaiveDate) -> Option<i64> {
    market::windows(day)
        .into_iter()
        .find(|w| w.session == Session::Open)
        .map(|w| w.end)
}

/// `startDate` for a window reaching `secs` back from now.
fn start_date(secs: i64) -> String {
    (Utc::now() - chrono::Duration::seconds(secs))
        .format("%Y-%m-%d")
        .to_string()
}

/// Hours arrive aligned to the clock, which puts 09:00 and 09:30 in one
/// bar; halves keep the opening bell's boundary (see `normalize_sessions`).
fn fetch_interval(interval: Interval) -> Interval {
    match interval {
        Interval::M60 => Interval::M30,
        other => other,
    }
}

fn resample(interval: Interval) -> &'static str {
    match interval {
        Interval::M1 => "1min",
        Interval::M2 => "2min",
        Interval::M5 => "5min",
        Interval::M15 => "15min",
        Interval::M30 => "30min",
        Interval::M60 => "1hour",
        Interval::D1 => "1day",
    }
}

fn parse_ts(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.timestamp())
}

const INVALID_KEY: &str = "invalid Tiingo key, press s to update it (keys at tiingo.com)";

/// What every used-up allowance message says, whichever allowance it is.
const USED_UP: &str = "allowance used up";

/// A refusal that the next poll would get again: the allowance resets on
/// the hour or the day, and a wrong key stays wrong until it is replaced,
/// which builds a new source.
fn lasting(error: &str) -> bool {
    error.contains(USED_UP) || error.contains(INVALID_KEY)
}

fn error_message(status: StatusCode, body: &str, symbol: Option<&str>) -> String {
    let msg = http::body_message(body).unwrap_or_else(|| http::snippet(body));
    describe(Some(status), &msg, symbol)
}

/// A refusal in the reader's terms. `status` is None for one that came
/// back as a 200 with a `detail` body.
fn describe(status: Option<StatusCode>, msg: &str, symbol: Option<&str>) -> String {
    let lower = msg.to_lowercase();
    // "You have run over your hourly request allocation", and the daily and
    // monthly-symbol variants of it.
    if status == Some(StatusCode::TOO_MANY_REQUESTS) || lower.contains("allocation") {
        let which = if lower.contains("hour") {
            "hourly request"
        } else if lower.contains("symbol") {
            "monthly symbol"
        } else if lower.contains("day") || lower.contains("daily") {
            "daily request"
        } else {
            "request"
        };
        return http::rate_limit_msg(&format!(
            "tiingo {which} {USED_UP} (the free plan allows 50 requests an hour)"
        ));
    }
    let code = status.map(|s| s.as_u16());
    if lower.contains("token") && matches!(code, None | Some(401 | 403))
        || (msg.is_empty() && matches!(code, Some(401 | 403)))
    {
        return INVALID_KEY.into();
    }
    if let Some(symbol) = symbol
        && lower.contains("not found")
        && matches!(code, None | Some(404))
    {
        return http::unknown_symbol(symbol, Some(HINT)).to_string();
    }
    match (status, msg.is_empty()) {
        (Some(status), true) => format!("tiingo API {status}"),
        (Some(status), false) => format!("tiingo API {status}: {msg}"),
        (None, _) => format!("tiingo: {msg}"),
    }
}

// ---------------------------------------------------------------------------
// API shapes (tolerant subset).

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct IexTop {
    ticker: String,
    timestamp: Option<String>,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    /// Tiingo's reference price; `last` itself needs an IEX agreement.
    tngo_last: Option<f64>,
    prev_close: Option<f64>,
    volume: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CryptoTop {
    ticker: String,
    top_of_book_data: Vec<CryptoBook>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CryptoBook {
    last_price: Option<f64>,
    last_sale_timestamp: Option<String>,
    quote_timestamp: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Bar {
    date: String,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    volume: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct DailyBar {
    date: String,
    open: Option<f64>,
    high: Option<f64>,
    low: Option<f64>,
    close: Option<f64>,
    volume: Option<f64>,
    split_factor: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CryptoPrices {
    ticker: String,
    price_data: Vec<Bar>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> i64 {
        DateTime::parse_from_rfc3339(s).unwrap().timestamp()
    }

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    fn top(json: &str) -> IexTop {
        serde_json::from_str(json).unwrap()
    }

    /// AAPL's `/iex/` row at 01:00 ET on 30 September 2026, verbatim but
    /// for the null fields: the official daily bar, stamped 16:00 ET.
    const OFFICIAL: &str = r#"{"ticker":"AAPL","timestamp":"2026-09-29T20:00:00+00:00",
        "open":336.965,"high":337.085,"low":328.7,"mid":null,"tngoLast":329.4,"last":null,
        "prevClose":338.4,"volume":38478037}"#;

    /// The same row at 15:00 ET that day: a live reference price, IEX's
    /// own volume.
    const LIVE: &str = r#"{"ticker":"AAPL","timestamp":"2026-09-29T15:00:07.078869649-04:00",
        "open":337.15,"high":337.28,"low":330.45,"mid":330.965,"tngoLast":330.965,"last":null,
        "prevClose":338.4,"volume":860648.0}"#;

    /// Split-adjusted daily history as `split_adjusted` returns it.
    fn history() -> Vec<Candle> {
        split_adjusted(
            serde_json::from_str(
                r#"[
                {"date":"2026-09-25T00:00:00.000Z","open":336.04,"high":341.67,"low":334.53,"close":341.07,"volume":30002507,"splitFactor":1.0},
                {"date":"2026-09-28T00:00:00.000Z","open":340.37,"high":342.988,"low":338.04,"close":338.4,"volume":32820848,"splitFactor":1.0}
            ]"#,
            )
            .unwrap(),
        )
    }

    #[test]
    fn symbols_map_to_tiingo_tickers_and_the_rest_is_left_out() {
        let stock = |t: &str| Some(Asset::Stock(t.into()));
        assert_eq!(asset("AAPL"), stock("AAPL"));
        assert_eq!(asset("BRK.B"), stock("BRK-B"));
        assert_eq!(asset("BRK-B"), stock("BRK-B"));
        assert_eq!(asset("BTC-USD"), Some(Asset::Crypto("btcusd".into())));
        assert_eq!(asset("ETH-USDT"), Some(Asset::Crypto("ethusdt".into())));
        for foreign in ["BINANCE:BTCUSDT", "VOD.L", "^GSPC", "EURUSD=X", "-USD"] {
            assert_eq!(asset(foreign), None, "{foreign}");
        }
    }

    /// Verbatim rows around O'Reilly's 15-for-1 split: `splitFactor` sits
    /// on the first post-split date, and the rows before it are divided.
    #[test]
    fn daily_rows_are_split_adjusted_without_dividends() {
        let rows: Vec<DailyBar> = serde_json::from_str(
            r#"[
            {"date":"2025-06-06T00:00:00.000Z","open":1370.0,"high":1380.0,"low":1365.0,"close":1377.72,"volume":333777,"adjClose":91.848,"splitFactor":1.0,"divCash":0.0},
            {"date":"2025-06-09T00:00:00.000Z","open":1375.0,"high":1376.0,"low":1340.0,"close":1348.1,"volume":372411,"adjClose":89.8733333333,"splitFactor":1.0,"divCash":0.0},
            {"date":"2025-06-10T00:00:00.000Z","open":90.0,"high":92.0,"low":89.5,"close":91.71,"volume":5190679,"adjClose":91.71,"splitFactor":15.0,"divCash":0.0}
        ]"#,
        )
        .unwrap();
        let c = split_adjusted(rows);
        assert_eq!(c.len(), 3);
        assert!((c[0].close - 91.848).abs() < 1e-9);
        assert!((c[1].close - 89.873_333).abs() < 1e-5);
        assert_eq!(c[1].volume, Some(372_411.0 * 15.0));
        assert_eq!(c[2].close, 91.71);
        assert_eq!(c[2].volume, Some(5_190_679.0));
        assert!(c.iter().all(|c| c.feed == PriceFeed::Tiingo));
        // Stamped inside the trading day in New York, not at 00:00 UTC,
        // which is the evening before there.
        assert_eq!(market::et_date(c[2].ts), Some(date("2025-06-10")));
        assert!(c.windows(2).all(|w| w[0].ts < w[1].ts));
    }

    #[test]
    fn a_row_is_read_by_its_timestamp() {
        let day = date("2026-09-29");
        assert_eq!(stamp(at("2026-09-29T20:00:00Z")), Stamp::Official(day));
        assert_eq!(
            stamp(at("2026-09-29T19:00:07Z")),
            Stamp::Live(Session::Open, day)
        );
        assert_eq!(
            stamp(at("2026-09-29T12:30:00Z")),
            Stamp::Live(Session::Pre, day)
        );
        assert_eq!(
            stamp(at("2026-09-29T20:30:00Z")),
            Stamp::Live(Session::Post, day)
        );
        // A second past the bell is an after-hours price, not the close.
        assert_eq!(
            stamp(at("2026-09-29T20:00:01Z")),
            Stamp::Live(Session::Post, day)
        );
        assert_eq!(stamp(at("2026-09-27T15:00:00Z")), Stamp::Other, "Sunday");
    }

    #[test]
    fn the_official_row_is_the_close_with_the_whole_market_volume() {
        let row = top(OFFICIAL);
        let ts = at("2026-09-29T20:00:00Z");
        let now = at("2026-09-30T05:00:00Z");
        let q = stock_quote(
            "AAPL",
            &row,
            (ts, 329.4),
            stamp(ts),
            &history(),
            None,
            DateTime::from_timestamp(now, 0).unwrap(),
        );
        assert_eq!(q.price, 329.4);
        assert_eq!(q.prev_close, Some(338.4));
        assert_eq!(q.volume, Some(38_478_037.0));
        assert_eq!(q.day_range, Some((328.7, 337.085)));
        assert_eq!(q.timing.regular_feed, PriceFeed::Tiingo);
        assert_eq!(q.extended, None);
        // History plus the day itself.
        assert_eq!(q.fifty_two_week, Some((328.7, 342.988)));
    }

    #[test]
    fn a_live_row_keeps_iex_volume_out_of_the_quote() {
        let row = top(LIVE);
        let ts = at("2026-09-29T19:00:07Z");
        let q = stock_quote(
            "AAPL",
            &row,
            (ts, 330.965),
            stamp(ts),
            &history(),
            None,
            Utc::now(),
        );
        assert_eq!(q.price, 330.965);
        assert_eq!(q.prev_close, Some(338.4));
        assert_eq!(q.volume, None, "860K is IEX's count, not the market's");
        assert_eq!(q.timing.regular, Some(ts));
        assert_eq!(q.timing.regular_feed, PriceFeed::Iex);
        assert_eq!(q.extended, None);
    }

    /// Premarket the headline is the last close in the history and the IEX
    /// price is the extended print beside it, whatever the row says about
    /// the previous close.
    #[test]
    fn premarket_prints_sit_beside_the_last_close() {
        let row = top(r#"{"ticker":"AAPL","tngoLast":340.5,"prevClose":null}"#);
        let ts = at("2026-09-29T12:30:00Z");
        let q = stock_quote(
            "AAPL",
            &row,
            (ts, 340.5),
            stamp(ts),
            &history(),
            None,
            Utc::now(),
        );
        assert_eq!(q.price, 338.4, "Monday's close");
        assert_eq!(q.prev_close, Some(341.07), "Friday's close");
        assert_eq!(q.volume, Some(32_820_848.0));
        assert_eq!(q.day_range, Some((338.04, 342.988)));
        assert_eq!(q.timing.regular_feed, PriceFeed::Tiingo);
        assert_eq!(q.extended, Some(340.5));
        assert_eq!(q.timing.extended, Some(ts));
        assert_eq!(q.timing.extended_feed, PriceFeed::Iex);
        assert_eq!(q.extended_reference(), 338.4);
        assert_eq!(
            q.extended_price_at(DateTime::from_timestamp(ts + 60, 0).unwrap()),
            Some(340.5)
        );
    }

    #[test]
    fn after_the_bell_the_close_leads_until_the_official_row() {
        let row =
            top(r#"{"ticker":"AAPL","tngoLast":330.1,"prevClose":338.4,"low":328.7,"high":337.1}"#);
        let ts = at("2026-09-29T20:30:00Z");
        let q = stock_quote(
            "AAPL",
            &row,
            (ts, 330.1),
            stamp(ts),
            &history(),
            Some(329.57),
            Utc::now(),
        );
        assert_eq!(q.price, 329.57);
        assert_eq!(q.prev_close, Some(338.4));
        assert_eq!(q.timing.regular_feed, PriceFeed::Iex, "provisional");
        assert_eq!(q.extended, Some(330.1));
        assert_eq!(q.extended_reference(), 329.57);
        assert_eq!(q.volume, None);
        // Without any close the IEX price stays the price.
        let bare = stock_quote("AAPL", &row, (ts, 330.1), stamp(ts), &[], None, Utc::now());
        assert_eq!((bare.price, bare.extended), (330.1, None));
    }

    #[test]
    fn an_official_row_keeps_the_after_hours_print_from_the_bars() {
        let ts = at("2026-09-29T20:00:00Z");
        let mut q = stock_quote(
            "AAPL",
            &top(OFFICIAL),
            (ts, 329.4),
            stamp(ts),
            &history(),
            None,
            Utc::now(),
        );
        let bar = |at: i64, close: f64, feed| Candle {
            ts: at,
            close,
            open: close,
            high: close,
            low: close,
            volume: None,
            feed,
        };
        let day = date("2026-09-29");
        let regular = bar(ts - 300, 329.57, PriceFeed::Iex);
        late_print(&mut q, &[regular], day);
        assert_eq!(q.extended, None, "no bar after the bell");
        let bars = [
            regular,
            bar(ts, 329.24, PriceFeed::Iex),
            bar(ts + 300, 329.3, PriceFeed::Tiingo),
        ];
        late_print(&mut q, &bars, day);
        assert_eq!(q.extended, Some(329.24));
        assert_eq!(q.timing.extended, Some(ts));
        assert_eq!(q.extended_reference(), 329.4);
        assert_eq!(
            q.extended_price_at(DateTime::from_timestamp(ts + 9 * 3_600, 0).unwrap()),
            Some(329.24),
            "still the latest print at 01:00 ET"
        );
    }

    #[test]
    fn a_daily_chart_gets_today_until_the_history_has_it() {
        let daily = history();
        let official = top(OFFICIAL);
        let ts = at("2026-09-29T20:00:00Z");
        let q = stock_quote(
            "AAPL",
            &official,
            (ts, 329.4),
            stamp(ts),
            &daily,
            None,
            Utc::now(),
        );
        let bar = today_bar(&official, stamp(ts), &q, &daily).unwrap();
        assert_eq!(market::et_date(bar.ts), Some(date("2026-09-29")));
        assert_eq!((bar.open, bar.close), (336.965, 329.4));
        assert_eq!(bar.volume, Some(38_478_037.0));
        assert_eq!(bar.feed, PriceFeed::Tiingo);

        let live = top(LIVE);
        let ts = at("2026-09-29T19:00:07Z");
        let q = stock_quote(
            "AAPL",
            &live,
            (ts, 330.965),
            stamp(ts),
            &daily,
            None,
            Utc::now(),
        );
        let bar = today_bar(&live, stamp(ts), &q, &daily).unwrap();
        assert_eq!(bar.volume, None);
        assert_eq!(bar.feed, PriceFeed::Iex, "so the live quote moves it");

        let mut through_today = daily.clone();
        through_today.push(Candle {
            ts: day_ts(date("2026-09-29")),
            ..daily[1]
        });
        assert!(today_bar(&live, stamp(ts), &q, &through_today).is_none());
        let pre = at("2026-09-29T12:30:00Z");
        assert!(today_bar(&live, stamp(pre), &q, &daily).is_none());

        // After the bell the bar needs a close; the after-hours price
        // must not stand in for one.
        let post = at("2026-09-29T20:30:00Z");
        let row =
            top(r#"{"ticker":"AAPL","tngoLast":330.1,"open":336.9,"high":337.1,"low":328.7}"#);
        let unknown = stock_quote(
            "AAPL",
            &row,
            (post, 330.1),
            stamp(post),
            &daily,
            None,
            Utc::now(),
        );
        assert!(today_bar(&row, stamp(post), &unknown, &daily).is_none());
        let known = stock_quote(
            "AAPL",
            &row,
            (post, 330.1),
            stamp(post),
            &daily,
            Some(329.57),
            Utc::now(),
        );
        let bar = today_bar(&row, stamp(post), &known, &daily).unwrap();
        assert_eq!(bar.close, 329.57);
    }

    #[test]
    fn live_prices_move_the_last_bar_or_open_the_next() {
        let open = at("2026-09-29T19:00:00Z");
        let bar = Candle {
            ts: open,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: Some(10.0),
            feed: PriceFeed::Iex,
        };
        let mut bars = vec![bar];
        splice(&mut bars, "AAPL", open + 60, 102.0, 300, PriceFeed::Iex);
        splice(&mut bars, "AAPL", open + 120, 100.8, 300, PriceFeed::Iex);
        assert_eq!(bars.len(), 1);
        assert_eq!((bars[0].high, bars[0].close), (102.0, 100.8), "high kept");
        assert_eq!(bars[0].volume, Some(10.0));

        splice(&mut bars, "AAPL", open + 301, 101.2, 300, PriceFeed::Iex);
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[1].ts, open + 300);
        assert_eq!(bars[1].volume, None);

        // An older print, another feed, or no session: nothing moves.
        splice(&mut bars, "AAPL", open + 10, 90.0, 300, PriceFeed::Iex);
        splice(&mut bars, "AAPL", open + 310, 90.0, 300, PriceFeed::Tiingo);
        splice(
            &mut bars,
            "AAPL",
            at("2026-09-30T05:00:00Z"),
            90.0,
            300,
            PriceFeed::Iex,
        );
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[1].low, 101.2);

        // Crypto buckets follow the clock.
        let mut btc = Vec::new();
        splice(
            &mut btc,
            "BTC-USD",
            at("2026-09-30T05:03:20Z"),
            83_000.0,
            300,
            PriceFeed::Tiingo,
        );
        assert_eq!(btc[0].ts, at("2026-09-30T05:00:00Z"));
    }

    #[test]
    fn refusals_say_what_ran_out() {
        let msg = describe(
            Some(StatusCode::TOO_MANY_REQUESTS),
            "You have run over your hourly request allocation. Please upgrade at https://api.tiingo.com/pricing to have your limits increased.",
            Some("AAPL"),
        );
        assert!(
            msg.starts_with("tiingo hourly request allowance used up"),
            "{msg}"
        );
        assert!(msg.contains("50 requests an hour"), "{msg}");
        let daily = describe(
            None,
            "You have run over your daily request allocation.",
            None,
        );
        assert!(daily.contains("daily request"), "{daily}");
        assert!(
            describe(Some(StatusCode::FORBIDDEN), "Invalid token.", None)
                .starts_with("invalid Tiingo key")
        );
        assert!(
            describe(Some(StatusCode::FORBIDDEN), "Please supply a token", None)
                .starts_with("invalid Tiingo key")
        );
        assert_eq!(
            describe(
                Some(StatusCode::NOT_FOUND),
                "Error: Ticker 'ZZZZQ' not found",
                Some("ZZZZQ")
            ),
            http::unknown_symbol("ZZZZQ", Some(HINT)).to_string()
        );
        assert_eq!(
            describe(Some(StatusCode::BAD_REQUEST), "bad range", None),
            "tiingo API 400 Bad Request: bad range"
        );
    }

    type Seen = std::sync::Arc<Mutex<Vec<String>>>;

    /// A stand-in API on a local port: `route` answers each request path
    /// with a status line and a body, and every path asked is recorded.
    /// It also checks the key travels in the header Tiingo reads.
    async fn serve(route: fn(&str) -> (&'static str, String)) -> (String, Seen) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Seen::default();
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let log = log.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut byte = [0; 1];
                    while !request.ends_with(b"\r\n\r\n") {
                        stream.read_exact(&mut byte).await.unwrap();
                        request.push(byte[0]);
                    }
                    let text = String::from_utf8(request).unwrap();
                    assert!(text.contains("authorization: Token test-key"), "{text}");
                    let path = text.split_whitespace().nth(1).unwrap().to_string();
                    log.lock().unwrap().push(path.clone());
                    let (status, body) = route(&path);
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).await.unwrap();
                });
            }
        });
        (base, seen)
    }

    fn count(seen: &Seen, prefix: &str) -> usize {
        seen.lock()
            .unwrap()
            .iter()
            .filter(|p| p.starts_with(prefix))
            .count()
    }

    /// Every symbol fetched at once after one `begin_cycle`, the way the
    /// poller does it.
    async fn cycle(
        source: &std::sync::Arc<Tiingo>,
        symbols: &[&str],
    ) -> HashMap<String, Result<TickerData>> {
        let symbols: Vec<String> = symbols.iter().map(|s| s.to_string()).collect();
        source.begin_cycle(&symbols);
        let mut set = tokio::task::JoinSet::new();
        for symbol in symbols {
            let source = source.clone();
            set.spawn(async move {
                let res = source
                    .fetch(&symbol, Range::D5, Interval::M5, Sessions::Extended)
                    .await;
                (symbol, res)
            });
        }
        let mut results = HashMap::new();
        while let Some(joined) = set.join_next().await {
            let (symbol, res) = joined.unwrap();
            results.insert(symbol, res);
        }
        results
    }

    fn api(path: &str) -> (&'static str, String) {
        if path.starts_with("/iex/?") {
            (
                "200 OK",
                format!("[{OFFICIAL},{}]", OFFICIAL.replace("AAPL", "NVDA")),
            )
        } else if path.starts_with("/iex/") {
            (
                "200 OK",
                r#"[{"date":"2026-09-29T19:50:00.000Z","open":329.4,"high":329.9,"low":329.1,"close":329.7,"volume":100.0},
                    {"date":"2026-09-29T19:55:00.000Z","open":329.7,"high":329.9,"low":328.7,"close":329.6,"volume":200.0}]"#
                    .to_string(),
            )
        } else if path.starts_with("/tiingo/daily/ZZZZ/") {
            (
                "404 Not Found",
                r#"{"detail":"Error: Ticker 'ZZZZ' not found"}"#.to_string(),
            )
        } else if path.starts_with("/tiingo/daily/") {
            (
                "200 OK",
                r#"[{"date":"2026-09-28T00:00:00.000Z","open":340.37,"high":342.988,"low":338.04,"close":338.4,"volume":32820848,"splitFactor":1.0}]"#
                    .to_string(),
            )
        } else if path.starts_with("/tiingo/crypto/top") {
            (
                "200 OK",
                r#"[{"ticker":"btcusd","topOfBookData":[{"lastPrice":83000.0,"lastSaleTimestamp":"2026-09-30T04:53:37.014000+00:00"}]}]"#
                    .to_string(),
            )
        } else {
            (
                "200 OK",
                r#"[{"ticker":"btcusd","priceData":[{"date":"2026-09-29T23:55:00+00:00","open":82900.0,"high":83100.0,"low":82800.0,"close":82950.0,"volume":1.5}]}]"#
                    .to_string(),
            )
        }
    }

    /// The whole watchlist is quoted by one `/iex/` request and one crypto
    /// request, an unknown ticker costs no extra quote, and the next cycle
    /// refetches only the quotes while bars and daily history come from
    /// memory.
    #[tokio::test]
    async fn one_quote_request_covers_a_cycle() {
        let (base, seen) = serve(api).await;
        let source = std::sync::Arc::new(Tiingo::at("test-key".into(), base).unwrap());
        for round in 1..=2 {
            let results = cycle(&source, &["AAPL", "NVDA", "ZZZZ", "BTC-USD", "VOD.L"]).await;
            let aapl = results["AAPL"].as_ref().unwrap();
            assert_eq!(aapl.quote.price, 329.4);
            assert_eq!(aapl.candles.len(), 2);
            assert!(aapl.candles.iter().all(|c| c.feed == PriceFeed::Iex));
            assert_eq!(results["NVDA"].as_ref().unwrap().quote.symbol, "NVDA");
            let btc = results["BTC-USD"].as_ref().unwrap();
            assert_eq!(btc.quote.price, 83_000.0);
            assert_eq!(btc.quote.currency.as_deref(), Some("USD"));
            for unknown in ["ZZZZ", "VOD.L"] {
                let err = results[unknown].as_ref().unwrap_err().to_string();
                assert!(err.contains("unknown symbol"), "{unknown}: {err}");
            }
            assert_eq!(
                count(&seen, "/iex/?"),
                round,
                "one stock quote request a cycle"
            );
            assert_eq!(count(&seen, "/tiingo/crypto/top"), round);
            // Cached from the first cycle on: AAPL, NVDA, ZZZZ and BTC.
            assert_eq!(
                count(&seen, "/tiingo/daily/"),
                3,
                "{:?}",
                seen.lock().unwrap()
            );
            assert_eq!(count(&seen, "/tiingo/crypto/prices"), 1);
        }
        let quoted = seen
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.starts_with("/iex/?"))
            .cloned()
            .unwrap();
        assert!(!quoted.contains("VOD"), "{quoted}");
    }

    /// A used-up allowance is answered from memory for a minute: asking
    /// every poll would spend the whole wait on refused requests. A plain
    /// failure is asked again on the next poll.
    #[tokio::test]
    async fn a_used_up_allowance_is_not_asked_again_every_poll() {
        fn used_up(path: &str) -> (&'static str, String) {
            if path.starts_with("/iex/?") {
                (
                    "429 Too Many Requests",
                    r#"{"detail":"Error: You have run over your hourly request allocation. Please upgrade at https://api.tiingo.com/pricing to have your limits increased."}"#
                        .to_string(),
                )
            } else {
                api(path)
            }
        }
        fn broken(path: &str) -> (&'static str, String) {
            if path.starts_with("/iex/?") {
                ("500 Internal Server Error", String::new())
            } else {
                api(path)
            }
        }

        let (base, seen) = serve(used_up).await;
        let source = std::sync::Arc::new(Tiingo::at("test-key".into(), base).unwrap());
        for _ in 0..3 {
            let results = cycle(&source, &["AAPL", "NVDA"]).await;
            for (symbol, res) in &results {
                let err = res.as_ref().unwrap_err().to_string();
                assert!(
                    err.contains("hourly request allowance used up"),
                    "{symbol}: {err}"
                );
            }
        }
        assert_eq!(count(&seen, "/iex/?"), 1);
        // A symbol the refused request did not cover is still asked for.
        let _ = cycle(&source, &["AAPL", "NVDA", "CRWV"]).await;
        assert_eq!(count(&seen, "/iex/?"), 2);

        let (base, seen) = serve(broken).await;
        let source = std::sync::Arc::new(Tiingo::at("test-key".into(), base).unwrap());
        for _ in 0..2 {
            let results = cycle(&source, &["AAPL"]).await;
            assert!(results["AAPL"].is_err());
        }
        assert_eq!(count(&seen, "/iex/?"), 2);
    }

    /// Live end-to-end check against the real API (about 7 requests).
    /// Run: TIINGO_API_KEY=… cargo test live_tiingo -- --ignored
    #[tokio::test]
    #[ignore = "live API call; needs TIINGO_API_KEY"]
    async fn live_tiingo_smoke() {
        let key = std::env::var("TIINGO_API_KEY").expect("set TIINGO_API_KEY");
        let source = Tiingo::new(key).unwrap();
        let symbols = vec!["AAPL".to_string(), "BTC-USD".to_string()];
        for (range, interval) in [(Range::D5, Interval::M15), (Range::Y1, Interval::D1)] {
            source.begin_cycle(&symbols);
            for symbol in &symbols {
                let data = source
                    .fetch(symbol, range, interval, Sessions::Extended)
                    .await
                    .unwrap();
                assert!(data.quote.price > 0.0, "{symbol}: no price");
                assert!(
                    data.quote.prev_close.is_some(),
                    "{symbol}: no previous close"
                );
                assert!(!data.candles.is_empty(), "{symbol}: no candles");
                assert!(
                    data.candles.windows(2).all(|w| w[0].ts < w[1].ts),
                    "{symbol}: candles not ascending"
                );
            }
        }
    }
}
