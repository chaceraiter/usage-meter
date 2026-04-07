//! usage-meter — Tauri application entry point.
//!
//! Wires together the secret store, background scheduler, IPC
//! commands, and the Tauri runtime. The scheduler polls each
//! provider's usage endpoint on a 60-second interval and pushes
//! `usage-update` events to the frontend. IPC commands let the
//! frontend pull, connect, and disconnect providers on demand.

pub mod model;
pub mod providers;
pub mod scheduler;
pub mod secrets;

use std::sync::Arc;

use serde::Serialize;
use tauri::Emitter;

use providers::chatgpt::{self, ChatGptAuth};
use providers::claude::{self, ClaudeAuth};
use scheduler::{AppState, UsageUpdate, CHATGPT_AUTH_KEY, CLAUDE_AUTH_KEY};
use secrets::{KeychainStore, MemoryStore};

/// Service identifier for the OS credential store. Convention:
/// reverse-DNS matching the app's bundle identifier.
const KEYCHAIN_SERVICE: &str = "com.chaceraiter.usage-meter";

/// Base URLs for providers. `pub(crate)` so the scheduler can reuse them.
pub(crate) const CLAUDE_BASE_URL: &str = "https://claude.ai";
pub(crate) const CHATGPT_BASE_URL: &str = "https://chatgpt.com";

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: env!("CARGO_PKG_NAME"),
        version: env!("CARGO_PKG_VERSION"),
    }
}

/// Returns the latest cached usage snapshots for all providers.
#[tauri::command]
async fn get_usage(state: tauri::State<'_, Arc<AppState>>) -> Result<UsageUpdate, ()> {
    Ok(state.snapshots.read().await.clone())
}

/// Connects a Claude account by validating the pasted cookie, auto-
/// discovering the org ID, storing credentials, and returning the
/// first usage snapshot. If validation fails, nothing is stored.
#[tauri::command]
async fn connect_claude(
    cookie: String,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<UsageUpdate, String> {
    let client = reqwest::Client::new();

    // Auto-discover org ID — proves the cookie is valid AND gives us
    // the UUID we need for the usage endpoint.
    let org_id = claude::discover_org_id(&client, CLAUDE_BASE_URL, &cookie)
        .await
        .map_err(|e| format!("Cookie validation failed: {e}"))?;

    let auth = ClaudeAuth {
        org_id,
        cookie,
        device_id: uuid::Uuid::new_v4().to_string(),
        anonymous_id: uuid::Uuid::new_v4().to_string(),
        client_version: "1.0.0".to_string(),
    };

    // Validate by actually fetching usage.
    let snap = claude::fetch_usage(&client, CLAUDE_BASE_URL, &auth)
        .await
        .map_err(|e| format!("Usage fetch failed: {e}"))?;

    // Cookie is good — store credentials.
    let auth_json =
        serde_json::to_string(&auth).map_err(|e| format!("Failed to serialize auth: {e}"))?;
    state
        .secrets
        .set(CLAUDE_AUTH_KEY, &auth_json)
        .map_err(|e| format!("Failed to store credentials: {e}"))?;

    // Update cache and notify frontend.
    let mut snapshots = state.snapshots.write().await;
    snapshots.claude = Some(snap);
    let update = snapshots.clone();
    drop(snapshots);

    let _ = app.emit("usage-update", &update);
    Ok(update)
}

/// Connects a ChatGPT account. Same validate-on-save pattern as Claude
/// but simpler — no org discovery step needed.
#[tauri::command]
async fn connect_chatgpt(
    cookie: String,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<UsageUpdate, String> {
    let client = reqwest::Client::new();

    let auth = ChatGptAuth {
        cookie,
        device_id: uuid::Uuid::new_v4().to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        client_version: "1.0.0".to_string(),
        build_number: "1".to_string(),
    };

    // Validate by fetching.
    let snap = chatgpt::fetch_usage(&client, CHATGPT_BASE_URL, &auth)
        .await
        .map_err(|e| format!("Usage fetch failed: {e}"))?;

    let auth_json =
        serde_json::to_string(&auth).map_err(|e| format!("Failed to serialize auth: {e}"))?;
    state
        .secrets
        .set(CHATGPT_AUTH_KEY, &auth_json)
        .map_err(|e| format!("Failed to store credentials: {e}"))?;

    let mut snapshots = state.snapshots.write().await;
    snapshots.chatgpt = Some(snap);
    let update = snapshots.clone();
    drop(snapshots);

    let _ = app.emit("usage-update", &update);
    Ok(update)
}

/// Disconnects a provider by clearing stored credentials and cached
/// snapshot.
#[tauri::command]
async fn disconnect(
    provider: String,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let key = match provider.as_str() {
        "claude" => CLAUDE_AUTH_KEY,
        "chatgpt" => CHATGPT_AUTH_KEY,
        _ => return Err(format!("unknown provider: {provider}")),
    };

    state
        .secrets
        .delete(key)
        .map_err(|e| format!("Failed to clear credentials: {e}"))?;

    let mut snapshots = state.snapshots.write().await;
    match provider.as_str() {
        "claude" => snapshots.claude = None,
        "chatgpt" => snapshots.chatgpt = None,
        _ => {}
    }
    let update = snapshots.clone();
    drop(snapshots);

    let _ = app.emit("usage-update", &update);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let secrets: Box<dyn secrets::SecretStore> = if cfg!(test) {
        Box::new(MemoryStore::new())
    } else {
        Box::new(KeychainStore::new(KEYCHAIN_SERVICE))
    };

    let state = Arc::new(AppState::new(secrets));

    tauri::Builder::default()
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![
            app_info,
            get_usage,
            connect_claude,
            connect_chatgpt,
            disconnect,
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(scheduler::run(handle, state));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
