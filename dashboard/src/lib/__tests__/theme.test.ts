// Coverage for lib/theme — resolveTheme + installThemeObserver. We
// stub matchMedia because happy-dom/jsdom don't implement it
// realistically for colour-scheme queries.

import { beforeEach, describe, expect, it, vi } from "vitest";

import { __resetForTests, setPreferences } from "../preferences";
import { installThemeObserver, resolveTheme } from "../theme";

interface MediaQueryStub {
  matches: boolean;
  media: string;
  addEventListener: ReturnType<typeof vi.fn>;
  removeEventListener: ReturnType<typeof vi.fn>;
  dispatchEvent: (ev: MediaQueryListEvent) => boolean;
  onchange: ((e: MediaQueryListEvent) => void) | null;
  addListener: ReturnType<typeof vi.fn>;
  removeListener: ReturnType<typeof vi.fn>;
}

function stubMatchMedia(systemIsDark: boolean): {
  listeners: Set<(e: MediaQueryListEvent) => void>;
  setSystem: (isDark: boolean) => void;
} {
  const listeners = new Set<(e: MediaQueryListEvent) => void>();
  const current = { isDark: systemIsDark };
  const mk = (): MediaQueryStub => ({
    get matches() {
      return current.isDark;
    },
    media: "(prefers-color-scheme: dark)",
    addEventListener: vi.fn((_evt: string, cb: (e: MediaQueryListEvent) => void) => {
      listeners.add(cb);
    }),
    removeEventListener: vi.fn(
      (_evt: string, cb: (e: MediaQueryListEvent) => void) => {
        listeners.delete(cb);
      },
    ),
    dispatchEvent: () => true,
    onchange: null,
    addListener: vi.fn(),
    removeListener: vi.fn(),
  });
  (window as unknown as { matchMedia: (q: string) => MediaQueryStub }).matchMedia =
    mk;
  return {
    listeners,
    setSystem(isDark) {
      current.isDark = isDark;
      const synthetic = { matches: isDark } as MediaQueryListEvent;
      for (const cb of [...listeners]) cb(synthetic);
    },
  };
}

describe("resolveTheme", () => {
  beforeEach(() => {
    window.localStorage.clear();
    __resetForTests();
    document.documentElement.classList.remove("theme-light");
  });

  it("honours explicit light/dark", () => {
    stubMatchMedia(true);
    expect(resolveTheme("light")).toBe("light");
    expect(resolveTheme("dark")).toBe("dark");
  });

  it("follows system media query when preference is 'system'", () => {
    stubMatchMedia(false); // system is light
    expect(resolveTheme("system")).toBe("light");
    stubMatchMedia(true); // system flips to dark
    expect(resolveTheme("system")).toBe("dark");
  });
});

describe("installThemeObserver", () => {
  beforeEach(() => {
    window.localStorage.clear();
    __resetForTests();
    document.documentElement.classList.remove("theme-light");
  });

  it("adds .theme-light on <html> when preference is light", () => {
    stubMatchMedia(true);
    const dispose = installThemeObserver();

    setPreferences({
      theme: "light",
      audibleEnabled: false,
      audibleSeverities: ["critical"],
    });

    expect(document.documentElement.classList.contains("theme-light")).toBe(
      true,
    );
    dispose();
  });

  it("removes .theme-light on <html> when preference is dark", () => {
    stubMatchMedia(true);
    document.documentElement.classList.add("theme-light"); // pre-existing
    const dispose = installThemeObserver();

    setPreferences({
      theme: "dark",
      audibleEnabled: false,
      audibleSeverities: ["critical"],
    });

    expect(document.documentElement.classList.contains("theme-light")).toBe(
      false,
    );
    dispose();
  });

  it("in 'system' mode tracks OS flips live", () => {
    const media = stubMatchMedia(false); // start light

    setPreferences({
      theme: "system",
      audibleEnabled: false,
      audibleSeverities: ["critical"],
    });
    const dispose = installThemeObserver();

    // Under system=light the class should be present.
    expect(document.documentElement.classList.contains("theme-light")).toBe(
      true,
    );

    // Flip the OS to dark; listener should strip the light class.
    media.setSystem(true);
    expect(document.documentElement.classList.contains("theme-light")).toBe(
      false,
    );

    dispose();
  });

  it("leaving 'system' for 'dark' detaches the media listener", () => {
    const media = stubMatchMedia(false);
    setPreferences({
      theme: "system",
      audibleEnabled: false,
      audibleSeverities: ["critical"],
    });
    const dispose = installThemeObserver();
    expect(media.listeners.size).toBe(1);

    setPreferences({
      theme: "dark",
      audibleEnabled: false,
      audibleSeverities: ["critical"],
    });

    expect(media.listeners.size).toBe(0);
    dispose();
  });
});
