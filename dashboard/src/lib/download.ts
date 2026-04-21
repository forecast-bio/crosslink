// Authenticated file download helper.
//
// The dashboard API wraps `globalThis.fetch` with a bearer-token
// Authorization header in `auth/bootstrap.ts`. A plain `<a href>`
// anchor does *not* go through that wrapper, so clicking it would
// hit protected export endpoints without auth and 401.
//
// This helper does the fetch ourselves (carrying the header), then
// hands the blob to the browser via an object URL and a synthetic
// anchor click — the same net effect as a direct link, but routed
// through the auth wrapper.

const API_BASE = "/api/v1/dashboard";

export interface DownloadOptions {
  /// Path under `/api/v1/dashboard`, e.g. `/export/projects.csv`.
  path: string;
  /// Name suggested to the browser. Overrides any server-provided
  /// Content-Disposition filename.
  filename: string;
}

/// Fetch `path` with the wrapped (auth-carrying) fetch and trigger a
/// browser download of the response body.
export async function downloadAuthenticated(opts: DownloadOptions): Promise<void> {
  const resp = await fetch(`${API_BASE}${opts.path}`, {
    method: "GET",
    headers: { Accept: "*/*" },
  });
  if (!resp.ok) {
    let message = `HTTP ${resp.status}`;
    try {
      const body = (await resp.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // Non-JSON body; keep the status-only message.
    }
    throw new Error(message);
  }
  const blob = await resp.blob();
  const url = URL.createObjectURL(blob);
  try {
    const a = document.createElement("a");
    a.href = url;
    a.download = opts.filename;
    document.body.appendChild(a);
    a.click();
    a.remove();
  } finally {
    // Free the blob URL after the browser picks it up. A short delay
    // keeps Safari happy on some older versions; modern browsers can
    // revoke immediately.
    setTimeout(() => URL.revokeObjectURL(url), 1_000);
  }
}
