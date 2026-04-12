// Pure formatting helpers for the usage-meter widget.
//
// Extracted into their own module so they can be unit-tested
// without importing Tauri APIs or touching the DOM.

export interface UsageWindow {
  used_percent: number;
  resets_at: string; // ISO 8601
  window_seconds: number;
}

export function formatPercent(window: UsageWindow | null): string {
  if (!window) return "—";
  return `${Math.round(window.used_percent)}%`;
}

export function formatResetsIn(
  window: UsageWindow | null,
  now?: number,
): string {
  if (!window?.resets_at) return "";
  const currentTime = now ?? Date.now();
  const resets = new Date(window.resets_at).getTime();
  const diffMs = resets - currentTime;
  if (diffMs <= 0) return "resetting…";

  const mins = Math.floor(diffMs / 60_000);
  if (mins < 60) return `${mins}m`;
  const hrs = Math.floor(mins / 60);
  const remMins = mins % 60;
  if (hrs < 24) return remMins > 0 ? `${hrs}h ${remMins}m` : `${hrs}h`;
  const days = Math.floor(hrs / 24);
  const remHrs = hrs % 24;
  return remHrs > 0 ? `${days}d ${remHrs}h` : `${days}d`;
}
