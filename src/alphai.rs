//! AlphAI public REST API client (https://alphai.io).
//!
//! Powers the News and Insider views: relevance-scored financial news and
//! SEC Form 4 insider activity. Needs an `ak_live_…` API key (free tier at
//! alphai.io, 20 req/min and 100 req/day), so fetches are demand-driven and
//! cached: the app only asks for the symbol on screen and re-asks after
//! `CACHE_TTL`. Keep it that way — a per-poll fetch would burn the free
//! daily budget in minutes.
//!
//! Re-asking is a delta poll (`Sort::Ingested`) rather than a refetch of the
//! newest page: an article reaches the feed later than it was published, so
//! the published head page is exactly where an arrival is NOT. Same cadence,
//! same one request, rows that would otherwise never be shown.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::poller::SourceEvent;

pub const DEFAULT_BASE_URL: &str = "https://api.alphai.io";

/// The public site, for article page links (distinct from the API base).
pub const SITE_URL: &str = "https://alphai.io";

/// How long a fetched bundle stays fresh before a view triggers a re-fetch.
/// This is the default; `[ui] alphai_ttl_secs` overrides it per user, with
/// a floor guarding the request budget (`config::ALPHAI_TTL_RANGE`).
pub const CACHE_TTL: Duration = Duration::from_secs(300);

/// Rows per feed page. The API defaults to 10 but accepts up to 20 on every
/// tier (21 and above need Pro, see `fetch_feed_page`), and a wider page is
/// free: it is the same one request, it delays the next paging request, and
/// it widens the window in which a late-arriving article can still be seen.
/// Articles reach the feed well after their publish time, so a narrow page
/// hides them behind the horizon of what a refetch ever looks at.
pub const PAGE_SIZE: u8 = 20;

/// Cache key for the market-wide (unfiltered) news feed.
pub const MARKET_KEY: &str = "*";

/// Cache key for the trending feed (`~` cannot appear in a ticker).
pub const TRENDING_KEY: &str = "~trending";

/// Shown when paging back hits the plan's news-archive horizon. Terminal for
/// the current feed: no retry will help, only a higher tier. Kept short: it
/// renders inside the list border even in the side-by-side layout.
pub const ARCHIVE_GATE_MSG: &str = "plan archive limit · upgrade: alphai.io/pricing";

/// Whether a fetch error is the archive gate (see `ARCHIVE_GATE_MSG`).
pub fn is_archive_gate(error: &str) -> bool {
    error.contains("archive limit")
}

/// Whether a fetch failed because the API owns no such symbol (a 404 with
/// `unknown_symbol`). Terminal like the archive gate: no retry helps, so the
/// view says so instead of offering `r`. Crypto and typos land here.
pub fn is_unknown_symbol(error: &str) -> bool {
    error.contains("API 404")
}

/// Keep the HTTP status available to the calendar queue without coupling its
/// pause policy to the wording of a user-facing error.
#[derive(Debug)]
struct ApiError {
    status: reqwest::StatusCode,
    message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

fn calendar_error(key: String, error: anyhow::Error) -> Event {
    let blocked = error
        .downcast_ref::<ApiError>()
        .is_some_and(|e| matches!(e.status.as_u16(), 401 | 403 | 429));
    let error = format!("{error:#}");
    if blocked {
        Event::CalendarBlocked { key, error }
    } else {
        Event::Error { key, error }
    }
}

pub struct Client {
    http: reqwest::Client,
    base: String,
    key: String,
}

impl Client {
    pub fn new(key: String) -> Result<Self> {
        let base = std::env::var("ALPHAI_API_URL")
            .ok()
            .filter(|u| !u.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
        let http = reqwest::Client::builder()
            .user_agent(concat!("alphai-tui/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(15))
            .build()?;
        Ok(Self { http, base, key })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str, query: &[(&str, &str)]) -> Result<T> {
        let resp = self
            .http
            .get(format!("{}{}", self.base, path))
            .bearer_auth(&self.key)
            .query(query)
            .send()
            .await
            .context("request failed")?;

        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ApiError {
                status,
                message: "invalid AlphAI API key, press s to update it (free keys: alphai.io)"
                    .into(),
            }
            .into());
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("60");
            return Err(ApiError {
                status,
                message: format!(
                    "AlphAI rate limit hit, retry in {wait}s (Free tier: 20/min, 100/day)"
                ),
            }
            .into());
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let parsed = serde_json::from_str::<ErrorBody>(&body).ok();
            // Paging back past the plan's archive horizon (Free 30 days,
            // Basic 90) is a 403 with a machine-readable reason.
            if parsed
                .as_ref()
                .and_then(|e| e.extra.as_ref())
                .and_then(|x| x.reason.as_deref())
                == Some("archive_horizon")
            {
                bail!("{ARCHIVE_GATE_MSG}");
            }
            let msg = parsed
                .and_then(|e| e.message.or(e.detail).or(e.error))
                .unwrap_or_else(|| body.chars().take(120).collect());
            return Err(ApiError {
                status,
                message: format!("AlphAI API {status}: {msg}"),
            }
            .into());
        }
        resp.json().await.context("bad JSON from AlphAI")
    }

    /// One page of enriched news; `symbol: None` = market-wide feed, `cursor`
    /// pages back (opaque, from the prior page's `next_cursor`), `page_size`
    /// takes 1 to 20 on every tier and up to 50 only on Pro keys (omit for
    /// the server default of 10; anything over the tier's cap is a 400, never
    /// a silent clamp), `min_relevance`
    /// filters server-side by score 1..10 (the server defaults to 4 when
    /// omitted) so low-score rows never occupy page slots.
    /// Market-wide requests collapse syndicated reprints to one row per story
    /// (`sources_count` reports the outlet count). The symbol-scoped feed is
    /// left uncollapsed: the collapse filter matches the story root, which may
    /// not mention the symbol, and would silently drop relevant coverage.
    /// `sort` picks the ordering: `Published` is the reverse-chronological
    /// feed this pages through, `Ingested` is delta mode (see `Sort`), where
    /// the same `cursor` slot carries a position in the arrival stream.
    pub async fn news(
        &self,
        symbol: Option<&str>,
        cursor: Option<&str>,
        page_size: Option<u8>,
        min_relevance: Option<u8>,
        sort: Sort,
    ) -> Result<NewsPage> {
        let mut query: Vec<(&str, &str)> = Vec::new();
        match symbol {
            Some(s) => query.push(("symbol", s)),
            None => query.push(("collapse", "story")),
        }
        if let Some(c) = cursor {
            query.push(("cursor", c));
        }
        let size = page_size.map(|n| n.to_string());
        if let Some(size) = &size {
            query.push(("page_size", size));
        }
        let min = min_relevance.map(|n| n.to_string());
        if let Some(min) = &min {
            query.push(("min_relevance", min));
        }
        if let Some(order) = sort.param() {
            query.push(("sort", order));
        }
        self.get_json("/api/news/", &query).await
    }

    /// Top 10 stories of the trailing 48h (relevance 8+, server-collapsed).
    /// The response is a bare array, not the paginated news shape.
    pub async fn trending(&self) -> Result<Vec<Article>> {
        self.get_json("/api/news/trending/", &[]).await
    }

    /// One page of the SEC Form 4 insider feed for one symbol; `cursor`,
    /// `page_size` and `min_relevance` behave as in `news`. Insider rows are
    /// scored from the event's total dollar value, so the relevance filter
    /// doubles as a trade-size filter (7 keeps roughly the $10M+ trades).
    /// `sort` works as in `news`, and delta mode matters most here: a Form 4
    /// is filed days after the trade it reports, so a new filing routinely
    /// enters the feed below the head of the publish-ordered page.
    pub async fn insider_news(
        &self,
        symbol: &str,
        cursor: Option<&str>,
        page_size: Option<u8>,
        min_relevance: Option<u8>,
        sort: Sort,
    ) -> Result<NewsPage> {
        let mut query: Vec<(&str, &str)> = vec![("symbol", symbol)];
        if let Some(c) = cursor {
            query.push(("cursor", c));
        }
        let size = page_size.map(|n| n.to_string());
        if let Some(size) = &size {
            query.push(("page_size", size));
        }
        let min = min_relevance.map(|n| n.to_string());
        if let Some(min) = &min {
            query.push(("min_relevance", min));
        }
        if let Some(order) = sort.param() {
            query.push(("sort", order));
        }
        self.get_json("/api/news/insider/", &query).await
    }

    /// 7-day bullish/neutral/bearish rollup from press coverage.
    pub async fn sentiment(&self, ticker: &str) -> Result<SentimentSummary> {
        self.get_json(&format!("/api/symbols/{ticker}/sentiment-summary/"), &[])
            .await
    }

    /// Form 4 chart bundle: 3m/12m/all-time rollups with top insiders,
    /// Monday-keyed weekly dollar buckets and every chart event of the
    /// trailing 365 days, in one request. `page_size=1` keeps the paginated
    /// events table (the TUI's list is the news feed) out of the payload.
    pub async fn insider_trades(&self, ticker: &str) -> Result<InsiderTrades> {
        self.get_json(
            &format!("/api/symbols/{ticker}/insider-trades/"),
            &[("page_size", "1")],
        )
        .await
    }

    /// Every published earnings read for one ticker, newest first, plus the
    /// date of its next report. One request covers the whole surface: the
    /// reads carry their full analysis, so nothing needs a second fetch.
    /// A ticker with no read answers 200 with an empty list; one no listing
    /// owns answers 404 (see `is_unknown_symbol`).
    pub async fn earnings(&self, ticker: &str) -> Result<TickerEarnings> {
        self.get_json(&format!("/api/symbols/{ticker}/earnings/"), &[])
            .await
    }

    /// The official schedule of US macro releases in `[from, to)`, dates as
    /// `YYYY-MM-DD`. Market-wide and unpaginated: one request covers every
    /// series in the window.
    pub async fn calendar(&self, from: &str, to: &str) -> Result<Vec<CalendarEvent>> {
        let page: CalendarEvents = self
            .get_json("/api/calendar/", &[("from_date", from), ("to_date", to)])
            .await?;
        Ok(page.events)
    }
}

// ---------------------------------------------------------------------------
// Background task: the UI sends commands, results come back as SourceEvents
// on the same channel the price poller uses.

/// Ordering a feed fetch asks for.
///
/// `Published` is the reverse-chronological feed: no cursor means the newest
/// page, and `next_cursor` walks back into history.
///
/// `Ingested` is delta polling, "what appeared in the feed since my last
/// poll": rows in the order they became available, ascending. An article
/// reaches the feed later than it was published (a median of roughly half an
/// hour for general news, days for a Form 4, which is filed after the trade),
/// so most arrivals land below the head of the published page and a refetch
/// of that page can never see them. Without a cursor the server answers with
/// the newest page and parks the position at the feed head; with one it
/// answers with what arrived since, and an empty page means caught up, so its
/// `next_cursor` is always set. The two modes mint separate cursor families
/// and a cursor replayed into the other mode is a 400, which is why the app
/// keeps the paging cursor and the poll cursor in separate fields.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    #[default]
    Published,
    Ingested,
}

impl Sort {
    /// Wire value, or None when the request carries no `sort` at all:
    /// published is the server's default, so every published request stays
    /// byte for byte what it was before delta mode existed.
    fn param(self) -> Option<&'static str> {
        match self {
            Self::Published => None,
            Self::Ingested => Some("ingested"),
        }
    }
}

/// What a fetch's result does to the bundle it lands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedMode {
    /// Head page: replace the bundle, side payload included.
    Replace,
    /// Cursor page of the published feed: extend it with older rows.
    Append,
    /// Delta page: merge arrivals into the top, keeping the loaded pages,
    /// the side payload and the row under the reader's cursor.
    Merge,
}

/// The mode a fetch's result lands in, from what the fetch asked for.
fn feed_mode(sort: Sort, cursor: Option<&str>) -> FeedMode {
    match (sort, cursor) {
        (Sort::Ingested, _) => FeedMode::Merge,
        (Sort::Published, Some(_)) => FeedMode::Append,
        (Sort::Published, None) => FeedMode::Replace,
    }
}

pub enum Cmd {
    /// Swap the API key at runtime (settings screen). None disables fetching.
    SetKey(Option<String>),
    /// Fetch news (+ sentiment when symbol-scoped). None = market-wide.
    /// A cursor means "load the next page" of an already shown feed.
    /// `min_relevance` is the score filter the app wants (echoed back on the
    /// resulting `Event::Feed` so the bundle can record it). `sort` picks the
    /// published feed or a delta poll of it (see `Sort`); a delta fetch skips
    /// the sentiment rollup, which the bundle already holds.
    FetchNews {
        symbol: Option<String>,
        cursor: Option<String>,
        min_relevance: Option<u8>,
        sort: Sort,
    },
    /// Fetch the 48h trending top 10 (cached under `TRENDING_KEY`).
    FetchTrending,
    /// Fetch the insider feed + the Form 4 chart bundle for one symbol; a
    /// cursor pages. `min_relevance` works as in `FetchNews` (insider
    /// scores track the trade size, so this is effectively a size filter).
    /// `sort` works as in `FetchNews`, and a delta fetch skips the chart
    /// bundle the same way.
    FetchInsider {
        symbol: String,
        cursor: Option<String>,
        min_relevance: Option<u8>,
        sort: Sort,
    },
    /// Fetch one ticker's earnings reads and its next report date.
    FetchEarnings { symbol: String },
    /// Fetch the macro calendar window `[from, to)`, dates as `YYYY-MM-DD`.
    FetchCalendar { from: String, to: String },
}

pub enum Event {
    /// The serial worker has retired all requests made with the previous key.
    KeyChanged,
    /// One feed fetch's result; `mode` says what it does to the bundle.
    Feed {
        /// Cache key: a symbol / `MARKET_KEY` / `TRENDING_KEY` for news,
        /// `ins:SYM` for insider — the same key `App` tracks inflight and
        /// errors under.
        key: String,
        articles: Vec<Article>,
        /// Side payload of the head fetch; pages never refetch it.
        side: Option<FeedPayload>,
        next_cursor: Option<String>,
        /// Replace the bundle, extend it with an older page, or merge
        /// arrivals into its top (see `FeedMode`).
        mode: FeedMode,
        /// The score filter this fetch carried; None for feeds without one
        /// (trending, insider). The bundle records it so a changed setting
        /// marks the cached feed stale.
        min_relevance: Option<u8>,
    },
    /// A load-more failed. The shown feed stays; `gated` marks the archive
    /// horizon (terminal for this feed, offer an upgrade instead of a retry).
    PageError {
        key: String,
        error: String,
        gated: bool,
    },
    /// A background delta poll failed for a reason that is not the cursor:
    /// the shown feed stays untouched and the app stops polling it until the
    /// reader retries, because nothing here ever retries by itself.
    PollError { key: String, error: String },
    /// A background delta poll came back with the cursor rejected (400) or
    /// past the plan's archive horizon (403). Neither is worth showing: drop
    /// the poll position and the next poll starts a fresh one.
    PollReprime { key: String },
    /// One ticker's earnings reads. Boxed: a single read runs to ~16 KB and
    /// this rides the same channel as every price tick.
    Earnings {
        /// `earn:SYM`, the key the app tracks inflight and errors under.
        key: String,
        data: Box<TickerEarnings>,
    },
    /// A successful macro window, including the UTC date bounds requested.
    Calendar {
        events: Vec<CalendarEvent>,
        from: String,
        to: String,
    },
    /// Stop the watchlist date sweep after an account-wide access/limit error.
    CalendarBlocked { key: String, error: String },
    /// `key` matches the cache key of the fetch that failed.
    Error { key: String, error: String },
}

/// Rollup fetched alongside a feed's head page. The trades bundle is boxed:
/// it dwarfs the sentiment rollup, and the payload travels inside every
/// `Event::Feed`.
pub enum FeedPayload {
    Sentiment(SentimentSummary),
    Insider(Box<InsiderTrades>),
}

/// Cache key for a symbol-scoped or market-wide news fetch.
pub fn news_key(symbol: Option<&str>) -> String {
    symbol.map_or_else(|| MARKET_KEY.to_string(), str::to_string)
}

/// Cache key for an insider fetch (kept distinct from news keys).
pub fn insider_key(symbol: &str) -> String {
    format!("ins:{symbol}")
}

/// Cache key for a ticker's earnings reads (kept distinct from feed keys).
pub fn earnings_key(symbol: &str) -> String {
    format!("earn:{symbol}")
}

/// Cache key for the macro calendar. It is the one payload here that is not
/// scoped to a ticker: one window covers the whole market.
pub const CALENDAR_KEY: &str = "~calendar";

/// How far ahead the calendar window reaches. Long enough to always hold the
/// next CPI and FOMC, short enough to stay one small response.
pub const CALENDAR_DAYS: i64 = 45;
pub const CALENDAR_LOOKBACK_DAYS: i64 = 7;

/// Which paginated feed a page fetch targets, each with its score filter.
enum Feed<'a> {
    News(Option<&'a str>, Option<u8>),
    Insider(&'a str, Option<u8>),
}

/// Fetch one feed page, self-tuning the page size to the key's tier.
/// `PAGE_SIZE` (20) is what every tier allows and is used everywhere else.
/// `page50`: None = tier unknown, probe 50 on the first explicit paging
/// request (one extra request per session for non-Pro keys, and only when
/// the user asked for more); Some(true) = Pro confirmed, 50 everywhere;
/// Some(false) = 50 rejected once, stay on `PAGE_SIZE`.
async fn fetch_feed_page(
    client: &Client,
    feed: Feed<'_>,
    cursor: Option<&str>,
    sort: Sort,
    page50: &mut Option<bool>,
) -> Result<NewsPage> {
    // The probe spends a 400 on a non-Pro key, so it stays on the path the
    // user asked for: an explicit load-more of the published feed. A delta
    // poll never probes; it only takes 50 on a tier already known to be Pro.
    let try50 = matches!(*page50, Some(true))
        || (page50.is_none() && cursor.is_some() && sort == Sort::Published);
    if try50 {
        let result = match &feed {
            Feed::News(symbol, min) => client.news(*symbol, cursor, Some(50), *min, sort).await,
            Feed::Insider(symbol, min) => {
                client
                    .insider_news(symbol, cursor, Some(50), *min, sort)
                    .await
            }
        };
        match result {
            Ok(page) => {
                *page50 = Some(true);
                return Ok(page);
            }
            // The Pro-only page size bounces with a 400; fall back silently.
            Err(e) if page50.is_none() && format!("{e:#}").contains("API 400") => {
                *page50 = Some(false);
            }
            Err(e) => return Err(e),
        }
    }
    match &feed {
        Feed::News(symbol, min) => {
            client
                .news(*symbol, cursor, Some(PAGE_SIZE), *min, sort)
                .await
        }
        Feed::Insider(symbol, min) => {
            client
                .insider_news(symbol, cursor, Some(PAGE_SIZE), *min, sort)
                .await
        }
    }
}

/// Error event for a fetch, by what the fetch was: a load-more degrades to a
/// non-destructive `PageError`, a background delta poll to `PollError` (or to
/// a silent reprime when the cursor it carried is what the server rejected),
/// and an initial fetch replaces the view via `Error`.
fn error_event(key: String, e: anyhow::Error, mode: FeedMode, cursor: Option<&str>) -> Event {
    let error = format!("{e:#}");
    match mode {
        FeedMode::Append => {
            let gated = is_archive_gate(&error);
            Event::PageError { key, error, gated }
        }
        // Only a poll carrying a cursor can have its cursor rejected, so the
        // reprime it asks for (a poll with no cursor) cannot loop back here.
        FeedMode::Merge if cursor.is_some() && is_cursor_rejected(&error) => {
            Event::PollReprime { key }
        }
        FeedMode::Merge => Event::PollError { key, error },
        FeedMode::Replace => Event::Error { key, error },
    }
}

/// Whether a delta poll failed over the cursor it carried: a 400, which is
/// how the server answers a cursor it cannot read and one replayed into the
/// other sort mode (it never silently restarts), or the archive gate, which
/// in delta mode means the position aged past the plan's horizon rather than
/// the reader paging too deep.
fn is_cursor_rejected(error: &str) -> bool {
    error.contains("API 400") || is_archive_gate(error)
}

pub async fn run(
    initial_key: Option<String>,
    mut cmds: UnboundedReceiver<Cmd>,
    tx: UnboundedSender<SourceEvent>,
) {
    let mut client = initial_key.and_then(|k| Client::new(k).ok());
    // Whether this key may request page_size=50 (Pro); learned per session.
    let mut page50: Option<bool> = None;
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Cmd::SetKey(key) => {
                client = key.and_then(|k| Client::new(k).ok());
                page50 = None;
                if tx.send(SourceEvent::Alphai(Event::KeyChanged)).is_err() {
                    return;
                }
            }
            Cmd::FetchNews {
                symbol,
                cursor,
                min_relevance,
                sort,
            } => {
                let key = news_key(symbol.as_deref());
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphAI API key configured");
                    continue;
                };
                let mode = feed_mode(sort, cursor.as_deref());
                // Sentiment is a nice-to-have on the initial symbol fetch:
                // its failure must not blank the news list, so it degrades
                // to None. Pages and delta polls never refetch it.
                let (page, sentiment) = match (&symbol, mode) {
                    (Some(s), FeedMode::Replace) => {
                        let (p, senti) = tokio::join!(
                            fetch_feed_page(
                                client,
                                Feed::News(Some(s), min_relevance),
                                None,
                                sort,
                                &mut page50,
                            ),
                            client.sentiment(s)
                        );
                        (p, senti.ok())
                    }
                    _ => (
                        fetch_feed_page(
                            client,
                            Feed::News(symbol.as_deref(), min_relevance),
                            cursor.as_deref(),
                            sort,
                            &mut page50,
                        )
                        .await,
                        None,
                    ),
                };
                let event = match page {
                    Ok(p) => Event::Feed {
                        key,
                        articles: p.results,
                        side: sentiment.map(FeedPayload::Sentiment),
                        next_cursor: p.next_cursor,
                        mode,
                        min_relevance,
                    },
                    Err(e) => error_event(key, e, mode, cursor.as_deref()),
                };
                if tx.send(SourceEvent::Alphai(event)).is_err() {
                    return;
                }
            }
            Cmd::FetchTrending => {
                let key = TRENDING_KEY.to_string();
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphAI API key configured");
                    continue;
                };
                let event = match client.trending().await {
                    // A head-only feed: no cursor means paging never starts.
                    Ok(articles) => Event::Feed {
                        key,
                        articles,
                        side: None,
                        next_cursor: None,
                        mode: FeedMode::Replace,
                        min_relevance: None,
                    },
                    Err(e) => Event::Error {
                        key,
                        error: format!("{e:#}"),
                    },
                };
                if tx.send(SourceEvent::Alphai(event)).is_err() {
                    return;
                }
            }
            Cmd::FetchInsider {
                symbol,
                cursor,
                min_relevance,
                sort,
            } => {
                let key = insider_key(&symbol);
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphAI API key configured");
                    continue;
                };
                let mode = feed_mode(sort, cursor.as_deref());
                let (page, trades) = match mode {
                    FeedMode::Replace => {
                        let (p, t) = tokio::join!(
                            fetch_feed_page(
                                client,
                                Feed::Insider(&symbol, min_relevance),
                                None,
                                sort,
                                &mut page50,
                            ),
                            client.insider_trades(&symbol)
                        );
                        (p, t.ok())
                    }
                    _ => (
                        fetch_feed_page(
                            client,
                            Feed::Insider(&symbol, min_relevance),
                            cursor.as_deref(),
                            sort,
                            &mut page50,
                        )
                        .await,
                        None,
                    ),
                };
                let event = match page {
                    Ok(p) => Event::Feed {
                        key,
                        articles: p.results,
                        side: trades.map(|t| FeedPayload::Insider(Box::new(t))),
                        next_cursor: p.next_cursor,
                        mode,
                        min_relevance,
                    },
                    Err(e) => error_event(key, e, mode, cursor.as_deref()),
                };
                if tx.send(SourceEvent::Alphai(event)).is_err() {
                    return;
                }
            }
            Cmd::FetchEarnings { symbol } => {
                let key = earnings_key(&symbol);
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphAI API key configured");
                    continue;
                };
                let event = match client.earnings(&symbol).await {
                    Ok(data) => Event::Earnings {
                        key,
                        data: Box::new(data),
                    },
                    // A string no listing owns (crypto, a typo) is a state,
                    // not a failure: reporting it as an error would offer a
                    // retry that can never succeed.
                    Err(e) if is_unknown_symbol(&format!("{e:#}")) => Event::Earnings {
                        key,
                        data: Box::new(TickerEarnings {
                            ticker: symbol,
                            unknown: true,
                            ..Default::default()
                        }),
                    },
                    Err(e) => calendar_error(key, e),
                };
                if tx.send(SourceEvent::Alphai(event)).is_err() {
                    return;
                }
            }
            Cmd::FetchCalendar { from, to } => {
                let key = CALENDAR_KEY.to_string();
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphAI API key configured");
                    continue;
                };
                let event = match client.calendar(&from, &to).await {
                    Ok(events) => Event::Calendar { events, from, to },
                    Err(e) => calendar_error(key, e),
                };
                if tx.send(SourceEvent::Alphai(event)).is_err() {
                    return;
                }
            }
        }
    }
}

fn send_error(tx: &UnboundedSender<SourceEvent>, key: String, msg: &str) {
    let _ = tx.send(SourceEvent::Alphai(Event::Error {
        key,
        error: msg.to_string(),
    }));
}

// ---------------------------------------------------------------------------
// API shapes (tolerant subset of the OpenAPI schema at alphai.io/api/schema/).

#[derive(Deserialize)]
pub struct NewsPage {
    #[serde(default)]
    pub results: Vec<Article>,
    /// Cursor for the next (older) page; None at the end of the feed.
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
    detail: Option<String>,
    error: Option<String>,
    extra: Option<ErrorExtra>,
}

#[derive(Deserialize)]
struct ErrorExtra {
    reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Article {
    pub original: Original,
    #[serde(default)]
    pub enrichment: Enrichment,
    /// Story-collapse only: distinct outlets covering this story. None in the
    /// symbol-scoped feed and on rows predating the field.
    #[serde(default)]
    pub sources_count: Option<i64>,
    /// Insider feed only: the structured Form 4 event behind this row.
    /// None on news rows and on insider rows without transaction data.
    #[serde(default)]
    pub insider: Option<InsiderEvent>,
}

/// Structured SEC Form 4 event on an insider-feed row: the aggregate of the
/// filing's whole transaction group (a 10b5-1 ladder is one event: shares and
/// value are summed, the price is value-weighted). Replaces deriving the
/// trade side from the templated headline.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct InsiderEvent {
    /// "buy" (code P) / "sell" (S) / "other" — note code D (sale back to the
    /// issuer, a buyback) is "other", not a market disposition.
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub transaction_code: Option<String>,
    /// Decimal string, like the money fields below (both nullable when the
    /// filing prices no tranche).
    #[serde(default)]
    pub shares: Option<String>,
    #[serde(default)]
    pub avg_price_usd: Option<String>,
    #[serde(default)]
    pub total_value_usd: Option<String>,
    #[serde(default)]
    pub is_10b5_1: bool,
    #[serde(default)]
    pub insider_name: String,
    #[serde(default)]
    pub insider_title: String,
    #[serde(default)]
    pub is_officer: bool,
    #[serde(default)]
    pub is_director: bool,
    #[serde(default)]
    pub is_ten_percent_owner: bool,
    /// "YYYY-MM-DD".
    #[serde(default)]
    pub transaction_date: Option<String>,
}

impl InsiderEvent {
    /// The reporting owner's role: their title when the filing carries one,
    /// otherwise derived from the role flags.
    pub fn role(&self) -> &str {
        if !self.insider_title.is_empty() {
            &self.insider_title
        } else if self.is_officer {
            "Officer"
        } else if self.is_director {
            "Director"
        } else if self.is_ten_percent_owner {
            "10% owner"
        } else {
            ""
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Original {
    #[serde(default)]
    pub uid: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub time_published: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_domain: String,
    /// SEC Form 4 rows only: "direct" or "indirect" holding pool.
    #[serde(default)]
    pub ownership_form: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Enrichment {
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub tickers: Vec<String>,
    #[serde(default)]
    pub relevance_score: Option<i64>,
    #[serde(default)]
    pub ai_trading_insights: Option<Insights>,
    #[serde(default)]
    pub news_context_enhancement: Option<ContextEnhancement>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Insights {
    #[serde(default)]
    pub ticker_analysis: Vec<TickerAnalysis>,
    #[serde(default)]
    pub news_trading_value: Option<TradingValue>,
    #[serde(default)]
    pub alternative_perspectives: Option<Perspectives>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TickerAnalysis {
    #[serde(default)]
    pub ticker: String,
    #[serde(default)]
    pub impact_analysis: Option<Impact>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Impact {
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub sentiment: Option<String>,
    #[serde(default)]
    pub price_impact_prediction: Option<String>,
    #[serde(default)]
    pub confidence: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TradingValue {
    #[serde(default)]
    pub actionability_score: Option<String>,
    /// 1-10 novelty of the information; 0 marks rows enriched before the
    /// field existed and must render as unknown, not as a zero.
    #[serde(default)]
    pub information_novelty: Option<i64>,
    #[serde(default)]
    pub timing_relevance: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Perspectives {
    #[serde(default)]
    pub contrarian_view: Option<String>,
    #[serde(default)]
    pub overlooked_factors: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ContextEnhancement {
    #[serde(default)]
    pub background_context: Option<String>,
    #[serde(default)]
    pub key_entities: Vec<KeyEntity>,
    #[serde(default)]
    pub market_relevance_summary: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct KeyEntity {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl Article {
    pub fn published(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.original.time_published)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    /// Compact age like "35m" / "4h" / "3d"; empty when unparsable.
    pub fn age(&self, now: DateTime<Utc>) -> String {
        let Some(ts) = self.published() else {
            return String::new();
        };
        let mins = (now - ts).num_minutes().max(0);
        match mins {
            0..=59 => format!("{mins}m"),
            60..=1439 => format!("{}h", mins / 60),
            _ => format!("{}d", mins / 1440),
        }
    }

    /// The full AI impact analysis for one ticker (case-insensitive).
    pub fn impact_for(&self, ticker: &str) -> Option<&Impact> {
        self.enrichment
            .ai_trading_insights
            .as_ref()?
            .ticker_analysis
            .iter()
            .find(|t| t.ticker.eq_ignore_ascii_case(ticker))?
            .impact_analysis
            .as_ref()
    }

    /// The AI sentiment call for one ticker: "positive" / "neutral" / "negative".
    pub fn sentiment_for(&self, ticker: &str) -> Option<&str> {
        self.impact_for(ticker)?.sentiment.as_deref()
    }

    pub fn score(&self) -> i64 {
        self.enrichment.relevance_score.unwrap_or(0)
    }

    pub fn trading_value(&self) -> Option<&TradingValue> {
        self.enrichment
            .ai_trading_insights
            .as_ref()?
            .news_trading_value
            .as_ref()
    }

    /// Displayable novelty (1-10); None when missing or when the API sends
    /// the 0 sentinel for rows enriched before the field existed.
    pub fn novelty(&self) -> Option<i64> {
        self.trading_value()?.information_novelty.filter(|&n| n > 0)
    }

    /// Displayable outlet count; None when absent or when only one outlet
    /// carries the story (nothing worth badging).
    pub fn sources_badge(&self) -> Option<i64> {
        self.sources_count.filter(|&n| n > 1)
    }

    /// The article's page on alphai.io.
    pub fn alphai_url(&self) -> Option<String> {
        article_url_for(&self.original.uid, &self.original.title, self.published())
    }
}

/// The article page for one uid: `/news/article/{MM-DD}/{uid}/{slug}`. None
/// when there is no uid, no timestamp, or the title slugifies to nothing;
/// callers fall back to the original source URL. Free-standing so an
/// earnings read can build its link without a feed row.
pub fn article_url_for(uid: &str, title: &str, published: Option<DateTime<Utc>>) -> Option<String> {
    let uid = uid.trim();
    if uid.is_empty() {
        return None;
    }
    let date = published?.format("%m-%d");
    let slug = slugify(title);
    if slug.is_empty() {
        return None;
    }
    Some(format!("{SITE_URL}/news/article/{date}/{uid}/{slug}"))
}

/// Whether a feed row is the earnings filing itself rather than coverage of
/// it. Only the filing carries a read: it comes from SEC EDGAR (an 8-K item
/// 2.02, or a foreign private issuer's 6-K) and lands in the earnings
/// category, while reprints of the same quarter come from news outlets.
pub fn is_earnings_filing(a: &Article) -> bool {
    a.original.source_domain == "sec.gov" && a.enrichment.category.as_deref() == Some("earnings")
}

/// Mirror of the site's slugify: keep ASCII word chars, turn whitespace and
/// hyphen runs into single hyphens, drop the rest. An imperfect match is
/// harmless: the article page resolves by uid and 301s to the canonical slug.
fn slugify(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-') || c.is_whitespace())
        .map(|c| if c == '-' { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

#[derive(Clone, Debug, Deserialize)]
pub struct SentimentSummary {
    #[serde(default)]
    pub days: i64,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub bullish: i64,
    #[serde(default)]
    pub neutral: i64,
    #[serde(default)]
    pub bearish: i64,
}

/// Form 4 chart bundle from `/api/symbols/{t}/insider-trades/`. One filing's
/// tranche group is one event throughout; money is decimal strings like the
/// feed. Everything is optional-tolerant: the view renders whatever arrived
/// and skips the rest.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct InsiderTrades {
    /// Earliest recorded fill ("YYYY-MM-DD"); the chart window never claims
    /// history from before it.
    #[serde(default)]
    pub coverage_start: Option<String>,
    #[serde(default)]
    pub summary: Option<TradesSummary>,
    /// Monday-keyed weekly buy/sell dollar buckets, zero-filled up to the
    /// current week, capped to the trailing 12 months.
    #[serde(default)]
    pub series_weekly: Vec<WeekBucket>,
    /// Every event of the trailing 365 days, both sides, unfiltered by the
    /// feed's relevance score: the chart is always the full picture.
    #[serde(default)]
    pub chart_events: Vec<TradeEvent>,
}

/// The subset of the endpoint's summary the TUI shows (the head line and
/// the top reporters); the 3m and all-time windows ride along in the JSON
/// and are simply not parsed.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TradesSummary {
    #[serde(default)]
    pub last_12m: Option<TradesWindow>,
    /// Most active reporters of the last 12 months, at most five.
    #[serde(default)]
    pub top_insiders: Vec<TradeTopInsider>,
}

/// One trailing-window rollup. Counts are grouped events (not tranches);
/// a value is None when the window has no priced events on that side.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TradesWindow {
    #[serde(default)]
    pub buy_count: i64,
    #[serde(default)]
    pub sell_count: i64,
    #[serde(default)]
    pub buy_value_usd: Option<String>,
    #[serde(default)]
    pub sell_value_usd: Option<String>,
    #[serde(default)]
    pub unique_insiders: i64,
    #[serde(default)]
    pub pct_10b5_1: i64,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TradeTopInsider {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub event_count: i64,
    #[serde(default)]
    pub net_value_usd: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct WeekBucket {
    /// Monday of the bucket, "YYYY-MM-DD".
    #[serde(default)]
    pub week_start: String,
    #[serde(default)]
    pub buy_count: i64,
    #[serde(default)]
    pub sell_count: i64,
    #[serde(default)]
    pub buy_value_usd: Option<String>,
    #[serde(default)]
    pub sell_value_usd: Option<String>,
}

/// One chart event, trimmed to what the panel plots (who and how much per
/// share stay on the feed row's structured block). Unlike the feed's
/// `insider.side` (where code D maps to "other"), this surface counts D — a
/// sale back to the issuer — as "sell"; `transaction_code` tells the two
/// apart (P buy / S sale / D to issuer).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TradeEvent {
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub transaction_code: Option<String>,
    #[serde(default)]
    pub total_value_usd: Option<String>,
    /// Fills folded into this event (a 10b5-1 ladder files many).
    #[serde(default)]
    pub tranche_count: Option<i64>,
    /// Percent of the pre-event stake this event moved; sells negative.
    #[serde(default)]
    pub stake_change_pct: Option<String>,
    #[serde(default)]
    pub is_10b5_1: bool,
    #[serde(default)]
    pub late_filing: bool,
    /// Last fill of the group, "YYYY-MM-DD".
    #[serde(default)]
    pub transaction_date: Option<String>,
    /// Uid of the feed article behind this event; joins the chart to the
    /// list (selection highlight, stake/tranches in the detail pane).
    #[serde(default)]
    pub news_uid: Option<String>,
}

// ---------------------------------------------------------------------------
// Earnings reads: AlphAI's structured analysis of an earnings filing, and
// the macro calendar. Neither paginates.

/// One ticker's earnings reads plus the date of its next report, from
/// `GET /api/symbols/{ticker}/earnings/` — a single request per ticker.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TickerEarnings {
    #[serde(default)]
    pub ticker: String,
    /// Newest first, capped at 20 by the server. An empty list is a normal
    /// answer, not an error: no read has been published for this ticker.
    #[serde(default)]
    pub reports: Vec<EarningsRead>,
    /// The company-confirmed date of its next report (America/New_York), or
    /// None when none is confirmed. Never an estimate, so None means "not
    /// confirmed yet", not "does not report".
    #[serde(default)]
    pub next_report_date: Option<String>,
    /// Set by the fetcher when the API owns no such symbol (404), never on
    /// the wire. Terminal: the view says so instead of offering a retry.
    #[serde(skip)]
    pub unknown: bool,
}

impl TickerEarnings {
    pub fn next_report_day(&self) -> Option<NaiveDate> {
        let day = self.next_report_date.as_deref()?.trim();
        let parsed = NaiveDate::parse_from_str(day, "%Y-%m-%d").ok()?;
        (parsed.to_string() == day).then_some(parsed)
    }

    /// The newest read, the one the view opens on.
    pub fn latest(&self) -> Option<&EarningsRead> {
        self.reports.first()
    }
}

/// One published read in a ticker's history.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EarningsRead {
    /// Article uid: the same read is served inline by `/api/news/{uid}/`, and
    /// it joins a read to the filing's row in the news feed.
    #[serde(default)]
    pub uid: String,
    /// The share class the filing was actually made under, which is not
    /// always the one asked for: a GOOGL request carries reads filed under
    /// GOOG.
    #[serde(default)]
    pub ticker: String,
    #[serde(default)]
    pub fiscal_period: String,
    #[serde(default)]
    pub time_published: String,
    #[serde(default)]
    pub title: String,
    /// `sec_form8k` for a US filer's item 2.02, `sec_form6k` for a foreign
    /// private issuer's earnings release.
    #[serde(default)]
    pub source_type: String,
    /// The read itself. Reached through `report()` so no call site has to
    /// read `read.analysis.analysis`.
    #[serde(default)]
    pub analysis: EarningsReport,
}

impl EarningsRead {
    pub fn report(&self) -> &EarningsReport {
        &self.analysis
    }

    pub fn filed(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.time_published)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }

    /// The filing form, for the feed cell and the view header.
    pub fn form(&self) -> &'static str {
        if self.source_type == "sec_form6k" {
            "6-K"
        } else {
            "8-K"
        }
    }

    /// The read's own article page on alphai.io.
    pub fn alphai_url(&self) -> Option<String> {
        article_url_for(&self.uid, &self.title, self.filed())
    }
}

/// AlphAI's structured read of one earnings release. Every figure is a
/// string copied verbatim from the filing (the server checks each one
/// against the filing text before publishing), so the client shortens units
/// but never recomputes a number: see `short_metric`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EarningsReport {
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub ticker: String,
    /// Free text as the filing words it: "Second Quarter Fiscal 2027",
    /// "fiscal 2026 third quarter", "six months ended June 30, 2026".
    #[serde(default)]
    pub fiscal_period: String,
    #[serde(default)]
    pub period_end: Option<String>,
    #[serde(default)]
    pub headline: String,
    /// strong / solid / mixed / weak. Kept a string: an unknown value must
    /// render, not break the view.
    #[serde(default)]
    pub verdict: String,
    #[serde(default)]
    pub verdict_reason: String,
    #[serde(default)]
    pub key_metrics: Vec<KeyMetric>,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub guidance: Option<Guidance>,
    #[serde(default)]
    pub vs_prior_guidance: Vec<GuidanceCheck>,
    #[serde(default)]
    pub capital_returns: Vec<String>,
    #[serde(default)]
    pub balance_sheet_cash_flow: Vec<String>,
    #[serde(default)]
    pub drivers: Vec<String>,
    #[serde(default)]
    pub concerns: Vec<String>,
    #[serde(default)]
    pub what_to_watch: Vec<String>,
    #[serde(default)]
    pub quotes: Vec<SpeakerQuote>,
    /// Several paragraphs, separated by blank lines.
    #[serde(default)]
    pub analysis: String,
    /// What the filing did NOT state, named rather than guessed.
    #[serde(default)]
    pub missing_items: Vec<String>,
    #[serde(default)]
    pub numbers_verified_from_document: bool,
}

impl EarningsReport {
    /// The multi-paragraph narrative, by a name that is not `analysis`.
    pub fn narrative(&self) -> &str {
        &self.analysis
    }
}

/// One line of the filing's own numbers. Comparisons are None when the
/// filing did not state them (the guard nulls anything it cannot verify).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct KeyMetric {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
    /// GAAP / non-GAAP / other.
    #[serde(default)]
    pub basis: String,
    #[serde(default)]
    pub prior_year: Option<String>,
    #[serde(default)]
    pub prior_quarter: Option<String>,
    #[serde(default)]
    pub yoy_change: Option<String>,
    #[serde(default)]
    pub qoq_change: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Segment {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub revenue: String,
    #[serde(default)]
    pub yoy_change: Option<String>,
    #[serde(default)]
    pub qoq_change: Option<String>,
    #[serde(default)]
    pub driver: String,
}

/// The company's own outlook, when the release gave one.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Guidance {
    #[serde(default)]
    pub period: String,
    #[serde(default)]
    pub revenue: Option<String>,
    #[serde(default)]
    pub gross_margin: Option<String>,
    #[serde(default)]
    pub operating_expenses: Option<String>,
    #[serde(default)]
    pub tax_rate: Option<String>,
    #[serde(default)]
    pub other: Vec<String>,
}

/// A reported figure against the company's own prior outlook.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct GuidanceCheck {
    #[serde(default)]
    pub metric: String,
    #[serde(default)]
    pub prior_guidance: String,
    #[serde(default)]
    pub actual: String,
    /// above / in line / below / n/a.
    #[serde(default)]
    pub verdict: String,
}

/// Management, verbatim. Named for the price `Quote` in `crate::domain`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SpeakerQuote {
    #[serde(default)]
    pub speaker: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub text: String,
}

/// One scheduled US macro release, from `GET /api/calendar/`.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct CalendarEvent {
    #[serde(default, deserialize_with = "calendar_string")]
    pub uid: String,
    #[serde(default, deserialize_with = "calendar_string")]
    pub event_key: String,
    #[serde(default, deserialize_with = "calendar_string")]
    pub reference_period: String,
    #[serde(default)]
    pub release_stage: Option<String>,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default, deserialize_with = "calendar_string")]
    pub title: String,
    #[serde(default, deserialize_with = "calendar_string")]
    pub scheduled_at: String,
    /// upcoming / elapsed. Says only that the moment passed, never that the
    /// agency published.
    #[serde(default, deserialize_with = "calendar_string")]
    pub phase: String,
    /// scheduled / postponed / cancelled. Read this before `phase`.
    #[serde(default, deserialize_with = "calendar_string")]
    pub schedule_status: String,
    /// official (printed on the agency's schedule) / inferred (derived from
    /// the documented cadence). Shown, not hidden.
    #[serde(default, deserialize_with = "calendar_string")]
    pub schedule_basis: String,
    /// high / medium / low.
    #[serde(default, deserialize_with = "calendar_string")]
    pub importance: String,
    /// FOMC decisions only: the press conference.
    #[serde(default)]
    pub press_conference_at: Option<String>,
    /// FOMC decisions only: the meeting carries a Summary of Economic
    /// Projections (the dot plot).
    #[serde(default)]
    pub has_sep: bool,
}

// Some schedule fields may be absent on postponed/undated events. Null
// carries the same lack of information as a missing string, not a bad window.
fn calendar_string<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

impl CalendarEvent {
    pub fn short_name(&self) -> &str {
        if self.event_key == "fomc_decision" {
            return "FOMC";
        }
        let title = self.title.split(" (").next().unwrap_or("").trim();
        // Use the published title for series whose event_key is not known.
        // These are display abbreviations, never assumptions about a wire enum.
        for (prefix, short) in [
            ("FOMC minutes", "minutes"),
            ("CPI", "CPI"),
            ("PPI", "PPI"),
            ("Nonfarm payrolls", "jobs"),
            ("Employment Situation", "jobs"),
            ("GDP", "GDP"),
            ("PCE", "PCE"),
            ("Retail sales", "retail"),
            ("Initial jobless claims", "claims"),
            ("JOLTS", "JOLTS"),
        ] {
            if title.starts_with(prefix) {
                return short;
            }
        }
        if title.is_empty() {
            "Macro event"
        } else {
            title
        }
    }

    pub fn scheduled(&self) -> Option<DateTime<Utc>> {
        DateTime::parse_from_rfc3339(&self.scheduled_at)
            .ok()
            .map(|t| t.with_timezone(&Utc))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CalendarEvents {
    #[serde(default)]
    pub events: Vec<CalendarEvent>,
}

/// "25000.0000" (API decimal string) -> "25,000"; fractional shares round.
/// Unparsable input passes through untouched, like `fmt_usd`.
pub fn fmt_shares(decimal: &str) -> String {
    let Ok(v) = decimal.parse::<f64>() else {
        return decimal.to_string();
    };
    let digits = format!("{:.0}", v.abs());
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if v < 0.0 { format!("-{out}") } else { out }
}

/// Shorten a filing figure for a narrow column: units only, never the
/// number itself. "$96,221 million" becomes "$96,221M" and never "$96.2B" —
/// a rounded figure is a new number, and every figure here is quoted from
/// the filing precisely so it is not one.
pub fn short_metric(value: &str) -> String {
    let mut trimmed = value.trim();
    // Balance-sheet lines carry the date they were measured on; the column
    // header already says which period the column is.
    if let Some(i) = trimmed.find(" as of ") {
        trimmed = trimmed[..i].trim_end();
    }
    let mut out = trimmed.to_string();
    for (long, short) in [
        (" billion", "B"),
        (" million", "M"),
        (" percentage points", "pt"),
        (" percentage point", "pt"),
        (" basis points", "bp"),
        (" pts", "pt"),
        (" pt", "pt"),
        (" percent", "%"),
        (" per diluted share", ""),
        (" per share", ""),
    ] {
        out = out.replace(long, short);
    }
    out
}

/// Phrases a filing appends to a change that the column header already
/// states. Dropping them shortens the cell without touching the figure.
const CHANGE_TAILS: [&str; 6] = [
    " year over year",
    " from a year ago",
    " from the year-ago quarter",
    " sequentially",
    " quarter over quarter",
    " from the prior quarter",
];

/// A change as the filing states it, with a leading `+` when it carries no
/// sign of its own ("18%" -> "+18%"). Only the sign is added: a filing that
/// wrote a decline wrote the minus itself.
pub fn signed_change(change: &str) -> String {
    let mut out = short_metric(change);
    let lower = out.to_lowercase();
    for tail in CHANGE_TAILS {
        if let Some(cut) = lower.find(tail) {
            out.truncate(cut);
            break;
        }
    }
    // "up 16%" and "down 5%" are the filing's own words for a sign, so they
    // become one. The figure itself is never touched.
    let t = out.trim();
    for (word, sign) in [("up ", "+"), ("down ", "-")] {
        if let Some(rest) = t.strip_prefix(word) {
            return format!("{sign}{rest}");
        }
    }
    if t.starts_with(|c: char| c.is_ascii_digit()) {
        return format!("+{t}");
    }
    t.to_string()
}

/// "Second Quarter Fiscal 2027" -> "Q2 FY27", for headers too narrow for the
/// filing's own wording. Anything that does not name a quarter and a year is
/// returned untouched: the wording is free text and inventing a period is
/// worse than a long one.
pub fn short_fiscal_period(period: &str) -> String {
    let lower = period.to_lowercase();
    let quarter = [
        ("first quarter", 1),
        ("second quarter", 2),
        ("third quarter", 3),
        ("fourth quarter", 4),
        ("q1", 1),
        ("q2", 2),
        ("q3", 3),
        ("q4", 4),
    ]
    .into_iter()
    .find(|(word, _)| lower.contains(word))
    .map(|(_, n)| n);
    let year = lower
        .split(|c: char| !c.is_ascii_digit())
        .find(|t| t.len() == 4 && (t.starts_with('1') || t.starts_with('2')))
        .and_then(|t| t.parse::<u32>().ok());
    match (quarter, year) {
        (Some(q), Some(y)) => format!("Q{q} FY{:02}", y % 100),
        _ => period.to_string(),
    }
}

/// "1234567.89" (API decimal string) -> "$1.2M"; sign kept in front.
pub fn fmt_usd(decimal: &str) -> String {
    let Ok(v) = decimal.parse::<f64>() else {
        return decimal.to_string();
    };
    let sign = if v < 0.0 { "-" } else { "" };
    let a = v.abs();
    if a >= 1e9 {
        format!("{sign}${:.1}B", a / 1e9)
    } else if a >= 1e6 {
        format!("{sign}${:.1}M", a / 1e6)
    } else if a >= 1e3 {
        format!("{sign}${:.1}K", a / 1e3)
    } else {
        format!("{sign}${a:.0}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "results": [{
        "original": {
          "uid": "788e477c66f3849b",
          "title": "NVIDIA beats on Q2 earnings",
          "url": "https://example.com/nvda",
          "time_published": "2026-07-10T12:30:00Z",
          "summary": "Data-center revenue grew again.",
          "source": "Example Wire",
          "source_domain": "example.com"
        },
        "enrichment": {
          "category": "earnings",
          "tickers": ["NVDA"],
          "relevance_score": 9,
          "ai_trading_insights": {
            "ticker_analysis": [{
              "ticker": "NVDA",
              "impact_analysis": {
                "summary": "Beat lifts the data-center thesis.",
                "sentiment": "positive",
                "price_impact_prediction": "+2-4% near term",
                "confidence": "high",
                "reasoning": "Guidance raised on top of the beat."
              }
            }],
            "news_trading_value": {
              "actionability_score": "high",
              "information_novelty": 7,
              "timing_relevance": "pre-market"
            },
            "alternative_perspectives": {
              "contrarian_view": "Growth is priced in.",
              "overlooked_factors": "Export limits loom."
            }
          },
          "news_context_enhancement": {
            "background_context": "Third consecutive beat.",
            "key_entities": [
              {"name": "NVIDIA", "type": "company", "description": "GPU maker"}
            ],
            "market_relevance_summary": "Sets the tone for semis."
          }
        },
        "story_id": null,
        "sources_count": 7,
        "sources": null
      }],
      "next_cursor": null
    }"#;

    #[test]
    fn parses_news_page() {
        let page: NewsPage = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(page.results.len(), 1);
        let a = &page.results[0];
        assert_eq!(a.original.title, "NVIDIA beats on Q2 earnings");
        assert_eq!(a.score(), 9);
        assert_eq!(a.sentiment_for("nvda"), Some("positive"));
        assert_eq!(a.sentiment_for("AAPL"), None);
        assert!(a.published().is_some());
        let impact = a.impact_for("NVDA").unwrap();
        assert_eq!(impact.confidence.as_deref(), Some("high"));
        assert_eq!(
            impact.price_impact_prediction.as_deref(),
            Some("+2-4% near term")
        );
        assert_eq!(a.novelty(), Some(7));
        assert_eq!(a.sources_badge(), Some(7));
        assert_eq!(
            a.trading_value().unwrap().actionability_score.as_deref(),
            Some("high")
        );
        let ctx = a.enrichment.news_context_enhancement.as_ref().unwrap();
        assert_eq!(ctx.key_entities[0].kind.as_deref(), Some("company"));
    }

    #[test]
    fn parses_next_cursor() {
        let page: NewsPage = serde_json::from_str(SAMPLE).unwrap();
        assert_eq!(page.next_cursor, None);
        let raw = r#"{"results": [], "next_cursor": "abc123"}"#;
        let page: NewsPage = serde_json::from_str(raw).unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("abc123"));
    }

    #[test]
    fn archive_gate_is_detectable() {
        assert!(is_archive_gate(ARCHIVE_GATE_MSG));
        assert!(!is_archive_gate("AlphAI API 400 Bad Request: bad cursor"));
    }

    /// Published requests must stay byte for byte what they were before delta
    /// mode existed: `published` is the server's default, so it goes on the
    /// wire as no parameter at all.
    #[test]
    fn only_delta_requests_carry_a_sort() {
        assert_eq!(Sort::Published.param(), None);
        assert_eq!(Sort::Ingested.param(), Some("ingested"));
        assert_eq!(Sort::default(), Sort::Published);
    }

    #[test]
    fn fetch_mode_follows_the_sort_and_the_cursor() {
        assert_eq!(feed_mode(Sort::Published, None), FeedMode::Replace);
        assert_eq!(feed_mode(Sort::Published, Some("c1")), FeedMode::Append);
        // A delta page merges either way: the priming poll carries no cursor.
        assert_eq!(feed_mode(Sort::Ingested, None), FeedMode::Merge);
        assert_eq!(feed_mode(Sort::Ingested, Some("d1")), FeedMode::Merge);
    }

    /// A poll that carried a position and had it rejected reprimes silently;
    /// the same failure without a position is a real error, which is what
    /// keeps a reprime from looping into another reprime.
    #[test]
    fn poll_failures_split_by_what_the_poll_carried() {
        let bad_cursor = || anyhow::anyhow!("AlphAI API 400: invalid cursor");
        assert!(matches!(
            error_event("k".into(), bad_cursor(), FeedMode::Merge, Some("d1")),
            Event::PollReprime { .. }
        ));
        assert!(matches!(
            error_event(
                "k".into(),
                anyhow::anyhow!(ARCHIVE_GATE_MSG),
                FeedMode::Merge,
                Some("d1")
            ),
            Event::PollReprime { .. }
        ));
        assert!(matches!(
            error_event("k".into(), bad_cursor(), FeedMode::Merge, None),
            Event::PollError { .. }
        ));
        assert!(matches!(
            error_event(
                "k".into(),
                anyhow::anyhow!("AlphAI API 429: slow down"),
                FeedMode::Merge,
                Some("d1")
            ),
            Event::PollError { .. }
        ));
        // The published paths keep their old shapes.
        assert!(matches!(
            error_event(
                "k".into(),
                anyhow::anyhow!(ARCHIVE_GATE_MSG),
                FeedMode::Append,
                Some("c1")
            ),
            Event::PageError { gated: true, .. }
        ));
        assert!(matches!(
            error_event("k".into(), bad_cursor(), FeedMode::Replace, None),
            Event::Error { .. }
        ));
    }

    #[test]
    fn novelty_and_sources_sentinels_hide() {
        // novelty 0 marks rows enriched before the field existed; a single
        // outlet is not worth a badge. Both must read as "unknown".
        let raw = r#"{
          "original": {"title": "x"},
          "enrichment": {"ai_trading_insights": {"news_trading_value": {"information_novelty": 0}}},
          "sources_count": 1
        }"#;
        let a: Article = serde_json::from_str(raw).unwrap();
        assert_eq!(a.novelty(), None);
        assert_eq!(a.sources_badge(), None);
        let bare: Article = serde_json::from_str(r#"{"original": {"title": "y"}}"#).unwrap();
        assert_eq!(bare.novelty(), None);
        assert_eq!(bare.sources_badge(), None);
    }

    #[test]
    fn parses_trending_bare_array() {
        // /api/news/trending/ returns a bare array, not the paginated shape.
        let raw = r#"[{"original": {"title": "Fed cuts"}, "sources_count": 4}]"#;
        let items: Vec<Article> = serde_json::from_str(raw).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].sources_badge(), Some(4));
    }

    #[test]
    fn parses_insider_article_fields() {
        let raw = r#"{"original": {"title": "sold stock", "ownership_form": "indirect"}}"#;
        let a: Article = serde_json::from_str(raw).unwrap();
        assert_eq!(a.original.ownership_form.as_deref(), Some("indirect"));
        let plain: Article = serde_json::from_str(r#"{"original": {"title": "no form"}}"#).unwrap();
        assert_eq!(plain.original.ownership_form, None);
        assert!(
            plain.insider.is_none(),
            "legacy rows must parse without the block"
        );
    }

    #[test]
    fn parses_structured_insider_block() {
        let raw = r#"{
          "original": {"title": "STEVENS MARK A sold shares"},
          "insider": {
            "side": "sell",
            "transaction_code": "S",
            "shares": "25000.0000",
            "avg_price_usd": "187.3200",
            "total_value_usd": "4683000.00",
            "is_10b5_1": true,
            "insider_name": "STEVENS MARK A",
            "insider_title": "Director",
            "is_officer": false,
            "is_director": true,
            "is_ten_percent_owner": false,
            "transaction_date": "2026-07-09"
          }
        }"#;
        let a: Article = serde_json::from_str(raw).unwrap();
        let t = a.insider.as_ref().unwrap();
        assert_eq!(t.side.as_deref(), Some("sell"));
        assert_eq!(t.transaction_code.as_deref(), Some("S"));
        assert_eq!(fmt_shares(t.shares.as_deref().unwrap()), "25,000");
        assert_eq!(fmt_usd(t.total_value_usd.as_deref().unwrap()), "$4.7M");
        assert!(t.is_10b5_1);
        assert_eq!(t.role(), "Director");
        assert_eq!(t.transaction_date.as_deref(), Some("2026-07-09"));

        // Unpriced filings null the money fields; the block still parses.
        let raw = r#"{
          "original": {"title": "grant"},
          "insider": {"side": "other", "shares": "100", "is_10b5_1": false,
                      "insider_name": "X", "insider_title": "",
                      "is_officer": true, "is_director": false,
                      "is_ten_percent_owner": false,
                      "avg_price_usd": null, "total_value_usd": null}
        }"#;
        let a: Article = serde_json::from_str(raw).unwrap();
        let t = a.insider.as_ref().unwrap();
        assert_eq!(t.total_value_usd, None);
        assert_eq!(t.role(), "Officer");
    }

    #[test]
    fn shares_formatting_groups_thousands() {
        assert_eq!(fmt_shares("25000.0000"), "25,000");
        assert_eq!(fmt_shares("512"), "512");
        assert_eq!(fmt_shares("1234567"), "1,234,567");
        assert_eq!(fmt_shares("-2500"), "-2,500");
        assert_eq!(fmt_shares("garbage"), "garbage");
    }

    #[test]
    fn builds_alphai_article_url() {
        let page: NewsPage = serde_json::from_str(SAMPLE).unwrap();
        let a = &page.results[0];
        assert_eq!(a.original.uid, "788e477c66f3849b");
        assert_eq!(
            a.alphai_url().unwrap(),
            "https://alphai.io/news/article/07-10/788e477c66f3849b/nvidia-beats-on-q2-earnings"
        );

        // No uid (or a title with no ASCII word chars) -> fall back to original.
        let mut b = a.clone();
        b.original.uid = String::new();
        assert!(b.alphai_url().is_none());
        let mut c = a.clone();
        c.original.title = "Новости — заголовок кириллицей".into();
        assert!(c.alphai_url().is_none());
    }

    #[test]
    fn slugify_matches_site_convention() {
        assert_eq!(
            slugify("NVIDIA beats on Q2 earnings"),
            "nvidia-beats-on-q2-earnings"
        );
        assert_eq!(slugify("Apple's Q2: beats!"), "apples-q2-beats");
        assert_eq!(
            slugify("AI  -  the new   gold rush"),
            "ai-the-new-gold-rush"
        );
        assert_eq!(slugify("Привет"), "");
    }

    #[test]
    fn tolerates_missing_enrichment() {
        let raw = r#"{"results": [{"original": {"title": "x"}}]}"#;
        let page: NewsPage = serde_json::from_str(raw).unwrap();
        assert_eq!(page.results[0].score(), 0);
        assert!(page.results[0].sentiment_for("NVDA").is_none());
    }

    #[test]
    fn parses_insider_trades() {
        // Mirrors the live /api/symbols/{t}/insider-trades/ shape: summary
        // windows, Monday-keyed weekly buckets, per-event chart rows with
        // stake/tranche extras, decimals as strings.
        let raw = r#"{
          "ticker": "CRWV",
          "coverage_start": "2025-08-14",
          "summary": {
            "last_3m": { "buy_count": 0, "sell_count": 58, "buy_value_usd": null,
                         "sell_value_usd": "1900000000", "unique_insiders": 5, "pct_10b5_1": 64 },
            "last_12m": { "buy_count": 0, "sell_count": 487, "buy_value_usd": null,
                          "sell_value_usd": "6420000000", "unique_insiders": 11, "pct_10b5_1": 70 },
            "all_time": { "buy_count": 0, "sell_count": 487, "buy_value_usd": null,
                          "sell_value_usd": "6420000000", "unique_insiders": 11, "pct_10b5_1": 70 },
            "top_insiders": [
              { "name": "Intrator Michael N", "title": "CEO and President",
                "event_count": 134, "net_value_usd": "-505000000" }
            ]
          },
          "series_weekly": [
            { "week_start": "2026-07-27", "buy_count": 0, "sell_count": 9,
              "buy_value_usd": "0", "sell_value_usd": "48200000" },
            { "week_start": "2026-08-03", "buy_count": 0, "sell_count": 3,
              "buy_value_usd": "0", "sell_value_usd": "30600000" }
          ],
          "chart_events": [
            { "side": "sell", "transaction_code": "S", "ownership_form": "I",
              "security_title": "Common Stock", "shares": "107692",
              "avg_price_usd": "91.8", "total_value_usd": "9886021.65",
              "tranche_count": 8, "stake_change_pct": "-100.0",
              "is_10b5_1": true, "late_filing": false,
              "insider_name": "Intrator Michael N", "insider_title": "CEO and President",
              "is_officer": true, "is_director": true, "is_ten_percent_owner": true,
              "transaction_date": "2026-08-04", "filed_at": "2026-08-07T00:36:56+00:00",
              "news_uid": "788e477c66f3849b", "news_title": "…", "has_article": true },
            { "side": "sell", "transaction_code": "D", "shares": "1000",
              "avg_price_usd": null, "total_value_usd": null,
              "is_10b5_1": false, "insider_name": "X", "insider_title": "",
              "transaction_date": "2026-07-20", "news_uid": null }
          ],
          "events": [],
          "next_cursor": "cD0yMDI2"
        }"#;
        let t: InsiderTrades = serde_json::from_str(raw).unwrap();
        assert_eq!(t.coverage_start.as_deref(), Some("2025-08-14"));
        let s = t.summary.as_ref().unwrap();
        let m12 = s.last_12m.as_ref().unwrap();
        assert_eq!((m12.buy_count, m12.sell_count), (0, 487));
        assert_eq!(m12.buy_value_usd, None);
        assert_eq!(fmt_usd(m12.sell_value_usd.as_deref().unwrap()), "$6.4B");
        assert_eq!(m12.unique_insiders, 11);
        assert_eq!(s.top_insiders[0].event_count, 134);
        assert_eq!(
            fmt_usd(s.top_insiders[0].net_value_usd.as_deref().unwrap()),
            "-$505.0M"
        );
        assert_eq!(t.series_weekly.len(), 2);
        assert_eq!(t.series_weekly[0].week_start, "2026-07-27");
        assert_eq!(t.series_weekly[0].sell_count, 9);
        let e = &t.chart_events[0];
        assert_eq!(e.side.as_deref(), Some("sell"));
        assert_eq!(e.tranche_count, Some(8));
        assert_eq!(e.stake_change_pct.as_deref(), Some("-100.0"));
        assert_eq!(e.news_uid.as_deref(), Some("788e477c66f3849b"));
        // The unpriced code-D event still parses; sale-to-issuer stays
        // distinguishable via the code even though side says "sell" here.
        let d = &t.chart_events[1];
        assert_eq!(d.transaction_code.as_deref(), Some("D"));
        assert_eq!(d.total_value_usd, None);

        // A ticker with no coverage serves nulls and empty arrays.
        let bare: InsiderTrades = serde_json::from_str(r#"{"ticker": "X"}"#).unwrap();
        assert!(bare.summary.is_none());
        assert!(bare.chart_events.is_empty());
    }

    #[test]
    fn usd_formatting_bands() {
        assert_eq!(fmt_usd("512"), "$512");
        assert_eq!(fmt_usd("2500"), "$2.5K");
        assert_eq!(fmt_usd("1300000000"), "$1.3B");
        assert_eq!(fmt_usd("garbage"), "garbage");
    }

    /// Live end-to-end check against the real API (14 requests).
    /// Run: ALPHAI_API_KEY=ak_live_… cargo test live_api -- --ignored
    #[tokio::test]
    #[ignore = "live API call; needs ALPHAI_API_KEY"]
    async fn live_api_smoke() {
        let key = std::env::var("ALPHAI_API_KEY").expect("set ALPHAI_API_KEY");
        let client = Client::new(key).unwrap();
        // min_relevance 4 mirrors the server default, so the filter param is
        // exercised without changing what the feed returns.
        // PAGE_SIZE is what the app actually sends, and it rides on the API
        // allowing it without a Pro key: ask for it here so a tightened cap
        // surfaces as a failing smoke test rather than a 400 for every free
        // user.
        let news = client
            .news(
                Some("NVDA"),
                None,
                Some(PAGE_SIZE),
                Some(4),
                Sort::Published,
            )
            .await
            .unwrap();
        assert!(!news.results.is_empty(), "empty NVDA news feed");
        assert!(
            news.results.len() <= PAGE_SIZE as usize,
            "page over PAGE_SIZE"
        );
        assert!(news.results[0].original.title.len() > 3);
        // The feed is deeper than one page, so the cursor must be present
        // and must fetch an older second page.
        let cursor = news.next_cursor.expect("no next_cursor on page 1");
        let page2 = client
            .news(Some("NVDA"), Some(&cursor), None, Some(4), Sort::Published)
            .await
            .unwrap();
        assert!(!page2.results.is_empty(), "empty second news page");
        let senti = client.sentiment("NVDA").await.unwrap();
        assert!(senti.days > 0);
        let filings = client
            .insider_news("NVDA", None, None, Some(4), Sort::Published)
            .await
            .unwrap();
        assert!(!filings.results.is_empty(), "empty NVDA insider feed");
        // Every Form 4 row is backed by transaction data, so the structured
        // block must be present on the live feed.
        assert!(
            filings.results.iter().any(|a| a.insider.is_some()),
            "no structured insider block on the live feed"
        );
        let trades = client.insider_trades("NVDA").await.unwrap();
        let windows = trades.summary.expect("no summary block on insider-trades");
        assert!(windows.last_12m.is_some(), "no last_12m window");
        assert!(
            !trades.chart_events.is_empty(),
            "empty chart_events for NVDA"
        );
        assert!(
            !trades.series_weekly.is_empty(),
            "empty series_weekly for NVDA"
        );
        let trending = client.trending().await.unwrap();
        assert!(!trending.is_empty(), "empty trending feed");

        // The earnings surface: a ticker with a read, a ticker without one
        // (an empty list is a normal 200), and a string no listing owns.
        let earnings = client.earnings("NVDA").await.unwrap();
        let read = earnings.latest().expect("no earnings read for NVDA");
        assert!(
            !read.report().key_metrics.is_empty(),
            "a published read always carries metrics"
        );
        assert!(read.report().numbers_verified_from_document);
        let bare = client.earnings("KO").await.unwrap();
        assert!(
            bare.latest().is_none() || !bare.reports.is_empty(),
            "the empty history must parse either way"
        );
        let missing = client.earnings("ZZZQQ").await.unwrap_err();
        assert!(
            is_unknown_symbol(&format!("{missing:#}")),
            "an unknown ticker stopped answering 404: {missing:#}"
        );

        // The macro calendar: one window, one request, always populated.
        let today = Utc::now().date_naive();
        let events = client
            .calendar(
                &today.to_string(),
                &(today + chrono::Duration::days(CALENDAR_DAYS)).to_string(),
            )
            .await
            .unwrap();
        assert!(!events.is_empty(), "empty calendar window");
        assert!(events.iter().all(|e| e.scheduled().is_some()));

        // Delta mode, the whole of the app's refresh path. The priming page
        // carries rows and a position; replaying that position returns only
        // what has arrived since, which is usually nothing, and still hands
        // back a position (there is no terminal page in this mode).
        let prime = client
            .news(Some("NVDA"), None, Some(PAGE_SIZE), Some(4), Sort::Ingested)
            .await
            .unwrap();
        assert!(!prime.results.is_empty(), "empty priming delta page");
        let delta_cursor = prime.next_cursor.expect("no position on the priming page");
        let delta = client
            .news(
                Some("NVDA"),
                Some(&delta_cursor),
                None,
                Some(4),
                Sort::Ingested,
            )
            .await
            .unwrap();
        assert!(
            delta.next_cursor.is_some(),
            "delta mode dropped the position"
        );
        // The two cursor families must stay mutually unreadable: the app's
        // reprime path exists because this is a 400 and not a silent restart
        // into a different range of the feed.
        let crossed = match client
            .news(Some("NVDA"), Some(&cursor), None, Some(4), Sort::Ingested)
            .await
        {
            Ok(_) => panic!("a published cursor was accepted in delta mode"),
            Err(e) => format!("{e:#}"),
        };
        assert!(
            crossed.contains("API 400"),
            "crossed cursor did not 400: {crossed}"
        );
        assert!(
            is_cursor_rejected(&crossed),
            "a crossed cursor must reprime"
        );
        // The insider feed answers delta mode too (it did not in July 2026),
        // which is where it matters most: a Form 4 is filed days after its
        // trade and enters the feed below the published head.
        let insider_delta = client
            .insider_news("NVDA", None, Some(PAGE_SIZE), Some(4), Sort::Ingested)
            .await
            .unwrap();
        assert!(
            insider_delta.next_cursor.is_some(),
            "insider feed refused a delta position"
        );
    }

    /// Trimmed to four metrics; the shape is the live payload's.
    const EARNINGS_SAMPLE: &str = r#"{
      "ticker": "NVDA",
      "next_report_date": "2026-11-17",
      "reports": [{
        "uid": "352613cc3f6089cc",
        "time_published": "2026-08-26T20:21:19Z",
        "title": "NVIDIA CORP (NVDA): Results of Operations and Financial Condition",
        "source_type": "sec_form8k",
        "ticker": "NVDA",
        "fiscal_period": "Second Quarter Fiscal 2027",
        "analysis": {
          "company": "NVIDIA CORP",
          "ticker": "NVDA",
          "fiscal_period": "Second Quarter Fiscal 2027",
          "period_end": "July 26, 2026",
          "headline": "NVIDIA Announces Financial Results for Second Quarter Fiscal 2027",
          "verdict": "strong",
          "verdict_reason": "Revenue grew 18% sequentially and 106% year over year.",
          "key_metrics": [
            {"name": "Revenue", "value": "$96,221 million", "basis": "GAAP",
             "prior_year": "$46,743 million", "prior_quarter": "$81,615 million",
             "yoy_change": "106%", "qoq_change": "18%"},
            {"name": "Cost of revenue", "value": "$24,079 million", "basis": "GAAP",
             "prior_year": "$12,890 million", "prior_quarter": "$20,458 million",
             "yoy_change": null, "qoq_change": null},
            {"name": "Basic earnings per share", "value": "$2.47 per share", "basis": "GAAP",
             "prior_year": "$1.08 per share", "prior_quarter": null,
             "yoy_change": null, "qoq_change": null},
            {"name": "Non-GAAP diluted earnings per share", "value": "$2.22 per diluted share",
             "basis": "non-GAAP", "prior_year": "$1.01 per diluted share",
             "prior_quarter": "$1.87 per diluted share",
             "yoy_change": "120%", "qoq_change": "19%"}
          ],
          "segments": [{"name": "Data Center", "revenue": "$89.0 billion",
                        "yoy_change": "117%", "qoq_change": "18%",
                        "driver": "Vera Rubin ramping into full production."}],
          "guidance": {"period": "Third quarter fiscal 2027",
                       "revenue": "$108.0 billion, plus or minus 2%",
                       "gross_margin": "74.0%, plus or minus 50 basis points",
                       "operating_expenses": null, "tax_rate": null, "other": []},
          "vs_prior_guidance": [],
          "capital_returns": ["Returned $26.0 billion to shareholders."],
          "balance_sheet_cash_flow": [],
          "drivers": [],
          "concerns": ["No Data Center compute revenue from China is assumed."],
          "what_to_watch": ["Vera Rubin ramp."],
          "quotes": [{"speaker": "Jensen Huang", "role": "CEO", "text": "Demand is extraordinary."}],
          "analysis": "First paragraph.\n\nSecond paragraph.",
          "missing_items": ["Segment operating income."],
          "numbers_verified_from_document": true
        }
      }]
    }"#;

    #[test]
    fn parses_ticker_earnings() {
        let data: TickerEarnings = serde_json::from_str(EARNINGS_SAMPLE).unwrap();
        assert_eq!(data.ticker, "NVDA");
        assert_eq!(data.next_report_date.as_deref(), Some("2026-11-17"));
        assert!(!data.unknown, "the wire never sets the terminal-state flag");
        let read = data.latest().unwrap();
        assert_eq!(read.form(), "8-K");
        assert_eq!(read.report().key_metrics.len(), 4);
        assert_eq!(
            read.report().guidance.as_ref().unwrap().period,
            "Third quarter fiscal 2027"
        );
        assert_eq!(read.report().narrative().lines().count(), 3);
        assert!(
            read.alphai_url()
                .unwrap()
                .contains("/news/article/08-26/352613cc3f6089cc/")
        );
    }

    /// A foreign private issuer's 6-K: no segments, no guidance, no prior
    /// periods, figures in the filing's own currency. The renderer has to
    /// live on this shape as happily as on the full one.
    #[test]
    fn parses_a_foreign_issuer_read() {
        let data: TickerEarnings = serde_json::from_str(
            r#"{"ticker": "TSM", "next_report_date": null, "reports": [{
                 "uid": "8aec41a2fdb476c0",
                 "time_published": "2026-08-11T11:45:48Z",
                 "title": "TSMC 6-K",
                 "source_type": "sec_form6k",
                 "ticker": "TSM",
                 "fiscal_period": "six months ended June 30, 2026",
                 "analysis": {
                   "company": "Taiwan Semiconductor Manufacturing Company Limited",
                   "fiscal_period": "six months ended June 30, 2026",
                   "verdict": "solid",
                   "key_metrics": [{"name": "Second quarter consolidated revenue",
                                    "value": "NT$1,270.38 billion", "basis": "other",
                                    "prior_year": null, "prior_quarter": null,
                                    "yoy_change": null, "qoq_change": null}],
                   "guidance": null,
                   "analysis": "Board approved the report."
                 }}]}"#,
        )
        .unwrap();
        let read = data.latest().unwrap();
        assert_eq!(read.form(), "6-K");
        assert!(read.report().guidance.is_none());
        assert!(read.report().segments.is_empty());
        assert_eq!(data.next_report_date, None);
    }

    /// The most common answer of all: covered, but nothing published yet.
    #[test]
    fn parses_an_empty_earnings_history() {
        let data: TickerEarnings = serde_json::from_str(
            r#"{"ticker":"AVGO","reports":[],"next_report_date":"2026-09-02"}"#,
        )
        .unwrap();
        assert!(data.latest().is_none());
        assert_eq!(data.next_report_date.as_deref(), Some("2026-09-02"));
    }

    #[test]
    fn parses_calendar_events() {
        let page: CalendarEvents = serde_json::from_str(
            r#"{"events":[{"title":"CPI (Consumer Price Index)","scheduled_at":"2026-09-11T12:30:00Z",
                 "phase":"upcoming","schedule_status":"scheduled","schedule_basis":"official",
                 "importance":"high","press_conference_at":null,"has_sep":false}]}"#,
        )
        .unwrap();
        assert_eq!(page.events.len(), 1);
        assert!(page.events[0].scheduled().is_some());
    }

    /// Units shorten, figures never change. A rounded number is a new
    /// number, and the whole point of the layer is that every figure is the
    /// filing's own.
    #[test]
    fn short_metric_shortens_units_without_rounding() {
        assert_eq!(short_metric("$96,221 million"), "$96,221M");
        assert_eq!(short_metric("NT$1,270.38 billion"), "NT$1,270.38B");
        assert_eq!(short_metric("$2.46 per diluted share"), "$2.46");
        assert_eq!(
            short_metric("$10,605 million as of January 25, 2026"),
            "$10,605M"
        );
        assert_eq!(short_metric("50.1 percent"), "50.1%");
        assert_eq!(short_metric("—"), "—");
        assert_ne!(short_metric("$96,221 million"), "$96.2B");
    }

    #[test]
    fn signed_change_adds_only_a_sign() {
        assert_eq!(signed_change("18%"), "+18%");
        assert_eq!(signed_change("2.6 pts"), "+2.6pt");
        assert_eq!(signed_change("-5%"), "-5%");
        assert_eq!(signed_change("up 16 percent year over year"), "+16%");
        assert_eq!(signed_change("down 5 percent sequentially"), "-5%");
        assert_eq!(signed_change("—"), "—");
    }

    #[test]
    fn fiscal_periods_shorten_when_they_name_a_quarter() {
        assert_eq!(short_fiscal_period("Second Quarter Fiscal 2027"), "Q2 FY27");
        assert_eq!(short_fiscal_period("fiscal 2026 third quarter"), "Q3 FY26");
        // Nothing to shorten: a half-year period keeps the filing's wording
        // rather than being forced into a quarter it does not name.
        assert_eq!(
            short_fiscal_period("six months ended June 30, 2026"),
            "six months ended June 30, 2026"
        );
    }

    #[test]
    fn unknown_symbol_is_recognised() {
        assert!(is_unknown_symbol(
            "AlphAI API 404 Not Found: Unknown symbol 'ZZZQQ'."
        ));
        assert!(!is_unknown_symbol("AlphAI API 400 Bad Request: bad cursor"));
    }

    /// The feed cannot say whether a row has a read, so the client tells the
    /// filing apart from coverage of it by where the row came from.
    #[test]
    fn earnings_filings_are_told_from_coverage() {
        let filing: Article = serde_json::from_str(
            r#"{"original":{"source_domain":"sec.gov","source":"SEC EDGAR 8-K"},
                "enrichment":{"category":"earnings"}}"#,
        )
        .unwrap();
        let coverage: Article = serde_json::from_str(
            r#"{"original":{"source_domain":"reuters.com"},"enrichment":{"category":"earnings"}}"#,
        )
        .unwrap();
        let form4: Article = serde_json::from_str(
            r#"{"original":{"source_domain":"sec.gov"},"enrichment":{"category":"insider"}}"#,
        )
        .unwrap();
        assert!(is_earnings_filing(&filing));
        assert!(!is_earnings_filing(&coverage));
        assert!(!is_earnings_filing(&form4));
    }

    /// 21 and above is a Pro-only page size: raising `PAGE_SIZE` past 20
    /// would turn every free key's feed into a 400.
    #[test]
    fn page_size_is_allowed_on_every_tier() {
        assert!((1..=20).contains(&PAGE_SIZE));
    }

    #[test]
    fn age_buckets() {
        let now = DateTime::parse_from_rfc3339("2026-07-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut a: Article =
            serde_json::from_str(r#"{"original": {"time_published": "2026-07-10T11:20:00Z"}}"#)
                .unwrap();
        assert_eq!(a.age(now), "40m");
        a.original.time_published = "2026-07-10T02:00:00Z".into();
        assert_eq!(a.age(now), "10h");
        a.original.time_published = "2026-07-01T02:00:00Z".into();
        assert_eq!(a.age(now), "9d");
        a.original.time_published = "not-a-date".into();
        assert_eq!(a.age(now), "");
    }
}

#[cfg(test)]
mod calendar_tests;
