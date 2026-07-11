//! Shared HTTP plumbing for price sources: one client shape, one GET+parse
//! path, and the small error helpers every backend otherwise reinvents.
//!
//! What stays per-source (see the existing backends for examples): auth
//! style (query token, auth headers), base URLs and their env overrides,
//! symbol and interval mapping, response shapes, quote fallback chains, and
//! any API-specific error semantics — those are passed in via `err_map`.
//! The AlphaAI client in `crate::alphai` is deliberately separate: different
//! timeout, bearer auth and a richer error envelope.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;

/// App identity for keyed APIs. AlphaAI tracks client adoption by this exact
/// format; keep it in sync with the client in `crate::alphai`.
pub const APP_UA: &str = concat!("alphai-tui/", env!("CARGO_PKG_VERSION"));

const TIMEOUT: Duration = Duration::from_secs(10);

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
pub async fn get_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    api: &str,
    url: &str,
    query: &[(&str, &str)],
    err_map: impl Fn(StatusCode, &str) -> String,
) -> Result<T> {
    let resp = client
        .get(url)
        .query(query)
        .send()
        .await
        .context("request failed")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("{}", err_map(status, &body));
    }
    resp.json().await.with_context(|| format!("bad JSON from {api}"))
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
        assert_eq!(body_message(r#"{"message":"boom"}"#).as_deref(), Some("boom"));
        assert_eq!(body_message(r#"{"detail":"nope"}"#).as_deref(), Some("nope"));
        assert_eq!(body_message(r#"{"error":"bad"}"#).as_deref(), Some("bad"));
        // Message wins over the later keys when several are present.
        assert_eq!(
            body_message(r#"{"error":"e","message":"m"}"#).as_deref(),
            Some("m")
        );
    }

    #[test]
    fn body_message_rejects_non_json_and_non_string() {
        assert_eq!(body_message("<html>401 Authorization Required</html>"), None);
        assert_eq!(body_message(""), None);
        assert_eq!(body_message(r#"{"code":42}"#), None);
        assert_eq!(body_message(r#"{"message":""}"#), None);
    }

    #[test]
    fn snippet_truncates_long_bodies() {
        assert_eq!(snippet("short"), "short");
        assert_eq!(snippet(&"x".repeat(300)).chars().count(), 120);
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
