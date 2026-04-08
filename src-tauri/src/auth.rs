//! Embedded webview authentication flow.
//!
//! Opens a secondary Tauri window pointed at the provider's real login
//! page. After the user logs in, polls the webview's native cookie
//! store (which includes HttpOnly cookies) for the session token.
//! Once found, the cookie is validated via a real usage fetch, stored
//! in the keychain, and the login window is cleaned up.
//!
//! This is the primary auth UX — cookie paste is the fallback.

use std::sync::Arc;
use std::time::Duration;

use log::{info, warn};
use tauri::{Emitter, Manager, Url, WebviewUrl, WebviewWindowBuilder};

use crate::providers::chatgpt::{self, ChatGptAuth};
use crate::providers::claude::{self, ClaudeAuth};
use crate::scheduler::{AppState, UsageUpdate, CHATGPT_AUTH_KEY, CLAUDE_AUTH_KEY};
use crate::{CHATGPT_BASE_URL, CLAUDE_BASE_URL};

/// Label for the login webview window. Only one login window can be
/// open at a time — attempting to open a second is a no-op.
const LOGIN_WINDOW_LABEL: &str = "login";

/// How often to poll the webview cookie store for the session token.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Maximum time to wait for the user to log in before giving up.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// The cookie name that indicates a successful Claude login.
const CLAUDE_SESSION_COOKIE: &str = "sessionKey";

/// Opens a login window for the given provider. The window loads the
/// provider's real login page. A background task polls for the session
/// cookie and completes the connection automatically once login succeeds.
///
/// Returns immediately — the frontend receives an `auth-complete` or
/// `auth-error` event when the flow finishes.
#[tauri::command]
pub async fn open_auth_window(
    provider: String,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let login_url = match provider.as_str() {
        "claude" => format!("{}/login", CLAUDE_BASE_URL),
        "chatgpt" => CHATGPT_BASE_URL.to_string(),
        _ => return Err(format!("unknown provider: {provider}")),
    };

    // Prevent duplicate login windows.
    if app.get_webview_window(LOGIN_WINDOW_LABEL).is_some() {
        return Err("A login window is already open.".into());
    }

    let url = Url::parse(&login_url).map_err(|e| format!("Invalid URL: {e}"))?;

    // Temporarily lower the main widget so the login window isn't hidden behind it.
    if let Some(main_win) = app.get_webview_window("main") {
        let _ = main_win.set_always_on_top(false);
    }

    WebviewWindowBuilder::new(&app, LOGIN_WINDOW_LABEL, WebviewUrl::External(url))
        .title(format!("Sign in — {provider}"))
        .inner_size(480.0, 700.0)
        .resizable(true)
        .focused(true)
        .build()
        .map_err(|e| format!("Failed to open login window: {e}"))?;

    // Spawn a background task to poll for the session cookie.
    let handle = app.app_handle().clone();
    let state_arc = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        match poll_for_login(&handle, &provider, &state_arc).await {
            Ok(update) => {
                let _ = handle.emit("auth-complete", &update);
            }
            Err(e) => {
                warn!("auth flow failed for {provider}: {e}");
                let _ = handle.emit("auth-error", &e);
            }
        }
        // Clean up the login window regardless of outcome.
        cleanup_login_window(&handle);
    });

    Ok(())
}

/// Closes the login window if the user wants to cancel.
#[tauri::command]
pub async fn cancel_auth(app: tauri::AppHandle) -> Result<(), String> {
    cleanup_login_window(&app);
    Ok(())
}

/// Polls the login webview's cookie store until the provider's session
/// cookie appears, then validates and stores credentials.
async fn poll_for_login(
    app: &tauri::AppHandle,
    provider: &str,
    state: &Arc<AppState>,
) -> Result<UsageUpdate, String> {
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > LOGIN_TIMEOUT {
            return Err("Login timed out after 5 minutes.".into());
        }

        tokio::time::sleep(POLL_INTERVAL).await;

        let win = match app.get_webview_window(LOGIN_WINDOW_LABEL) {
            Some(w) => w,
            None => return Err("Login window was closed.".into()),
        };

        // Cookie extraction touches the native webview cookie store,
        // which runs on the main thread. If the user is dragging the
        // window at the same moment, this can race. Treat extraction
        // failures as "not ready yet" and retry on the next tick.
        let cookie_header = match provider {
            "claude" => match extract_claude_cookies(&win) {
                Ok(h) => h,
                Err(e) => {
                    warn!("cookie read failed (will retry): {e}");
                    continue;
                }
            },
            "chatgpt" => match extract_chatgpt_cookies(&win) {
                Ok(h) => h,
                Err(e) => {
                    warn!("cookie read failed (will retry): {e}");
                    continue;
                }
            },
            _ => return Err(format!("unknown provider: {provider}")),
        };

        let Some(cookie_header) = cookie_header else {
            continue; // Not logged in yet — keep polling.
        };

        info!("{provider}: session cookie detected, validating…");

        // Validate and store using the same flow as cookie-paste.
        return match provider {
            "claude" => connect_claude_from_cookies(&cookie_header, state, app).await,
            "chatgpt" => connect_chatgpt_from_cookies(&cookie_header, state, app).await,
            _ => unreachable!(),
        };
    }
}

/// Extracts cookies from the Claude login webview. Returns `Some(cookie_header)`
/// when the `sessionKey` cookie is present, `None` otherwise.
fn extract_claude_cookies(win: &tauri::WebviewWindow) -> Result<Option<String>, String> {
    let url = Url::parse(CLAUDE_BASE_URL).unwrap();
    let cookies = win
        .cookies_for_url(url)
        .map_err(|e| format!("Failed to read cookies: {e}"))?;

    let has_session = cookies.iter().any(|c| c.name() == CLAUDE_SESSION_COOKIE);
    if !has_session {
        return Ok(None);
    }

    // Build a full Cookie header from all cookies for the domain.
    let header = cookies
        .iter()
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ");

    Ok(Some(header))
}

/// Extracts cookies from the ChatGPT login webview. ChatGPT uses
/// multiple auth cookies — we consider login complete when we see
/// a cookie containing auth-related tokens. The heuristic: if there
/// are more than 2 cookies set for chatgpt.com, login has likely
/// succeeded (pre-login typically has 0-1 cookies).
fn extract_chatgpt_cookies(win: &tauri::WebviewWindow) -> Result<Option<String>, String> {
    let url = Url::parse(CHATGPT_BASE_URL).unwrap();
    let cookies = win
        .cookies_for_url(url)
        .map_err(|e| format!("Failed to read cookies: {e}"))?;

    // Heuristic: ChatGPT sets several cookies after login. Before login
    // there are typically 0-1 tracking cookies. We wait for at least 3.
    if cookies.len() < 3 {
        return Ok(None);
    }

    let header = cookies
        .iter()
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ");

    Ok(Some(header))
}

/// Validates and stores Claude credentials from an extracted cookie header.
async fn connect_claude_from_cookies(
    cookie: &str,
    state: &Arc<AppState>,
    app: &tauri::AppHandle,
) -> Result<UsageUpdate, String> {
    let client = reqwest::Client::new();

    let org_id = claude::discover_org_id(&client, CLAUDE_BASE_URL, cookie)
        .await
        .map_err(|e| format!("Cookie validation failed: {e}"))?;

    let auth = ClaudeAuth {
        org_id,
        cookie: cookie.to_string(),
        device_id: uuid::Uuid::new_v4().to_string(),
        anonymous_id: uuid::Uuid::new_v4().to_string(),
        client_version: "1.0.0".to_string(),
    };

    let snap = claude::fetch_usage(&client, CLAUDE_BASE_URL, &auth)
        .await
        .map_err(|e| format!("Usage fetch failed: {e}"))?;

    let auth_json =
        serde_json::to_string(&auth).map_err(|e| format!("Failed to serialize auth: {e}"))?;
    state
        .secrets
        .set(CLAUDE_AUTH_KEY, &auth_json)
        .map_err(|e| format!("Failed to store credentials: {e}"))?;

    let mut snapshots = state.snapshots.write().await;
    snapshots.claude = Some(snap);
    let update = snapshots.clone();
    drop(snapshots);

    let _ = app.emit("usage-update", &update);
    Ok(update)
}

/// Validates and stores ChatGPT credentials from an extracted cookie header.
async fn connect_chatgpt_from_cookies(
    cookie: &str,
    state: &Arc<AppState>,
    app: &tauri::AppHandle,
) -> Result<UsageUpdate, String> {
    let client = reqwest::Client::new();

    let auth = ChatGptAuth {
        cookie: cookie.to_string(),
        device_id: uuid::Uuid::new_v4().to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        client_version: "1.0.0".to_string(),
        build_number: "1".to_string(),
    };

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

/// Clears browsing data, closes the login window, and restores the
/// main widget's always-on-top state.
fn cleanup_login_window(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window(LOGIN_WINDOW_LABEL) {
        let _ = win.clear_all_browsing_data();
        let _ = win.close();
    }

    // Restore always-on-top on the main widget.
    if let Some(main_win) = app.get_webview_window("main") {
        let _ = main_win.set_always_on_top(true);
    }
}
