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
            let msg = serde_json::from_str::<ErrorBody>(&body)
                .ok()
                .and_then(|e| e.message.or(e.detail).or(e.error))
                .unwrap_or_else(|| body.chars().take(120).collect());
            bail!("AlphaAI API {status}: {msg}");
        }
        resp.json().await.context("bad JSON from AlphaAI")
    }

    /// Newest page of enriched news; `symbol: None` = market-wide feed.
    pub async fn news(&self, symbol: Option<&str>) -> Result<Vec<Article>> {
        let mut query = Vec::new();
        if let Some(s) = symbol {
            query.push(("symbol", s));
        }
        let page: NewsPage = self.get_json("/api/news/", &query).await?;
        Ok(page.results)
    }

    /// Newest page of the SEC Form 4 insider feed for one symbol.
    pub async fn insider_news(&self, symbol: &str) -> Result<Vec<Article>> {
        let page: NewsPage = self
            .get_json("/api/news/insider/", &[("symbol", symbol)])
            .await?;
        Ok(page.results)
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
    FetchNews { symbol: Option<String> },
    /// Fetch the insider feed + 30d summary for one symbol.
    FetchInsider { symbol: String },
}

pub enum Event {
    News {
        key: String,
        articles: Vec<Article>,
        sentiment: Option<SentimentSummary>,
    },
    Insider {
        symbol: String,
        articles: Vec<Article>,
        summary: Option<InsiderSummary>,
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

pub async fn run(
    initial_key: Option<String>,
    mut cmds: UnboundedReceiver<Cmd>,
    tx: UnboundedSender<SourceEvent>,
) {
    let mut client = initial_key.and_then(|k| Client::new(k).ok());
    while let Some(cmd) = cmds.recv().await {
        match cmd {
            Cmd::SetKey(key) => client = key.and_then(|k| Client::new(k).ok()),
            Cmd::FetchNews { symbol } => {
                let key = news_key(symbol.as_deref());
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphaAI API key configured");
                    continue;
                };
                // Sentiment is a nice-to-have: its failure must not blank the
                // news list, so it degrades to None.
                let (articles, sentiment) = match &symbol {
                    Some(s) => {
                        let (a, senti) = tokio::join!(client.news(Some(s)), client.sentiment(s));
                        (a, senti.ok())
                    }
                    None => (client.news(None).await, None),
                };
                let event = match articles {
                    Ok(articles) => Event::News { key, articles, sentiment },
                    Err(e) => Event::Error { key, error: format!("{e:#}") },
                };
                if tx.send(SourceEvent::Alphai(event)).is_err() {
                    return;
                }
            }
            Cmd::FetchInsider { symbol } => {
                let key = insider_key(&symbol);
                let Some(client) = &client else {
                    send_error(&tx, key, "no AlphaAI API key configured");
                    continue;
                };
                let (articles, summary) =
                    tokio::join!(client.insider_news(&symbol), client.insider_summary(&symbol));
                let event = match articles {
                    Ok(articles) => Event::Insider { symbol, articles, summary: summary.ok() },
                    Err(e) => Event::Error { key, error: format!("{e:#}") },
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
struct NewsPage {
    #[serde(default)]
    results: Vec<Article>,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: Option<String>,
    detail: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Article {
    pub original: Original,
    #[serde(default)]
    pub enrichment: Enrichment,
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
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Insights {
    #[serde(default)]
    pub ticker_analysis: Vec<TickerAnalysis>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TickerAnalysis {
    #[serde(default)]
    pub ticker: String,
    #[serde(default)]
    pub impact_analysis: Option<Impact>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Impact {
    #[serde(default)]
    pub sentiment: Option<String>,
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

    /// The AI sentiment call for one ticker: "positive" / "neutral" / "negative".
    pub fn sentiment_for(&self, ticker: &str) -> Option<&str> {
        self.enrichment
            .ai_trading_insights
            .as_ref()?
            .ticker_analysis
            .iter()
            .find(|t| t.ticker.eq_ignore_ascii_case(ticker))?
            .impact_analysis
            .as_ref()?
            .sentiment
            .as_deref()
    }

    pub fn score(&self) -> i64 {
        self.enrichment.relevance_score.unwrap_or(0)
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
              "impact_analysis": {"sentiment": "positive"}
            }]
          }
        },
        "story_id": null,
        "sources_count": null,
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
    }

    #[test]
    fn usd_formatting_bands() {
        assert_eq!(fmt_usd("512"), "$512");
        assert_eq!(fmt_usd("2500"), "$2.5K");
        assert_eq!(fmt_usd("1300000000"), "$1.3B");
        assert_eq!(fmt_usd("garbage"), "garbage");
    }

    /// Live end-to-end check against the real API (4 requests).
    /// Run: ALPHAI_API_KEY=ak_live_… cargo test live_api -- --ignored
    #[tokio::test]
    #[ignore = "live API call; needs ALPHAI_API_KEY"]
    async fn live_api_smoke() {
        let key = std::env::var("ALPHAI_API_KEY").expect("set ALPHAI_API_KEY");
        let client = Client::new(key).unwrap();
        let news = client.news(Some("NVDA")).await.unwrap();
        assert!(!news.is_empty(), "empty NVDA news feed");
        assert!(news[0].original.title.len() > 3);
        let senti = client.sentiment("NVDA").await.unwrap();
        assert!(senti.days > 0);
        let filings = client.insider_news("NVDA").await.unwrap();
        assert!(!filings.is_empty(), "empty NVDA insider feed");
        let summary = client.insider_summary("NVDA").await.unwrap();
        assert!(summary.days > 0);
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
