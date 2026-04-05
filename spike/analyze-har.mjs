// HAR analyzer for endpoint discovery. Reads a HAR file, finds requests
// whose response body (JSON) contains usage-related keywords, and prints
// a safe report: URLs, field paths, value types, but never raw values.
//
// Usage: node analyze-har.mjs <path-to-har>

import { readFile } from 'node:fs/promises';

const USAGE_KEYWORDS = [
  'usage',
  'limit',
  'quota',
  'rate_limit',
  'rateLimit',
  'five_hour',
  'fiveHour',
  'weekly',
  'remaining',
  'resets_at',
  'resetsAt',
  'reset_at',
  'resetAt',
  'tokens_used',
  'tokensUsed',
  'messages_used',
  'messagesUsed',
  'cap',
  'allowance',
  'consumed',
];

function keywordHit(key) {
  const lower = key.toLowerCase();
  return USAGE_KEYWORDS.some((kw) => lower.includes(kw.toLowerCase()));
}

/**
 * Walk a JSON value and return an array of { path, type } for keys whose
 * name matches a usage keyword. Never includes the value itself.
 */
function findUsageFields(obj, path = '', results = []) {
  if (obj === null || typeof obj !== 'object') return results;

  if (Array.isArray(obj)) {
    // Sample the first element to discover shape; note it's an array
    if (obj.length > 0) {
      findUsageFields(obj[0], `${path}[0]`, results);
    }
    return results;
  }

  for (const [key, value] of Object.entries(obj)) {
    const childPath = path ? `${path}.${key}` : key;
    if (keywordHit(key)) {
      results.push({
        path: childPath,
        type: Array.isArray(value) ? 'array' : typeof value,
      });
    }
    if (value && typeof value === 'object') {
      findUsageFields(value, childPath, results);
    }
  }

  return results;
}

async function main() {
  const harPath = process.argv[2];
  if (!harPath) {
    console.error('Usage: node analyze-har.mjs <path-to-har>');
    process.exit(1);
  }

  const raw = await readFile(harPath, 'utf8');
  const har = JSON.parse(raw);
  const entries = har.log?.entries ?? [];
  console.log(`Loaded HAR: ${harPath}`);
  console.log(`Total entries: ${entries.length}\n`);

  const jsonEntries = [];
  const flagged = [];

  for (const entry of entries) {
    const req = entry.request;
    const res = entry.response;
    if (!req || !res) continue;

    const contentType = (res.content?.mimeType || '').toLowerCase();
    if (!contentType.includes('json')) continue;

    jsonEntries.push({
      method: req.method,
      url: req.url,
      status: res.status,
      size: res.content?.size ?? 0,
    });

    const text = res.content?.text;
    if (!text) continue;

    let parsed;
    try {
      parsed = JSON.parse(text);
    } catch {
      // body wasn't actually JSON (could be gzipped or truncated)
      continue;
    }

    const hits = findUsageFields(parsed);
    if (hits.length > 0) {
      flagged.push({
        method: req.method,
        url: req.url,
        status: res.status,
        hits,
      });
    }
  }

  console.log(`JSON responses: ${jsonEntries.length}`);
  console.log(`Flagged (response body contains usage-related field names): ${flagged.length}\n`);

  if (flagged.length === 0) {
    console.log('No usage-related fields found in any JSON response body.');
    console.log('Dumping all JSON endpoints for manual review:\n');
    for (const e of jsonEntries) {
      console.log(`  ${e.method} ${e.status} ${e.url}`);
    }
    return;
  }

  console.log('=== Flagged endpoints ===\n');
  for (const f of flagged) {
    console.log(`${f.method} ${f.status} ${f.url}`);
    for (const h of f.hits) {
      console.log(`    .${h.path}  (${h.type})`);
    }
    console.log('');
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
