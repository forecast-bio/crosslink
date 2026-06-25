// Fetch wrapper + React Query hooks for the /api/v1/dashboard endpoints.
//
// Bearer-token auth is installed globally by `auth/bootstrap.ts` before
// React mounts (it wraps `globalThis.fetch`), so these helpers can use
// the bare `fetch` API without re-plumbing headers.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import type {
  AlertItem,
  CloneRepoArgs,
  CloneRepoOutcome,
  GithubConfigUpdate,
  GithubConfigView,
  GithubRepoHit,
  GithubTrackAllOutcome,
  ProjectDetail,
  ProjectListItem,
  PtySession,
  PtySpawnRequest,
  SetWebhooksBody,
  TrackAllOrgArgs,
  WebhooksView,
} from "./types";

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

async function apiPost<T>(path: string, body?: unknown): Promise<T> {
  return apiWrite<T>("POST", path, body);
}

async function apiPut<T>(path: string, body?: unknown): Promise<T> {
  return apiWrite<T>("PUT", path, body);
}

async function apiWrite<T>(
  method: "POST" | "PUT",
  path: string,
  body?: unknown,
): Promise<T> {
  const resp = await fetch(`${API_BASE}${path}`, {
    method,
    headers: { Accept: "application/json", "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!resp.ok) {
    let message = `HTTP ${resp.status}`;
    try {
      const parsed = (await resp.json()) as { error?: string };
      if (parsed.error) message = parsed.error;
    } catch {
      // Non-JSON error body.
    }
    throw new ApiRequestError(resp.status, message);
  }
  const text = await resp.text();
  return (text ? JSON.parse(text) : ({} as T)) as T;
}

export interface ActionResponse {
  stdout: string;
  stderr: string;
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

/// Shared post-mutation invalidator. Projects list + project detail
/// + alerts list all get invalidated so every surface that could be
/// displaying stale state catches up immediately. Without the alerts
/// invalidation a close-from-the-alerts-page looks like a no-op
/// until the 5s polling tick refreshes independently (GH #709).
function useProjectMutations(slug: string) {
  const client = useQueryClient();
  return (after: () => void = () => undefined) => {
    client.invalidateQueries({ queryKey: ["dashboard", "projects"] });
    client.invalidateQueries({ queryKey: ["dashboard", "project", slug] });
    client.invalidateQueries({ queryKey: ["dashboard", "alerts"] });
    after();
  };
}

/// Close an issue via the dashboard's write surface. Under the hood
/// this shells out to `crosslink issue close <id>` in the tracked
/// project's workspace — identity, signing, and hub push all handled
/// by the user's normal crosslink setup.
export function useCloseIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<ActionResponse, ApiRequestError, number>({
    mutationFn: (issueId: number) =>
      apiPost<ActionResponse>(`/w/${slug}/issues/${issueId}/close`),
    onSuccess: () => invalidate(),
  });
}

/// Reopen a closed issue.
export function useReopenIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<ActionResponse, ApiRequestError, number>({
    mutationFn: (issueId: number) =>
      apiPost<ActionResponse>(`/w/${slug}/issues/${issueId}/reopen`),
    onSuccess: () => invalidate(),
  });
}

/// Post a comment on an issue. `content` goes through to the CLI's
/// `crosslink issue comment <id> "<content>"` — whitespace-only
/// content is rejected server-side with a 400.
export function useCommentIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { issueId: number; content: string }
  >({
    mutationFn: ({ issueId, content }) =>
      apiPost<ActionResponse>(`/w/${slug}/issues/${issueId}/comment`, {
        content,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Mark issue `issueId` as blocked by `blockerId`.
export function useBlockIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { issueId: number; blockerId: number }
  >({
    mutationFn: ({ issueId, blockerId }) =>
      apiPost<ActionResponse>(`/w/${slug}/issues/${issueId}/block`, {
        blocker_id: blockerId,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Drop a blocker relationship.
export function useUnblockIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { issueId: number; blockerId: number }
  >({
    mutationFn: ({ issueId, blockerId }) =>
      apiPost<ActionResponse>(`/w/${slug}/issues/${issueId}/unblock`, {
        blocker_id: blockerId,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Symmetric link between two issues.
export function useRelateIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { issueId: number; otherId: number }
  >({
    mutationFn: ({ issueId, otherId }) =>
      apiPost<ActionResponse>(`/w/${slug}/issues/${issueId}/relate`, {
        other_id: otherId,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Add a label to an issue.
export function useLabelIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { issueId: number; label: string }
  >({
    mutationFn: ({ issueId, label }) =>
      apiPost<ActionResponse>(`/w/${slug}/issues/${issueId}/label`, {
        label,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Remove a label from an issue.
export function useUnlabelIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { issueId: number; label: string }
  >({
    mutationFn: ({ issueId, label }) =>
      apiPost<ActionResponse>(`/w/${slug}/issues/${issueId}/unlabel`, {
        label,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Create a new milestone. `description` is optional.
export function useCreateMilestone(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { name: string; description?: string }
  >({
    mutationFn: ({ name, description }) =>
      apiPost<ActionResponse>(`/w/${slug}/milestones`, {
        name,
        description,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Attach issue to milestone.
export function useMilestoneAddIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { milestoneId: number; issueId: number }
  >({
    mutationFn: ({ milestoneId, issueId }) =>
      apiPost<ActionResponse>(`/w/${slug}/milestones/${milestoneId}/add`, {
        issue_id: issueId,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Detach issue from milestone.
export function useMilestoneRemoveIssue(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { milestoneId: number; issueId: number }
  >({
    mutationFn: ({ milestoneId, issueId }) =>
      apiPost<ActionResponse>(`/w/${slug}/milestones/${milestoneId}/remove`, {
        issue_id: issueId,
      }),
    onSuccess: () => invalidate(),
  });
}

/// Close a milestone (sets status to closed; does not delete).
export function useCloseMilestone(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<ActionResponse, ApiRequestError, number>({
    mutationFn: (milestoneId: number) =>
      apiPost<ActionResponse>(`/w/${slug}/milestones/${milestoneId}/close`),
    onSuccess: () => invalidate(),
  });
}

/// Claim a lock on an issue. `branch` is optional context metadata.
/// Operator-initiated claims are uncommon — normally agents claim
/// their own locks — but exposing the control lets an operator seed
/// a lock during triage.
export function useClaimLock(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    { issueId: number; branch?: string }
  >({
    mutationFn: ({ issueId, branch }) =>
      apiPost<ActionResponse>(`/w/${slug}/locks/${issueId}/claim`, { branch }),
    onSuccess: () => invalidate(),
  });
}

/// Release a lock held by the current driver.
export function useReleaseLock(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<ActionResponse, ApiRequestError, number>({
    mutationFn: (issueId: number) =>
      apiPost<ActionResponse>(`/w/${slug}/locks/${issueId}/release`),
    onSuccess: () => invalidate(),
  });
}

/// Steal a stale lock from another agent. The CLI enforces the
/// staleness threshold — this endpoint just passes through.
export function useStealLock(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<ActionResponse, ApiRequestError, number>({
    mutationFn: (issueId: number) =>
      apiPost<ActionResponse>(`/w/${slug}/locks/${issueId}/steal`),
    onSuccess: () => invalidate(),
  });
}

/// Live PTY sessions across all projects. Refetched lazily; the
/// terminal page renders this list.
export function usePtySessions() {
  return useQuery<PtySession[], ApiRequestError>({
    queryKey: ["pty", "sessions"],
    queryFn: async () => {
      const resp = await fetch("/api/v1/pty/sessions", {
        headers: { Accept: "application/json" },
      });
      if (!resp.ok) {
        throw new ApiRequestError(resp.status, `HTTP ${resp.status}`);
      }
      return (await resp.json()) as PtySession[];
    },
    refetchInterval: REFETCH_MS,
  });
}

/// Spawn a new PTY session on the server. Returns the session id;
/// the caller then opens `ws://.../ws/pty/<id>` to attach.
export function useSpawnPty() {
  const client = useQueryClient();
  return useMutation<PtySession, ApiRequestError, PtySpawnRequest>({
    mutationFn: async (req) => {
      const resp = await fetch("/api/v1/pty", {
        method: "POST",
        headers: {
          Accept: "application/json",
          "Content-Type": "application/json",
        },
        body: JSON.stringify(req),
      });
      if (!resp.ok) {
        let message = `HTTP ${resp.status}`;
        try {
          const body = (await resp.json()) as { error?: string };
          if (body.error) message = body.error;
        } catch {
          // non-JSON; fall back
        }
        throw new ApiRequestError(resp.status, message);
      }
      return (await resp.json()) as PtySession;
    },
    onSuccess: () =>
      client.invalidateQueries({ queryKey: ["pty", "sessions"] }),
  });
}

/// Current GitHub integration config — token presence, masked
/// fingerprint, and default org. Never exposes the raw token.
export function useGithubConfig() {
  return useQuery<GithubConfigView, ApiRequestError>({
    queryKey: ["dashboard", "github", "config"],
    queryFn: () => apiFetch<GithubConfigView>("/github/config"),
    refetchOnWindowFocus: false,
  });
}

/// Update the stored PAT and/or default org.
/// - `token: ""` deletes the stored secret.
/// - `default_org: null` clears the org.
/// - Omitting a field leaves it unchanged.
export function useSetGithubConfig() {
  const client = useQueryClient();
  return useMutation<GithubConfigView, ApiRequestError, GithubConfigUpdate>({
    mutationFn: (body) => apiPost<GithubConfigView>("/github/config", body),
    onSuccess: (data) => {
      client.setQueryData(["dashboard", "github", "config"], data);
    },
  });
}

/// Enumerate crosslink-touched repos in `org`. Triggered on demand
/// (not polled) because every call hits the GitHub REST API.
export function useOrgRepos(org: string | null, enabled: boolean) {
  return useQuery<GithubRepoHit[], ApiRequestError>({
    queryKey: ["dashboard", "github", "org-repos", org],
    queryFn: () =>
      apiFetch<GithubRepoHit[]>(
        `/github/orgs/${encodeURIComponent(org ?? "")}/repos`,
      ),
    enabled: enabled && !!org,
    refetchOnWindowFocus: false,
    staleTime: 60_000,
  });
}

/// Walk an org and clone+track every repo with a `crosslink/hub` branch.
///
/// - `cloneRoot` is optional; server default is `$HOME` (so repos
///   land at `~/<repo>` — flat, next to the user's manual clones).
/// - When `init` is true, the server also runs `crosslink init` +
///   `crosslink agent init <agentId>` in each freshly-cloned repo so
///   dashboard write actions work without manual bootstrap.
///   `agentId` is required in that case (server 400s otherwise).
export function useTrackAllOrg() {
  const client = useQueryClient();
  return useMutation<GithubTrackAllOutcome, ApiRequestError, TrackAllOrgArgs>({
    mutationFn: ({ org, cloneRoot, init, agentId }) =>
      // Always send a JSON body — even an empty one. The backend uses
      // axum's `Json<TrackAllBody>` extractor which 400s on an absent
      // body (400 "Failed to parse the request body") even when every
      // field is Option-typed. JSON.stringify drops `undefined` values,
      // so this serializes to `{}` when no flags are supplied,
      // which `#[serde(default)]` on the backend accepts.
      apiPost<GithubTrackAllOutcome>(
        `/github/orgs/${encodeURIComponent(org)}/track-all`,
        {
          clone_root: cloneRoot || undefined,
          init: init || undefined,
          agent_id: agentId?.trim() || undefined,
        },
      ),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["dashboard", "projects"] });
    },
  });
}

/// Run `crosslink integrity sign-backfill --confirm` in the
/// project's workspace. Triggered from the `signature_invalid`
/// alert's action panel — re-signs unsigned / invalidly-signed
/// commits on the hub branch so the alert resolves on the next
/// poll tick.
export function useSignBackfill(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<ActionResponse, ApiRequestError, void>({
    mutationFn: () =>
      apiPost<ActionResponse>(`/w/${slug}/integrity/sign-backfill`),
    onSuccess: () => invalidate(),
  });
}

/// Retrofit an already-tracked project: run `crosslink init` +
/// `crosslink agent init` in its workspace so write actions start
/// working. Idempotent-on-ready at the server level.
export function useInitProject(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<ActionResponse, ApiRequestError, { agentId: string }>({
    mutationFn: ({ agentId }) =>
      apiPost<ActionResponse>(`/w/${slug}/init`, { agent_id: agentId }),
    onSuccess: () => invalidate(),
  });
}

/// Clone an arbitrary git repo URL and register it as a tracked
/// project. Optional `init` + `agentId` bootstrap the clone so
/// write actions work right away. Standalone counterpart to the
/// PAT-gated track-all flow.
export function useCloneRepo() {
  const client = useQueryClient();
  return useMutation<CloneRepoOutcome, ApiRequestError, CloneRepoArgs>({
    mutationFn: ({ url, slug, cloneRoot, init, agentId }) =>
      apiPost<CloneRepoOutcome>("/clone", {
        url,
        slug: slug?.trim() || undefined,
        clone_root: cloneRoot?.trim() || undefined,
        init: init || undefined,
        agent_id: agentId?.trim() || undefined,
      }),
    onSuccess: () => {
      client.invalidateQueries({ queryKey: ["dashboard", "projects"] });
    },
  });
}

/// Send a control request to an agent via the hub branch.
/// Kinds: `kill` | `pause` | `resume` | `reprioritise`.
/// See design doc §9 for protocol details.
export function useAgentRequest(slug: string) {
  const invalidate = useProjectMutations(slug);
  return useMutation<
    ActionResponse,
    ApiRequestError,
    {
      agentId: string;
      kind: "kill" | "pause" | "resume" | "reprioritise";
      subjectIssue?: number;
      reason?: string;
    }
  >({
    mutationFn: ({ agentId, kind, subjectIssue, reason }) =>
      apiPost<ActionResponse>(`/w/${slug}/agents/${agentId}/request`, {
        kind,
        subject_issue: subjectIssue,
        reason,
      }),
    onSuccess: () => invalidate(),
  });
}

/// List the configured outbound webhook URLs (plaintext — the user
/// typed them and edits them here). Empty list on first run.
export function useWebhooks() {
  return useQuery<WebhooksView, ApiRequestError>({
    queryKey: ["dashboard", "webhooks"],
    queryFn: () => apiFetch<WebhooksView>("/webhooks"),
    refetchOnWindowFocus: false,
  });
}

/// Replace the full webhook URL list. Server validates each URL
/// (https + host, or http for loopback) and rejects the batch on any
/// failure without partial writes.
export function useSetWebhooks() {
  const client = useQueryClient();
  return useMutation<WebhooksView, ApiRequestError, SetWebhooksBody>({
    mutationFn: (body) => apiPut<WebhooksView>("/webhooks", body),
    onSuccess: (data) => {
      client.setQueryData(["dashboard", "webhooks"], data);
    },
  });
}
