//! usage-meter — Tauri application entry point.
//!
//! Current state: scaffolding only. Real provider scrapers, keychain
//! integration, and the scheduler will land in subsequent PRs. The
//! placeholder `app_version` command is the minimum surface needed to
//! prove IPC works end-to-end.

use serde::Serialize;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![app_info])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
