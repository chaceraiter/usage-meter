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
