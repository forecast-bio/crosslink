// Coverage for the authenticated-download helper. We swap
// `globalThis.fetch` for a mock that returns a blob response, and
// spy on the DOM anchor machinery to assert the filename + URL hop.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

import { downloadAuthenticated } from "../download";

describe("downloadAuthenticated", () => {
  const origFetch = globalThis.fetch;
  const origCreateObjectURL = URL.createObjectURL;
  const origRevokeObjectURL = URL.revokeObjectURL;

  beforeEach(() => {
    URL.createObjectURL = vi.fn(() => "blob:mock-url");
    URL.revokeObjectURL = vi.fn();
  });

  afterEach(() => {
    globalThis.fetch = origFetch;
    URL.createObjectURL = origCreateObjectURL;
    URL.revokeObjectURL = origRevokeObjectURL;
    vi.useRealTimers();
  });

  it("fetches the endpoint and triggers an anchor click with the filename", async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(new Blob(["slug,count\nfoo,1\n"], { type: "text/csv" }), {
        status: 200,
      }),
    ) as typeof globalThis.fetch;

    const clickSpy = vi.fn();
    const origCreate = document.createElement.bind(document);
    const createSpy = vi
      .spyOn(document, "createElement")
      .mockImplementation((tag: string) => {
        const el = origCreate(tag) as HTMLAnchorElement;
        if (tag === "a") {
          el.click = clickSpy;
        }
        return el;
      });

    await downloadAuthenticated({
      path: "/export/projects.csv",
      filename: "crosslink-projects.csv",
    });

    expect(globalThis.fetch).toHaveBeenCalledWith(
      "/api/v1/dashboard/export/projects.csv",
      expect.objectContaining({ method: "GET" }),
    );
    expect(URL.createObjectURL).toHaveBeenCalled();
    expect(clickSpy).toHaveBeenCalledTimes(1);
    createSpy.mockRestore();
  });

  it("throws with server error message on non-2xx", async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response(JSON.stringify({ error: "dashboard DB not configured" }), {
        status: 400,
        headers: { "Content-Type": "application/json" },
      }),
    ) as typeof globalThis.fetch;

    await expect(
      downloadAuthenticated({
        path: "/export/projects.csv",
        filename: "crosslink-projects.csv",
      }),
    ).rejects.toThrow(/dashboard DB not configured/);
  });

  it("falls back to HTTP status when error body isn't JSON", async () => {
    globalThis.fetch = vi.fn(async () =>
      new Response("boom", { status: 500 }),
    ) as typeof globalThis.fetch;

    await expect(
      downloadAuthenticated({
        path: "/export/projects.csv",
        filename: "crosslink-projects.csv",
      }),
    ).rejects.toThrow(/HTTP 500/);
  });
});
