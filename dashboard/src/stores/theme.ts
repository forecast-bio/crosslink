import { create } from "zustand";

/** Default HSL values for all theme CSS variables. */
export const THEME_DEFAULTS: Record<string, string> = {
  background: "224 71% 4%",
  foreground: "213 31% 91%",
  muted: "223 47% 11%",
  "muted-foreground": "215.4 16.3% 56.9%",
  accent: "216 34% 17%",
  "accent-foreground": "210 40% 98%",
  popover: "224 71% 4%",
  "popover-foreground": "215 20.2% 65.1%",
  border: "216 34% 17%",
  input: "216 34% 17%",
  card: "224 71% 4%",
  "card-foreground": "213 31% 91%",
  primary: "210 40% 98%",
  "primary-foreground": "222.2 47.4% 1.2%",
  secondary: "222.2 47.4% 11.2%",
  "secondary-foreground": "210 40% 98%",
  destructive: "0 63% 31%",
  "destructive-foreground": "210 40% 98%",
  ring: "216 34% 17%",
  "sidebar-background": "223 47% 7%",
  "sidebar-foreground": "213 31% 91%",
  "sidebar-border": "216 34% 14%",
  "sidebar-accent": "216 34% 14%",
  "sidebar-accent-foreground": "213 31% 91%",
  "sidebar-primary": "210 40% 98%",
  "sidebar-primary-foreground": "222.2 47.4% 1.2%",
  "sidebar-ring": "216 34% 17%",
};

/** Opacity defaults (0–100). */
export const OPACITY_DEFAULTS = {
  background: 100,
  card: 100,
  sidebar: 100,
  popover: 100,
};

const STORAGE_KEY = "crosslink-theme";

interface PersistedTheme {
  colors: Record<string, string>;
  opacity: Record<string, number>;
}

function loadFromStorage(): PersistedTheme {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw) return JSON.parse(raw) as PersistedTheme;
  } catch {
    // ignore
  }
  return { colors: {}, opacity: {} };
}

function saveToStorage(theme: PersistedTheme) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(theme));
}

interface ThemeState {
  /** Color overrides — only contains keys that differ from defaults. */
  colors: Record<string, string>;
  /** Opacity overrides (0–100) for background surfaces. */
  opacity: Record<string, number>;

  /** Get the effective value for a CSS variable (override or default). */
  getColor: (name: string) => string;
  /** Get the effective opacity for a surface (override or default). */
  getOpacity: (name: string) => number;

  /** Set a single color override. */
  setColor: (name: string, hsl: string) => void;
  /** Set opacity for a surface. */
  setOpacity: (name: string, value: number) => void;
  /** Reset everything to defaults. */
  reset: () => void;
  /** Reset a single color to its default. */
  resetColor: (name: string) => void;
}

export const useThemeStore = create<ThemeState>((set, get) => {
  const persisted = loadFromStorage();

  return {
    colors: persisted.colors,
    opacity: persisted.opacity,

    getColor: (name) => {
      return get().colors[name] ?? THEME_DEFAULTS[name] ?? "0 0% 0%";
    },

    getOpacity: (name) => {
      return get().opacity[name] ?? OPACITY_DEFAULTS[name as keyof typeof OPACITY_DEFAULTS] ?? 100;
    },

    setColor: (name, hsl) => {
      const colors = { ...get().colors, [name]: hsl };
      // Remove if same as default
      if (THEME_DEFAULTS[name] === hsl) delete colors[name];
      set({ colors });
      saveToStorage({ colors, opacity: get().opacity });
    },

    setOpacity: (name, value) => {
      const opacity = { ...get().opacity, [name]: value };
      const def = OPACITY_DEFAULTS[name as keyof typeof OPACITY_DEFAULTS];
      if (def !== undefined && def === value) delete opacity[name];
      set({ opacity });
      saveToStorage({ colors: get().colors, opacity });
    },

    reset: () => {
      set({ colors: {}, opacity: {} });
      localStorage.removeItem(STORAGE_KEY);
    },

    resetColor: (name) => {
      const colors = { ...get().colors };
      delete colors[name];
      set({ colors });
      saveToStorage({ colors, opacity: get().opacity });
    },
  };
});
