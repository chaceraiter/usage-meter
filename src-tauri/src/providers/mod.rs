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
//! 3. (In a later PR) exposing a `fetch_usage` async function that
//!    performs the actual HTTPS request and calls `to_snapshot`.
//!
//! Keeping parsing and fetching as separate functions means the parser
//! can be unit-tested hermetically and the fetcher can be tested
//! against a `wiremock` stub, without either side blocking the other.

pub mod claude;
