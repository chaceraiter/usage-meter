# Architecture

This document describes the technical design of usage-meter, including the security threat model for how session cookies are handled.

## Goals

1. Show live 5-hour and weekly usage percentages for a user's Claude and ChatGPT Codex subscriptions.
2. Report **account-level** usage, so multi-device activity is captured correctly.
3. Ship as a **floating, always-on-top widget** with an optional menu-bar mode on macOS.
4. Keep all credentials local. No telemetry, no backend.
5. Be portable enough to support Linux and Windows later without a rewrite.

## Non-goals (for v1)

- Historical analytics / charts beyond the current window.
- Multi-user dashboards or team features.
- Any cloud sync. (A self-hosted, E2E-encrypted sync option may come later.)
- Predictive limit forecasting.

## High-level design

```
┌─────────────────────────────────────────────────────────────────┐
│                        Tauri application                        │
│                                                                  │
│  ┌──────────────┐          ┌────────────────────────────────┐  │
│  │              │  IPC     │                                │  │
│  │  Web UI      │ ◄──────► │  Rust core                     │  │
│  │  (widget)    │          │                                │  │
│  │              │          │   ┌──────────────┐             │  │
│  │  - floating  │          │   │  Scheduler   │             │  │
│  │  - tray menu │          │   └──────┬───────┘             │  │
│  │              │          │          │                     │  │
│  └──────────────┘          │          ▼                     │  │
│                            │   ┌──────────────┐             │  │
│                            │   │  Scrapers    │             │  │
│                            │   │  - claude    │             │  │
│                            │   │  - chatgpt   │             │  │
│                            │   └──────┬───────┘             │  │
│                            │          │                     │  │
│                            │          ▼                     │  │
│                            │   ┌──────────────┐             │  │
│                            │   │  Keychain    │             │  │
│                            │   │  (cookies)   │             │  │
│                            │   └──────────────┘             │  │
│                            └────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────┘
            │                                    │
            │ HTTPS                              │ HTTPS
            ▼                                    ▼
      claude.ai                            chatgpt.com
```

## Components

### Rust core (`src-tauri/`)

- **`scheduler`** — tokio task that polls each provider on a configurable interval.
- **`scrapers/`** — one module per provider (`claude.rs`, `chatgpt.rs`). Each exposes a trait like `UsageSource` that returns a normalized `UsageSnapshot { five_hour_pct, weekly_pct, ... }`.
- **`keychain`** — thin wrapper around the [`keyring`](https://crates.io/crates/keyring) crate. The only code path that touches stored credentials.
- **`ipc`** — Tauri commands exposed to the frontend (`get_latest_snapshot`, `set_cookie`, `clear_cookies`, etc.).

### Frontend (`src/`)

- Web tech (HTML/CSS/JS — framework TBD, likely SolidJS or vanilla for footprint).
- Two window modes:
  - **Floating widget**: small, borderless, always-on-top toggle.
  - **Tray menu**: shows summary numbers in the dropdown.
- Settings panel for cookie capture, poll interval, theme.

## Authentication UX (embedded webview sign-in)

Neither Anthropic nor OpenAI offer OAuth for subscription-tier usage data — their OAuth flows cover only the pay-per-token API, which does not expose the 5-hour / weekly quotas we need. So we can't use the standard "open system browser, redirect to localhost callback" pattern.

Instead, usage-meter uses an **embedded webview sign-in** flow, which is what most desktop apps that wrap web services do:

1. User clicks "Connect Claude account" (or ChatGPT).
2. The app opens a secondary Tauri window containing a webview pointed at the provider's real login page (`claude.ai/login` or `chatgpt.com`).
3. The user logs in normally inside that window. It looks and feels exactly like the real site because it *is* the real site — same HTML, same JS, same 2FA flow, same everything.
4. After successful login, the Rust core reads the resulting session cookies from the webview's cookie store (via Tauri's cookie APIs — WKWebView on macOS, WebView2 on Windows).
5. The relevant cookies are moved into the macOS Keychain. The webview's cookie store is then cleared, and the window is closed.
6. The scraper uses the stored cookies for polling.

This gives us the "pro UX" pattern (like OAuth's sign-in popup) without needing provider OAuth support. It's also safer than asking users to paste cookies from devtools.

**Fallback for v0.x**: a manual "paste Cookie header" input, for debugging and for platforms where the webview cookie APIs misbehave.

## Data flow

1. User clicks "Connect" for a provider; embedded webview sign-in flow captures session cookies (see above).
2. Cookies are written to the OS keychain via the Rust core. Webview cookie store is cleared.
3. Scheduler fires every N seconds; scrapers read cookies from keychain, hit the provider's usage endpoint, parse the response.
4. Normalized `UsageSnapshot` is cached in memory and pushed to the frontend via Tauri event.
5. Frontend renders; no disk writes, no network calls.

## Provider endpoints

**Status: to be confirmed in the initial spike.**

Neither Anthropic nor OpenAI document a public API for subscription-tier usage quotas. Their web settings pages clearly display 5-hour and weekly usage, so internal endpoints exist. The first implementation task is a time-boxed spike to:

1. Log into each service in a browser with devtools open.
2. Navigate to the usage/settings view.
3. Identify the XHR/fetch calls that populate the usage display.
4. Confirm the responses contain the fields we need.
5. Document the endpoints here.

If no suitable endpoint exists for one provider, fall back options:
- DOM scraping via a headless browser (heavier, more fragile).
- Token-counting the local CLI state (device-level only, not account-level — last resort).

## Threat model

### What we protect

- **Session cookies at rest**: stored in macOS Keychain, encrypted by the OS, protected by the login keychain's access controls.
- **Session cookies in transit**: only ever sent over HTTPS to the originating service.
- **No exfiltration**: no telemetry, crash reporting, or analytics makes any network request outside of `claude.ai`/`chatgpt.com`.

### What we assume

- The user's macOS login account is not already compromised. If an attacker has your login session, they already have your Keychain.
- Tauri's IPC boundary is trustworthy. We still validate commands and avoid passing raw cookies to the frontend (the frontend never sees cookies; it only sees normalized snapshots).
- The HTTPS certificate chain is valid (standard `rustls` defaults).

### What we explicitly don't protect against

- A malicious OS-level process running as your user.
- Physical access to an unlocked machine.
- Supply-chain attacks on dependencies (mitigated via `cargo audit` + Dependabot, but not eliminated).

### Provider-side considerations

- Polling is rate-limited to avoid triggering abuse detection. Default: 60s, minimum: 30s.
- If a provider returns 401/403, we clear the in-memory cookie reference and surface a "re-auth required" state in the UI. We do NOT retry with a stored cookie we know is bad.
- If a provider returns 429, back off exponentially.

## Build + release

- `pnpm tauri dev` for local development.
- `pnpm tauri build` produces a `.app` / `.dmg` on macOS.
- GitHub Actions builds tagged releases. Initial releases are unsigned with install instructions; Apple notarization is a later polish item.

## Open questions

- Framework for the frontend? (SolidJS, Svelte, or vanilla.)
- Which specific cookies do we need to harvest from the webview store for each provider? (Determined by the endpoint spike.)
- Will the embedded webview trip bot-detection on the providers' login pages? (To be validated — user-agent and feature set should match a real browser, so probably fine.)
- Should snapshots be persisted to a local SQLite for simple historical charts, even in v1?
- How aggressively to back off on 429 — provider-specific tuning.
