// usage-meter — frontend entry point.
//
// Two views: the usage display (default) and a settings panel for
// connecting/disconnecting provider accounts via cookie paste.
// Subscribes to `usage-update` events pushed by the Rust scheduler
// and also pulls the latest snapshot on mount via `get_usage`.

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

// --- State ---

let lastUpdate: UsageUpdate = { claude: null, chatgpt: null };

// --- DOM helpers ---

function $(id: string): HTMLElement | null {
  return document.getElementById(id);
}

function formatPercent(window: UsageWindow | null): string {
  if (!window) return "—";
  return `${Math.round(window.used_percent)}%`;
}

function renderUpdate(update: UsageUpdate): void {
  lastUpdate = update;

  const c5h = $("claude-5h");
  const cw = $("claude-weekly");
  const g5h = $("chatgpt-5h");
  const gw = $("chatgpt-weekly");
  const status = $("status");

  if (c5h) c5h.textContent = formatPercent(update.claude?.five_hour ?? null);
  if (cw) cw.textContent = formatPercent(update.claude?.weekly ?? null);
  if (g5h) g5h.textContent = formatPercent(update.chatgpt?.five_hour ?? null);
  if (gw) gw.textContent = formatPercent(update.chatgpt?.weekly ?? null);

  if (status) {
    const hasAny = update.claude || update.chatgpt;
    status.textContent = hasAny ? "" : "no accounts connected";
    status.style.display = hasAny ? "none" : "";
  }

  // Sync settings panel auth indicators.
  syncAuthUI(update);
}

function syncAuthUI(update: UsageUpdate): void {
  const claudeConnected = $("claude-connected");
  const claudeForm = $("claude-form");
  const chatgptConnected = $("chatgpt-connected");
  const chatgptForm = $("chatgpt-form");

  if (claudeConnected && claudeForm) {
    claudeConnected.style.display = update.claude ? "" : "none";
    claudeForm.style.display = update.claude ? "none" : "";
  }
  if (chatgptConnected && chatgptForm) {
    chatgptConnected.style.display = update.chatgpt ? "" : "none";
    chatgptForm.style.display = update.chatgpt ? "none" : "";
  }
}

// --- View toggling ---

function showView(view: "usage" | "settings"): void {
  const usageView = $("usage-view");
  const settingsView = $("settings-view");
  if (usageView) usageView.style.display = view === "usage" ? "" : "none";
  if (settingsView)
    settingsView.style.display = view === "settings" ? "" : "none";

  if (view === "settings") syncAuthUI(lastUpdate);
}

// --- Connect / disconnect ---

async function connectProvider(
  provider: "claude" | "chatgpt",
  cookie: string,
): Promise<void> {
  const errorEl = $(`${provider}-error`);
  const btn = $(
    `${provider}-connect-btn`,
  ) as HTMLButtonElement | null;

  if (!cookie.trim()) {
    if (errorEl) errorEl.textContent = "Cookie cannot be empty.";
    return;
  }

  if (errorEl) errorEl.textContent = "";
  if (btn) {
    btn.disabled = true;
    btn.textContent = "Connecting…";
  }

  try {
    const command = provider === "claude" ? "connect_claude" : "connect_chatgpt";
    const update = await invoke<UsageUpdate>(command, { cookie: cookie.trim() });
    renderUpdate(update);

    // Clear the input on success.
    const input = $(`${provider}-cookie`) as HTMLInputElement | null;
    if (input) input.value = "";
  } catch (e) {
    if (errorEl) errorEl.textContent = String(e);
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = "Connect";
    }
  }
}

async function disconnectProvider(provider: string): Promise<void> {
  try {
    await invoke("disconnect", { provider });
    const update = await invoke<UsageUpdate>("get_usage");
    renderUpdate(update);
  } catch (e) {
    const errorEl = $(`${provider}-error`);
    if (errorEl) errorEl.textContent = String(e);
  }
}

// --- Init ---

window.addEventListener("DOMContentLoaded", async () => {
  // View toggling.
  $("open-settings")?.addEventListener("click", () => showView("settings"));
  $("close-settings")?.addEventListener("click", () => showView("usage"));

  // Connect buttons.
  $("claude-connect-btn")?.addEventListener("click", () => {
    const input = $("claude-cookie") as HTMLInputElement | null;
    if (input) connectProvider("claude", input.value);
  });
  $("chatgpt-connect-btn")?.addEventListener("click", () => {
    const input = $("chatgpt-cookie") as HTMLInputElement | null;
    if (input) connectProvider("chatgpt", input.value);
  });

  // Disconnect buttons.
  document.querySelectorAll(".btn-disconnect").forEach((btn) => {
    btn.addEventListener("click", () => {
      const provider = (btn as HTMLElement).dataset.provider;
      if (provider) disconnectProvider(provider);
    });
  });

  // Subscribe to push events from the scheduler.
  await listen<UsageUpdate>("usage-update", (event) => {
    renderUpdate(event.payload);
  });

  // Pull current state in case the scheduler already ran.
  try {
    const current = await invoke<UsageUpdate>("get_usage");
    renderUpdate(current);
  } catch {
    // Scheduler may not have run yet — the listen above will catch it.
  }
});
