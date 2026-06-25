/**
 * sessionStorage key used to persist the bearer token across reloads in the
 * same browser tab. Tab-scoped on purpose — closing the tab forgets the
 * token and the user must paste the `?token=` URL again.
 */
export const TOKEN_STORAGE_KEY = "crosslink_api_token";

/**
 * Bootstrap API auth for the dashboard.
 *
 * `crosslink dashboard` protects every `/api/*` route (except `/api/v1/health`
 * and `/ws`) with a randomly-generated bearer token that it prints at
 * startup. This function runs once, synchronously, before React mounts:
 *
 * 1. If `?token=<value>` is present on the current URL, store it in
 *    `sessionStorage` and strip it from the URL via `history.replaceState`
 *    so it doesn't leak into the browser history.
 * 2. Otherwise, try to read a previously-persisted token from sessionStorage.
 * 3. If a token is available, wrap `globalThis.fetch` to attach
 *    `Authorization: Bearer <token>` to every request.
 *
 * @returns the bearer token that was installed, or `null` if none was found.
 */
export function bootstrapAuth(): string | null {
  let token: string | null = null;

  try {
    const url = new URL(window.location.href);
    const urlToken = url.searchParams.get("token");
    if (urlToken) {
      token = urlToken;
      try {
        window.sessionStorage.setItem(TOKEN_STORAGE_KEY, urlToken);
      } catch {
        // sessionStorage may be unavailable (sandboxed iframes, some private
        // browsing modes). Fall through — the token still works for this load.
      }
      url.searchParams.delete("token");
      window.history.replaceState({}, "", url.toString());
    } else {
      try {
        token = window.sessionStorage.getItem(TOKEN_STORAGE_KEY);
      } catch {
        token = null;
      }
    }
  } catch {
    // Unknown failure resolving the current URL; skip auth setup.
    return null;
  }

  if (!token) return null;

  const nativeFetch = globalThis.fetch.bind(globalThis);
  const capturedToken = token;
  globalThis.fetch = ((input, init) => {
    const headers = new Headers(init?.headers);
    headers.set("Authorization", `Bearer ${capturedToken}`);
    return nativeFetch(input, { ...init, headers });
  }) as typeof globalThis.fetch;

  return token;
}
