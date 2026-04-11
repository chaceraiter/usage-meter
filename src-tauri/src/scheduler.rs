//! Background polling scheduler.
//!
//! Spawned as a tokio task during `tauri::Builder::setup`. Wakes every
//! `POLL_INTERVAL` seconds, reads provider credentials from the secret
//! store, fetches usage snapshots via `providers::*::fetch_usage`, and
//! pushes results to the frontend through a Tauri event.
//!
//! ## Error handling philosophy
//!
//! The scheduler is the last line of defense — it must never panic,
//! must never block the UI thread, and must keep trying on the next
//! tick even if the current tick fails for every provider. Individual
//! provider failures are logged and the stale cached value is
//! preserved until a fresh fetch succeeds or the user re-authenticates.
//!
//! Specific status → behavior mapping:
//!
//! - `FetchError::Unauthorized` — clear cached snapshot, log. The
//!   frontend sees `None` for that provider and can prompt re-auth.
//! - `FetchError::RateLimited` — keep stale snapshot, log. Future:
//!   exponential back-off per provider.
//! - `FetchError::ServerError` / `Network` — keep stale, log.
//! - `FetchError::Parse` — keep stale, log prominently. Means the
//!   provider changed their API — a code update is needed.

use std::sync::Arc;
use std::time::Duration;

use log::{info, warn};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::model::UsageSnapshot;
use crate::providers::chatgpt::{self, ChatGptAuth};
use crate::providers::claude::{self, ClaudeAuth};
use crate::providers::FetchError;
use crate::secrets::SecretStore;
use crate::{CHATGPT_BASE_URL, CLAUDE_BASE_URL};

/// Polling interval in seconds. Per `docs/ARCHITECTURE.md`: default 60s,
/// minimum 30s. This will become user-configurable in a settings PR.
const POLL_INTERVAL_SECS: u64 = 60;

/// Secret store key for the Claude auth blob (JSON-serialized
/// [`ClaudeAuth`]).
pub const CLAUDE_AUTH_KEY: &str = "claude.auth";

/// Secret store key for the ChatGPT auth blob (JSON-serialized
/// [`ChatGptAuth`]).
pub const CHATGPT_AUTH_KEY: &str = "chatgpt.auth";

/// Payload emitted to the frontend on every poll cycle via the
/// `usage-update` Tauri event. `None` means "no data yet or auth
/// required" — the frontend renders a placeholder.
#[derive(Debug, Clone, Serialize)]
pub struct UsageUpdate {
    pub claude: Option<UsageSnapshot>,
    pub chatgpt: Option<UsageSnapshot>,
}

/// Shared application state passed to the scheduler and IPC handlers.
///
/// The scheduler writes snapshots; IPC commands read them. Both go
/// through the `tokio::sync::RwLock` so async tasks and sync Tauri
/// commands coexist without deadlocks.
pub struct AppState {
    pub secrets: Box<dyn SecretStore>,
    pub snapshots: tokio::sync::RwLock<UsageUpdate>,
}

impl AppState {
    pub fn new(secrets: Box<dyn SecretStore>) -> Self {
        Self {
            secrets,
            snapshots: tokio::sync::RwLock::new(UsageUpdate {
                claude: None,
                chatgpt: None,
            }),
        }
    }
}

/// Entry point for the background polling loop. Runs forever — the
/// only way it stops is when the Tauri runtime shuts down and drops
/// the tokio runtime.
pub async fn run(handle: AppHandle, state: Arc<AppState>) {
    let client = reqwest::Client::new();

    // First tick is immediate so the UI doesn't sit empty for a full
    // interval on cold start.
    loop {
        let claude_snap = poll_claude(&client, CLAUDE_BASE_URL, &state).await;
        let chatgpt_snap = poll_chatgpt(&client, CHATGPT_BASE_URL, &state).await;

        let update = UsageUpdate {
            claude: claude_snap,
            chatgpt: chatgpt_snap,
        };

        // Cache for IPC `get_usage` command.
        *state.snapshots.write().await = update.clone();

        // Push to frontend.
        if let Err(e) = handle.emit("usage-update", &update) {
            warn!("failed to emit usage-update event: {e}");
        }

        tokio::time::sleep(Duration::from_secs(POLL_INTERVAL_SECS)).await;
    }
}

/// Attempts one Claude fetch cycle. Returns `Some(snapshot)` on
/// success, `None` if credentials are missing or the fetch failed.
///
/// `base_url` is a parameter (rather than reading [`CLAUDE_BASE_URL`]
/// directly) so tests can point it at a wiremock stub.
async fn poll_claude(
    client: &reqwest::Client,
    base_url: &str,
    state: &AppState,
) -> Option<UsageSnapshot> {
    let auth_json = match state.secrets.get(CLAUDE_AUTH_KEY) {
        Ok(Some(json)) => json,
        Ok(None) => return None, // not configured yet
        Err(e) => {
            warn!("failed to read Claude credentials from secret store: {e}");
            return state.snapshots.read().await.claude.clone();
        }
    };

    let auth: ClaudeAuth = match serde_json::from_str(&auth_json) {
        Ok(a) => a,
        Err(e) => {
            warn!("corrupt Claude auth blob in secret store: {e}");
            return None;
        }
    };

    match claude::fetch_usage(client, base_url, &auth).await {
        Ok(snap) => {
            info!(
                "Claude: {}% (5h), {}% (weekly)",
                snap.five_hour.as_ref().map_or(-1.0, |w| w.used_percent),
                snap.weekly.as_ref().map_or(-1.0, |w| w.used_percent),
            );
            Some(snap)
        }
        Err(FetchError::Unauthorized { status }) => {
            warn!("Claude: auth failed (HTTP {status}) — re-auth required");
            None
        }
        Err(e) => {
            warn!("Claude fetch failed: {e}");
            // Keep stale data.
            state.snapshots.read().await.claude.clone()
        }
    }
}

/// Attempts one ChatGPT fetch cycle. Same logic as [`poll_claude`].
async fn poll_chatgpt(
    client: &reqwest::Client,
    base_url: &str,
    state: &AppState,
) -> Option<UsageSnapshot> {
    let auth_json = match state.secrets.get(CHATGPT_AUTH_KEY) {
        Ok(Some(json)) => json,
        Ok(None) => return None,
        Err(e) => {
            warn!("failed to read ChatGPT credentials from secret store: {e}");
            return state.snapshots.read().await.chatgpt.clone();
        }
    };

    let auth: ChatGptAuth = match serde_json::from_str(&auth_json) {
        Ok(a) => a,
        Err(e) => {
            warn!("corrupt ChatGPT auth blob in secret store: {e}");
            return None;
        }
    };

    match chatgpt::fetch_usage(client, base_url, &auth).await {
        Ok(snap) => {
            info!(
                "ChatGPT: {}% (5h), {}% (weekly)",
                snap.five_hour.as_ref().map_or(-1.0, |w| w.used_percent),
                snap.weekly.as_ref().map_or(-1.0, |w| w.used_percent),
            );
            Some(snap)
        }
        Err(FetchError::Unauthorized { status }) => {
            warn!("ChatGPT: auth failed (HTTP {status}) — re-auth required");
            None
        }
        Err(e) => {
            warn!("ChatGPT fetch failed: {e}");
            state.snapshots.read().await.chatgpt.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    //! Integration tests for the per-provider poll functions.
    //!
    //! These tests drive [`poll_claude`] and [`poll_chatgpt`] against a
    //! wiremock HTTP stub, exercising the full secret-store → fetch →
    //! error-classification → stale-data pipeline without a running
    //! Tauri runtime. The entry point [`run`] is not tested directly
    //! because it owns an infinite loop and an `AppHandle` (event
    //! emission); its behavior is covered structurally by the per-
    //! provider poll tests plus the provider-level wiremock tests.
    use super::*;
    use crate::model::{ProviderExtras, UsageWindow, FIVE_HOUR_SECONDS};
    use crate::secrets::MemoryStore;
    use chrono::{DateTime, Utc};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const CLAUDE_FIXTURE: &str = include_str!("../fixtures/claude-usage.json");
    const CHATGPT_FIXTURE: &str = include_str!("../fixtures/chatgpt-usage.json");

    fn claude_auth_json() -> String {
        serde_json::to_string(&ClaudeAuth {
            org_id: "test-org-uuid".to_string(),
            cookie: "sessionKey=test-session".to_string(),
            device_id: "00000000-0000-0000-0000-000000000001".to_string(),
            anonymous_id: "00000000-0000-0000-0000-000000000002".to_string(),
            client_version: "test-version".to_string(),
        })
        .unwrap()
    }

    fn chatgpt_auth_json() -> String {
        serde_json::to_string(&ChatGptAuth {
            cookie: "session=test-session".to_string(),
            device_id: "00000000-0000-0000-0000-000000000001".to_string(),
            session_id: "00000000-0000-0000-0000-000000000002".to_string(),
            client_version: "test-version".to_string(),
            build_number: "test-build".to_string(),
        })
        .unwrap()
    }

    fn state_with_secret(key: &str, value: &str) -> Arc<AppState> {
        let store = Box::new(MemoryStore::new());
        store.set(key, value).unwrap();
        Arc::new(AppState::new(store))
    }

    fn sample_snapshot(percent: f32) -> UsageSnapshot {
        UsageSnapshot {
            five_hour: Some(UsageWindow {
                used_percent: percent,
                resets_at: DateTime::parse_from_rfc3339("2026-04-09T12:34:56Z")
                    .unwrap()
                    .with_timezone(&Utc),
                window_seconds: FIVE_HOUR_SECONDS,
            }),
            weekly: None,
            fetched_at: Utc::now(),
            extras: ProviderExtras::None,
        }
    }

    // -------------------------------------------------------------------
    // AppState / constants
    // -------------------------------------------------------------------

    #[test]
    fn app_state_new_starts_with_empty_snapshots() {
        let state = AppState::new(Box::new(MemoryStore::new()));
        let update = state.snapshots.try_read().unwrap();
        assert!(update.claude.is_none());
        assert!(update.chatgpt.is_none());
    }

    #[test]
    fn poll_interval_matches_documented_default() {
        // docs/ARCHITECTURE.md says 60s default. Regression guard.
        assert_eq!(POLL_INTERVAL_SECS, 60);
    }

    #[test]
    fn auth_keys_are_stable() {
        // Changing these would orphan everyone's stored credentials.
        assert_eq!(CLAUDE_AUTH_KEY, "claude.auth");
        assert_eq!(CHATGPT_AUTH_KEY, "chatgpt.auth");
    }

    #[test]
    fn usage_update_serializes_to_json() {
        let update = UsageUpdate {
            claude: None,
            chatgpt: None,
        };
        let json = serde_json::to_string(&update).unwrap();
        assert!(json.contains("\"claude\""));
        assert!(json.contains("\"chatgpt\""));
    }

    // -------------------------------------------------------------------
    // poll_claude — full pipeline
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn poll_claude_returns_none_when_no_credentials() {
        let state = Arc::new(AppState::new(Box::new(MemoryStore::new())));
        let client = reqwest::Client::new();

        let result = poll_claude(&client, "http://unused.invalid", &state).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn poll_claude_returns_none_when_auth_blob_is_corrupt() {
        let state = state_with_secret(CLAUDE_AUTH_KEY, "not valid json");
        let client = reqwest::Client::new();

        let result = poll_claude(&client, "http://unused.invalid", &state).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn poll_claude_returns_snapshot_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/organizations/test-org-uuid/usage"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(CLAUDE_FIXTURE, "application/json"),
            )
            .mount(&server)
            .await;

        let state = state_with_secret(CLAUDE_AUTH_KEY, &claude_auth_json());
        let client = reqwest::Client::new();

        let snap = poll_claude(&client, &server.uri(), &state)
            .await
            .expect("poll should succeed");
        assert_eq!(snap.five_hour.unwrap().used_percent, 1.0);
    }

    #[tokio::test]
    async fn poll_claude_returns_none_on_401_clearing_cached_snapshot() {
        // Unauthorized must clear the cache so the UI prompts re-auth
        // instead of silently showing stale numbers from before the
        // cookie expired.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let state = state_with_secret(CLAUDE_AUTH_KEY, &claude_auth_json());
        // Seed a stale value.
        state.snapshots.write().await.claude = Some(sample_snapshot(50.0));

        let client = reqwest::Client::new();
        let result = poll_claude(&client, &server.uri(), &state).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn poll_claude_keeps_stale_on_500() {
        // Server errors are transient — the UI should keep showing the
        // last known value rather than flickering to "—".
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let state = state_with_secret(CLAUDE_AUTH_KEY, &claude_auth_json());
        let stale = sample_snapshot(77.0);
        state.snapshots.write().await.claude = Some(stale.clone());

        let client = reqwest::Client::new();
        let result = poll_claude(&client, &server.uri(), &state).await;
        assert_eq!(result, Some(stale));
    }

    #[tokio::test]
    async fn poll_claude_keeps_stale_on_429() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let state = state_with_secret(CLAUDE_AUTH_KEY, &claude_auth_json());
        let stale = sample_snapshot(33.0);
        state.snapshots.write().await.claude = Some(stale.clone());

        let client = reqwest::Client::new();
        let result = poll_claude(&client, &server.uri(), &state).await;
        assert_eq!(result, Some(stale));
    }

    // -------------------------------------------------------------------
    // poll_chatgpt — parallel coverage
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn poll_chatgpt_returns_none_when_no_credentials() {
        let state = Arc::new(AppState::new(Box::new(MemoryStore::new())));
        let client = reqwest::Client::new();

        let result = poll_chatgpt(&client, "http://unused.invalid", &state).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn poll_chatgpt_returns_none_when_auth_blob_is_corrupt() {
        let state = state_with_secret(CHATGPT_AUTH_KEY, "{}");
        let client = reqwest::Client::new();

        let result = poll_chatgpt(&client, "http://unused.invalid", &state).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn poll_chatgpt_returns_snapshot_on_200() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/backend-api/wham/usage"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(CHATGPT_FIXTURE, "application/json"),
            )
            .mount(&server)
            .await;

        let state = state_with_secret(CHATGPT_AUTH_KEY, &chatgpt_auth_json());
        let client = reqwest::Client::new();

        let snap = poll_chatgpt(&client, &server.uri(), &state)
            .await
            .expect("poll should succeed");
        // Don't assert specific values — just that a snapshot came back.
        // The exact percentages live in provider-level fixture tests.
        assert!(snap.five_hour.is_some() || snap.weekly.is_some());
    }

    #[tokio::test]
    async fn poll_chatgpt_returns_none_on_403_clearing_cached_snapshot() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let state = state_with_secret(CHATGPT_AUTH_KEY, &chatgpt_auth_json());
        state.snapshots.write().await.chatgpt = Some(sample_snapshot(50.0));

        let client = reqwest::Client::new();
        let result = poll_chatgpt(&client, &server.uri(), &state).await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn poll_chatgpt_keeps_stale_on_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let state = state_with_secret(CHATGPT_AUTH_KEY, &chatgpt_auth_json());
        let stale = sample_snapshot(88.0);
        state.snapshots.write().await.chatgpt = Some(stale.clone());

        let client = reqwest::Client::new();
        let result = poll_chatgpt(&client, &server.uri(), &state).await;
        assert_eq!(result, Some(stale));
    }
}
