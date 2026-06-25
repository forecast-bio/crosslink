import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { bootstrapAuth, TOKEN_STORAGE_KEY } from "../bootstrap";

describe("bootstrapAuth", () => {
  const originalFetch = globalThis.fetch;
  let fetchSpy: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    window.sessionStorage.clear();
    window.history.replaceState({}, "", "/");
    fetchSpy = vi.fn(() =>
      Promise.resolve(new Response("{}", { status: 200 })),
    );
    globalThis.fetch = fetchSpy as unknown as typeof globalThis.fetch;
  });

  afterEach(() => {
    globalThis.fetch = originalFetch;
  });

  it("returns null when no token is present", () => {
    expect(bootstrapAuth()).toBeNull();
  });

  it("persists and strips a ?token= URL parameter", () => {
    window.history.replaceState({}, "", "/?token=abc123");
    const returned = bootstrapAuth();

    expect(returned).toBe("abc123");
    expect(window.sessionStorage.getItem(TOKEN_STORAGE_KEY)).toBe("abc123");
    expect(window.location.search).toBe("");
  });

  it("restores a previously-stored token from sessionStorage", () => {
    window.sessionStorage.setItem(TOKEN_STORAGE_KEY, "persisted");
    expect(bootstrapAuth()).toBe("persisted");
  });

  it("installs a fetch wrapper that attaches the bearer token", async () => {
    window.sessionStorage.setItem(TOKEN_STORAGE_KEY, "xyz");
    bootstrapAuth();

    await globalThis.fetch("/api/v1/health");
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    const init = fetchSpy.mock.calls[0][1] as RequestInit | undefined;
    const headers = new Headers(init?.headers);
    expect(headers.get("Authorization")).toBe("Bearer xyz");
  });

  it("preserves caller-provided headers when attaching the token", async () => {
    window.sessionStorage.setItem(TOKEN_STORAGE_KEY, "tkn");
    bootstrapAuth();

    await globalThis.fetch("/api/v1/projects", {
      headers: { "Content-Type": "application/json" },
    });
    const init = fetchSpy.mock.calls[0][1] as RequestInit;
    const headers = new Headers(init.headers);
    expect(headers.get("Content-Type")).toBe("application/json");
    expect(headers.get("Authorization")).toBe("Bearer tkn");
  });
});
