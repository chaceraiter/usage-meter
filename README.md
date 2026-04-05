# usage-meter

> A floating desktop widget that tracks your Claude and ChatGPT Codex subscription usage across all devices — at the account level, not per-machine.

<!-- Badges (added once CI is set up)
![build](https://github.com/chaceraiter/usage-meter/actions/workflows/ci.yml/badge.svg)
![license](https://img.shields.io/github/license/chaceraiter/usage-meter)
![release](https://img.shields.io/github/v/release/chaceraiter/usage-meter)
-->

## The problem

Claude (Pro/Max) and ChatGPT (Plus with Codex) both enforce rolling usage limits — 5-hour windows and weekly caps — but neither surfaces that data in a way you can monitor at a glance. If you use these tools across multiple machines (laptop, desktop, work) the per-device trackers on the market miss the picture entirely. The limits are enforced per account, so the meter should be too.

## What it does

- **Floating, always-on-top widget** showing live 5-hour and weekly usage percentages for both services
- **Optional menu-bar icon** for a more minimal footprint
- **Account-level aggregation** — pulls from the same endpoints the web UIs use, so multi-device usage is captured correctly
- **Local-only** — your session cookies never leave your machine, stored in the OS keychain

## Screenshots

_Coming soon once the UI is built._

## Install

_Coming soon. Once v0.1 ships there will be a signed `.dmg` in [Releases](https://github.com/chaceraiter/usage-meter/releases)._

### Build from source

```bash
# Prereqs: Rust, Node 20+, pnpm
git clone https://github.com/chaceraiter/usage-meter.git
cd usage-meter
pnpm install
pnpm tauri dev
```

## How it works

usage-meter talks directly to the internal endpoints that `claude.ai` and `chatgpt.com` use to render their own usage displays. It authenticates using session cookies you provide once (stored in the macOS Keychain), polls on a configurable interval, and renders the result in a small always-on-top window or menu-bar item.

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full design, including the threat model for cookie handling.

## Roadmap

- [ ] Spike: confirm Claude + ChatGPT internal usage endpoints
- [ ] Cookie capture + Keychain storage
- [ ] Polling + parsing layer
- [ ] Tauri floating widget UI
- [ ] Menu-bar tray
- [ ] Signed macOS release
- [ ] Cross-platform (Linux, Windows)
- [ ] Self-hostable sync (optional, for truly shared monitoring across people)

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Security

See [`SECURITY.md`](SECURITY.md) for the disclosure policy and a description of how session cookies are handled.

## License

[MIT](LICENSE)
