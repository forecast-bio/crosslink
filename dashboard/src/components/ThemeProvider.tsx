import { useEffect } from "react";
import { useThemeStore, THEME_DEFAULTS, OPACITY_DEFAULTS } from "@/stores/theme";

/** Maps opacity store keys to the CSS variable names they affect. */
const OPACITY_TARGETS: Record<string, string[]> = {
  background: ["background"],
  card: ["card"],
  sidebar: ["sidebar-background"],
  popover: ["popover"],
};

/**
 * Applies theme CSS variable overrides to :root.
 * Renders nothing — mount once in App.
 */
export function ThemeProvider() {
  const colors = useThemeStore((s) => s.colors);
  const opacity = useThemeStore((s) => s.opacity);

  useEffect(() => {
    const root = document.documentElement;

    // Build a set of CSS vars that need alpha appended
    const alphaMap = new Map<string, number>();
    for (const [opKey, targets] of Object.entries(OPACITY_TARGETS)) {
      const alpha =
        opacity[opKey] ??
        OPACITY_DEFAULTS[opKey as keyof typeof OPACITY_DEFAULTS] ??
        100;
      if (alpha < 100) {
        for (const t of targets) alphaMap.set(t, alpha);
      }
    }

    // Apply all CSS variables
    for (const name of Object.keys(THEME_DEFAULTS)) {
      const value = colors[name] ?? THEME_DEFAULTS[name];
      const alpha = alphaMap.get(name);
      if (alpha !== undefined) {
        root.style.setProperty(`--${name}`, `${value} / ${alpha / 100}`);
      } else {
        root.style.setProperty(`--${name}`, value);
      }
    }

    return () => {
      // Clean up on unmount — restore defaults
      for (const name of Object.keys(THEME_DEFAULTS)) {
        root.style.setProperty(`--${name}`, THEME_DEFAULTS[name]);
      }
    };
  }, [colors, opacity]);

  return null;
}
