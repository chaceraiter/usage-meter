//! usage-meter — Tauri application entry point.
//!
//! Current state: scaffolding plus a secret-store abstraction. Real
//! provider scrapers and the scheduler land in subsequent PRs. The
//! placeholder `app_info` command is the minimum surface needed to
//! prove IPC works end-to-end.

pub mod model;
pub mod providers;
pub mod secrets;

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
