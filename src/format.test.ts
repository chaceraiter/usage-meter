import { describe, expect, it } from "vitest";
import { formatPercent, formatResetsIn, type UsageWindow } from "./format";

function window(overrides: Partial<UsageWindow> = {}): UsageWindow {
  return {
    used_percent: 50,
    resets_at: new Date(Date.now() + 3_600_000).toISOString(), // +1h
    window_seconds: 18_000,
    ...overrides,
  };
}

describe("formatPercent", () => {
  it("returns dash for null", () => {
    expect(formatPercent(null)).toBe("—");
  });

  it("rounds to nearest integer", () => {
    expect(formatPercent(window({ used_percent: 42.4 }))).toBe("42%");
    expect(formatPercent(window({ used_percent: 42.5 }))).toBe("43%");
  });

  it("handles 0%", () => {
    expect(formatPercent(window({ used_percent: 0 }))).toBe("0%");
  });

  it("handles 100%", () => {
    expect(formatPercent(window({ used_percent: 100 }))).toBe("100%");
  });
});

describe("formatResetsIn", () => {
  // Use a fixed "now" so tests are deterministic.
  const now = new Date("2026-04-12T12:00:00Z").getTime();

  it("returns empty string for null window", () => {
    expect(formatResetsIn(null, now)).toBe("");
  });

  it('returns "resetting…" when reset time is in the past', () => {
    const w = window({
      resets_at: new Date("2026-04-12T11:59:00Z").toISOString(),
    });
    expect(formatResetsIn(w, now)).toBe("resetting…");
  });

  it('returns "resetting…" when reset time is exactly now', () => {
    const w = window({
      resets_at: new Date("2026-04-12T12:00:00Z").toISOString(),
    });
    expect(formatResetsIn(w, now)).toBe("resetting…");
  });

  it("formats minutes only when under 1 hour", () => {
    const w = window({
      resets_at: new Date("2026-04-12T12:30:00Z").toISOString(),
    });
    expect(formatResetsIn(w, now)).toBe("30m");
  });

  it("formats hours and minutes", () => {
    const w = window({
      resets_at: new Date("2026-04-12T14:15:00Z").toISOString(),
    });
    expect(formatResetsIn(w, now)).toBe("2h 15m");
  });

  it("formats exact hours without minutes", () => {
    const w = window({
      resets_at: new Date("2026-04-12T15:00:00Z").toISOString(),
    });
    expect(formatResetsIn(w, now)).toBe("3h");
  });

  it("formats days and hours", () => {
    const w = window({
      resets_at: new Date("2026-04-14T18:00:00Z").toISOString(),
    });
    expect(formatResetsIn(w, now)).toBe("2d 6h");
  });

  it("formats exact days without hours", () => {
    const w = window({
      resets_at: new Date("2026-04-15T12:00:00Z").toISOString(),
    });
    expect(formatResetsIn(w, now)).toBe("3d");
  });

  it("handles 0 minutes (just crossed the hour)", () => {
    // 1 hour exactly
    const w = window({
      resets_at: new Date("2026-04-12T13:00:00Z").toISOString(),
    });
    expect(formatResetsIn(w, now)).toBe("1h");
  });
});
