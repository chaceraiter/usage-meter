// usage-meter — frontend entry point.
//
// Two views: the usage display (default) and a settings panel for
// connecting/disconnecting provider accounts. Primary auth uses an
// embedded webview sign-in flow; cookie paste is available as fallback.

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

  syncAuthUI(update);
}

function syncAuthUI(update: UsageUpdate): void {
  syncProviderAuth("claude", !!update.claude);
  syncProviderAuth("chatgpt", !!update.chatgpt);
}

function syncProviderAuth(provider: string, connected: boolean): void {
  const connectedEl = $(`${provider}-connected`);
  const disconnectedEl = $(`${provider}-disconnected`);
  const formEl = $(`${provider}-form`);

  if (connectedEl) connectedEl.style.display = connected ? "" : "none";
  if (disconnectedEl) disconnectedEl.style.display = connected ? "none" : "";
  if (formEl && connected) formEl.style.display = "none";
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

// --- Webview sign-in ---

async function openSignIn(provider: string): Promise<void> {
  const errorEl = $(`${provider}-error`);
  if (errorEl) errorEl.textContent = "";

  try {
    await invoke("open_auth_window", { provider });
  } catch (e) {
    if (errorEl) errorEl.textContent = String(e);
  }
}

// --- Cookie-paste fallback ---

async function connectProvider(
  provider: "claude" | "chatgpt",
  cookie: string,
): Promise<void> {
  const errorEl = $(`${provider}-error`);
  const btn = $(`${provider}-connect-btn`) as HTMLButtonElement | null;

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
    const command =
      provider === "claude" ? "connect_claude" : "connect_chatgpt";
    const update = await invoke<UsageUpdate>(command, {
      cookie: cookie.trim(),
    });
    renderUpdate(update);

    const input = $(`${provider}-cookie`) as HTMLInputElement | null;
    if (input) input.value = "";
    const form = $(`${provider}-form`);
    if (form) form.style.display = "none";
  } catch (e) {
    if (errorEl) errorEl.textContent = String(e);
  } finally {
    if (btn) {
      btn.disabled = false;
      btn.textContent = "Save";
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

  // Sign-in buttons (webview auth).
  document.querySelectorAll(".btn-signin").forEach((btn) => {
    btn.addEventListener("click", () => {
      const provider = (btn as HTMLElement).dataset.provider;
      if (provider) openSignIn(provider);
    });
  });

  // Fallback toggle — show/hide cookie paste form.
  document.querySelectorAll(".btn-fallback-toggle").forEach((btn) => {
    btn.addEventListener("click", () => {
      const provider = (btn as HTMLElement).dataset.provider;
      if (!provider) return;
      const form = $(`${provider}-form`);
      if (form) {
        form.style.display = form.style.display === "none" ? "" : "none";
      }
    });
  });

  // Cookie-paste connect buttons.
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

  // Listen for auth completion from the webview flow.
  await listen<UsageUpdate>("auth-complete", (event) => {
    renderUpdate(event.payload);
    showView("usage");
  });

  await listen<string>("auth-error", (event) => {
    // Show error in whichever provider section is relevant.
    // For now, show in both — the user knows which one they tried.
    const claudeErr = $("claude-error");
    const chatgptErr = $("chatgpt-error");
    if (claudeErr) claudeErr.textContent = event.payload;
    if (chatgptErr) chatgptErr.textContent = event.payload;
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
