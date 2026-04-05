// Endpoint discovery spike — launches a headed browser, records every
// network request/response while the user logs in and navigates the
// settings page, then writes a full log plus a short summary of
// promising URLs.
//
// This is NOT part of the shipped app. See spike/README.md.

import { chromium } from 'playwright';
import { mkdir, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const OUTPUT_DIR = join(__dirname, 'output');

const PROVIDERS = {
  claude: {
    name: 'claude',
    startUrl: 'https://claude.ai/login',
    urlFilter: (url) => url.includes('claude.ai') || url.includes('anthropic'),
  },
  chatgpt: {
    name: 'chatgpt',
    startUrl: 'https://chatgpt.com',
    urlFilter: (url) => url.includes('chatgpt.com') || url.includes('openai.com'),
  },
};

// Keywords we expect to see in a response body that actually contains
// subscription usage data. Broad net on purpose — we refine by eye.
const USAGE_KEYWORDS = [
  'usage',
  'limit',
  'quota',
  'rate_limit',
  'five_hour',
  'weekly',
  'remaining',
  'resets_at',
  'reset_at',
  'tokens_used',
  'messages_used',
  'cap',
];

// Response headers we strip before writing anything to disk. We keep
// the output gitignored regardless, but belt-and-suspenders.
const SENSITIVE_HEADERS = new Set([
  'set-cookie',
  'cookie',
  'authorization',
  'x-api-key',
]);

function redactHeaders(headers) {
  const out = {};
  for (const [k, v] of Object.entries(headers)) {
    out[k] = SENSITIVE_HEADERS.has(k.toLowerCase()) ? '[REDACTED]' : v;
  }
  return out;
}

function looksLikeUsage(body) {
  if (!body) return false;
  const lower = body.toLowerCase();
  return USAGE_KEYWORDS.some((kw) => lower.includes(kw));
}

async function runCapture(providerKey) {
  const provider = PROVIDERS[providerKey];
  if (!provider) {
    console.error(`Unknown provider: ${providerKey}. Use one of: ${Object.keys(PROVIDERS).join(', ')}`);
    process.exit(1);
  }

  await mkdir(OUTPUT_DIR, { recursive: true });
  const timestamp = new Date().toISOString().replace(/[:.]/g, '-');
  const fullLogPath = join(OUTPUT_DIR, `${provider.name}-${timestamp}.json`);
  const summaryPath = join(OUTPUT_DIR, `${provider.name}-${timestamp}-summary.txt`);

  console.log(`\n=== usage-meter endpoint spike: ${provider.name} ===`);
  console.log(`Output: ${fullLogPath}`);
  console.log('Launching browser. Log in, navigate to the settings/usage page,');
  console.log('do whatever you normally do to see usage, then close the window.\n');

  const browser = await chromium.launch({ headless: false });
  const context = await browser.newContext();
  const page = await context.newPage();

  const records = [];

  page.on('requestfinished', async (request) => {
    const url = request.url();
    if (!provider.urlFilter(url)) return;

    try {
      const response = await request.response();
      if (!response) return;

      const contentType = (response.headers()['content-type'] || '').toLowerCase();
      let body = null;
      if (contentType.includes('json') || contentType.includes('text')) {
        try {
          body = await response.text();
          if (body.length > 200_000) {
            body = body.slice(0, 200_000) + '\n\n[TRUNCATED]';
          }
        } catch {
          body = null;
        }
      }

      records.push({
        method: request.method(),
        url,
        status: response.status(),
        contentType,
        requestHeaders: redactHeaders(request.headers()),
        responseHeaders: redactHeaders(response.headers()),
        body,
        flagged: looksLikeUsage(body),
      });
    } catch (err) {
      records.push({
        method: request.method(),
        url,
        error: String(err),
      });
    }
  });

  await page.goto(provider.startUrl);

  // Wait for the user to finish (close the browser manually).
  await new Promise((resolve) => {
    browser.on('disconnected', resolve);
  });

  console.log(`\nCaptured ${records.length} requests. Writing report...`);

  await writeFile(fullLogPath, JSON.stringify(records, null, 2), 'utf8');

  const flagged = records.filter((r) => r.flagged);
  const summaryLines = [
    `# ${provider.name} — endpoint discovery summary`,
    `# ${new Date().toISOString()}`,
    `# Total requests captured: ${records.length}`,
    `# Flagged (body contains usage-related keywords): ${flagged.length}`,
    '',
    '## Flagged endpoints',
    '',
    ...flagged.map((r) => `${r.method} ${r.status} ${r.url}`),
    '',
    '## All JSON endpoints (for manual review)',
    '',
    ...records
      .filter((r) => r.contentType && r.contentType.includes('json'))
      .map((r) => `${r.method} ${r.status} ${r.url}`),
  ];

  await writeFile(summaryPath, summaryLines.join('\n'), 'utf8');

  console.log(`Full log:  ${fullLogPath}`);
  console.log(`Summary:   ${summaryPath}`);
  console.log(`Flagged:   ${flagged.length} endpoint(s) contain usage-related keywords.`);
}

const providerArg = process.argv[2];
if (!providerArg) {
  console.error('Usage: node capture.mjs <claude|chatgpt>');
  process.exit(1);
}

runCapture(providerArg).catch((err) => {
  console.error(err);
  process.exit(1);
});
