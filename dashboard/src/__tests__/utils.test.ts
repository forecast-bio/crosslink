import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { cn, formatRelativeTime, formatDateTime, isStale } from "../lib/utils";

describe("cn (class name merger)", () => {
  it("merges multiple class strings", () => {
    expect(cn("foo", "bar")).toBe("foo bar");
  });

  it("resolves tailwind conflicts (last wins)", () => {
    // tailwind-merge keeps the last of conflicting utilities
    const result = cn("p-2", "p-4");
    expect(result).toBe("p-4");
  });

  it("ignores falsy values", () => {
    expect(cn("foo", false && "bar", undefined, null, "baz")).toBe("foo baz");
  });
});

describe("formatRelativeTime", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("formats seconds ago", () => {
    const now = new Date("2024-01-01T12:00:30Z");
    vi.setSystemTime(now);
    const past = new Date("2024-01-01T12:00:00Z").toISOString();
    expect(formatRelativeTime(past)).toBe("30s ago");
  });

  it("formats minutes ago", () => {
    const now = new Date("2024-01-01T12:05:00Z");
    vi.setSystemTime(now);
    const past = new Date("2024-01-01T12:00:00Z").toISOString();
    expect(formatRelativeTime(past)).toBe("5m ago");
  });

  it("formats hours ago", () => {
    const now = new Date("2024-01-01T15:00:00Z");
    vi.setSystemTime(now);
    const past = new Date("2024-01-01T12:00:00Z").toISOString();
    expect(formatRelativeTime(past)).toBe("3h ago");
  });

  it("formats days ago", () => {
    const now = new Date("2024-01-04T12:00:00Z");
    vi.setSystemTime(now);
    const past = new Date("2024-01-01T12:00:00Z").toISOString();
    expect(formatRelativeTime(past)).toBe("3d ago");
  });
});

describe("isStale", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns false when within threshold", () => {
    const now = new Date("2024-01-01T12:01:00Z");
    vi.setSystemTime(now);
    const recent = new Date("2024-01-01T12:00:00Z").toISOString();
    expect(isStale(recent, 120_000)).toBe(false);
  });

  it("returns true when past threshold", () => {
    const now = new Date("2024-01-01T12:05:00Z");
    vi.setSystemTime(now);
    const old = new Date("2024-01-01T12:00:00Z").toISOString();
    expect(isStale(old, 120_000)).toBe(true);
  });
});
