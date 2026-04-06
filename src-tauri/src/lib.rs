//! usage-meter — Tauri application entry point.
//!
//! Wires together the secret store, background scheduler, IPC
//! commands, and the Tauri runtime. The scheduler polls each
//! provider's usage endpoint on a 60-second interval and pushes
//! `usage-update` events to the frontend. IPC commands let the
//! frontend pull the latest cached snapshot on demand.

pub mod model;
pub mod providers;
pub mod scheduler;
pub mod secrets;

use std::sync::Arc;

use serde::Serialize;

use scheduler::{AppState, UsageUpdate};
use secrets::{KeychainStore, MemoryStore};

/// Service identifier for the OS credential store. Convention:
/// reverse-DNS matching the app's bundle identifier.
const KEYCHAIN_SERVICE: &str = "com.chaceraiter.usage-meter";

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
/// This is a pull complement to the push-based `usage-update` event —
/// the frontend calls it on mount to avoid waiting for the next tick.
#[tauri::command]
async fn get_usage(state: tauri::State<'_, Arc<AppState>>) -> Result<UsageUpdate, ()> {
    Ok(state.snapshots.read().await.clone())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Use the real OS keychain in release/dev builds. Tests inject a
    // `MemoryStore` instead. If the keychain backend fails to
    // initialize (shouldn't happen on supported platforms), fall back
    // to an in-memory store so the app still launches — the user
    // will just have to re-auth on every restart.
    let secrets: Box<dyn secrets::SecretStore> = if cfg!(test) {
        Box::new(MemoryStore::new())
    } else {
        Box::new(KeychainStore::new(KEYCHAIN_SERVICE))
    };

    let state = Arc::new(AppState::new(secrets));

    tauri::Builder::default()
        .manage(state.clone())
        .invoke_handler(tauri::generate_handler![app_info, get_usage])
        .setup(move |app| {
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(scheduler::run(handle, state));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
