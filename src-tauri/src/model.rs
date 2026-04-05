//! Normalized usage data model shared by all providers.
//!
//! The goal of this module is to define a single shape — `UsageSnapshot`
//! — that the rest of the application (IPC layer, frontend, future
//! scheduler cache) can depend on, regardless of which provider the
//! data came from. Each provider's scraper owns the mapping from its
//! raw wire format into this shape, and from that point on nothing
//! downstream needs to know whether a number came from Anthropic or
//! OpenAI.
//!
//! Design notes worth keeping in mind:
//!
//! - **Percentages are always `0.0..=100.0` floats.** Never fractions,
//!   never integers. Claude's `utilization` field is already a 0..100
//!   integer and ChatGPT's `used_percent` is already 0..100 — both map
//!   trivially into an `f32` with no scaling.
//!
//! - **Reset times are retained even though v0.1 only shows `%`.** The
//!   call site that decides to surface "resets in 2h 14m" labels, the
//!   smart-polling loop, and any future predictive "you'll hit the
//!   limit in X hours" feature all need `resets_at` — adding the field
//!   now costs nothing and prevents a schema migration later.
//!
//! - **Window sizes are recorded explicitly.** Claude does not include
//!   window length in its response (we hardcode by key name), while
//!   ChatGPT does (`limit_window_seconds`). Persisting the value on
//!   `UsageWindow` means downstream code never needs to remember which
//!   provider supplies it.
//!
//! - **`Option<UsageWindow>` instead of sentinel values.** A provider
//!   may legitimately omit a window (e.g. Claude returns `null` for
//!   per-model weekly caps on plans that don't have them). `None` is
//!   the honest representation; the UI maps it to a "—" dash.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Length of the rolling 5-hour window in seconds. Hardcoded because
/// Anthropic does not include it in the response; ChatGPT happens to
/// use the same 18 000s window for its short primary limit.
pub const FIVE_HOUR_SECONDS: u32 = 5 * 60 * 60;

/// Length of the rolling weekly window in seconds. Same rationale as
/// above — Claude doesn't tell us, and ChatGPT also uses 604 800s.
pub const SEVEN_DAY_SECONDS: u32 = 7 * 24 * 60 * 60;

/// A single usage window (5-hour, weekly, per-model, etc.) after
/// normalization.
///
/// `Serialize` is derived so the struct can cross the Tauri IPC
/// boundary straight to the frontend. `Deserialize` makes it easy to
/// snapshot-test mappers by parsing a golden JSON file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Current usage as a percentage, `0.0..=100.0`. `f32` is plenty
    /// of precision — providers only ever give us integer percents —
    /// and `f32` keeps the IPC payload small.
    pub used_percent: f32,

    /// Absolute UTC instant at which this window resets. Always UTC
    /// internally; any local-time conversion happens in the UI layer.
    pub resets_at: DateTime<Utc>,

    /// Length of the window in seconds. Preserved so downstream code
    /// can display "per 5 hours" vs "per week" without consulting
    /// provider-specific metadata.
    pub window_seconds: u32,
}

/// Provider-specific extras that don't fit the normalized shape.
///
/// This is deliberately minimal in the first parser PR. As real UI
/// features land (per-model weekly caps, credit overage indicators,
/// code-review sub-limits), variants will be added here — but the
/// core `five_hour`/`weekly` fields on `UsageSnapshot` will stay
/// stable, so consumers that only care about the two headline numbers
/// won't be disturbed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderExtras {
    /// No extras surfaced yet.
    None,
}

/// Normalized snapshot of a single account's usage at a point in time.
///
/// This is the one type that crosses the IPC boundary to the frontend.
/// Keeping it provider-agnostic means the UI can render the same
/// two-row layout for Claude and ChatGPT without special cases.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    /// The rolling 5-hour limit, if the provider reported it.
    pub five_hour: Option<UsageWindow>,

    /// The rolling weekly limit, if the provider reported it.
    pub weekly: Option<UsageWindow>,

    /// When we fetched this snapshot. Injected by the caller rather
    /// than read from `Utc::now()` inside the mapper so tests stay
    /// deterministic and so the scheduler can attribute the timestamp
    /// to the request start, not the parse time.
    pub fetched_at: DateTime<Utc>,

    /// Provider-specific extras. See [`ProviderExtras`].
    pub extras: ProviderExtras,
}
