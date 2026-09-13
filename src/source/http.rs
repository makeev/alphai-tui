//! Shared HTTP plumbing for price sources: one client shape, one GET+parse
//! path, and the small error helpers every backend otherwise reinvents.
//!
//! What stays per-source (see the existing backends for examples): auth
//! style (query token, auth headers), base URLs and their env overrides,
//! symbol and interval mapping, response shapes, quote fallback chains, and
//! any API-specific error semantics — those are passed in via `err_map`.
//! The AlphAI client in `crate::alphai` is deliberately separate: different
//! timeout, bearer auth and a richer error envelope.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;

/// App identity for keyed APIs. AlphAI tracks client adoption by this exact
/// format; keep it in sync with the client in `crate::alphai`.
pub const APP_UA: &str = concat!("alphai-tui/", env!("CARGO_PKG_VERSION"));

const TIMEOUT: Duration = Duration::from_secs(10);

/// Base pause before a retry, plus up to `RETRY_JITTER` on top, and how
/// many retries a request gets.
///
/// Alpaca's data edge refuses roughly one request in seven with a bare 429:
/// no `Retry-After`, no rate-limit headers, and nothing to do with load.
/// Measured 2026-09-10 against an idle key, 20 serial requests per regime:
/// 2/20 refused as fast as the socket allowed, and *4/20* refused at two
/// requests per second, an eighth of the pace. A limiter would ease off
/// when the pace drops; this does not, so slowing down or spacing requests
/// out buys nothing and a retry is the only lever. Two of them put the
/// residual under a percent, and cost at most two extra requests against a
/// 200 req/min ceiling.
///
/// The pause stays short because the refusal clears immediately (a lone
/// retry succeeded 16 times out of 16, even at 200ms). The jitter is not
/// for Alpaca at all: it is for the sources that meter for real, where a
/// batch refused together and retried in lockstep would just rebuild the
/// burst that got it refused.
const RETRY_DELAY: Duration = Duration::from_millis(200);
const RETRY_JITTER: Duration = Duration::from_millis(400);
const MAX_RETRIES: usize = 2;

/// Which refusals a source wants retried.
///
/// `Transient` is the default: a rate limiter that refused a burst plus the
/// gateway-class blips in front of every one of these APIs. `GatewayOnly`
/// is for a provider whose 429 is not a burst limiter at all but a block
/// with a timer on it, where a retry cannot win and each attempt is another
/// request against the counter that is holding the block open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    Transient,
    GatewayOnly,
}

impl Retry {
    fn covers(self, status: StatusCode) -> bool {
        match self {
            Self::Transient => is_transient(status),
            Self::GatewayOnly => matches!(status.as_u16(), 502..=504),
        }
    }
}

/// Default client for a source: app UA, shared timeout.
pub fn client() -> Result<reqwest::Client> {
    client_with(APP_UA, None)
}

/// Client with a custom UA (Yahoo blocks non-browser agents) and optional
/// default headers (header-auth APIs like Alpaca).
pub fn client_with(ua: &str, headers: Option<HeaderMap>) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().user_agent(ua).timeout(TIMEOUT);
    if let Some(headers) = headers {
        builder = builder.default_headers(headers);
    }
    Ok(builder.build()?)
}

/// GET `url` with `query` and parse a 2xx JSON body as `T`. A non-2xx status
/// routes `(status, body)` through `err_map`, so each source keeps its own
/// API-specific messages; `api` names the source in the bad-JSON context.
///
/// A transient status is retried up to `MAX_RETRIES` times (see
/// `is_transient`). That budget is deliberate: price sources are metered by
/// the minute, so a couple of extra requests cost nothing, and without them
/// a single unlucky 429 leaves the ticker showing an error until the next
/// poll. The AlphAI client is a separate path on purpose, so its per-day
/// budget never sees these retries.
pub async fn get_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    api: &str,
    url: &str,
    query: &[(&str, &str)],
    err_map: impl Fn(StatusCode, &str) -> String,
) -> Result<T> {
    get_json_retrying(client, api, url, query, err_map, Retry::Transient).await
}

/// `get_json` for a source that wants a different retry policy; see
/// `Retry`.
pub async fn get_json_retrying<T: DeserializeOwned>(
    client: &reqwest::Client,
    api: &str,
    url: &str,
    query: &[(&str, &str)],
    err_map: impl Fn(StatusCode, &str) -> String,
    retry: Retry,
) -> Result<T> {
    let mut resp = client
        .get(url)
        .query(query)
        .send()
        .await
        .context("request failed")?;
    for _ in 0..MAX_RETRIES {
        if !retry.covers(resp.status()) {
            break;
        }
        tokio::time::sleep(retry_delay()).await;
        resp = client
            .get(url)
            .query(query)
            .send()
            .await
            .context("request failed")?;
    }
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("{}", err_map(status, &body));
    }
    resp.json()
        .await
        .with_context(|| format!("bad JSON from {api}"))
}

/// Statuses worth exactly one more try: a rate limiter that refused a burst,
/// and the gateway-class blips in front of every one of these APIs. A plain
/// 500 stays out — it is as likely to be deterministic as not, and a source
/// that is genuinely down should say so on the first tick rather than after
/// a doubled wait. Auth and unknown-symbol errors are never transient.
fn is_transient(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 502 | 503 | 504)
}

/// Bumped once per retry, so two of them scheduled inside the same clock
/// tick still get different inputs. Needed because the clock alone is not
/// enough: `SystemTime::now()` returns the *same* value for calls in quick
/// succession (measured on macOS: eight back-to-back reads, one distinct
/// value), which is exactly the situation a refused pair is in.
static RETRY_SEQ: AtomicU64 = AtomicU64::new(0);

/// `RETRY_DELAY` plus a jittered tail, so requests refused together do not
/// come back together.
///
/// Neither input works raw. The clock stands still between neighbouring
/// calls (see `RETRY_SEQ`), and a bare counter would hand out delays in
/// lockstep across processes; `now % window` fails a third way, keeping
/// near inputs near. So: seed with the clock, separate with the counter,
/// then decorrelate through splitmix64's finalizer, which is what turns
/// neighbouring inputs into delays hundreds of milliseconds apart. Cheap
/// enough to save an RNG dependency for one call site.
fn retry_delay() -> Duration {
    let seq = RETRY_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut z = nanos.wrapping_add(seq.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    RETRY_DELAY + Duration::from_nanos(z % RETRY_JITTER.as_nanos() as u64)
}

/// Shared advice for a 429; the caller prefixes its plan's numbers, e.g.
/// "finnhub rate limit hit (60 req/min free tier)".
pub fn rate_limit_msg(prefix: &str) -> String {
    format!("{prefix}, raise --every or drop tickers")
}

/// Best-effort human message out of an error body: the common
/// `{"message"|"detail"|"error": "..."}` JSON shapes. None for anything else
/// (HTML error pages, plain text) so callers can fall back to `snippet` —
/// that None is what keeps nginx HTML bodies out of user-facing errors.
pub fn body_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    ["message", "detail", "error"]
        .iter()
        .find_map(|key| v.get(key).and_then(|m| m.as_str()))
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// First 120 chars of a raw body, for fallback error text.
pub fn snippet(body: &str) -> String {
    body.chars().take(120).collect()
}

/// The "no data" error every source hits for a bad ticker; `hint` carries
/// source-specific symbol-format advice.
pub fn unknown_symbol(symbol: &str, hint: Option<&str>) -> anyhow::Error {
    match hint {
        Some(hint) => anyhow!("no data for '{symbol}' (unknown symbol? {hint})"),
        None => anyhow!("no data for '{symbol}' (unknown symbol?)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_message_extracts_common_keys() {
        assert_eq!(
            body_message(r#"{"message":"boom"}"#).as_deref(),
            Some("boom")
        );
        assert_eq!(
            body_message(r#"{"detail":"nope"}"#).as_deref(),
            Some("nope")
        );
        assert_eq!(body_message(r#"{"error":"bad"}"#).as_deref(), Some("bad"));
        // Message wins over the later keys when several are present.
        assert_eq!(
            body_message(r#"{"error":"e","message":"m"}"#).as_deref(),
            Some("m")
        );
    }

    #[test]
    fn body_message_rejects_non_json_and_non_string() {
        assert_eq!(
            body_message("<html>401 Authorization Required</html>"),
            None
        );
        assert_eq!(body_message(""), None);
        assert_eq!(body_message(r#"{"code":42}"#), None);
        assert_eq!(body_message(r#"{"message":""}"#), None);
    }

    #[test]
    fn snippet_truncates_long_bodies() {
        assert_eq!(snippet("short"), "short");
        assert_eq!(snippet(&"x".repeat(300)).chars().count(), 120);
    }

    /// Yahoo's 429 is an IP block with a timer on it, not a burst limiter:
    /// measured 2026-09-10 it held for 19 minutes, and the second block
    /// that day came after fewer requests and held for over an hour. A
    /// retry cannot win that and each attempt feeds the counter.
    #[test]
    fn gateway_only_leaves_rate_limits_alone() {
        let status = |code| StatusCode::from_u16(code).unwrap();
        assert!(!Retry::GatewayOnly.covers(status(429)));
        assert!(Retry::Transient.covers(status(429)));
        for code in [502, 503, 504] {
            assert!(Retry::GatewayOnly.covers(status(code)), "{code}");
        }
        for code in [200, 400, 401, 403, 404, 500] {
            assert!(!Retry::GatewayOnly.covers(status(code)), "{code}");
        }
    }

    #[test]
    fn only_rate_limit_and_gateway_statuses_retry() {
        for code in [429, 502, 503, 504] {
            assert!(
                is_transient(StatusCode::from_u16(code).unwrap()),
                "{code} should retry"
            );
        }
        // Retrying these would double the wait before a real answer: the
        // key is wrong, the symbol is unknown, or the API is really broken.
        for code in [200, 400, 401, 403, 404, 422, 500] {
            assert!(
                !is_transient(StatusCode::from_u16(code).unwrap()),
                "{code} should not retry"
            );
        }
    }

    #[test]
    fn retry_delay_stays_in_its_jittered_window() {
        let draws: Vec<Duration> = (0..64).map(|_| retry_delay()).collect();
        assert!(
            draws
                .iter()
                .all(|d| *d >= RETRY_DELAY && *d < RETRY_DELAY + RETRY_JITTER)
        );
    }

    #[test]
    fn back_to_back_retry_delays_spread_across_the_window() {
        // The point of the mixer. Draws taken microseconds apart, as a
        // refused pair takes them, must land far apart: feeding the raw
        // clock through a plain modulo passes the window check above and
        // still lands every neighbour within microseconds of the last,
        // which recreates the very burst the retry is spreading out.
        let draws: Vec<u128> = (0..64).map(|_| retry_delay().as_nanos()).collect();
        let span = draws.iter().max().unwrap() - draws.iter().min().unwrap();
        let window = RETRY_JITTER.as_nanos();
        assert!(
            span > window / 2,
            "draws spanned {span}ns of a {window}ns window"
        );
        let scattered = draws
            .windows(2)
            .filter(|p| p[1].abs_diff(p[0]) > window / 8)
            .count();
        assert!(
            scattered > draws.len() / 2,
            "only {scattered} of {} neighbouring draws moved",
            draws.len() - 1
        );
    }

    #[test]
    fn rate_limit_msg_appends_shared_advice() {
        assert_eq!(
            rate_limit_msg("finnhub rate limit hit (60 req/min free tier)"),
            "finnhub rate limit hit (60 req/min free tier), raise --every or drop tickers"
        );
    }

    #[test]
    fn unknown_symbol_with_and_without_hint() {
        assert_eq!(
            unknown_symbol("NOPE", None).to_string(),
            "no data for 'NOPE' (unknown symbol?)"
        );
        assert_eq!(
            unknown_symbol("BTC", Some("crypto needs EXCHANGE:PAIR")).to_string(),
            "no data for 'BTC' (unknown symbol? crypto needs EXCHANGE:PAIR)"
        );
    }
}
