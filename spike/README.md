# Endpoint Discovery Spike

One-time dev tooling for finding the internal usage endpoints that `claude.ai` and `chatgpt.com` call when they render the 5-hour / weekly usage display in settings.

**This is not part of the shipped app.** It's a throwaway script I run once, document the findings in `docs/ARCHITECTURE.md`, and then the Rust scraper hits those endpoints directly.

## How it works

`capture.mjs` launches a headed Chromium via Playwright, opens one provider's site, and records every network request/response. You log in normally, navigate to the settings/usage page, and the script dumps everything it sees into `output/<provider>-<timestamp>.json`.

Then we grep the output for the URLs that return JSON containing usage-related fields (`limit`, `quota`, `usage`, `five_hour`, etc.) and document those as the target endpoints.

## Usage

```bash
cd spike
pnpm install
pnpm exec playwright install chromium   # first time only

# Claude
node capture.mjs claude

# ChatGPT
node capture.mjs chatgpt
```

Log in when the browser opens. Navigate to the settings/usage page. Do whatever you'd do to see the usage display refresh. When you're done, close the browser window and the script writes its report.

## Outputs

- `output/<provider>-<iso-timestamp>.json` — full network log: URL, method, status, headers (redacted), response body for JSON content types
- `output/<provider>-<iso-timestamp>-summary.txt` — short list of promising endpoints (URLs whose response body contains usage-related keywords)

**`spike/output/` is gitignored.** Response bodies will contain cookies and session data. Never commit them.

## After the spike

- Document confirmed endpoints in `docs/ARCHITECTURE.md` under "Provider endpoints"
- Delete `spike/output/`
- Keep `spike/` itself in the repo as a reproducible artifact — if the endpoints change later, re-run the spike
