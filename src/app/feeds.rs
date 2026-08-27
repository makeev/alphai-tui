//! AlphaAI feed state and every request-budget guard, in one place.
//!
//! The free tier allows 20 requests/min and 100/day, so fetching is
//! demand-driven: only the feed behind the visible view, only when missing
//! or older than the TTL (`app.alphai_ttl`, default `alphai::CACHE_TTL`,
//! `[ui] alphai_ttl_secs` overrides), never while a fetch for the same key
//! is in flight, never on top of an error (`r` retries), and paging costs
//! at most one request per explicit keypress. Keep every guard here.
//!
//! A TTL tick is a delta poll: it asks what has arrived since the last one
//! and merges that into the top of the bundle, so the loaded pages, the side
//! payload and the row under the reader's cursor all survive. The head fetch
//! that replaces the bundle outright now runs only when the feed is missing,
//! when the score filter moved, and every `SIDE_REFRESH_FACTOR` TTLs to renew
//! the side payload. Either way it is at most one request per TTL per
//! visible feed, exactly the budget it was before.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::alphai::{self, Article, FeedMode, FeedPayload, InsiderTrades, SentimentSummary, Sort};
use crate::ui;

use super::{App, NewsScope};

/// How many TTLs a polling feed's head page may live for. Merges keep the
/// rows current, so the head fetch is only renewing the side payload and
/// re-anchoring the order; at the default 300s TTL that is every 20 minutes.
/// One request per TTL per visible feed is the budget either way: a tick that
/// refetches the head does not also poll.
const SIDE_REFRESH_FACTOR: u32 = 4;

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
    /// Position in the arrival stream for the delta poll (`Sort::Ingested`),
    /// None until a poll primes one. Deliberately a separate field from
    /// `next_cursor`: the two cursor families are mutually unreadable and
    /// crossing them is a 400. It rides in the bundle so it dies exactly when
    /// the bundle is replaced (`r`, a moved score filter) and can never
    /// outlive the feed it points into.
    pub delta_cursor: Option<String>,
    /// Last load-more or delta-poll failure, shown under the list without
    /// dropping it.
    pub page_error: Option<String>,
    /// Paging hit the plan's archive horizon: stop offering more pages.
    pub gated: bool,
    /// A delta poll failed over something other than its cursor: stop polling
    /// this bundle until `r`, because nothing here ever retries by itself.
    pub poll_stopped: bool,
    /// Score filter the head fetch carried; None for feeds without one
    /// (trending, insider). A mismatch with the live `news_min_score`
    /// marks the bundle stale.
    pub min_score: Option<u8>,
    /// When the head fetch behind this bundle landed: the age of the side
    /// payload, and what the periodic full refresh is measured against.
    pub fetched: Instant,
    /// When the feed last heard from the server at all, head fetch or delta
    /// poll: what the poll cadence is measured against.
    pub polled: Instant,
}

impl FeedBundle {
    pub fn new(
        articles: Vec<Article>,
        side: Option<FeedPayload>,
        next_cursor: Option<String>,
    ) -> Self {
        let now = Instant::now();
        Self {
            articles,
            side,
            next_cursor,
            delta_cursor: None,
            page_error: None,
            gated: false,
            poll_stopped: false,
            min_score: None,
            fetched: now,
            polled: now,
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

    pub fn insider_trades(&self) -> Option<&InsiderTrades> {
        match &self.side {
            Some(FeedPayload::Insider(t)) => Some(t.as_ref()),
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
                    sort: Sort::Published,
                },
                NewsScope::Market => alphai::Cmd::FetchNews {
                    symbol: None,
                    cursor: None,
                    min_relevance: Some(self.news_min_score),
                    sort: Sort::Published,
                },
                NewsScope::Trending => alphai::Cmd::FetchTrending,
            },
            FeedKind::Insider => alphai::Cmd::FetchInsider {
                symbol: self.selected_symbol().to_string(),
                cursor: None,
                min_relevance: Some(self.insider_min_score),
                sort: Sort::Published,
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
                sort: Sort::Published,
            },
            FeedKind::Insider => alphai::Cmd::FetchInsider {
                symbol: self.selected_symbol().to_string(),
                cursor: Some(cursor),
                min_relevance: Some(self.insider_min_score),
                sort: Sort::Published,
            },
        }
    }

    /// Whether a feed has a delta poll behind it. Trending is the one that
    /// does not: a fixed top-10 endpoint with no cursor and no `sort`, so it
    /// keeps refetching its head on the TTL like every feed did before.
    fn feed_polls(&self, kind: FeedKind) -> bool {
        !(kind == FeedKind::News && self.news_scope == NewsScope::Trending)
    }

    /// Delta poll of a shown feed: same filters, ingest ordering, and the
    /// bundle's own poll position (None primes a fresh one).
    fn poll_cmd(&self, kind: FeedKind, cursor: Option<String>) -> Option<alphai::Cmd> {
        if !self.feed_polls(kind) {
            return None;
        }
        Some(match kind {
            FeedKind::News => alphai::Cmd::FetchNews {
                symbol: (self.news_scope == NewsScope::Ticker)
                    .then(|| self.selected_symbol().to_string()),
                cursor,
                min_relevance: Some(self.news_min_score),
                sort: Sort::Ingested,
            },
            FeedKind::Insider => alphai::Cmd::FetchInsider {
                symbol: self.selected_symbol().to_string(),
                cursor,
                min_relevance: Some(self.insider_min_score),
                sort: Sort::Ingested,
            },
        })
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
            alphai::Event::Feed {
                key,
                articles,
                side,
                next_cursor,
                mode,
                min_relevance,
            } => {
                self.inflight.remove(&key);
                self.alphai_errors.remove(&key);
                // Read before the bundle is borrowed: a merge into the feed
                // under the reader's cursor has to move the cursor with it.
                // The Split strip shows a feed but navigates nothing, so it
                // has no cursor to move.
                let active = ui::VIEWS[self.view_idx]
                    .navigates_articles()
                    .then(|| self.active_feed().map(|(k, _)| k))
                    .flatten();
                match self.feeds.get_mut(&key) {
                    // A page extends the shown feed (the side payload and the
                    // recorded score filter stay from the head fetch); a
                    // fresh fetch replaces the bundle.
                    Some(b) if mode == FeedMode::Append => {
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
                    // A delta page merges arrivals into the top: the loaded
                    // pages, the side payload and the row under the cursor all
                    // survive, and the poll position advances.
                    Some(b) if mode == FeedMode::Merge => {
                        // The priming poll carries no position yet, so its
                        // page is "what the published head page could not
                        // show", not "what arrived while you watched": it
                        // merges as baseline, unmarked. Every later poll marks.
                        let prime = b.delta_cursor.is_none();
                        b.delta_cursor = next_cursor;
                        b.page_error = None;
                        b.polled = Instant::now();
                        let fresh = merge_arrivals(&mut b.articles, articles);
                        if active.as_deref() == Some(key.as_str()) {
                            self.news_selected += fresh.len();
                        }
                        // Only the baseline writes to `feed_seen`: an arrival
                        // stays outside it and renders marked until hovered.
                        if prime {
                            self.feed_seen.entry(key).or_default().extend(fresh);
                        }
                    }
                    // A delta page whose bundle is gone (dropped by `r` while
                    // the poll was in flight) has nothing to merge into, and
                    // its ingest ordering is not a feed anyone can read: drop
                    // it and let the missing bundle refetch its head.
                    _ if mode == FeedMode::Merge => {}
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
                let Some(b) = self.feeds.get_mut(&key) else {
                    return;
                };
                b.page_error = Some(error);
                if gated {
                    b.gated = true;
                    b.next_cursor = None;
                }
            }
            // A background poll failed on its own: the shown feed is fine, so
            // the error goes under the list instead of replacing the view, and
            // polling stops until `r` (nothing here retries by itself).
            alphai::Event::PollError { key, error } => {
                self.inflight.remove(&key);
                if let Some(b) = self.feeds.get_mut(&key) {
                    b.page_error = Some(error);
                    b.poll_stopped = true;
                    b.polled = Instant::now();
                }
            }
            // The poll position was rejected (400) or aged past the plan's
            // archive horizon (403). Drop it silently; the next poll primes a
            // fresh one, and `polled` keeps that a TTL away rather than now.
            alphai::Event::PollReprime { key } => {
                self.inflight.remove(&key);
                if let Some(b) = self.feeds.get_mut(&key) {
                    b.delta_cursor = None;
                    b.polled = Instant::now();
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
            || self.help.open
            || self.symbols.is_empty()
        {
            return;
        }
        // The view declares which feed it shows (`View::feed_shown`); the
        // guards below are the single copy for every feed kind.
        let Some((key, kind)) = self.active_feed() else {
            return;
        };
        // The score filter a head fetch of this feed would carry right now
        // (trending is the exception: server-curated 8+, no filter).
        let wanted = match kind {
            FeedKind::News if self.news_scope != NewsScope::Trending => Some(self.news_min_score),
            FeedKind::News => None,
            FeedKind::Insider => Some(self.insider_min_score),
        };
        if self.inflight.contains(&key) || self.alphai_errors.contains_key(&key) {
            return;
        }
        // A head fetch replaces the whole bundle, dropping loaded pages and
        // the reader's place, so it waits until the reader is back at the top
        // row (missing bundles fetch regardless; the Split strip has no
        // selection and stays at 0). A changed score filter is the exception:
        // page 1 of the new filter is exactly what the user asked for.
        let at_top = self.news_selected == 0;
        let ttl = self.alphai_ttl;
        let cmd = match self.feeds.get(&key) {
            None => Some(self.head_cmd(kind)),
            Some(b) => {
                let filter_moved =
                    matches!((b.min_score, wanted), (Some(have), Some(want)) if have != want);
                // A polling feed keeps its rows current by merging arrivals,
                // so its head fetch is only there to renew the side payload
                // (the sentiment rollup, the Form 4 chart) and to re-anchor
                // the order after merges have prepended into it: once every
                // few TTLs is enough. A feed that cannot poll still refetches
                // its head every TTL, as every feed did before.
                let head_ttl = if self.feed_polls(kind) {
                    ttl * SIDE_REFRESH_FACTOR
                } else {
                    ttl
                };
                if filter_moved || (at_top && b.fetched.elapsed() > head_ttl) {
                    Some(self.head_cmd(kind))
                } else if !b.poll_stopped && b.polled.elapsed() > ttl {
                    // Merging keeps the loaded pages and the cursor, so this
                    // one runs wherever the reader is: scrolling down used to
                    // mean no updates at all.
                    self.poll_cmd(kind, b.delta_cursor.clone())
                } else {
                    None
                }
            }
        };
        if let Some(cmd) = cmd {
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
        let Some((key, kind)) = self.active_feed() else {
            return;
        };
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
        let Some((_, kind)) = self.active_feed() else {
            return;
        };
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
        let Some((key, _)) = self.active_feed() else {
            return;
        };
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

/// Merge a delta page into the top of a feed and return the uids it actually
/// inserted, in insertion order.
///
/// Rows already shown are dropped: the priming page overlaps the published
/// head almost entirely, and a poll can repeat a row across a page boundary.
/// Rows without a uid are dropped too, unlike in `append_page` — here they
/// cannot be deduped, marked or retired, so every poll would stack another
/// copy of them.
///
/// What is left goes ABOVE everything else, newest publication first. Sorting
/// an arrival to its published position is what hides it today: on the market
/// feed a page of 20 rows covers roughly ten minutes, while the median article
/// reaches the feed about half an hour after it was published, and a Form 4
/// days after its trade. The unseen marker is what explains the order to the
/// reader.
fn merge_arrivals(articles: &mut Vec<Article>, page: Vec<Article>) -> Vec<String> {
    let seen: HashSet<String> = articles
        .iter()
        .map(|a| a.original.uid.clone())
        .filter(|uid| !uid.is_empty())
        .collect();
    let mut fresh: Vec<Article> = page
        .into_iter()
        .filter(|a| !a.original.uid.is_empty() && !seen.contains(&a.original.uid))
        .collect();
    // Descending publication, with an unparsable timestamp pinned to the
    // bottom of the block instead of jumping it; equal timestamps keep the
    // server's order.
    fresh.sort_by_key(|a| Reverse(a.published().unwrap_or(DateTime::<Utc>::MIN_UTC)));
    let uids = fresh.iter().map(|a| a.original.uid.clone()).collect();
    articles.splice(0..0, fresh);
    uids
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
