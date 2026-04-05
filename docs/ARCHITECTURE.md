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
- **`scrapers/`** — one module per provider (`claude.rs`, `chatgpt.rs`). Each exposes a trait like `UsageSource` that returns a normalized `UsageSnapshot`.
- **`keychain`** — thin wrapper around the [`keyring`](https://crates.io/crates/keyring) crate. The only code path that touches stored credentials.
- **`ipc`** — Tauri commands exposed to the frontend (`get_latest_snapshot`, `set_cookie`, `clear_cookies`, etc.).

#### Normalized `UsageSnapshot`

Both providers expose reset timestamps alongside percentages. **The internal snapshot always retains both**, even if the v0.1 UI only displays the percent. This keeps downstream features — "resets in 2h 14m" labels, smart polling (don't re-poll during a dead window), predictive "you'll hit the limit in X hours" — unblocked without a schema migration.

```rust
struct UsageWindow {
    used_percent: f32,           // 0.0 ..= 100.0
    resets_at:    DateTime<Utc>, // absolute UTC instant
    window_seconds: u32,         // e.g. 18_000 for 5h, 604_800 for 7d
}

struct UsageSnapshot {
    five_hour:   Option<UsageWindow>,
    weekly:      Option<UsageWindow>,
    fetched_at:  DateTime<Utc>,
    // provider-specific extras (e.g. per-model weekly caps, credit
    // balance, code-review sub-limits) live under an enum variant
    // and are exposed to the frontend as read-only "details" blobs.
    extras:      ProviderExtras,
}
```

Provider scrapers convert their raw responses into this shape:

- **Claude**: `resets_at` is already ISO-8601; parse directly. `window_seconds` is hardcoded by key name (`five_hour` = 18 000, `seven_day` = 604 800) since Anthropic doesn't expose it in the response.
- **ChatGPT Codex**: `reset_at` is a Unix epoch (seconds); convert via `DateTime::from_timestamp`. `limit_window_seconds` is provided — use it to classify `primary_window`/`secondary_window` into `five_hour` vs `weekly` rather than assuming ordering.

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

Neither Anthropic nor OpenAI document a public API for subscription-tier usage quotas, but both web UIs clearly display 5-hour and weekly usage, so internal endpoints exist. We discovered them by exporting HAR files from a real browser on the provider's usage page (see `spike/`).

### Claude (confirmed 2026-04-04)

**Endpoint:**

```
GET https://claude.ai/api/organizations/<org-id>/usage
```

`<org-id>` is a UUID that must be discovered once per account, likely from `/api/bootstrap/<org-id>/app_start` or a `/api/organizations` list endpoint (to be finalized during implementation).

**Response (v1 shape):**

```jsonc
{
  "five_hour": {
    "utilization": <number 0..100>,   // percent
    "resets_at":   "<ISO datetime>"
  },
  "seven_day": {
    "utilization": <number 0..100>,
    "resets_at":   "<ISO datetime>"
  },
  "seven_day_opus":       null | { utilization, resets_at },  // per-model weekly cap, plan-dependent
  "seven_day_sonnet":     null | { utilization, resets_at },
  "seven_day_oauth_apps": null | {...},
  "seven_day_cowork":     null | {...},
  "iguana_necktie":       null | {...},   // internal codename, ignore
  "extra_usage": {
    "is_enabled":    <boolean>,
    "monthly_limit": <number>,            // overage credit limit (separate billing concept)
    "used_credits":  <number>,
    "utilization":   null | <number>
  }
}
```

`utilization` is a percent (0..100), NOT a fraction — confirmed against on-screen values during discovery.

**For v0.1 the app consumes only `five_hour.utilization` and `seven_day.utilization`.** The per-model weekly caps (`seven_day_opus`, `seven_day_sonnet`) are plan-dependent and may be surfaced later as a "details" view.

**Required request headers (from HAR):**
- Session cookies (standard)
- `anthropic-client-platform: web_claude_ai`
- `anthropic-client-version: <version-string>`
- `anthropic-device-id: <uuid>`         — we generate once per install, persist in Keychain
- `anthropic-anonymous-id: <uuid>`      — same
- `content-type: application/json`

`x-datadog-*` headers in the real browser are RUM telemetry and are not required.

### ChatGPT Codex (confirmed 2026-04-04)

**Endpoint:**

```
GET https://chatgpt.com/backend-api/wham/usage
```

`wham` appears to be OpenAI's internal codename for the Codex backend. The endpoint does not require any path parameter — authentication alone is enough to resolve the current account.

**Response (v1 shape):**

```jsonc
{
  "user_id":    "<string>",
  "account_id": "<string>",
  "email":      "<string>",
  "plan_type":  "<string>",              // "plus", "pro", etc.

  "rate_limit": {
    "allowed":       <boolean>,
    "limit_reached": <boolean>,
    "primary_window": {
      "used_percent":         <number 0..100>,
      "limit_window_seconds": <number>,   // window size in seconds (e.g. 18000 = 5h, 604800 = 7d)
      "reset_after_seconds":  <number>,
      "reset_at":             <unix epoch seconds>
    },
    "secondary_window": { ...same shape... }
  },

  "code_review_rate_limit": { ...same shape... },   // per-feature limit
  "additional_rate_limits": null | {...},

  "credits": {
    "has_credits":           <boolean>,
    "unlimited":             <boolean>,
    "balance":               "<string>",
    "approx_local_messages": [<number>, <number>],
    "approx_cloud_messages": [<number>, <number>]
  },
  "spend_control": { "reached": <boolean> },
  "promo":         null | {...}
}
```

**For v0.1 the app consumes `rate_limit.primary_window.used_percent` and `rate_limit.secondary_window.used_percent`.** We identify which window is the 5-hour vs the weekly cap by reading `limit_window_seconds` rather than assuming ordering — this is more robust if OpenAI reshuffles.

**Required request headers (from HAR):**
- Session cookies (standard)
- `oai-client-version: <version-string>`
- `oai-client-build-number: <number>`
- `oai-device-id: <uuid>`      — we generate once per install, persist in Keychain
- `oai-session-id: <uuid>`     — same
- `oai-language: en-US`        — harmless, recommended
- `accept: */*`

`x-openai-target-path` / `x-openai-target-route` headers in the real browser appear to be set by an edge proxy and are not required from clients.

### Fallback
If a provider's endpoint becomes unreachable from outside the real web UI (e.g., bot-detection on the endpoint itself), fall back options:
- DOM scraping via an embedded webview (heavier, more fragile).
- Parsing local CLI state — device-level only, not account-level, last resort.

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
