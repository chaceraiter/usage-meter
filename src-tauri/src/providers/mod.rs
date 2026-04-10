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

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn check_status_accepts_all_2xx() {
        for code in [200u16, 201, 204, 299] {
            let status = StatusCode::from_u16(code).unwrap();
            assert!(
                check_status(status).is_ok(),
                "expected {code} to be treated as success"
            );
        }
    }

    #[test]
    fn check_status_maps_401_to_unauthorized() {
        let err = check_status(StatusCode::UNAUTHORIZED).unwrap_err();
        assert!(matches!(err, FetchError::Unauthorized { status: 401 }));
    }

    #[test]
    fn check_status_maps_403_to_unauthorized() {
        let err = check_status(StatusCode::FORBIDDEN).unwrap_err();
        assert!(matches!(err, FetchError::Unauthorized { status: 403 }));
    }

    #[test]
    fn check_status_maps_429_to_rate_limited() {
        let err = check_status(StatusCode::TOO_MANY_REQUESTS).unwrap_err();
        assert!(matches!(err, FetchError::RateLimited));
    }

    #[test]
    fn check_status_maps_5xx_to_server_error() {
        for code in [500u16, 502, 503, 504, 599] {
            let status = StatusCode::from_u16(code).unwrap();
            let err = check_status(status).unwrap_err();
            assert!(
                matches!(err, FetchError::ServerError { status } if status == code),
                "expected {code} to map to ServerError, got {err:?}"
            );
        }
    }

    #[test]
    fn check_status_maps_other_4xx_to_unexpected() {
        // 404 and similar codes we don't have dedicated handling for
        // should fall through to UnexpectedStatus so the scheduler
        // surfaces them rather than silently retrying.
        let err = check_status(StatusCode::NOT_FOUND).unwrap_err();
        assert!(matches!(err, FetchError::UnexpectedStatus { status: 404 }));

        let err = check_status(StatusCode::BAD_REQUEST).unwrap_err();
        assert!(matches!(err, FetchError::UnexpectedStatus { status: 400 }));
    }

    #[test]
    fn fetch_error_display_messages_are_non_empty() {
        // Display impls feed logs and UI — all variants must say
        // something meaningful.
        let variants = [
            FetchError::Unauthorized { status: 401 },
            FetchError::RateLimited,
            FetchError::ServerError { status: 500 },
            FetchError::UnexpectedStatus { status: 418 },
            FetchError::Parse("bad json".into()),
            FetchError::Network("dns failure".into()),
        ];
        for err in variants {
            let msg = err.to_string();
            assert!(!msg.is_empty(), "empty Display for {err:?}");
        }
    }

    #[test]
    fn fetch_error_unauthorized_display_includes_status_code() {
        let err = FetchError::Unauthorized { status: 403 };
        assert!(err.to_string().contains("403"));
    }

    #[test]
    fn fetch_error_parse_display_includes_detail() {
        let err = FetchError::Parse("unexpected field: foo".into());
        assert!(err.to_string().contains("unexpected field: foo"));
    }
}
