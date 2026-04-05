# Security Policy

## Reporting a vulnerability

If you believe you've found a security issue in usage-meter, **please do not open a public GitHub issue**. Instead, email the maintainer directly or use GitHub's [private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability) feature on this repository.

I'll acknowledge your report within 7 days and work with you on a fix and disclosure timeline.

## How usage-meter handles your credentials

usage-meter needs your `claude.ai` and `chatgpt.com` session cookies to query the same internal endpoints those sites use to render their own usage displays. This is sensitive. Here is exactly what happens to those cookies:

### Storage

- Session cookies are stored in the **macOS Keychain** (via the [`keyring`](https://crates.io/crates/keyring) crate on macOS, backed by `Security.framework`).
- They are **never** written to plain-text config files, logs, or the context-management folder.
- The app reads them only at fetch time and holds them in memory only as long as a request is in flight.

### Transmission

- Cookies are only ever sent to the original services (`claude.ai`, `chatgpt.com`) over HTTPS.
- There is **no telemetry, analytics, or third-party network call** in this app. Not now, not ever. If that changes it will require a major version bump and an explicit opt-in.

### Scope

- usage-meter runs entirely on your local machine. There is no backend server that sees your credentials.
- If a future version adds optional multi-device sync, it will be self-hosted and end-to-end encrypted, and will be clearly documented before release.

### Your control

- You can delete the stored cookies at any time from the app's settings, or directly from Keychain Access.
- Uninstalling the app removes the keychain entries.

## Threat model

See the "Threat model" section of [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for a deeper discussion of what this project does and does not protect against.

## Terms-of-service note

usage-meter reads your own account's usage data via the same endpoints your browser uses when you visit the settings page. This is a personal-use monitoring tool. If the providers' terms of service prohibit this kind of access, you are responsible for complying with them. The maintainer provides this tool for personal use and takes no responsibility for account actions taken by third parties.
