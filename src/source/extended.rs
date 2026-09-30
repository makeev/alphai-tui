//! Pre and post market data borrowed from a whole-market feed.
//!
//! IEX sources see one exchange (Alpaca's free feed, Tiingo), where next to
//! nothing trades before the open, and Finnhub sees none of it. The
//! `extended_source` choice puts Alpaca's consolidated tape or Yahoo's
//! under any price source: `Borrowed` wraps the source, cached by symbol
//! and candle window to one refresh a minute. Failures retain the last
//! good session and never fail the price poll.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};

use super::{DataSource, alpaca::Alpaca, normalize_sessions, yahoo::Yahoo};
use crate::config::Config;
use crate::domain::{Candle, Interval, PriceFeed, Quote, Range, Sessions, TickerData};
use crate::market::{self, Session};

/// Where pre and post market prices come from: the `extended_source`
/// config key and its settings row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExtendedSource {
    /// The whole market, when the price source sees less of it.
    #[default]
    Auto,
    /// Only what the price source reports itself.
    Same,
    Alpaca,
    Yahoo,
}

impl ExtendedSource {
    pub const ALL: [Self; 4] = [Self::Auto, Self::Same, Self::Alpaca, Self::Yahoo];

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Same => "same",
            Self::Alpaca => "alpaca",
            Self::Yahoo => "yahoo",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.name().eq_ignore_ascii_case(s.trim()))
    }

    pub fn step(self, dir: isize) -> Self {
        let i = Self::ALL.iter().position(|c| *c == self).unwrap_or(0) as isize;
        Self::ALL[(i + dir).rem_euclid(Self::ALL.len() as isize) as usize]
    }

    /// The configured choice. A value `config::resolve` warned about reads
    /// as the default.
    pub fn from_config(cfg: &Config) -> Self {
        cfg.extended_source
            .as_deref()
            .and_then(Self::parse)
            .unwrap_or_default()
    }

    /// What this choice comes to under the price source `source` (registry
    /// id), given how much it sees itself and whether Alpaca keys are
    /// configured. A provider the source already is costs requests and
    /// buys nothing, so it reads as the source's own data.
    pub fn provider(self, source: &str, coverage: Coverage, alpaca_keys: bool) -> Provider {
        match self {
            Self::Same => Provider::Own,
            Self::Auto if coverage == Coverage::Market => Provider::Own,
            Self::Auto if alpaca_keys => Provider::Sip { backup: true },
            Self::Auto => Provider::Yahoo,
            Self::Alpaca if !alpaca_keys => Provider::Own,
            Self::Alpaca if source == "alpaca" && coverage == Coverage::Market => Provider::Own,
            Self::Alpaca => Provider::Sip { backup: false },
            Self::Yahoo if source == "yahoo" => Provider::Own,
            Self::Yahoo => Provider::Yahoo,
        }
    }
}

/// How much of the pre and post market a price source sees by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    /// Every exchange: Yahoo, Alpaca on a SIP feed.
    Market,
    /// IEX alone: a few percent of the tape, and next to nothing before
    /// the open (one trade of 11 shares in a CRWV premarket that saw
    /// 581,000 on the consolidated tape, 30 September 2026).
    Venue,
    /// No extended prices at all.
    Nothing,
}

/// What an `ExtendedSource` comes to for one price source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// The price source's own data, no extra request.
    Own,
    /// Alpaca's consolidated tape, 15 minutes behind. `backup` asks Yahoo
    /// when it fails or has nothing for the session yet.
    Sip {
        backup: bool,
    },
    Yahoo,
}

/// Put `provider` under `inner`, or hand `inner` back for `Provider::Own`.
pub fn borrow(
    inner: Arc<dyn DataSource>,
    provider: Provider,
    alpaca_keys: Option<(String, String)>,
) -> Result<Arc<dyn DataSource>> {
    let alpaca = match (provider, alpaca_keys) {
        (Provider::Sip { .. }, Some((id, secret))) => Some(Alpaca::new(id, secret)?),
        _ => None,
    };
    let yahoo = match provider {
        Provider::Yahoo | Provider::Sip { backup: true } => Some(Yahoo::new()?),
        _ => None,
    };
    if alpaca.is_none() && yahoo.is_none() {
        return Ok(inner);
    }
    Ok(Arc::new(Borrowed {
        inner,
        alpaca,
        yahoo,
        cache: Mutex::default(),
    }))
}

/// A price source whose pre and post market come from another feed.
pub struct Borrowed {
    inner: Arc<dyn DataSource>,
    /// The consolidated tape, for `Provider::Sip`.
    alpaca: Option<Alpaca>,
    /// `Provider::Yahoo`, or the tape's backup.
    yahoo: Option<Yahoo>,
    cache: Mutex<Cache>,
}

#[async_trait]
impl DataSource for Borrowed {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn delay_note(&self) -> Option<&'static str> {
        self.inner.delay_note()
    }

    fn extended_candles(&self) -> bool {
        self.inner.extended_candles()
    }

    fn begin_cycle(&self, symbols: &[String]) {
        self.inner.begin_cycle(symbols)
    }

    async fn fetch(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        sessions: Sessions,
    ) -> Result<TickerData> {
        let mut data = self.inner.fetch(symbol, range, interval, sessions).await?;
        if !market::is_us_equity(symbol) {
            return Ok(data);
        }
        let now = Utc::now();
        // A source without extended candles of its own (Finnhub) gets the
        // quote only: borrowed candles would sit beside a history it
        // accrued itself.
        let history = sessions == Sessions::Extended
            && interval != Interval::D1
            && self.inner.extended_candles();
        if !history && market::extended_window(now).is_none() {
            return Ok(data);
        }
        let earliest = data.candles.first().map(|c| c.ts);
        let extra = self
            .supplement(symbol, range, interval, history, earliest, now)
            .await;
        if history {
            apply(&mut data, &extra, now);
            data.candles = normalize_sessions(data.candles, symbol, interval, sessions);
        } else {
            // Daily and regular-only charts keep their candles, while the
            // extended quote is still useful to the rail and portfolio.
            apply_quote(&mut data.quote, &extra, now);
        }
        Ok(data)
    }
}

impl Borrowed {
    async fn supplement(
        &self,
        symbol: &str,
        range: Range,
        interval: Interval,
        history: bool,
        earliest: Option<i64>,
        now: chrono::DateTime<Utc>,
    ) -> Supplement {
        let (range, interval) = if history {
            (range, interval)
        } else {
            (Range::D1, Interval::M5)
        };
        let key = format!("{symbol}:{}:{}", range.as_str(), interval.as_str());
        let (mut cached, due) = self.cache.lock().unwrap().begin(&key, Instant::now());
        if !due {
            return cached;
        }
        // Whether the provider asked first refused.
        let mut failed = false;
        // A page refused halfway leaves the older sessions without
        // consolidated bars; the window is walked again rather than marked.
        let mut walk_broken = false;
        if let Some(alpaca) = &self.alpaca {
            let start = (now - chrono::Duration::seconds(range.secs()))
                .to_rfc3339_opts(SecondsFormat::Secs, true);
            // Consolidated history is paged back to the start of the
            // source's series once per window, so the two cover the same
            // span; later refreshes take the newest page only, the rest
            // waits in the cache until it ages out of the range.
            let until = earliest.filter(|_| history && !cached.paged);
            let (quote, bars) = alpaca.consolidated(symbol, interval, &start, until).await;
            failed = quote.is_err() || (history && bars.is_err());
            merge_quote(&mut cached.quote, quote.ok());
            if let Ok((bars, complete)) = bars {
                cached.candles = merge_history(&cached.candles, &bars);
                cached.record_volumes(&bars);
                cached.paged |= until.is_some() && complete;
                walk_broken = until.is_some() && !complete;
            }
        }
        let ask_yahoo = self.alpaca.is_none()
            || failed
            || missing_current(&cached, history, now)
            || (history && cached.candles.is_empty());
        let mut gated = false;
        if let Some(yahoo) = self.yahoo.as_ref().filter(|_| ask_yahoo) {
            if self.cache.lock().unwrap().claim_yahoo(Instant::now()) {
                match yahoo
                    .fetch(symbol, range, interval, Sessions::Extended)
                    .await
                {
                    Ok(yahoo) => {
                        merge_quote(&mut cached.quote, Some(yahoo.quote));
                        cached.candles = merge_history(&cached.candles, &yahoo.candles);
                        cached.record_volumes(&yahoo.candles);
                    }
                    Err(error) => {
                        failed |= self.alpaca.is_none();
                        let blocked = error.to_string().contains("rate limiting");
                        self.cache
                            .lock()
                            .unwrap()
                            .yahoo_failed(Instant::now(), blocked);
                    }
                }
            } else {
                gated = true;
            }
        }
        let cutoff = now.timestamp() - range.secs();
        cached.candles.retain(|c| c.ts >= cutoff);
        cached.volumes = cached.volumes.split_off(&cutoff);
        // A refused first answer for a window must not be the chart's for a
        // whole refresh: it drew the IEX session alone for a minute and
        // then jumped when the delayed tape arrived. Nor may a ticker that
        // lost the shared Yahoo gate wait a minute with nothing. What is
        // kept from an earlier answer can wait the full minute.
        let missing = (failed
            && ((history && cached.candles.is_empty()) || cached.quote.is_none()))
            || walk_broken
            || (gated && cached.quote.is_none());
        let retry = missing.then(|| Instant::now() + RETRY);
        self.cache
            .lock()
            .unwrap()
            .finish(&key, cached.clone(), retry);
        cached
    }
}

/// Whether the tape has nothing yet for the extended session on the clock.
/// At 04:00 the delayed tape naturally cannot have today's PRE, so the
/// first 15 minutes of a session are not counted as missing.
fn missing_current(cached: &Supplement, history: bool, now: chrono::DateTime<Utc>) -> bool {
    market::extended_window(now).is_some_and(|w| {
        now.timestamp() >= w.start + 16 * 60
            && (cached
                .quote
                .as_ref()
                .and_then(|q| q.extended_price_at(now))
                .is_none()
                || (now.timestamp() < w.end
                    && cached
                        .quote
                        .as_ref()
                        .and_then(|q| q.timing.extended)
                        .is_some_and(|ts| ts < now.timestamp() - 30 * 60))
                || (history
                    && !cached
                        .candles
                        .iter()
                        .any(|c| c.ts >= w.start && c.ts < w.end)))
    })
}

pub const REFRESH: Duration = Duration::from_secs(60);
/// How soon a refresh that came back with nothing to draw is tried again.
/// Longer than a poll, so a source that keeps refusing costs a couple of
/// requests per ticker every few polls rather than every one.
pub const RETRY: Duration = Duration::from_secs(15);

#[derive(Clone, Default)]
pub struct Supplement {
    pub quote: Option<Quote>,
    pub candles: Vec<Candle>,
    /// Consolidated share counts by bar start, regular session included,
    /// with the feed that counted them. IEX bars borrow these, so one chart
    /// never plots a single venue's volume beside the whole market's.
    pub volumes: BTreeMap<i64, (PriceFeed, f64)>,
    /// Whether the consolidated history has been paged back to the start
    /// of the primary series for this window (see `Borrowed::supplement`).
    pub paged: bool,
}

impl Supplement {
    /// Alpaca's consolidated tape is the reference: its answer overwrites
    /// what is known about its bars, the newest of which is still filling.
    /// A fallback fills gaps and refreshes its own counts, never the
    /// reference's.
    pub fn record_volumes(&mut self, bars: &[Candle]) {
        for c in bars {
            let Some(v) = c.volume else { continue };
            let reference = matches!(c.feed, PriceFeed::DelayedSip | PriceFeed::Sip);
            match self.volumes.get(&c.ts) {
                Some((feed, _)) if !reference && *feed != c.feed => {}
                _ => {
                    self.volumes.insert(c.ts, (c.feed, v));
                }
            }
        }
    }
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

    /// Store a refresh. `retry` brings the next one forward, for an answer
    /// that left the chart with nothing to draw.
    pub fn finish(&mut self, key: &str, data: Supplement, retry: Option<Instant>) {
        if let Some((after, old)) = self.entries.get_mut(key) {
            *old = data;
            if let Some(at) = retry {
                *after = (*after).min(at);
            }
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
                || (previous[0].feed == PriceFeed::Iex && new.len() > previous.len()))
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

/// The provider's print replaces the source's own whenever it has one for
/// the session on the clock. The provider was picked for its numbers, and
/// a single-venue print is not the market's however recent: Tiingo stamps
/// a premarket IEX trade with the time of the request, not the trade.
pub fn apply_quote(quote: &mut Quote, extra: &Supplement, now: chrono::DateTime<Utc>) {
    if let Some(q) = &extra.quote
        && q.extended_price_at(now).is_some()
    {
        quote.extended = q.extended;
        quote.timing.extended = q.timing.extended;
        quote.timing.extended_feed = q.timing.extended_feed;
        quote.timing.extended_reference = Some(q.extended_reference());
    }
}

pub fn apply(data: &mut TickerData, extra: &Supplement, now: chrono::DateTime<Utc>) {
    apply_quote(&mut data.quote, extra, now);
    let ext = merge_history(&data.candles, &extra.candles);
    data.candles.retain(|c| session_key(c).is_none());
    data.candles.extend(ext);
    data.candles.sort_by_key(|c| c.ts);
    // IEX is a few percent of the tape: beside consolidated after-hours
    // bars its regular session read as the quietest part of the day. Once
    // such bars are on the chart, IEX bars take the consolidated count of
    // the same bar, or none until the delayed feed has reached it.
    if data
        .candles
        .iter()
        .any(|c| c.feed != PriceFeed::Iex && c.volume.is_some())
    {
        for c in data.candles.iter_mut().filter(|c| c.feed == PriceFeed::Iex) {
            c.volume = extra.volumes.get(&c.ts).map(|(_, v)| *v);
        }
    }
}

/// Puts consolidated daily bars under an IEX daily chart. IEX is one venue
/// with a few percent of the tape, so its daily bar understates volume
/// some thirtyfold and carries that venue's own high and low. A finished
/// day takes the consolidated bar whole. The day still trading keeps its
/// live IEX close, since the consolidated bar is fifteen minutes behind,
/// and takes the rest from the wider tape. A day the delayed feed has not
/// reached yet loses its volume rather than plot one venue's count beside
/// the market's.
pub fn consolidate_daily(
    candles: &mut [Candle],
    sip: &[Candle],
    now: chrono::DateTime<chrono::Utc>,
) {
    if sip.is_empty() {
        return;
    }
    let by_ts: HashMap<i64, &Candle> = sip.iter().map(|c| (c.ts, c)).collect();
    let today = market::et_date(now.timestamp());
    for c in candles.iter_mut().filter(|c| c.feed == PriceFeed::Iex) {
        match by_ts.get(&c.ts) {
            Some(s) if market::et_date(c.ts) == today => {
                c.open = s.open;
                c.high = c.high.max(s.high);
                c.low = c.low.min(s.low);
                c.volume = s.volume;
            }
            Some(s) => *c = **s,
            None => c.volume = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::QuoteTiming;

    async fn supplemental_http_case(sip_fails: bool) {
        use serde_json::json;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let now = Utc::now();
        let mut date = market::et_date(now.timestamp()).unwrap();
        while market::windows(date).is_empty() {
            date = date.pred_opt().unwrap();
        }
        let windows = market::windows(date);
        let ext = market::extended_window(now).unwrap_or(windows[0]);
        let ext_ts = (now.timestamp() - 1).min(ext.end - 1).max(ext.start);
        let format_ts = |ts| {
            chrono::DateTime::from_timestamp(ts, 0)
                .unwrap()
                .to_rfc3339()
        };
        let regular_ts = format_ts(windows[1].start);
        let extra_ts = format_ts(ext_ts);
        let first = format_ts(ext.start);
        let requests = if sip_fails { 7 } else { 6 };
        let server = tokio::spawn(async move {
            let mut paths = Vec::new();
            for _ in 0..requests {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut byte = [0; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                let text = String::from_utf8(request).unwrap();
                let path = text.split_whitespace().nth(1).unwrap().to_string();
                let supplemental = path.contains("feed=sip") || path.contains("feed=delayed_sip");
                let (status, body) = if sip_fails && supplemental {
                    ("403 Forbidden", json!({"message":"test SIP unavailable"}))
                } else if path.starts_with("/yahoo/") {
                    (
                        "200 OK",
                        json!({"chart":{"result":[{"meta":{"symbol":"AAPL", "regularMarketPrice":100.0},
                        "timestamp":[ext.start, ext.start+60], "indicators":{"quote":[{"open":[101.0,102.0],
                        "high":[101.0,102.0],"low":[101.0,102.0],"close":[101.0,102.0],"volume":[10.0,20.0]}]}}]}}),
                    )
                } else if path.contains("/snapshot") {
                    (
                        "200 OK",
                        json!({"latestTrade":{"t":if supplemental {&extra_ts} else {&regular_ts},"p":if supplemental {101.0} else {100.0}},
                        "dailyBar":{"t":regular_ts,"c":100.0},"prevDailyBar":{"t":regular_ts,"c":99.0}}),
                    )
                } else {
                    assert!(!path.contains("feed=delayed_sip"), "bars must use feed=sip");
                    if supplemental {
                        let url = reqwest::Url::parse(&format!("http://localhost{path}")).unwrap();
                        let end = url.query_pairs().find(|(key, _)| key == "end").unwrap().1;
                        let end = chrono::DateTime::parse_from_rfc3339(&end).unwrap();
                        let delay = Utc::now().signed_duration_since(end).num_seconds();
                        assert!((900..930).contains(&delay), "SIP bars must lag by 15m");
                    }
                    let bar = |t: &str, v: f64| json!({"t":t,"o":100.0,"h":102.0,"l":99.0,"c":101.0,"v":v});
                    // Consolidated bars cover the regular session too, with
                    // the market's (much larger) count.
                    let bars = if supplemental {
                        json!([bar(&first, 10.0), bar(&regular_ts, 900.0)])
                    } else {
                        json!([bar(&regular_ts, 10.0)])
                    };
                    ("200 OK", json!({ "bars": bars }))
                };
                paths.push(path);
                let body = body.to_string();
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
            paths
        });
        // One server plays both feeds; the query says which one is asked.
        let source = Borrowed {
            inner: Arc::new(Alpaca::test_at(base.clone(), "iex")),
            alpaca: Some(Alpaca::test_at(base.clone(), "iex")),
            yahoo: Some(Yahoo::test_at(format!("{base}/yahoo"))),
            cache: Mutex::default(),
        };
        for _ in 0..2 {
            let data = source
                .fetch("AAPL", Range::D5, Interval::M5, Sessions::Extended)
                .await
                .unwrap();
            assert_eq!(data.quote.price, 100.0);
            let expected = if sip_fails {
                PriceFeed::Yahoo
            } else {
                PriceFeed::DelayedSip
            };
            assert!(data.candles.iter().any(|c| c.feed == expected));
            let iex = data.candles.iter().find(|c| c.feed == PriceFeed::Iex);
            // The IEX bar never keeps its single-venue count beside
            // consolidated bars; Yahoo's answer here has no regular bar.
            assert_eq!(
                iex.expect("regular IEX candle").volume,
                if sip_fails { None } else { Some(900.0) }
            );
        }
        let paths = tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            paths
                .iter()
                .filter(|p| p.contains("feed=delayed_sip"))
                .count(),
            1
        );
        assert_eq!(paths.iter().filter(|p| p.contains("feed=sip")).count(), 1);
        assert_eq!(
            paths.iter().filter(|p| p.starts_with("/yahoo")).count(),
            usize::from(sip_fails)
        );
    }

    #[tokio::test]
    async fn sip_uses_distinct_endpoint_parameters_and_is_cached() {
        supplemental_http_case(false).await;
    }

    #[tokio::test]
    async fn yahoo_fills_extended_history_when_sip_fails_without_changing_iex() {
        supplemental_http_case(true).await;
    }

    #[test]
    fn choices_resolve_per_source() {
        use Coverage::{Market, Nothing, Venue};
        use ExtendedSource as E;
        // Auto borrows only what the source lacks, the tape before Yahoo.
        assert_eq!(E::Auto.provider("yahoo", Market, true), Provider::Own);
        assert_eq!(
            E::Auto.provider("tiingo", Venue, true),
            Provider::Sip { backup: true }
        );
        assert_eq!(E::Auto.provider("tiingo", Venue, false), Provider::Yahoo);
        assert_eq!(E::Auto.provider("finnhub", Nothing, false), Provider::Yahoo);
        assert_eq!(E::Same.provider("tiingo", Venue, true), Provider::Own);
        // A named provider has no backup, and without its keys none at all.
        assert_eq!(
            E::Alpaca.provider("tiingo", Venue, true),
            Provider::Sip { backup: false }
        );
        assert_eq!(E::Alpaca.provider("tiingo", Venue, false), Provider::Own);
        assert_eq!(
            E::Alpaca.provider("alpaca", Venue, true),
            Provider::Sip { backup: false }
        );
        assert_eq!(
            E::Alpaca.provider("alpaca", Market, true),
            Provider::Own,
            "a SIP feed is the tape already"
        );
        assert_eq!(E::Yahoo.provider("yahoo", Market, false), Provider::Own);
        assert_eq!(E::Yahoo.provider("alpaca", Venue, true), Provider::Yahoo);
        for c in E::ALL {
            assert_eq!(E::parse(c.name()), Some(c));
        }
        assert_eq!(E::parse(" YAHOO "), Some(E::Yahoo));
        assert_eq!(E::parse("sip"), None);
        assert_eq!(E::Auto.step(-1), E::Yahoo);
        assert_eq!(E::Yahoo.step(1), E::Auto);
    }

    /// The provider's print for the session on the clock wins over the
    /// source's own even when that one looks newer: Tiingo stamps a stale
    /// IEX trade with the time of the request. Without a current print the
    /// source's own stands.
    #[test]
    fn the_providers_print_wins_while_it_is_current() {
        let now = at("2026-09-30T12:56:00Z");
        let own = || Quote {
            symbol: "CRWV".into(),
            price: 85.93,
            extended: Some(85.63),
            timing: QuoteTiming {
                extended: Some(now.timestamp() - 240),
                extended_feed: PriceFeed::Iex,
                ..Default::default()
            },
            ..Default::default()
        };
        let tape = |ts: i64| Supplement {
            quote: Some(Quote {
                symbol: "CRWV".into(),
                price: 85.93,
                extended: Some(87.09),
                timing: QuoteTiming {
                    extended: Some(ts),
                    extended_feed: PriceFeed::DelayedSip,
                    extended_reference: Some(85.93),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut quote = own();
        apply_quote(&mut quote, &tape(now.timestamp() - 960), now);
        assert_eq!(quote.extended_price_at(now), Some(87.09));
        assert_eq!(quote.timing.extended_feed, PriceFeed::DelayedSip);
        // Yesterday's after-hours print is not this premarket's.
        let mut quote = own();
        apply_quote(&mut quote, &tape(now.timestamp() - 14 * 3600), now);
        assert_eq!(quote.extended, Some(85.63));
        assert_eq!(quote.timing.extended_feed, PriceFeed::Iex);
    }

    #[test]
    fn iex_daily_bars_take_the_consolidated_tape() {
        let day = |d: u32| at(&format!("2026-09-{d:02}T04:00:00Z")).timestamp();
        let iex = |ts, price| Candle {
            volume: Some(1.0),
            ..bar(ts, PriceFeed::Iex, price)
        };
        let sip = |ts, price| Candle {
            high: price + 5.0,
            low: price - 5.0,
            volume: Some(30.0),
            ..bar(ts, PriceFeed::DelayedSip, price)
        };
        let mut candles = vec![
            iex(day(23), 100.0),
            iex(day(24), 101.0),
            iex(day(25), 102.0),
        ];
        let now = at("2026-09-25T15:00:00Z");

        // Nothing consolidated came back: IEX stays as it was, labelled.
        consolidate_daily(&mut candles, &[], now);
        assert!(candles.iter().all(|c| c.volume == Some(1.0)));

        // The feed has not reached the 24th: that day loses its volume.
        consolidate_daily(&mut candles, &[sip(day(23), 99.0), sip(day(25), 98.0)], now);
        let done = candles[0];
        assert_eq!(done.feed, PriceFeed::DelayedSip);
        assert_eq!((done.open, done.high, done.close), (99.0, 104.0, 99.0));
        assert_eq!(done.volume, Some(30.0));
        assert_eq!(candles[1].volume, None);
        let today = candles[2];
        assert_eq!(today.feed, PriceFeed::Iex, "today's close is the live one");
        assert_eq!(
            (today.open, today.high, today.low, today.close),
            (98.0, 103.0, 93.0, 102.0)
        );
        assert_eq!(today.volume, Some(30.0));
    }

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
    fn iex_bars_take_consolidated_volume_once_consolidated_bars_are_charted() {
        let pre = at("2026-09-14T08:00:00Z").timestamp();
        let open = at("2026-09-14T13:30:00Z").timestamp();
        let iex = |ts, v| Candle {
            volume: Some(v),
            ..bar(ts, PriceFeed::Iex, 100.0)
        };
        let primary = || TickerData {
            quote: Quote::default(),
            candles: vec![iex(open, 40.0), iex(open + 300, 30.0)],
        };
        let now = at("2026-09-14T14:00:00Z");

        // The delayed feed has counted the first regular bar, not the second.
        let mut extra = Supplement {
            candles: vec![bar(pre, PriceFeed::DelayedSip, 99.0)],
            ..Default::default()
        };
        let counted = |ts, feed, v| Candle {
            volume: Some(v),
            ..bar(ts, feed, 100.0)
        };
        extra.record_volumes(&[
            bar(pre, PriceFeed::DelayedSip, 99.0),
            counted(open, PriceFeed::DelayedSip, 900.0),
        ]);
        let mut data = primary();
        apply(&mut data, &extra, now);
        let volumes: Vec<_> = data.candles.iter().map(|c| c.volume).collect();
        assert_eq!(volumes, vec![Some(10.0), Some(900.0), None]);

        // A fallback fills gaps but never rewrites a SIP count.
        extra.record_volumes(&[
            counted(open, PriceFeed::Yahoo, 1.0),
            counted(open + 300, PriceFeed::Yahoo, 700.0),
        ]);
        let mut data = primary();
        apply(&mut data, &extra, now);
        assert_eq!(data.candles[1].volume, Some(900.0));
        assert_eq!(data.candles[2].volume, Some(700.0));

        // The fallback's own bar keeps filling on later refreshes (a frozen
        // first minute would understate it for a quarter hour), and the
        // reference takes over as soon as it reaches the bar.
        extra.record_volumes(&[counted(open + 300, PriceFeed::Yahoo, 750.0)]);
        assert_eq!(extra.volumes[&(open + 300)], (PriceFeed::Yahoo, 750.0));
        extra.record_volumes(&[counted(open + 300, PriceFeed::DelayedSip, 800.0)]);
        extra.record_volumes(&[counted(open + 300, PriceFeed::Yahoo, 760.0)]);
        assert_eq!(extra.volumes[&(open + 300)], (PriceFeed::DelayedSip, 800.0));

        // Nothing consolidated on the chart: IEX keeps its own counts, and
        // the chart labels them.
        let mut data = primary();
        let quote_only = Supplement {
            volumes: extra.volumes.clone(),
            ..Default::default()
        };
        apply(&mut data, &quote_only, now);
        let volumes: Vec<_> = data.candles.iter().map(|c| c.volume).collect();
        assert_eq!(volumes, vec![Some(40.0), Some(30.0)]);
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
            ..Default::default()
        };
        apply(&mut primary, &extra, now);
        assert_eq!(primary.quote.price, 100.0);
        assert_eq!(primary.quote.extended_price_at(now), Some(101.0));
        assert_eq!(primary.quote.extended_reference(), 99.9);
        assert_eq!(primary.quote.timing.extended_feed, PriceFeed::DelayedSip);
    }

    /// An answer with nothing to draw is asked again sooner than a good one
    /// is refreshed, and never later than that refresh.
    #[test]
    fn an_empty_answer_is_retried_before_the_refresh() {
        let now = Instant::now();
        let mut cache = Cache::default();
        assert!(cache.begin("AAPL:1mo:60m", now).1);
        cache.finish("AAPL:1mo:60m", Supplement::default(), Some(now + RETRY));
        assert!(!cache.begin("AAPL:1mo:60m", now + RETRY / 2).1);
        assert!(cache.begin("AAPL:1mo:60m", now + RETRY).1);
        // A good answer keeps the full refresh.
        cache.finish("AAPL:1mo:60m", Supplement::default(), None);
        assert!(!cache.begin("AAPL:1mo:60m", now + RETRY * 2).1);
        // A retry never pushes the next refresh back.
        cache.finish(
            "AAPL:1mo:60m",
            Supplement::default(),
            Some(now + REFRESH * 5),
        );
        assert!(cache.begin("AAPL:1mo:60m", now + RETRY + REFRESH).1);
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
