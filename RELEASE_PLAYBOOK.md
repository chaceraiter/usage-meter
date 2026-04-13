# Release Playbook

Notes on how this project ships releases. Written for AI agents and future contributors.

## Philosophy

- **Releases are tag-driven.** Push a semver tag → CI builds artifacts → draft release appears on GitHub. No manual builds.
- **Draft first, publish second.** The release workflow creates a draft so a human can review before it goes live.
- **Dry-run with pre-release tags.** Use `-rc.N` suffixes (e.g. `v0.1.0-rc.1`) to test the pipeline without burning the real version number. The tag glob `v[0-9]+.[0-9]+.[0-9]+*` matches both.
- **Test coverage gates the release, not the tag.** CI runs on every PR (fmt, clippy, tests, tsc, vite build). By the time code reaches main, it's already passed. The release workflow builds but does not re-run tests — if CI is green on main, the release is safe.

## How to cut a release

### Prerequisites

1. All CI checks pass on `main`.
2. Manual testing complete (see below).
3. Version in `package.json` and `src-tauri/tauri.conf.json` matches the tag you're about to push (Tauri uses `tauri.conf.json` version for the DMG filename).

### Steps

```bash
# 1. Make sure you're on main and up to date.
git checkout main && git pull

# 2. Tag and push.
git tag v0.2.0
git push origin v0.2.0

# 3. Wait for the Release workflow to finish (~5 min on macOS runner).
gh run watch

# 4. Review the draft release on GitHub.
gh release view v0.2.0

# 5. When satisfied, publish it.
gh release edit v0.2.0 --draft=false
```

### Dry-run (validate pipeline without publishing)

```bash
git tag v0.2.0-rc.1
git push origin v0.2.0-rc.1
# Check the draft release, then delete it when done:
gh release delete v0.2.0-rc.1 --yes
git push origin :v0.2.0-rc.1
git tag -d v0.2.0-rc.1
```

## What the pipeline does

See `.github/workflows/release.yml`. In short:

1. **Trigger**: push of a tag matching `v[0-9]+.[0-9]+.[0-9]+*`
2. **Runner**: `macos-latest` (Apple Silicon / aarch64)
3. **Build**: `pnpm install --frozen-lockfile && pnpm tauri build`
4. **Artifact**: finds the `.dmg` in `src-tauri/target/release/bundle/`
5. **Release**: `softprops/action-gh-release@v2` creates a draft release with auto-generated release notes and the `.dmg` attached

### What's NOT automated yet

- **Code signing / notarization.** The `.dmg` is unsigned. macOS Gatekeeper blocks first launch — users must right-click → Open. Fixing this requires an Apple Developer Program membership ($99/yr) and adding signing secrets to the repo. When ready, Tauri supports this via `tauri.conf.json > bundle > macOS > signingIdentity` and the `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` secrets.
- **Cross-platform builds.** Only macOS aarch64 today. Adding x86_64 macOS, Linux (.deb/.AppImage), and Windows (.msi/.exe) means a build matrix with per-OS Tauri dependencies.
- **Changelog generation.** Currently uses GitHub's auto-generated release notes (PR titles). A structured CHANGELOG.md with `git-cliff` or `conventional-changelog` is an upgrade path.
- **Version bumping.** Manual today. Could use `cargo-release` or a GitHub Action that bumps version in both `package.json` and `tauri.conf.json` on tag push.

## CI architecture

Two workflow files in `.github/workflows/`:

### `ci.yml` — runs on every push to main and every PR

| Job | Runner | What it checks |
|-----|--------|---------------|
| Rust (fmt · clippy · test) | `macos-latest` | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --locked` |
| Frontend (tsc · vite build) | `ubuntu-latest` | `pnpm build` (which runs `tsc && vite build`) |

Plus a separate `pr-title.yml` that enforces conventional commit format on PR titles.

Key CI decisions:
- **macOS for Rust** because it's the primary target and Tauri's native deps differ per OS. Ubuntu/Windows matrix is deferred until cross-platform builds matter.
- **Concurrency group** cancels in-flight runs when a new commit lands on the same branch.
- **Rust cache** (`Swatinem/rust-cache`) keyed on `Cargo.lock` — busts on dependency changes.
- **Actions pinned to version tags**, not SHAs. Dependabot auto-updates weekly. SHA pinning is an upgrade if the threat model tightens.

### `release.yml` — runs on tag push only

Described above. Separate from CI so it only burns macOS runner minutes when actually releasing.

## Test coverage at time of writing

**86 total tests** (73 Rust + 13 TypeScript):

| Module | Tests | What's covered |
|--------|-------|---------------|
| `providers/claude.rs` | 17 | Parser, fixture round-trip, wiremock fetch (200/401/403/429/500/bad JSON), org discovery |
| `providers/chatgpt.rs` | 12 | Parser, fixture round-trip, wiremock fetch, edge cases (missing fields, unknown windows) |
| `providers/mod.rs` | 9 | `check_status()` all HTTP code branches, `FetchError` Display messages |
| `model.rs` | 7 | Serde round-trips, constants, None handling, tag serialization |
| `secrets.rs` | 7 | CRUD, overwrite, delete, object safety |
| `scheduler.rs` | 15 | Poll functions via wiremock: missing creds, corrupt auth, 200/401/403/429/5xx, stale data retention |
| `src/format.ts` | 13 | `formatPercent` (null, rounding, 0%, 100%), `formatResetsIn` (null, past, minutes, hours, days) |
| `lib.rs` | 0 | Tauri IPC commands — coupled to runtime, tested manually |
| `auth.rs` | 0 | Webview auth — coupled to runtime, tested manually |

## What must be tested manually

These require a real Tauri window with real provider credentials:

1. **Sign-in flows** — Claude (email/password) and ChatGPT (email/password or Google) via embedded webview
2. **Cookie detection** — Claude checks for `sessionKey` cookie; ChatGPT uses a heuristic (3+ cookies)
3. **Window z-order** — login window should not be hidden behind the always-on-top widget
4. **Drag safety** — dragging the widget while login window is open should not crash
5. **Persistence** — quit and relaunch; credentials should survive via keychain
6. **Cancel mid-login** — close login window before completing; widget should restore cleanly
7. **DMG install** — mount the built `.dmg`, drag to Applications, launch (right-click → Open for unsigned)

## Lessons learned

1. **Dry-run releases with `-rc.N` tags.** Catches pipeline issues (missing deps, wrong artifact path) without wasting the real version number. Delete the draft + tag when done.
2. **Draft releases by default.** Auto-publishing on tag push is tempting but dangerous. A human should review release notes and verify the artifact before publishing.
3. **Keep the release workflow simple.** Build + find artifact + create release. Don't put tests in the release workflow — that's CI's job. The release workflow should be fast and focused.
4. **macOS signing is a separate concern.** Don't block shipping on it. Unsigned apps work fine with right-click → Open. Add signing when you have an Apple Developer account and the UX friction matters.
5. **Test the formatter, not the DOM.** Frontend tests on pure functions (format.ts) are fast and stable. Testing DOM manipulation or Tauri IPC requires a browser/runtime harness that's high-effort and fragile.
6. **Wiremock for integration tests.** The scheduler poll functions were made testable by adding a `base_url` parameter (same pattern as the provider fetchers). This one-line refactor unlocked 15 integration tests covering auth failures, stale data retention, and corrupt credentials — all without a Tauri runtime.
7. **`softprops/action-gh-release@v2`** with `generate_release_notes: true` gives you a clean changelog from PR titles for free. Conventional commit PR titles make this look professional.
