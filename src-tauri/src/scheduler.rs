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

/// Polling interval in seconds. Per `docs/ARCHITECTURE.md`: default 60s,
/// minimum 30s. This will become user-configurable in a settings PR.
const POLL_INTERVAL_SECS: u64 = 60;

/// Secret store key for the Claude auth blob (JSON-serialized
/// [`ClaudeAuth`]).
const CLAUDE_AUTH_KEY: &str = "claude.auth";

/// Secret store key for the ChatGPT auth blob (JSON-serialized
/// [`ChatGptAuth`]).
const CHATGPT_AUTH_KEY: &str = "chatgpt.auth";

/// Production base URLs. Extracted as constants so they're easy to find
/// when the inevitable domain change happens.
const CLAUDE_BASE_URL: &str = "https://claude.ai";
const CHATGPT_BASE_URL: &str = "https://chatgpt.com";

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
        let claude_snap = poll_claude(&client, &state).await;
        let chatgpt_snap = poll_chatgpt(&client, &state).await;

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
async fn poll_claude(client: &reqwest::Client, state: &AppState) -> Option<UsageSnapshot> {
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

    match claude::fetch_usage(client, CLAUDE_BASE_URL, &auth).await {
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
async fn poll_chatgpt(client: &reqwest::Client, state: &AppState) -> Option<UsageSnapshot> {
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

    match chatgpt::fetch_usage(client, CHATGPT_BASE_URL, &auth).await {
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
