# Contributing to usage-meter

Thanks for your interest. This document is the source of truth for how we work on this project.

## Development setup

Prereqs:
- **Rust** (stable, via [rustup](https://rustup.rs/))
- **Node.js** 20+
- **pnpm** (`npm i -g pnpm`)
- **Tauri** system deps — see the [Tauri prereqs guide](https://tauri.app/start/prerequisites/)

Then:

```bash
git clone https://github.com/chaceraiter/usage-meter.git
cd usage-meter
pnpm install
pnpm tauri dev
```

## Branch strategy

- `main` is protected. Direct pushes are disabled; all changes go through PRs with passing CI.
- Work happens on short-lived feature branches: `feat/<slug>`, `fix/<slug>`, `chore/<slug>`, `docs/<slug>`.
- Squash-merge PRs into `main` with a clean conventional-commit title.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add menu-bar tray toggle
fix(scraper): handle 429 from claude.ai
chore(deps): bump serde to 1.0.210
docs: clarify cookie capture flow
```

Allowed types: `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `ci`, `perf`, `build`.

## Code quality

All of the following run automatically via pre-commit hooks and again in CI:

- **Rust**: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test`
- **Frontend**: `pnpm lint`, `pnpm format:check`, `pnpm test`

Set up pre-commit hooks locally once with:

```bash
pnpm install           # wires up lefthook via postinstall
```

## Tests

- **Unit tests** live alongside source (`src/**/*.test.ts`, `src/**/mod.rs` with `#[cfg(test)]`).
- **Scraper/parser logic** must have unit tests — these are the most fragile parts of the system.
- **Integration tests** for end-to-end cookie → fetch → parse flow live in `tests/`.

Run locally:

```bash
cargo test                # Rust
pnpm test                 # Frontend
```

## Pull requests

Before opening a PR:

1. Rebase onto latest `main`.
2. Ensure `cargo fmt`, `cargo clippy`, `cargo test`, and `pnpm lint && pnpm test` all pass.
3. Update `CHANGELOG.md` under `## [Unreleased]` with a line describing your change.
4. If the change affects architecture or security, update `docs/ARCHITECTURE.md` and/or `SECURITY.md`.

PR description should answer:
- **What** does this change?
- **Why** is this change needed?
- **How** was it tested?

## Reporting bugs / requesting features

Use the issue templates in `.github/ISSUE_TEMPLATE/`.

## Security

Never commit real session cookies, `.env` files, or anything in `ai-context-management/` (private working notes, gitignored). If you believe you've found a security issue, follow the disclosure process in [`SECURITY.md`](SECURITY.md) instead of opening a public issue.

## Code of conduct

See [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md). Be kind; assume good faith.
