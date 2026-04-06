// usage-meter — frontend entry point.
//
// Subscribes to `usage-update` events pushed by the Rust scheduler
// and also pulls the latest snapshot on mount via `get_usage`. This
// dual approach means the UI is populated immediately if the
// scheduler has already fetched, and stays up-to-date as new polls
// land.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// --- Types mirroring the Rust model ---

interface UsageWindow {
  used_percent: number;
  resets_at: string; // ISO 8601
  window_seconds: number;
}

interface UsageSnapshot {
  five_hour: UsageWindow | null;
  weekly: UsageWindow | null;
  fetched_at: string;
  extras: { kind: string };
}

interface UsageUpdate {
  claude: UsageSnapshot | null;
  chatgpt: UsageSnapshot | null;
}

// --- DOM helpers ---

function formatPercent(window: UsageWindow | null): string {
  if (!window) return "—";
  return `${Math.round(window.used_percent)}%`;
}

function renderUpdate(update: UsageUpdate): void {
  const c5h = document.getElementById("claude-5h");
  const cw = document.getElementById("claude-weekly");
  const g5h = document.getElementById("chatgpt-5h");
  const gw = document.getElementById("chatgpt-weekly");
  const status = document.getElementById("status");

  if (c5h) c5h.textContent = formatPercent(update.claude?.five_hour ?? null);
  if (cw) cw.textContent = formatPercent(update.claude?.weekly ?? null);
  if (g5h) g5h.textContent = formatPercent(update.chatgpt?.five_hour ?? null);
  if (gw) gw.textContent = formatPercent(update.chatgpt?.weekly ?? null);

  if (status) {
    const hasAny = update.claude || update.chatgpt;
    status.textContent = hasAny ? "" : "no accounts connected";
    status.style.display = hasAny ? "none" : "";
  }
}

// --- Init ---

window.addEventListener("DOMContentLoaded", async () => {
  // Subscribe to push events from the scheduler.
  await listen<UsageUpdate>("usage-update", (event) => {
    renderUpdate(event.payload);
  });

  // Pull current state in case the scheduler already ran before
  // the frontend mounted (race on cold start).
  try {
    const current = await invoke<UsageUpdate>("get_usage");
    renderUpdate(current);
  } catch {
    // Scheduler may not have run yet — the listen above will catch it.
  }
});
