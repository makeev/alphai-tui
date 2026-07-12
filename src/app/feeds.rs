//! AlphaAI feed state and every request-budget guard, in one place.
//!
//! The free tier allows 20 requests/min and 100/day, so fetching is
//! demand-driven: only the feed behind the visible view, only when missing
//! or older than the TTL (`app.alphai_ttl`, default `alphai::CACHE_TTL`,
//! `[ui] alphai_ttl_secs` overrides), never while a fetch for the same key
//! is in flight, never on top of an error (`r` retries), and paging costs
//! at most one request per explicit keypress. Keep every guard here.

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
    /// Score filter the head fetch carried; None for feeds without one
    /// (trending, insider). A mismatch with the live `news_min_score`
    /// marks the bundle stale.
    pub min_score: Option<u8>,
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
            min_score: None,
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
                    min_relevance: Some(self.news_min_score),
                },
                NewsScope::Market => alphai::Cmd::FetchNews {
                    symbol: None,
                    cursor: None,
                    min_relevance: Some(self.news_min_score),
                },
                NewsScope::Trending => alphai::Cmd::FetchTrending,
            },
            FeedKind::Insider => alphai::Cmd::FetchInsider {
                symbol: self.selected_symbol().to_string(),
                cursor: None,
                min_relevance: Some(self.insider_min_score),
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
                min_relevance: Some(self.news_min_score),
            },
            FeedKind::Insider => alphai::Cmd::FetchInsider {
                symbol: self.selected_symbol().to_string(),
                cursor: Some(cursor),
                min_relevance: Some(self.insider_min_score),
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
            alphai::Event::Feed { key, articles, side, next_cursor, append, min_relevance } => {
                self.inflight.remove(&key);
                self.alphai_errors.remove(&key);
                match self.feeds.get_mut(&key) {
                    // A page extends the shown feed (the side payload and the
                    // recorded score filter stay from the head fetch); a
                    // fresh fetch replaces the bundle.
                    Some(b) if append => {
                        b.next_cursor = next_cursor;
                        b.page_error = None;
                        // Paged-in rows are older articles the reader asked
                        // for, not news arriving: never mark them unseen.
                        self.feed_seen
                            .entry(key)
                            .or_default()
                            .extend(uids(&articles));
                        append_page(&mut b.articles, articles);
                    }
                    _ => {
                        let mut b = FeedBundle::new(articles, side, next_cursor);
                        b.min_score = min_relevance;
                        // Unseen markers: the first sight of a feed is a
                        // baseline, and a refetch caused by moving the score
                        // filter is the reader slicing differently, so both
                        // count as already seen. A plain TTL or manual
                        // refetch leaves genuinely new uids outside
                        // `feed_seen` and they render marked until hovered.
                        let filter_moved = self
                            .feeds
                            .get(&key)
                            .is_some_and(|old| old.min_score != min_relevance);
                        let first_sight = !self.feed_seen.contains_key(&key);
                        let seen = self.feed_seen.entry(key.clone()).or_default();
                        if first_sight || filter_moved {
                            seen.extend(uids(&b.articles));
                        }
                        self.feeds.insert(key, b);
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
    /// only when missing or older than the TTL, never while a fetch for
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
        // The score filter a head fetch of this feed would carry right now
        // (trending is the exception: server-curated 8+, no filter).
        let wanted = match kind {
            FeedKind::News if self.news_scope != NewsScope::Trending => {
                Some(self.news_min_score)
            }
            FeedKind::News => None,
            FeedKind::Insider => Some(self.insider_min_score),
        };
        // A TTL refetch replaces the whole bundle, dropping loaded pages, so
        // it waits until the reader is back at the top row (missing bundles
        // fetch regardless; the Split strip has no selection and stays at 0).
        // A changed score filter refetches immediately: page 1 of the new
        // filter is exactly what the user asked for.
        let at_top = self.news_selected == 0;
        let stale = match self.feeds.get(&key) {
            None => true,
            Some(b) => match (b.min_score, wanted) {
                (Some(have), Some(want)) if have != want => true,
                _ => at_top && b.fetched.elapsed() > self.alphai_ttl,
            },
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

    /// +/-: move the visible feed's score filter one step (clamped to 1..=10);
    /// news and insider each keep their own value. No fetch happens here: the
    /// visible bundle's recorded `min_score` stops matching and
    /// `ensure_alphai_data` refetches it, at most one request per press
    /// (the shared inflight key absorbs faster presses).
    pub(super) fn adjust_min_score(&mut self, delta: i8) {
        let Some((_, kind)) = self.active_feed() else { return };
        let field = match kind {
            FeedKind::News => &mut self.news_min_score,
            FeedKind::Insider => &mut self.insider_min_score,
        };
        let new = (*field as i8).saturating_add(delta).clamp(1, 10) as u8;
        if new != *field {
            *field = new;
            self.news_selected = 0;
            self.card_scroll = 0;
        }
    }

    /// `r`: immediate price cycle, plus drop the visible AlphaAI bundle (and
    /// any error) so it refetches — this is also the retry path after 401/429.
    /// `feed_seen` stays: the refetch marks what is actually new.
    pub(super) fn manual_refresh(&mut self) {
        self.refresh.notify_one();
        if let Some((key, _)) = self.active_feed() {
            self.feeds.remove(&key);
            self.alphai_errors.remove(&key);
        }
    }

    /// The row under the cursor counts as read: called on every frame from
    /// `ui::draw`, it retires the row's unseen marker. Views that do not
    /// navigate articles (the Split strip) have no cursor and never clear
    /// markers.
    pub(crate) fn mark_selected_seen(&mut self) {
        if !ui::VIEWS[self.view_idx].navigates_articles() {
            return;
        }
        let Some((key, _)) = self.active_feed() else { return };
        let Some(uid) = self
            .feeds
            .get(&key)
            .and_then(|b| b.articles.get(self.news_selected))
            .map(|a| a.original.uid.clone())
            .filter(|u| !u.is_empty())
        else {
            return;
        };
        self.feed_seen.entry(key).or_default().insert(uid);
    }

    /// Whether a row is new since the reader last looked at this feed:
    /// fetched after the feed's baseline and not hovered yet. Rows without
    /// a uid cannot be tracked and never mark.
    pub(crate) fn is_unseen(&self, key: &str, a: &Article) -> bool {
        !a.original.uid.is_empty()
            && self
                .feed_seen
                .get(key)
                .is_some_and(|seen| !seen.contains(&a.original.uid))
    }
}

/// Non-empty uids of a batch of articles (rows without one cannot be
/// tracked by the unseen markers).
fn uids(articles: &[Article]) -> impl Iterator<Item = String> + '_ {
    articles
        .iter()
        .map(|a| a.original.uid.clone())
        .filter(|u| !u.is_empty())
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
