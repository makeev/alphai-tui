//! AlphaAI feed state and every request-budget guard, in one place.
//!
//! The free tier allows 20 requests/min and 100/day, so fetching is
//! demand-driven: only the feed behind the visible view, only when missing
//! or older than `CACHE_TTL`, never while a fetch for the same key is in
//! flight, never on top of an error (`r` retries), and paging costs at most
//! one request per explicit keypress. Keep every one of those guards here.

use std::collections::HashSet;
use std::time::Instant;

use crate::alphai::{self, Article, FeedPayload, InsiderSummary, SentimentSummary};
use crate::ui;

use super::{App, NewsScope};

/// The AlphaAI feeds a view can display (`View::feed_shown`). Trending is
/// not a kind: it is a news scope, a different cache key of the news feed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedKind {
    News,
    Insider,
}

/// Cached pageable feed for one cache key: a symbol, `alphai::MARKET_KEY`
/// or `alphai::TRENDING_KEY` for news, `ins:SYM` for insider — the same key
/// `inflight` and `alphai_errors` use.
pub struct FeedBundle {
    pub articles: Vec<Article>,
    /// Side payload of the head fetch (pages never refetch it).
    pub side: Option<FeedPayload>,
    /// Cursor for the next (older) page; None = end of the feed (or gated).
    pub next_cursor: Option<String>,
    /// Last load-more failure, shown under the list without dropping it.
    pub page_error: Option<String>,
    /// Paging hit the plan's archive horizon: stop offering more pages.
    pub gated: bool,
    pub fetched: Instant,
}

impl FeedBundle {
    pub fn new(
        articles: Vec<Article>,
        side: Option<FeedPayload>,
        next_cursor: Option<String>,
    ) -> Self {
        Self {
            articles,
            side,
            next_cursor,
            page_error: None,
            gated: false,
            fetched: Instant::now(),
        }
    }

    /// Typed side-payload accessors: a view cannot silently read the wrong
    /// rollup off a mismatched bundle, it just renders no rollup.
    pub fn sentiment(&self) -> Option<&SentimentSummary> {
        match &self.side {
            Some(FeedPayload::Sentiment(s)) => Some(s),
            _ => None,
        }
    }

    pub fn insider_summary(&self) -> Option<&InsiderSummary> {
        match &self.side {
            Some(FeedPayload::Insider(s)) => Some(s),
            _ => None,
        }
    }
}

impl App {
    /// Cache key the News view is currently looking at.
    pub fn news_cache_key(&self) -> String {
        match self.news_scope {
            NewsScope::Ticker => self.selected_symbol().to_string(),
            NewsScope::Market => alphai::MARKET_KEY.to_string(),
            NewsScope::Trending => alphai::TRENDING_KEY.to_string(),
        }
    }

    /// Whether a fetch for this cache key is in flight (drives the
    /// "loading…" hint under the feed lists).
    pub(crate) fn is_loading(&self, key: &str) -> bool {
        self.inflight.contains(key)
    }

    /// The feed the visible view displays: (cache key, kind). The single
    /// source of truth behind the demand-driven fetch, pagination, refresh
    /// and the article accessors.
    fn active_feed(&self) -> Option<(String, FeedKind)> {
        let kind = ui::VIEWS[self.view_idx].feed_shown()?;
        let key = match kind {
            FeedKind::News => self.news_cache_key(),
            FeedKind::Insider => alphai::insider_key(self.selected_symbol()),
        };
        Some((key, kind))
    }

    /// Head fetch (first page plus side payload) for a feed of `kind`.
    fn head_cmd(&self, kind: FeedKind) -> alphai::Cmd {
        match kind {
            FeedKind::News => match self.news_scope {
                NewsScope::Ticker => alphai::Cmd::FetchNews {
                    symbol: Some(self.selected_symbol().to_string()),
                    cursor: None,
                },
                NewsScope::Market => alphai::Cmd::FetchNews { symbol: None, cursor: None },
                NewsScope::Trending => alphai::Cmd::FetchTrending,
            },
            FeedKind::Insider => alphai::Cmd::FetchInsider {
                symbol: self.selected_symbol().to_string(),
                cursor: None,
            },
        }
    }

    /// Next-page fetch continuing an already shown feed.
    fn page_cmd(&self, kind: FeedKind, cursor: String) -> alphai::Cmd {
        match kind {
            FeedKind::News => alphai::Cmd::FetchNews {
                symbol: (self.news_scope == NewsScope::Ticker)
                    .then(|| self.selected_symbol().to_string()),
                cursor: Some(cursor),
            },
            FeedKind::Insider => alphai::Cmd::FetchInsider {
                symbol: self.selected_symbol().to_string(),
                cursor: Some(cursor),
            },
        }
    }

    /// Articles behind the current News/Insider view, if fetched. The Split
    /// strip is read-only and exposes none (v and Enter stay inert there).
    pub fn visible_articles(&self) -> Option<&[Article]> {
        if !ui::VIEWS[self.view_idx].navigates_articles() {
            return None;
        }
        let (key, _) = self.active_feed()?;
        self.feeds.get(&key).map(|b| b.articles.as_slice())
    }

    pub(crate) fn apply_alphai(&mut self, event: alphai::Event) {
        match event {
            alphai::Event::Feed { key, articles, side, next_cursor, append } => {
                self.inflight.remove(&key);
                self.alphai_errors.remove(&key);
                match self.feeds.get_mut(&key) {
                    // A page extends the shown feed (the side payload stays
                    // from the head fetch); a fresh fetch replaces the bundle.
                    Some(b) if append => {
                        b.next_cursor = next_cursor;
                        b.page_error = None;
                        append_page(&mut b.articles, articles);
                    }
                    _ => {
                        self.feeds.insert(key, FeedBundle::new(articles, side, next_cursor));
                    }
                }
            }
            alphai::Event::PageError { key, error, gated } => {
                self.inflight.remove(&key);
                let Some(b) = self.feeds.get_mut(&key) else { return };
                b.page_error = Some(error);
                if gated {
                    b.gated = true;
                    b.next_cursor = None;
                }
            }
            alphai::Event::Error { key, error } => {
                self.inflight.remove(&key);
                self.alphai_errors.insert(key, error);
            }
        }
    }

    /// Demand-driven AlphaAI fetching: only the data behind the visible view,
    /// only when missing or older than `CACHE_TTL`, never while a fetch for
    /// the same key is in flight, and never on top of an error (manual `r`
    /// clears the error and retries) — the free tier is 100 requests/day.
    pub(crate) fn ensure_alphai_data(&mut self) {
        // The overlay gate also keeps a TTL refetch from swapping the article
        // out from under the reader mid-scroll.
        if !self.alphai_enabled
            || self.settings.open
            || self.article_overlay.open
            || self.symbols.is_empty()
        {
            return;
        }
        // The view declares which feed it shows (`View::feed_shown`); the
        // guards below are the single copy for every feed kind.
        let Some((key, kind)) = self.active_feed() else { return };
        // A TTL refetch replaces the whole bundle, dropping loaded pages, so
        // it waits until the reader is back at the top row (missing bundles
        // fetch regardless; the Split strip has no selection and stays at 0).
        let at_top = self.news_selected == 0;
        let stale = match self.feeds.get(&key) {
            None => true,
            Some(b) => at_top && b.fetched.elapsed() > alphai::CACHE_TTL,
        };
        if stale && !self.inflight.contains(&key) && !self.alphai_errors.contains_key(&key) {
            let cmd = self.head_cmd(kind);
            self.inflight.insert(key);
            let _ = self.alphai_tx.send(cmd);
        }
    }

    /// j at the last row: ask for the feed's next page (explicitly
    /// user-driven, one request per keypress at most; the shared `inflight`
    /// key also blocks a concurrent TTL refetch of the same feed).
    pub(super) fn request_more_articles(&mut self) {
        // Only views that navigate articles page; the Split strip never does.
        if !ui::VIEWS[self.view_idx].navigates_articles() {
            return;
        }
        let Some((key, kind)) = self.active_feed() else { return };
        // A gated feed carries no cursor (the archive guard cleared it).
        let Some(cursor) = self.feeds.get(&key).and_then(|b| b.next_cursor.clone()) else {
            return;
        };
        if self.inflight.contains(&key) {
            return;
        }
        let cmd = self.page_cmd(kind, cursor);
        self.inflight.insert(key);
        let _ = self.alphai_tx.send(cmd);
    }

    /// `r`: immediate price cycle, plus drop the visible AlphaAI bundle (and
    /// any error) so it refetches — this is also the retry path after 401/429.
    pub(super) fn manual_refresh(&mut self) {
        self.refresh.notify_one();
        if let Some((key, _)) = self.active_feed() {
            self.feeds.remove(&key);
            self.alphai_errors.remove(&key);
        }
    }
}

/// Extend a feed with the next page, dropping rows already shown (a fresh
/// article can shift the window between requests and repeat on the page
/// boundary). Rows without a uid cannot be matched and are kept.
fn append_page(articles: &mut Vec<Article>, page: Vec<Article>) {
    let seen: HashSet<String> = articles
        .iter()
        .map(|a| a.original.uid.clone())
        .filter(|uid| !uid.is_empty())
        .collect();
    articles.extend(
        page.into_iter()
            .filter(|a| a.original.uid.is_empty() || !seen.contains(&a.original.uid)),
    );
}
