//! Claude usage scraper.
//!
//! Owns the full lifecycle for the Claude provider:
//!
//! 1. **Parsing** — [`parse_raw`] + [`to_snapshot`] convert a raw JSON
//!    body into the normalized [`UsageSnapshot`]. Pure functions, tested
//!    against a static HAR-derived fixture.
//! 2. **Fetching** — [`fetch_usage`] performs the HTTPS request against
//!    `GET https://claude.ai/api/organizations/<org-id>/usage`, applies
//!    the required headers, checks the status, parses the body, and
//!    returns a snapshot. Tested against a `wiremock` stub.
//!
//! ## Field selection
//!
//! For v0.1 the app consumes only `five_hour.utilization` and
//! `seven_day.utilization`. The per-model weekly caps (`seven_day_opus`,
//! `seven_day_sonnet`), the credit-overage block (`extra_usage`), and
//! the internal-codename fields (`iguana_necktie`) are all captured
//! structurally so that:
//!
//! - Deserialization never fails when Anthropic adds a new key.
//! - Future features (per-model display, credit indicators) can read
//!   them without touching the wire format again.
//!
//! `#[serde(default)]` is applied to every optional field so that
//! missing keys deserialize to `None` instead of erroring — important
//! because the provider is free to drop fields at any time without
//! bumping a version header.

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{
    ProviderExtras, UsageSnapshot, UsageWindow, FIVE_HOUR_SECONDS, SEVEN_DAY_SECONDS,
};
use crate::providers::FetchError;

/// Errors produced by the Claude parser.
///
/// `InvalidJson` wraps `serde_json::Error` as a string so callers don't
/// need to take a direct dep on `serde_json` just to match on the
/// error type. The underlying error is not especially actionable to
/// end users anyway — if Anthropic breaks the shape, the only fix is a
/// code update regardless of which specific key tripped the parse.
#[derive(Debug, Error)]
pub enum ClaudeParseError {
    #[error("failed to parse Claude usage response as JSON: {0}")]
    InvalidJson(String),
}

/// Raw Claude usage response, directly mirroring the wire format.
///
/// `Serialize` is derived alongside `Deserialize` so the same struct
/// can round-trip through a test fixture without a second type. Every
/// field is `Option<_>` so that forward-compatibility is automatic: a
/// provider-side rename or removal degrades a window to `None` rather
/// than panicking the scheduler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClaudeRawUsage {
    #[serde(default)]
    pub five_hour: Option<ClaudeRawWindow>,

    #[serde(default)]
    pub seven_day: Option<ClaudeRawWindow>,

    /// Per-model weekly cap. Plan-dependent; free and lower tiers
    /// receive `null` here.
    #[serde(default)]
    pub seven_day_opus: Option<ClaudeRawWindow>,

    /// Per-model weekly cap. See `seven_day_opus`.
    #[serde(default)]
    pub seven_day_sonnet: Option<ClaudeRawWindow>,
}

/// Single window in Claude's response (`five_hour`, `seven_day`, etc.).
///
/// `utilization` is a percent `0..100`. This was verified during the
/// endpoint spike by matching a live HAR against the on-screen meter
/// (32% / 1% / 1%) — see `ai-context-management/` for the capture
/// session if any ambiguity resurfaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ClaudeRawWindow {
    #[serde(default)]
    pub utilization: Option<f32>,

    #[serde(default)]
    pub resets_at: Option<DateTime<Utc>>,
}

/// Parses a JSON response body into [`ClaudeRawUsage`].
///
/// This is the only function in the module that can fail, and its
/// failure mode is narrow: "the bytes were not valid JSON for this
/// shape". Everything else — missing windows, null utilization, new
/// server-side fields — is absorbed by `#[serde(default)]` and shows
/// up as `None` in the snapshot.
pub fn parse_raw(body: &str) -> Result<ClaudeRawUsage, ClaudeParseError> {
    serde_json::from_str(body).map_err(|e| ClaudeParseError::InvalidJson(e.to_string()))
}

/// Maps a raw Claude response into a normalized [`UsageSnapshot`].
///
/// `fetched_at` is passed in rather than read from `Utc::now()` so the
/// scheduler can attribute the timestamp to the request start (not the
/// parse time) and so tests are deterministic. The mapper is a pure
/// function with no hidden state — it's safe to call from anywhere,
/// and it's safe to refactor without worrying about side effects.
pub fn to_snapshot(raw: &ClaudeRawUsage, fetched_at: DateTime<Utc>) -> UsageSnapshot {
    UsageSnapshot {
        five_hour: raw
            .five_hour
            .as_ref()
            .and_then(|w| window_from(w, FIVE_HOUR_SECONDS)),
        weekly: raw
            .seven_day
            .as_ref()
            .and_then(|w| window_from(w, SEVEN_DAY_SECONDS)),
        fetched_at,
        extras: ProviderExtras::None,
    }
}

/// Converts a single [`ClaudeRawWindow`] into a [`UsageWindow`],
/// collapsing any window missing either `utilization` or `resets_at`
/// into `None`. A partially populated window is not meaningful — we'd
/// rather render a dash than a number next to a missing timestamp —
/// so the "both present" rule is enforced here.
fn window_from(raw: &ClaudeRawWindow, window_seconds: u32) -> Option<UsageWindow> {
    match (raw.utilization, raw.resets_at) {
        (Some(used_percent), Some(resets_at)) => Some(UsageWindow {
            used_percent,
            resets_at,
            window_seconds,
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Auth + fetch
// ---------------------------------------------------------------------------

/// Credentials and header values required to call the Claude usage
/// endpoint. The auth UX (embedded webview or cookie paste) is
/// responsible for populating this struct and persisting it in the
/// keychain; the fetcher just consumes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeAuth {
    /// Organization UUID, discovered from `/api/organizations` or the
    /// bootstrap endpoint during sign-in.
    pub org_id: String,

    /// Full `Cookie` header value (e.g. `sessionKey=sk-ant-...`).
    pub cookie: String,

    /// `anthropic-device-id` header (UUID, generated once per install).
    pub device_id: String,

    /// `anthropic-anonymous-id` header (UUID, generated once per install).
    pub anonymous_id: String,

    /// `anthropic-client-version` header. Opaque string matching the
    /// real web client's current build. Populated during auth capture
    /// or hardcoded from a recent known-good value.
    pub client_version: String,
}

/// Discovers the user's organization UUID by calling
/// `GET <base_url>/api/organizations` with the session cookie.
/// Returns the first org's UUID — most accounts belong to exactly one.
pub async fn discover_org_id(
    client: &Client,
    base_url: &str,
    cookie: &str,
) -> Result<String, FetchError> {
    #[derive(Deserialize)]
    struct OrgEntry {
        uuid: String,
    }

    let url = format!("{}/api/organizations", base_url.trim_end_matches('/'));

    let resp = client
        .get(&url)
        .header("cookie", cookie)
        .header("content-type", "application/json")
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    super::check_status(resp.status())?;

    let body = resp
        .text()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let orgs: Vec<OrgEntry> =
        serde_json::from_str(&body).map_err(|e| FetchError::Parse(e.to_string()))?;

    orgs.first()
        .map(|o| o.uuid.clone())
        .ok_or_else(|| FetchError::Parse("no organizations found in response".into()))
}

/// Fetches the current usage snapshot from Claude's internal API.
///
/// `base_url` is extracted as a parameter so tests can point it at a
/// `wiremock` server. In production the caller passes
/// `"https://claude.ai"`.
pub async fn fetch_usage(
    client: &Client,
    base_url: &str,
    auth: &ClaudeAuth,
) -> Result<UsageSnapshot, FetchError> {
    let fetched_at = Utc::now();
    let url = format!(
        "{}/api/organizations/{}/usage",
        base_url.trim_end_matches('/'),
        auth.org_id
    );

    let resp = client
        .get(&url)
        .header("cookie", &auth.cookie)
        .header("anthropic-client-platform", "web_claude_ai")
        .header("anthropic-client-version", &auth.client_version)
        .header("anthropic-device-id", &auth.device_id)
        .header("anthropic-anonymous-id", &auth.anonymous_id)
        .header("content-type", "application/json")
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

    /// Sanitized copy of a real HAR-captured response body. Contains
    /// only shape and representative values; no account identifiers.
    const FIXTURE: &str = include_str!("../../fixtures/claude-usage.json");

    fn fetched_at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 4, 22, 0, 0).unwrap()
    }

    #[test]
    fn parses_fixture_into_raw_struct() {
        let raw = parse_raw(FIXTURE).unwrap();
        assert!(raw.five_hour.is_some());
        assert!(raw.seven_day.is_some());
        assert!(
            raw.seven_day_opus.is_none(),
            "opus should deserialize as None when wire value is null"
        );
        assert!(raw.seven_day_sonnet.is_some());
    }

    #[test]
    fn fixture_mapper_produces_expected_snapshot() {
        // The fixture matches the live spot-check from the discovery
        // session: five_hour=1%, seven_day=32%. If this test ever
        // starts failing it is almost certainly because the fixture
        // drifted, not because the mapper is wrong.
        let raw = parse_raw(FIXTURE).unwrap();
        let snap = to_snapshot(&raw, fetched_at());

        let five = snap.five_hour.expect("five_hour window present");
        assert_eq!(five.used_percent, 1.0);
        assert_eq!(five.window_seconds, FIVE_HOUR_SECONDS);
        assert_eq!(
            five.resets_at,
            Utc.with_ymd_and_hms(2026, 4, 5, 3, 0, 0).unwrap()
        );

        let week = snap.weekly.expect("weekly window present");
        assert_eq!(week.used_percent, 32.0);
        assert_eq!(week.window_seconds, SEVEN_DAY_SECONDS);
        assert_eq!(
            week.resets_at,
            Utc.with_ymd_and_hms(2026, 4, 9, 18, 0, 0).unwrap()
        );

        assert_eq!(snap.fetched_at, fetched_at());
        assert_eq!(snap.extras, ProviderExtras::None);
    }

    #[test]
    fn missing_window_maps_to_none() {
        // If Anthropic ever drops one of the top-level keys, the
        // corresponding snapshot field must be `None`, not an error.
        let body = r#"{ "seven_day": { "utilization": 50, "resets_at": "2026-04-10T00:00:00Z" } }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert!(snap.five_hour.is_none());
        assert_eq!(snap.weekly.unwrap().used_percent, 50.0);
    }

    #[test]
    fn window_with_null_utilization_is_none() {
        // A window object present but with `utilization: null` is not
        // a usable data point — render a dash, not a zero.
        let body = r#"{
            "five_hour": { "utilization": null, "resets_at": "2026-04-05T03:00:00Z" },
            "seven_day": null
        }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert!(snap.five_hour.is_none());
        assert!(snap.weekly.is_none());
    }

    #[test]
    fn window_with_missing_resets_at_is_none() {
        // Same rule, applied to the other half of the pair: a number
        // without a reset time is not enough to render a row.
        let body = r#"{ "five_hour": { "utilization": 10 } }"#;
        let snap = to_snapshot(&parse_raw(body).unwrap(), fetched_at());
        assert!(snap.five_hour.is_none());
    }

    #[test]
    fn unknown_top_level_fields_are_ignored() {
        // Forward compatibility: Anthropic may add new keys at any
        // time. The parser must not error on them.
        let body = r#"{
            "five_hour": { "utilization": 5, "resets_at": "2026-04-05T03:00:00Z" },
            "seven_day": { "utilization": 20, "resets_at": "2026-04-09T18:00:00Z" },
            "brand_new_field_from_the_future": { "anything": 42 }
        }"#;
        let raw = parse_raw(body).expect("unknown fields must not fail the parse");
        let snap = to_snapshot(&raw, fetched_at());
        assert!(snap.five_hour.is_some());
        assert!(snap.weekly.is_some());
    }

    #[test]
    fn invalid_json_returns_error() {
        let err = parse_raw("{ not valid json").unwrap_err();
        assert!(matches!(err, ClaudeParseError::InvalidJson(_)));
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        // The snapshot is what crosses the IPC boundary, so it must
        // survive serialize-then-deserialize without losing fidelity.
        let raw = parse_raw(FIXTURE).unwrap();
        let snap = to_snapshot(&raw, fetched_at());
        let json = serde_json::to_string(&snap).unwrap();
        let back: UsageSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, back);
    }

    // -------------------------------------------------------------------
    // Org discovery tests (wiremock)
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn discover_org_id_returns_first_uuid() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/organizations"))
            .and(header("cookie", "sessionKey=test-session"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"uuid": "org-uuid-1", "name": "Personal"},
                {"uuid": "org-uuid-2", "name": "Team"},
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let org_id = discover_org_id(&client, &server.uri(), "sessionKey=test-session")
            .await
            .expect("should discover org");
        assert_eq!(org_id, "org-uuid-1");
    }

    #[tokio::test]
    async fn discover_org_id_returns_error_on_empty_list() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/organizations"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        let err = discover_org_id(&Client::new(), &server.uri(), "sessionKey=test")
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Parse(_)));
    }

    #[tokio::test]
    async fn discover_org_id_returns_unauthorized_on_401() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = discover_org_id(&Client::new(), &server.uri(), "bad-cookie")
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Unauthorized { status: 401 }));
    }

    // -------------------------------------------------------------------
    // Fetch tests (wiremock)
    // -------------------------------------------------------------------

    fn test_auth(org_id: &str) -> ClaudeAuth {
        ClaudeAuth {
            org_id: org_id.to_string(),
            cookie: "sessionKey=test-session".to_string(),
            device_id: "00000000-0000-0000-0000-000000000001".to_string(),
            anonymous_id: "00000000-0000-0000-0000-000000000002".to_string(),
            client_version: "test-version".to_string(),
        }
    }

    #[tokio::test]
    async fn fetch_returns_snapshot_on_200() {
        let server = MockServer::start().await;
        let org_id = "test-org-uuid";

        Mock::given(method("GET"))
            .and(path(format!("/api/organizations/{org_id}/usage")))
            .and(header("cookie", "sessionKey=test-session"))
            .and(header("anthropic-client-platform", "web_claude_ai"))
            .and(header(
                "anthropic-device-id",
                "00000000-0000-0000-0000-000000000001",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(FIXTURE, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let client = Client::new();
        let snap = fetch_usage(&client, &server.uri(), &test_auth(org_id))
            .await
            .expect("fetch should succeed");

        assert_eq!(snap.five_hour.unwrap().used_percent, 1.0);
        assert_eq!(snap.weekly.unwrap().used_percent, 32.0);
    }

    #[tokio::test]
    async fn fetch_returns_unauthorized_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth("org"))
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

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth("org"))
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

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth("org"))
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

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth("org"))
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

        let err = fetch_usage(&Client::new(), &server.uri(), &test_auth("org"))
            .await
            .unwrap_err();
        assert!(matches!(err, FetchError::Parse(_)));
    }
}
