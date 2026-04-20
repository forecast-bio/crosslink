// Fetch wrapper + React Query hooks for the /api/v1/dashboard endpoints.
//
// Bearer-token auth is installed globally by `auth/bootstrap.ts` before
// React mounts (it wraps `globalThis.fetch`), so these helpers can use
// the bare `fetch` API without re-plumbing headers.

import { useQuery } from "@tanstack/react-query";

import type { AlertItem, ProjectDetail, ProjectListItem } from "./types";

const API_BASE = "/api/v1/dashboard";

/// Default refetch cadence. Matches the server-side poll loop
/// (`crosslink/src/dashboard/poll.rs::DEFAULT_TICK = 5s`) so the
/// frontend's view stays within one tick of the ground truth.
const REFETCH_MS = 5_000;

export class ApiRequestError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
    this.name = "ApiRequestError";
  }
}

async function apiFetch<T>(path: string): Promise<T> {
  const resp = await fetch(`${API_BASE}${path}`, {
    headers: { Accept: "application/json" },
  });
  if (!resp.ok) {
    let message = `HTTP ${resp.status}`;
    try {
      const body = (await resp.json()) as { error?: string };
      if (body.error) message = body.error;
    } catch {
      // Non-JSON error body; fall back to status-only message.
    }
    throw new ApiRequestError(resp.status, message);
  }
  return (await resp.json()) as T;
}

/// `useQuery` hook for the project-list endpoint. Polls every 5s so
/// tiles stay current without requiring the WebSocket upgrade
/// (which lands in P1.5).
export function useProjects() {
  return useQuery<ProjectListItem[], ApiRequestError>({
    queryKey: ["dashboard", "projects"],
    queryFn: () => apiFetch<ProjectListItem[]>("/projects"),
    refetchInterval: REFETCH_MS,
    refetchIntervalInBackground: false,
  });
}

/// Detail hook. `slug` is `owner/repo` — the wildcard route handles
/// the embedded slash server-side. `null` slug disables the query
/// (useful when the route param isn't resolved yet).
export function useProject(slug: string | null) {
  return useQuery<ProjectDetail, ApiRequestError>({
    queryKey: ["dashboard", "project", slug],
    queryFn: () => apiFetch<ProjectDetail>(`/projects/${slug}`),
    refetchInterval: REFETCH_MS,
    refetchIntervalInBackground: false,
    enabled: slug !== null,
  });
}

/// Currently-open alerts across all projects. Primary use case is
/// the alert rail in the header and the `/alerts` page. WS events
/// invalidate this cache on every `dashboard_alerts_changed` tick.
export function useAlerts() {
  return useQuery<AlertItem[], ApiRequestError>({
    queryKey: ["dashboard", "alerts"],
    queryFn: () => apiFetch<AlertItem[]>("/alerts"),
    refetchInterval: REFETCH_MS,
    refetchIntervalInBackground: false,
  });
}
