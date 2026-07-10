//! AlphaAI public REST API client (https://alphai.io).
//!
//! Powers the News and Insider views: relevance-scored financial news and
//! SEC Form 4 insider activity. Needs an `ak_live_…` API key (free tier at
//! alphai.io, 20 req/min and 100 req/day), so fetches are demand-driven and
//! cached: the app only asks for the symbol on screen and re-asks after
//! `CACHE_TTL`. Keep it that way — a per-poll fetch would burn the free
//! daily budget in minutes.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::poller::SourceEvent;

pub const DEFAULT_BASE_URL: &str = "https://api.alphai.io";

/// The public site, for article page links (distinct from the API base).
pub const SITE_URL: &str = "https://alphai.io";

/// How long a fetched bundle stays fresh before a view triggers a re-fetch.
pub const CACHE_TTL: Duration = Duration::from_secs(300);

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
            bail!("invalid AlphaAI API key, press s to update it (free keys: alphai.io)");
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("60");
            bail!("AlphaAI rate limit hit, retry in {wait}s (Free tier: 20/min, 100/day)");
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
            bail!("AlphaAI API {status}: {msg}");
        }
        resp.json().await.context("bad JSON from AlphaAI")
    }

    /// One page of enriched news; `symbol: None` = market-wide feed, `cursor`
    /// pages back (opaque, from the prior page's `next_cursor`), `page_size`
    /// only accepts 50 on Pro keys (omit for the default 10).
    /// Market-wide requests collapse syndicated reprints to one row per story
    /// (`sources_count` reports the outlet count). The symbol-scoped feed is
    /// left uncollapsed: the collapse filter matches the story root, which may
    /// not mention the symbol, and would silently drop relevant coverage.
    pub async fn news(
        &self,
        symbol: Option<&str>,
        cursor: Option<&str>,
        page_size: Option<u8>,
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
        self.get_json("/api/news/", &query).await
    }

    /// Top 10 stories of the trailing 48h (relevance 8+, server-collapsed).
    /// The response is a bare array, not the paginated news shape.
    pub async fn trending(&self) -> Result<Vec<Article>> {
        self.get_json("/api/news/trending/", &[]).await
    }

    /// One page of the SEC Form 4 insider feed for one symbol; `cursor` and
    /// `page_size` behave as in `news`.
    pub async fn insider_news(
        &self,
        symbol: &str,
        cursor: Option<&str>,
        page_size: Option<u8>,
    ) -> Result<NewsPage> {
        let mut query: Vec<(&str, &str)> = vec![("symbol", symbol)];
        if let Some(c) = cursor {
            query.push(("cursor", c));
        }
        let size = page_size.map(|n| n.to_string());
        if let Some(size) = &size {
            query.push(("page_size", size));
        }
        self.get_json("/api/news/insider/", &query).await
    }

    /// 7-day bullish/neutral/bearish rollup from press coverage.
    pub async fn sentiment(&self, ticker: &str) -> Result<SentimentSummary> {
        self.get_json(&format!("/api/symbols/{ticker}/sentiment-summary/"), &[])
            .await
    }

    /// 30-day Form 4 rollup: buy/sell counts, dollar volumes, top insiders.
    pub async fn insider_summary(&self, ticker: &str) -> Result<InsiderSummary> {
        self.get_json(&format!("/api/symbols/{ticker}/insider-summary/"), &[])
            .await
    }
}

// ---------------------------------------------------------------------------
// Background task: the UI sends commands, results come back as SourceEvents
// on the same channel the price poller uses.

pub enum Cmd {
    /// Swap the API key at runtime (settings screen). None disables fetching.
    SetKey(Option<String>),
    /// Fetch news (+ sentiment when symbol-scoped). None = market-wide.
    /// A cursor means "load the next page" of an already shown feed.
    FetchNews {
        symbol: Option<String>,
        cursor: Option<String>,
    },
    /// Fetch the 48h trending top 10 (cached under `TRENDING_KEY`).
    FetchTrending,
    /// Fetch the insider feed + 30d summary for one symbol; a cursor pages.
    FetchInsider {
        symbol: String,
        cursor: Option<String>,
    },
}

pub enum Event {
    News {
        key: String,
        articles: Vec<Article>,
        sentiment: Option<SentimentSummary>,
        next_cursor: Option<String>,
        /// True for a cursor fetch: extend the bundle instead of replacing it.
        append: bool,
    },
    Insider {
        symbol: String,
        articles: Vec<Article>,
        summary: Option<InsiderSummary>,
        next_cursor: Option<String>,
        append: bool,
    },
    /// A load-more failed. The shown feed stays; `gated` marks the archive
    /// horizon (terminal for this feed, offer an upgrade instead of a retry).
    PageError {
        key: String,
        error: String,
        gated: bool,
    },
    /// `key` matches the cache key of the fetch that failed.
    Error { key: String, error: String },
}

/// Cache key for a symbol-scoped or market-wide news fetch.
pub fn news_key(symbol: Option<&str>) -> String {
    symbol.map_or_else(|| MARKET_KEY.to_string(), str::to_string)
}

/// Cache key for an insider fetch (kept distinct from news keys).
pub fn insider_key(symbol: &str) -> String {
    format!("ins:{symbol}")
}

/// Which paginated feed a page fetch targets.
enum Feed<'a> {
    News(Option<&'a str>),
    Insider(&'a str),
}

/// Fetch one feed page, self-tuning the page size to the key's tier.
/// `page50`: None = tier unknown, probe 50 on the first explicit paging
/// request (one extra request per session for non-Pro keys, and only when
/// the user asked for more); Some(true) = Pro confirmed, 50 everywhere;
/// Some(false) = 50 rejected once, stay on the default 10.
async fn fetch_feed_page(
    client: &Client,
    feed: Feed<'_>,
    cursor: Option<&str>,
    page50: &mut Option<bool>,
) -> Result<NewsPage> {
    let try50 = matches!(*page50, Some(true)) || (page50.is_none() && cursor.is_some());
    if try50 {
        let result = match &feed {
            Feed::News(symbol) => client.news(*symbol, cursor, Some(50)).await,
            Feed::Insider(symbol) => client.insider_news(symbol, cursor, Some(50)).await,
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
        Feed::News(symbol) => client.news(*symbol, cursor, None).await,
        Feed::Insider(symbol) => client.insider_news(symbol, cursor, None).await,
    }
}

/// Error event for a fetch: page fetches degrade to a non-destructive
/// `PageError`, initial fetches replace the view via `Error`.
fn error_event(key: String, e: anyhow::Error, append: bool) -> Event {
    let error = format!("{e:#}");
    if append {
        let gated = is_archive_gate(&error);
        Event::PageError { key, error, gated }
    } else {
        Event::Error { key, error }
    }
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
            }
            Cmd::FetchNews { symbol, cursor } => {
                let key = news_key(symbol.as_deref());
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphaAI API key configured");
                    continue;
                };
                let append = cursor.is_some();
                // Sentiment is a nice-to-have on the initial symbol fetch:
                // its failure must not blank the news list, so it degrades
                // to None. Pages never refetch it.
                let (page, sentiment) = match (&symbol, append) {
                    (Some(s), false) => {
                        let (p, senti) = tokio::join!(
                            fetch_feed_page(client, Feed::News(Some(s)), None, &mut page50),
                            client.sentiment(s)
                        );
                        (p, senti.ok())
                    }
                    _ => (
                        fetch_feed_page(
                            client,
                            Feed::News(symbol.as_deref()),
                            cursor.as_deref(),
                            &mut page50,
                        )
                        .await,
                        None,
                    ),
                };
                let event = match page {
                    Ok(p) => Event::News {
                        key,
                        articles: p.results,
                        sentiment,
                        next_cursor: p.next_cursor,
                        append,
                    },
                    Err(e) => error_event(key, e, append),
                };
                if tx.send(SourceEvent::Alphai(event)).is_err() {
                    return;
                }
            }
            Cmd::FetchTrending => {
                let key = TRENDING_KEY.to_string();
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphaAI API key configured");
                    continue;
                };
                let event = match client.trending().await {
                    Ok(articles) => Event::News {
                        key,
                        articles,
                        sentiment: None,
                        next_cursor: None,
                        append: false,
                    },
                    Err(e) => Event::Error { key, error: format!("{e:#}") },
                };
                if tx.send(SourceEvent::Alphai(event)).is_err() {
                    return;
                }
            }
            Cmd::FetchInsider { symbol, cursor } => {
                let key = insider_key(&symbol);
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphaAI API key configured");
                    continue;
                };
                let append = cursor.is_some();
                let (page, summary) = match append {
                    false => {
                        let (p, s) = tokio::join!(
                            fetch_feed_page(client, Feed::Insider(&symbol), None, &mut page50),
                            client.insider_summary(&symbol)
                        );
                        (p, s.ok())
                    }
                    true => (
                        fetch_feed_page(
                            client,
                            Feed::Insider(&symbol),
                            cursor.as_deref(),
                            &mut page50,
                        )
                        .await,
                        None,
                    ),
                };
                let event = match page {
                    Ok(p) => Event::Insider {
                        symbol,
                        articles: p.results,
                        summary,
                        next_cursor: p.next_cursor,
                        append,
                    },
                    Err(e) => error_event(key, e, append),
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

    /// The article's page on alphai.io: `/news/article/{MM-DD}/{uid}/{slug}`.
    /// None when the feed item has no uid or the title slugifies to nothing;
    /// callers fall back to the original source URL.
    pub fn alphai_url(&self) -> Option<String> {
        let uid = self.original.uid.trim();
        if uid.is_empty() {
            return None;
        }
        let date = self.published()?.format("%m-%d");
        let slug = slugify(&self.original.title);
        if slug.is_empty() {
            return None;
        }
        Some(format!("{SITE_URL}/news/article/{date}/{uid}/{slug}"))
    }
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

#[derive(Clone, Debug, Deserialize)]
pub struct InsiderSummary {
    #[serde(default)]
    pub days: i64,
    #[serde(default)]
    pub total_transactions: i64,
    #[serde(default)]
    pub buy_count: i64,
    #[serde(default)]
    pub sell_count: i64,
    #[serde(default)]
    pub buy_value_usd: Option<String>,
    #[serde(default)]
    pub sell_value_usd: Option<String>,
    #[serde(default)]
    pub pct_10b5_1: i64,
    #[serde(default)]
    pub top_insiders: Vec<TopInsider>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TopInsider {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub transaction_count: i64,
    #[serde(default)]
    pub net_value: Option<String>,
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
        assert_eq!(impact.price_impact_prediction.as_deref(), Some("+2-4% near term"));
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
        assert!(!is_archive_gate("AlphaAI API 400 Bad Request: bad cursor"));
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
        let plain: Article =
            serde_json::from_str(r#"{"original": {"title": "no form"}}"#).unwrap();
        assert_eq!(plain.original.ownership_form, None);
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
        assert_eq!(slugify("NVIDIA beats on Q2 earnings"), "nvidia-beats-on-q2-earnings");
        assert_eq!(slugify("Apple's Q2: beats!"), "apples-q2-beats");
        assert_eq!(slugify("AI  -  the new   gold rush"), "ai-the-new-gold-rush");
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
    fn parses_insider_summary() {
        let raw = r#"{
          "ticker": "NVDA", "days": 30, "total_transactions": 14,
          "buy_count": 2, "sell_count": 12,
          "buy_value_usd": "1240000.00", "sell_value_usd": "224580213.05",
          "pct_10b5_1": 85,
          "top_insiders": [
            {"name": "STEVENS MARK A", "title": "", "transaction_count": 3, "net_value": "-221102600.00"}
          ]
        }"#;
        let s: InsiderSummary = serde_json::from_str(raw).unwrap();
        assert_eq!(s.total_transactions, 14);
        assert_eq!(fmt_usd(s.sell_value_usd.as_deref().unwrap()), "$224.6M");
        assert_eq!(fmt_usd(s.top_insiders[0].net_value.as_deref().unwrap()), "-$221.1M");
        assert_eq!(s.top_insiders[0].transaction_count, 3);
    }

    #[test]
    fn usd_formatting_bands() {
        assert_eq!(fmt_usd("512"), "$512");
        assert_eq!(fmt_usd("2500"), "$2.5K");
        assert_eq!(fmt_usd("1300000000"), "$1.3B");
        assert_eq!(fmt_usd("garbage"), "garbage");
    }

    /// Live end-to-end check against the real API (6 requests).
    /// Run: ALPHAI_API_KEY=ak_live_… cargo test live_api -- --ignored
    #[tokio::test]
    #[ignore = "live API call; needs ALPHAI_API_KEY"]
    async fn live_api_smoke() {
        let key = std::env::var("ALPHAI_API_KEY").expect("set ALPHAI_API_KEY");
        let client = Client::new(key).unwrap();
        let news = client.news(Some("NVDA"), None, None).await.unwrap();
        assert!(!news.results.is_empty(), "empty NVDA news feed");
        assert!(news.results[0].original.title.len() > 3);
        // The feed is deeper than one page, so the cursor must be present
        // and must fetch an older second page.
        let cursor = news.next_cursor.expect("no next_cursor on page 1");
        let page2 = client.news(Some("NVDA"), Some(&cursor), None).await.unwrap();
        assert!(!page2.results.is_empty(), "empty second news page");
        let senti = client.sentiment("NVDA").await.unwrap();
        assert!(senti.days > 0);
        let filings = client.insider_news("NVDA", None, None).await.unwrap();
        assert!(!filings.results.is_empty(), "empty NVDA insider feed");
        let summary = client.insider_summary("NVDA").await.unwrap();
        assert!(summary.days > 0);
        let trending = client.trending().await.unwrap();
        assert!(!trending.is_empty(), "empty trending feed");
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
