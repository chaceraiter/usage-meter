//! ChatGPT Codex usage scraper.
//!
//! Owns the full lifecycle for the ChatGPT Codex provider:
//!
//! 1. **Parsing** — [`parse_raw`] + [`to_snapshot`] convert a raw JSON
//!    body into the normalized [`UsageSnapshot`]. Pure functions, tested
//!    against a static fixture.
//! 2. **Fetching** — [`fetch_usage`] performs the HTTPS request against
//!    `GET https://chatgpt.com/backend-api/wham/usage`, applies the
//!    required headers, checks the status, parses the body, and returns
//!    a snapshot. Tested against a `wiremock` stub.
//!
//! ## Window classification
//!
//! ChatGPT reports two generic slots, `primary_window` and
//! `secondary_window`, each carrying its own `limit_window_seconds`.
//! Unlike Claude — which encodes window length in the *key name*
//! (`five_hour` / `seven_day`) — OpenAI leaves the label up to the
//! client. **We classify by the actual `limit_window_seconds` value,
//! not by ordering.** If OpenAI ever reshuffles which slot holds which
//! limit, or introduces a third window, the classifier degrades
//! gracefully: the `five_hour` / `weekly` slots are populated from
//! exact matches on the known window sizes, and any unrecognized
//! window is dropped rather than mis-labeled. A wrong number next to
//! the wrong label is a worse UX than a missing number, so "unknown
//! duration → drop" is the right default.
//!
//! The two recognized durations (`FIVE_HOUR_SECONDS` = 18 000,
//! `SEVEN_DAY_SECONDS` = 604 800) are shared with Claude via the
//! `model` module so a future tweak happens in exactly one place.
//!
//! ## Forward compatibility
//!
//! Every field on the raw structs is `Option<_>` with
//! `#[serde(default)]`, same rationale as `providers::claude`: the
//! provider is free to rename, remove, or add keys without notice,
//! and the scheduler must not panic on a partial payload. Unknown
//! top-level keys (`additional_rate_limits`, `promo`, `spend_control`,
//! anything future) are ignored by default — we only declare fields
//! we actually intend to read.

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{
    ProviderExtras, UsageSnapshot, UsageWindow, FIVE_HOUR_SECONDS, SEVEN_DAY_SECONDS,
};
use crate::providers::FetchError;

/// Errors produced by the ChatGPT Codex parser.
#[derive(Debug, Error)]
pub enum ChatGptParseError {
    #[error("failed to parse ChatGPT usage response as JSON: {0}")]
    InvalidJson(String),
}

/// Raw ChatGPT Codex usage response. We only declare the fields we
/// actually care about; everything else (`credits`, `spend_control`,
/// `promo`, `code_review_rate_limit`) is ignored by default and can
/// be surfaced later via `ProviderExtras` without a wire-format churn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ChatGptRawUsage {
    #[serde(default)]
    pub rate_limit: Option<ChatGptRawRateLimit>,
}

/// The `rate_limit` object. Always contains two slot fields; either
/// may be missing in theory. OpenAI reserves the right to reshuffle
/// which slot holds which window — we do NOT read meaning from the
/// slot name, only from `limit_window_seconds` inside the slot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ChatGptRawRateLimit {
    #[serde(default)]
    pub primary_window: Option<ChatGptRawWindow>,

    #[serde(default)]
    pub secondary_window: Option<ChatGptRawWindow>,
}

/// A single rate-limit window. `reset_at` is a Unix epoch in seconds
/// (verified during the endpoint spike — the v0.1 code converts via
/// `DateTime::from_timestamp`). `limit_window_seconds` is the length
/// of the rolling window and is the only input we use to classify
/// the window as 5-hour or weekly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ChatGptRawWindow {
    #[serde(default)]
    pub used_percent: Option<f32>,

    #[serde(default)]
    pub limit_window_seconds: Option<u32>,

    #[serde(default)]
    pub reset_at: Option<i64>,
}

/// Parses a JSON response body into [`ChatGptRawUsage`]. Narrow
/// failure mode: "bytes were not valid JSON for this shape".
/// Everything else degrades through `Option` + `#[serde(default)]`.
pub fn parse_raw(body: &str) -> Result<ChatGptRawUsage, ChatGptParseError> {
    serde_json::from_str(body).map_err(|e| ChatGptParseError::InvalidJson(e.to_string()))
}

/// Maps a raw ChatGPT response into a normalized [`UsageSnapshot`].
///
/// Pure function — no hidden clock reads, no network, no panics on
/// partial data. `fetched_at` is injected by the caller so the
/// scheduler can attribute the timestamp to the request start and so
/// tests are deterministic.
pub fn to_snapshot(raw: &ChatGptRawUsage, fetched_at: DateTime<Utc>) -> UsageSnapshot {
    // Collect both candidate windows regardless of slot, then
    // classify by actual window length. This is the entire point of
    // the "don't trust slot names" design — once both windows are in
    // the same iterator, slot order stops mattering.
    let candidates = raw
        .rate_limit
        .as_ref()
        .map(|rl| (rl.primary_window.as_ref(), rl.secondary_window.as_ref()))
        .unwrap_or((None, None));

    let mut five_hour: Option<UsageWindow> = None;
    let mut weekly: Option<UsageWindow> = None;

    for candidate in [candidates.0, candidates.1].into_iter().flatten() {
        let Some((kind, window)) = classify(candidate) else {
            // Unknown window duration, incomplete data, or an
            // out-of-range timestamp. Drop silently — a wrong label
            // would be worse than a missing one.
            continue;
        };
        match kind {
            WindowKind::FiveHour if five_hour.is_none() => five_hour = Some(window),
            WindowKind::Weekly if weekly.is_none() => weekly = Some(window),
            // Second window hitting the same slot would only happen
            // if OpenAI ever returns duplicates; keep the first and
            // drop the rest rather than overwriting.
            _ => {}
        }
    }

    UsageSnapshot {
        five_hour,
        weekly,
        fetched_at,
        extras: ProviderExtras::None,
    }
}

/// Internal classifier result — which normalized slot a raw window
/// belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowKind {
    FiveHour,
    Weekly,
}

/// Attempts to convert a raw ChatGPT window into a (kind, normalized
/// window) pair. Returns `None` if any required field is missing,
/// the timestamp is out of representable range, or the window length
/// is not one of the two values we know how to label.
fn classify(raw: &ChatGptRawWindow) -> Option<(WindowKind, UsageWindow)> {
    let used_percent = raw.used_percent?;
    let window_seconds = raw.limit_window_seconds?;
    let reset_unix = raw.reset_at?;
    let resets_at = DateTime::<Utc>::from_timestamp(reset_unix, 0)?;

    // Exact match by design. A near-match (e.g. 18 001 seconds) would
    // probably still be "the 5-hour limit" but we have no way to
    // verify that without a second source of truth, and mis-labeling
    // is a UX failure we're not willing to risk. If OpenAI ever ships
    // a new duration, this arm is the one file to edit.
    let kind = if window_seconds == FIVE_HOUR_SECONDS {
        WindowKind::FiveHour
    } else if window_seconds == SEVEN_DAY_SECONDS {
        WindowKind::Weekly
    } else {
        return None;
    };

    Some((
        kind,
        UsageWindow {
            used_percent,
            resets_at,
            window_seconds,
        },
    ))
}

// ---------------------------------------------------------------------------
// Auth + fetch
// ---------------------------------------------------------------------------

/// Credentials and header values required to call the ChatGPT usage
/// endpoint. Like [`super::claude::ClaudeAuth`], the auth UX populates
/// this and the fetcher consumes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatGptAuth {
    /// Full `Cookie` header value.
    pub cookie: String,

    /// `oai-device-id` header (UUID, generated once per install).
    pub device_id: String,

    /// `oai-session-id` header (UUID, generated once per install).
    pub session_id: String,

    /// `oai-client-version` header. Opaque version string matching the
    /// real web client's current build.
    pub client_version: String,

    /// `oai-client-build-number` header.
    pub build_number: String,
}

/// Fetches the current usage snapshot from ChatGPT's internal API.
///
/// `base_url` is extracted as a parameter so tests can point it at a
/// `wiremock` server. In production the caller passes
/// `"https://chatgpt.com"`.
pub async fn fetch_usage(
    client: &Client,
    base_url: &str,
    auth: &ChatGptAuth,
) -> Result<UsageSnapshot, FetchError> {
    let fetched_at = Utc::now();
    let url = format!("{}/backend-api/wham/usage", base_url.trim_end_matches('/'));

    let resp = client
        .get(&url)
        .header("cookie", &auth.cookie)
        .header("oai-client-version", &auth.client_version)
        .header("oai-client-build-number", &auth.build_number)
        .header("oai-device-id", &auth.device_id)
        .header("oai-session-id", &auth.session_id)
        .header("oai-language", "en-US")
        .header("accept", "*/*")
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    super::check_status(resp.status())?;

    let body = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let raw = parse_raw(&body).map_err(|e| FetchError::Parse(e.to_string()))?;
    Ok(to_snapshot(&raw, fetched_at))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIXTURE: &str = include_str!("../../fixtures/chatgpt-usage.json");

    fn fetched_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 4, 22, 0, 0).unwrap()
    }

    #[test]
    fn parses_fixture_into_raw_struct() {
        let raw = parse_raw(FIXTURE).unwrap();
        let rl = raw.rate_limit.as_ref().expect("rate_limit present");
        assert!(rl.primary_window.is_some());
        assert!(rl.secondary_window.is_some());
    }

    #[test]
    fn fixture_mapper_produces_expected_snapshot() {
        let raw = parse_raw(FIXTURE).unwrap();
        let snap = to_snapshot(&raw, fetched_at());

        let five = snap.five_hour.expect("five_hour window present");
        assert_eq!(five.used_percent, 12.5);
        assert_eq!(five.window_seconds, FIVE_HOUR_SECONDS);
        assert_eq!(
            five.resets_at,
            DateTime::<Utc>::from_timestamp(1775620800, 0).unwrap()
        );

        let week = snap.weekly.expect("weekly window present");
        assert_eq!(week.used_percent, 47.0);
        assert_eq!(week.window_seconds, SEVEN_DAY_SECONDS);
        assert_eq!(
            week.resets_at,
            DateTime::<Utc>::from_timestamp(1776052800, 0).unwrap()
        );

        assert_eq!(snap.fetched_at, fetched_at());
        assert_eq!(snap.extras, ProviderExtras::None);
    }

    /// The killer test for the classification-by-value design:
    /// swapping the contents of `primary_window` and `secondary_window`
    /// must produce the same normalized snapshot as the original. If
    /// this test ever fails, something in the pipeline has started
    /// trusting slot order again.
    #[test]
    fn classification_is_independent_of_slot_order() {
        let forward = r#"{
            "rate_limit": {
                "primary_window":   { "used_percent": 12.5, "limit_window_seconds": 18000,  "reset_at": 1775620800 },
                "secondary_window": { "used_percent": 47.0, "limit_window_seconds": 604800, "reset_at": 1776052800 }
            }
        }"#;
        let swapped = r#"{
            "rate_limit": {
                "primary_window":   { "used_percent": 47.0, "limit_window_seconds": 604800, "reset_at": 1776052800 },
                "secondary_window": { "used_percent": 12.5, "limit_window_seconds": 18000,  "reset_at": 1775620800 }
            }
        }"#;

        let a = to_snapshot(&parse_raw(forward).unwrap(), fetched_at());
        let b = to_snapshot(&parse_raw(swapped).unwrap(), fetched_at());
        assert_eq!(a, b);
        assert_eq!(a.five_hour.as_ref().unwrap().used_percent, 12.5);
        assert_eq!(a.weekly.as_ref().unwrap().used_percent, 47.0);
    }

    #[test]
    fn unknown_window_duration_is_dropped() {
        // A new window length OpenAI hasn't shown us before must not
        // be mis-labeled. Better a missing row than a wrong row.
        let body = r#"{
            "rate_limit": {
                "primary_window":   { "used_percent": 10, "limit_window_seconds": 3600,   "reset_at": 1775620800 },
                "secondary_window": { "used_percent": 20, "limit_window_seconds": 604800, "reset_at": 1776052800 }
            }
        }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert!(snap.five_hour.is_none());
        assert_eq!(snap.weekly.unwrap().used_percent, 20.0);
    }

    #[test]
    fn missing_rate_limit_yields_empty_snapshot() {
        // A payload with everything stripped but the top-level
        // object still deserializes; both windows are absent.
        let body = r#"{ "plan_type": "plus" }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert!(snap.five_hour.is_none());
        assert!(snap.weekly.is_none());
    }

    #[test]
    fn window_with_null_used_percent_is_dropped() {
        let body = r#"{
            "rate_limit": {
                "primary_window": { "used_percent": null, "limit_window_seconds": 18000, "reset_at": 1775620800 }
            }
        }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert!(snap.five_hour.is_none());
    }

    #[test]
    fn window_with_missing_reset_at_is_dropped() {
        let body = r#"{
            "rate_limit": {
                "primary_window": { "used_percent": 10, "limit_window_seconds": 18000 }
            }
        }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert!(snap.five_hour.is_none());
    }

    #[test]
    fn window_with_out_of_range_timestamp_is_dropped() {
        // `DateTime::from_timestamp` returns `None` for i64 values
        // outside chrono's representable range. The mapper must
        // tolerate that without panicking.
        let body = r#"{
            "rate_limit": {
                "primary_window": { "used_percent": 10, "limit_window_seconds": 18000, "reset_at": 9999999999999 }
            }
        }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert!(snap.five_hour.is_none());
    }

    #[test]
    fn unknown_top_level_fields_are_ignored() {
        // Forward-compat: OpenAI may add fields at any time.
        let body = r#"{
            "rate_limit": {
                "primary_window":   { "used_percent": 5,  "limit_window_seconds": 18000,  "reset_at": 1775620800 },
                "secondary_window": { "used_percent": 25, "limit_window_seconds": 604800, "reset_at": 1776052800 }
            },
            "brand_new_feature_from_the_future": { "whatever": true }
        }"#;
        let raw = parse_raw(body).expect("unknown fields must not fail the parse");
        let snap = to_snapshot(&raw, fetched_at());
        assert!(snap.five_hour.is_some());
        assert!(snap.weekly.is_some());
    }

    #[test]
    fn duplicate_slot_keeps_first_and_ignores_rest() {
        // If OpenAI ever ships both windows with the same duration,
        // we take the first one and drop the second rather than
        // overwriting. This is a defensive branch; in practice it is
        // not expected to fire.
        let body = r#"{
            "rate_limit": {
                "primary_window":   { "used_percent": 10, "limit_window_seconds": 18000, "reset_at": 1775620800 },
                "secondary_window": { "used_percent": 90, "limit_window_seconds": 18000, "reset_at": 1775620800 }
            }
        }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert_eq!(snap.five_hour.unwrap().used_percent, 10.0);
        assert!(snap.weekly.is_none());
    }

    #[test]
    fn invalid_json_returns_error() {
        let err = parse_raw("{ definitely not json").unwrap_err();
        assert!(matches!(err, ChatGptParseError::InvalidJson(_)));
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let raw = parse_raw(FIXTURE).unwrap();
        let snap = to_snapshot(&raw, fetched_at());
        let json = serde_json::to_string(&snap).unwrap();
        let back: UsageSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    // -------------------------------------------------------------------
    // Fetch tests (wiremock)
    // -------------------------------------------------------------------

    fn test_auth() -> ChatGptAuth {
        ChatGptAuth {
            cookie: "session=test-session".to_string(),
            device_id: "00000000-0000-0000-0000-000000000001".to_string(),
            session_id: "00000000-0000-0000-0000-000000000002".to_string(),
            client_version: "test-version".to_string(),
            build_number: "12345".to_string(),
        }
    }

    #[tokio::test]
    async fn fetch_returns_snapshot_on_200() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/backend-api/wham/usage"))
            .and(header("cookie", "session=test-session"))
            .and(header(
                "oai-device-id",
                "00000000-0000-0000-0000-000000000001",
            ))
            .and(header("oai-language", "en-US"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(FIXTURE, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let snap = fetch_usage(&client, &server.uri(), &test_auth())
            .await
            .expect("fetch should succeed");

        assert_eq!(snap.five_hour.unwrap().used_percent, 12.5);
        assert_eq!(snap.weekly.unwrap().used_percent, 47.0);
    }

    #[tokio::test]
    async fn fetch_returns_unauthorized_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Unauthorized { status: 401 }));
    }

    #[tokio::test]
    async fn fetch_returns_unauthorized_on_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Unauthorized { status: 403 }));
    }

    #[tokio::test]
    async fn fetch_returns_rate_limited_on_429() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::RateLimited));
    }

    #[tokio::test]
    async fn fetch_returns_server_error_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::ServerError { status: 500 }));
    }

    #[tokio::test]
    async fn fetch_returns_parse_error_on_non_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth())
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Parse(_)));
    }
}
