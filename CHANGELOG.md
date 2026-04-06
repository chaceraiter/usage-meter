# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Initial repository scaffolding: README, CONTRIBUTING, SECURITY, CODE_OF_CONDUCT, LICENSE (MIT).
- Architecture and threat-model draft in `docs/ARCHITECTURE.md`.
- Issue and PR templates under `.github/`.
- Embedded-webview sign-in UX documented in `docs/ARCHITECTURE.md` as the primary auth flow (cookie paste reserved as fallback).
- `spike/` directory with Playwright-based endpoint discovery tooling (throwaway, not part of the shipped app).
- `spike/analyze-har.mjs` — HAR file analyzer that scans for usage-related fields in JSON responses without ever logging raw values.
- Claude usage endpoint confirmed and documented in `docs/ARCHITECTURE.md`: `GET /api/organizations/<org-id>/usage` returning `five_hour` and `seven_day` objects with `utilization` (0..100 percent) and `resets_at` (ISO datetime).
- ChatGPT Codex usage endpoint confirmed and documented: `GET https://chatgpt.com/backend-api/wham/usage` returning a `rate_limit` object with `primary_window` and `secondary_window`, each exposing `used_percent`, `limit_window_seconds`, `reset_after_seconds`, and `reset_at` (unix epoch seconds).
- `UsageSnapshot` data model specified in `docs/ARCHITECTURE.md` — reset timestamps and window sizes are retained internally alongside percentages even if the v0.1 UI only shows `%`, to unblock future "resets in…" labels, smart polling, and predictive features without schema migration.
- Tauri 2 application scaffold (Rust backend + vanilla TypeScript frontend via Vite). Placeholder `app_info` IPC command proves end-to-end wiring. Window defaults to 320x200, resizable, decorated. Release profile tuned for size (`panic=abort`, `lto=true`, `opt-level="s"`, `strip=true`).
- `secrets` module: behavior-only `SecretStore` trait with a `KeychainStore` backend (macOS Keychain / Windows Credential Manager / Linux Secret Service via the `keyring` crate's native features) and a `MemoryStore` fake for tests. Cookie payloads are treated as opaque strings so swapping backends never forces a schema migration. Unit tests cover round-trip, overwrite, delete idempotency, key isolation, and trait-object safety.
- `model` module: normalized `UsageSnapshot` / `UsageWindow` / `ProviderExtras` types shared by all providers. Percentages are always `f32` in `0.0..=100.0`, reset timestamps are UTC, window sizes are recorded explicitly, and missing windows are modelled as `Option<UsageWindow>` so the UI can render a dash instead of faking a zero. Serialize-ready for direct IPC to the frontend.
- `providers::claude` parser: deserializes the `GET /api/organizations/<id>/usage` response body into a forgiving `ClaudeRawUsage` struct (every field `Option` with `#[serde(default)]` so unknown or dropped keys never break the parse) and maps it to a `UsageSnapshot` via a pure `to_snapshot` function. Eight unit tests cover the sanitized HAR fixture, missing windows, null `utilization`, missing `resets_at`, forward-compatibility with unknown fields, invalid JSON, and IPC round-trip.
- GitHub Actions CI: two parallel jobs — Rust (fmt + clippy + test on macOS) and Frontend (tsc + vite build on Ubuntu). Concurrency group cancels stale runs on the same ref. PR title validation enforces Conventional Commits on squash-merge titles via `amannn/action-semantic-pull-request`. Dependabot configured for Cargo, npm, and GitHub Actions on a weekly Monday cadence with conventional commit prefixes.
- `providers::chatgpt` parser: deserializes the `GET /backend-api/wham/usage` response body with the same forward-compatible `Option` + `#[serde(default)]` pattern. Classifies `primary_window` / `secondary_window` into the normalized `five_hour` / `weekly` slots by the actual `limit_window_seconds` value rather than slot order — unknown durations are dropped rather than mis-labeled, because a wrong number next to the wrong label is worse than a missing number. Unix-epoch `reset_at` values are converted via `DateTime::from_timestamp` and out-of-range timestamps degrade to `None`. Twelve unit tests cover the sanitized fixture, slot-order independence (the killer test), unknown durations, missing `rate_limit`, null `used_percent`, missing `reset_at`, out-of-range timestamps, duplicate slots, forward compatibility, invalid JSON, and IPC round-trip.
