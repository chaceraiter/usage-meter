// usage-meter — frontend entry point.
//
// Current state: scaffolding only. Calls the placeholder `app_info`
// command to prove IPC works end-to-end. Real usage rendering will
// land in subsequent PRs.

import { invoke } from "@tauri-apps/api/core";

interface AppInfo {
  name: string;
  version: string;
}

window.addEventListener("DOMContentLoaded", async () => {
  const statusEl = document.querySelector<HTMLElement>("#status");
  if (!statusEl) return;

  try {
    const info = await invoke<AppInfo>("app_info");
    statusEl.textContent = `${info.name} v${info.version}`;
  } catch (err) {
    statusEl.textContent = `error: ${String(err)}`;
  }
});
