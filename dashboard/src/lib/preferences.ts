// User preference store for the dashboard (design doc §14 Phase 5 —
// polish).
//
// Persists a small set of UI preferences (theme, audible alerts) to
// localStorage so they survive reload. Exposes a React hook that
// subscribes via `useSyncExternalStore` — no React Query / Zustand
// needed for a handful of flags.
//
// Single module-level value + subscriber set: any component can read
// the current prefs, subscribe to changes, and mutate via `setPref`.
// Writes broadcast to all subscribers synchronously.

import { useSyncExternalStore } from "react";

import type { AlertSeverity } from "@/api/types";

const STORAGE_KEY = "crosslink_dashboard_prefs";

export type ThemePreference = "light" | "dark" | "system";

export interface Preferences {
  theme: ThemePreference;
  audibleEnabled: boolean;
  /// Severities that fire an audible tone when an alert opens.
  /// Only consulted when `audibleEnabled` is true.
  audibleSeverities: AlertSeverity[];
}

export const DEFAULT_PREFERENCES: Preferences = {
  theme: "system",
  audibleEnabled: false,
  audibleSeverities: ["critical"],
};

function readStorage(): Preferences {
  if (typeof window === "undefined") return DEFAULT_PREFERENCES;
  let raw: string | null = null;
  try {
    raw = window.localStorage.getItem(STORAGE_KEY);
  } catch {
    return DEFAULT_PREFERENCES;
  }
  if (!raw) return DEFAULT_PREFERENCES;
  try {
    const parsed = JSON.parse(raw) as Partial<Preferences>;
    return {
      theme: isThemePreference(parsed.theme) ? parsed.theme : DEFAULT_PREFERENCES.theme,
      audibleEnabled:
        typeof parsed.audibleEnabled === "boolean"
          ? parsed.audibleEnabled
          : DEFAULT_PREFERENCES.audibleEnabled,
      audibleSeverities: sanitizeSeverities(parsed.audibleSeverities),
    };
  } catch {
    return DEFAULT_PREFERENCES;
  }
}

function isThemePreference(v: unknown): v is ThemePreference {
  return v === "light" || v === "dark" || v === "system";
}

function sanitizeSeverities(v: unknown): AlertSeverity[] {
  if (!Array.isArray(v)) return DEFAULT_PREFERENCES.audibleSeverities;
  const allowed: AlertSeverity[] = ["info", "warning", "critical"];
  const seen = new Set<AlertSeverity>();
  for (const item of v) {
    if (allowed.includes(item as AlertSeverity)) {
      seen.add(item as AlertSeverity);
    }
  }
  return allowed.filter((s) => seen.has(s));
}

let current: Preferences = readStorage();
const subscribers = new Set<() => void>();

function writeStorage(next: Preferences): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(next));
  } catch {
    // Storage unavailable (private mode, sandboxed iframe). Keep the
    // in-memory state so the UI still reflects the toggle.
  }
}

/// Replace the stored preferences wholesale. Broadcasts to subscribers.
export function setPreferences(next: Preferences): void {
  current = {
    theme: next.theme,
    audibleEnabled: next.audibleEnabled,
    // Copy so callers can't mutate the stored array.
    audibleSeverities: [...next.audibleSeverities],
  };
  writeStorage(current);
  for (const cb of subscribers) cb();
}

/// Merge a partial update into the stored preferences.
export function patchPreferences(patch: Partial<Preferences>): void {
  setPreferences({ ...current, ...patch });
}

/// Read the current preferences synchronously (outside React).
export function getPreferences(): Preferences {
  return current;
}

/// Subscribe to preference changes. Returns an unsubscribe function.
export function subscribePreferences(cb: () => void): () => void {
  subscribers.add(cb);
  return () => {
    subscribers.delete(cb);
  };
}

/// React hook: re-render when preferences change, returning the
/// current value. Stable identity between renders when nothing changed.
export function usePreferences(): Preferences {
  return useSyncExternalStore(subscribePreferences, getPreferences, getPreferences);
}

/// Test-only: reset to defaults and drop subscribers. Exposed so
/// vitest specs can isolate themselves without global leakage.
export function __resetForTests(): void {
  current = { ...DEFAULT_PREFERENCES, audibleSeverities: [...DEFAULT_PREFERENCES.audibleSeverities] };
  subscribers.clear();
  if (typeof window !== "undefined") {
    try {
      window.localStorage.removeItem(STORAGE_KEY);
    } catch {
      // Ignore storage failures in tests.
    }
  }
}
