//! Provider-specific scrapers.
//!
//! Each submodule owns one service (Claude, ChatGPT Codex, …) and is
//! responsible for:
//!
//! 1. Defining a `*RawUsage` struct that mirrors the provider's wire
//!    format exactly enough to deserialize with `serde`.
//! 2. Exposing a pure `to_snapshot` mapper that converts the raw shape
//!    into the normalized [`crate::model::UsageSnapshot`]. Pure so
//!    tests can drive it from HAR fixtures without any network.
//! 3. Exposing a `fetch_usage` async function that performs the actual
//!    HTTPS request and calls `to_snapshot`.
//!
//! Keeping parsing and fetching as separate functions means the parser
//! can be unit-tested hermetically and the fetcher can be tested
//! against a `wiremock` stub, without either side blocking the other.
//!
//! [`FetchError`] is the shared error type for all fetchers. The
//! scheduler's retry / back-off / re-auth logic only cares about a
//! handful of categories (auth expired, rate limited, transient
//! failure, parse failure) — it does not need to know which provider
//! triggered the error, so one enum serves both.

use thiserror::Error;

pub mod chatgpt;
pub mod claude;

/// Shared error type for all provider network fetchers.
///
/// Variants are chosen to map directly onto scheduler decisions:
///
/// - `Unauthorized` → surface "re-auth required" in the UI, stop
///   retrying with the current cookie.
/// - `RateLimited` → exponential back-off.
/// - `ServerError` → transient; retry with jitter.
/// - `Parse` → the response body was not the expected shape. Almost
///   always means the provider changed their API. Log prominently
///   and stop retrying — a code update is needed.
/// - `Network` → transport-level failure (DNS, TLS, timeout). Retry
///   with back-off.
#[derive(Debug, Error)]
pub enum FetchError {
    /// 401 or 403 — the session cookie is invalid or expired.
    #[error("authentication failed (HTTP {status}): re-auth required")]
    Unauthorized { status: u16 },

    /// 429 — the provider is rate-limiting us.
    #[error("rate limited (HTTP 429)")]
    RateLimited,

    /// 5xx — the provider is having issues.
    #[error("server error (HTTP {status})")]
    ServerError { status: u16 },

    /// Any other non-2xx status we did not anticipate.
    #[error("unexpected HTTP status {status}")]
    UnexpectedStatus { status: u16 },

    /// The response body was not valid JSON or did not match the
    /// expected shape.
    #[error("failed to parse response: {0}")]
    Parse(String),

    /// Transport-level failure (DNS, TLS handshake, timeout, etc.).
    #[error("network error: {0}")]
    Network(String),
}

/// Maps a `reqwest::Response` status into the appropriate `FetchError`
/// variant for non-2xx responses. Centralizes the status → error
/// mapping so each provider's `fetch_usage` doesn't repeat it.
pub(crate) fn check_status(status: reqwest::StatusCode) -> Result<(), FetchError> {
    if status.is_success() {
        return Ok(());
    }
    let code = status.as_u16();
    Err(match code {
        401 | 403 => FetchError::Unauthorized { status: code },
        429 => FetchError::RateLimited,
        500..=599 => FetchError::ServerError { status: code },
        _ => FetchError::UnexpectedStatus { status: code },
    })
}
