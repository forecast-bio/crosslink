// Applies the current theme preference to <html>. Separate from the
// preferences store so the DOM side-effect stays out of the store
// (keeps the store pure + easy to test in isolation).
//
// - "dark"   → remove .theme-light
// - "light"  → add .theme-light
// - "system" → follow `prefers-color-scheme`; update live when the OS
//              flips (macOS auto light/dark at sunset, etc.)

import { useEffect } from "react";

import {
  subscribePreferences,
  getPreferences,
  type ThemePreference,
} from "./preferences";

const LIGHT_CLASS = "theme-light";

/// Apply one concrete theme setting (not "system"). Idempotent; safe
/// to call with the currently-applied value.
function applyResolved(resolved: "light" | "dark"): void {
  if (typeof document === "undefined") return;
  const el = document.documentElement;
  if (resolved === "light") el.classList.add(LIGHT_CLASS);
  else el.classList.remove(LIGHT_CLASS);
}

function prefersDark(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return true;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/// Resolve a preference into the concrete palette that should render.
export function resolveTheme(pref: ThemePreference): "light" | "dark" {
  if (pref === "system") return prefersDark() ? "dark" : "light";
  return pref;
}

/// Install a persistent theme observer. Re-reads preferences from the
/// store on change, and — when in "system" mode — re-evaluates on
/// OS-level colour-scheme changes. Returns a disposer.
export function installThemeObserver(): () => void {
  let disposed = false;
  let mediaListener: ((e: MediaQueryListEvent) => void) | null = null;
  let mediaQuery: MediaQueryList | null = null;

  const apply = () => {
    if (disposed) return;
    const prefs = getPreferences();
    applyResolved(resolveTheme(prefs.theme));

    // Keep a media-query listener active only when we're in "system"
    // mode; otherwise a user flip in the OS would overwrite an
    // explicit dark/light choice.
    const wantListener = prefs.theme === "system";
    if (wantListener && !mediaListener && typeof window !== "undefined" && window.matchMedia) {
      mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
      mediaListener = () => {
        if (getPreferences().theme === "system") {
          applyResolved(prefersDark() ? "dark" : "light");
        }
      };
      // Older Safari used addListener/removeListener; addEventListener
      // is the modern path and all currently-supported browsers have
      // it, so we don't fall back.
      mediaQuery.addEventListener("change", mediaListener);
    } else if (!wantListener && mediaListener && mediaQuery) {
      mediaQuery.removeEventListener("change", mediaListener);
      mediaListener = null;
      mediaQuery = null;
    }
  };

  apply();
  const unsub = subscribePreferences(apply);
  return () => {
    disposed = true;
    unsub();
    if (mediaListener && mediaQuery) {
      mediaQuery.removeEventListener("change", mediaListener);
    }
  };
}

/// React bridge: mounts the theme observer for the component tree's
/// lifetime. Render once near the app root.
export function useThemeBridge(): void {
  useEffect(() => installThemeObserver(), []);
}
