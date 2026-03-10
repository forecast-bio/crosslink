import type {
  Agent,
  Comment,
  Config,
  HealthResponse,
  Issue,
  IssueDetail,
  IssuePriority,
  KnowledgePage,
  Lock,
  MilestoneDetail,
  OrchestratorPlan,
  Session,
  SyncStatus,
} from "@/lib/types";

const BASE = "/api/v1";

async function request<T>(
  path: string,
  options?: RequestInit,
): Promise<T> {
  const res = await fetch(`${BASE}${path}`, {
    headers: { "Content-Type": "application/json", ...options?.headers },
    ...options,
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status} ${res.statusText}: ${body}`);
  }
  return res.json() as Promise<T>;
}

// ── Health ────────────────────────────────────────────────────────────────────

export const health = {
  get: () => request<HealthResponse>("/health"),
};

// ── Issues ────────────────────────────────────────────────────────────────────

export interface IssueListParams {
  status?: "open" | "closed" | "all";
  label?: string;
  priority?: IssuePriority;
  search?: string;
  parent_id?: number;
}

export const issues = {
  list: (params?: IssueListParams) => {
    const q = new URLSearchParams(
      Object.entries(params ?? {}).filter(([, v]) => v !== undefined) as [
        string,
        string,
      ][],
    ).toString();
    return request<Issue[]>(`/issues${q ? `?${q}` : ""}`);
  },

  get: (id: number) => request<IssueDetail>(`/issues/${id}`),

  create: (data: { title: string; description?: string; priority?: IssuePriority }) =>
    request<Issue>("/issues", { method: "POST", body: JSON.stringify(data) }),

  update: (id: number, data: Partial<Pick<Issue, "title" | "description" | "priority">>) =>
    request<Issue>(`/issues/${id}`, { method: "PATCH", body: JSON.stringify(data) }),

  close: (id: number) =>
    request<Issue>(`/issues/${id}/close`, { method: "POST" }),

  reopen: (id: number) =>
    request<Issue>(`/issues/${id}/reopen`, { method: "POST" }),

  delete: (id: number) =>
    request<void>(`/issues/${id}`, { method: "DELETE" }),

  createSubissue: (parentId: number, data: { title: string; priority?: IssuePriority }) =>
    request<Issue>(`/issues/${parentId}/subissue`, {
      method: "POST",
      body: JSON.stringify(data),
    }),

  getComments: (id: number) =>
    request<Comment[]>(`/issues/${id}/comments`),

  addComment: (id: number, data: { content: string; kind?: string }) =>
    request<Comment>(`/issues/${id}/comments`, {
      method: "POST",
      body: JSON.stringify(data),
    }),

  addLabel: (id: number, label: string) =>
    request<void>(`/issues/${id}/labels`, {
      method: "POST",
      body: JSON.stringify({ label }),
    }),

  removeLabel: (id: number, label: string) =>
    request<void>(`/issues/${id}/labels/${encodeURIComponent(label)}`, {
      method: "DELETE",
    }),

  addBlocker: (id: number, blockerId: number) =>
    request<void>(`/issues/${id}/block`, {
      method: "POST",
      body: JSON.stringify({ blocker_id: blockerId }),
    }),

  removeBlocker: (id: number, blockerId: number) =>
    request<void>(`/issues/${id}/block/${blockerId}`, { method: "DELETE" }),

  getBlocked: () => request<Issue[]>("/issues/blocked"),
  getReady: () => request<Issue[]>("/issues/ready"),
};

// ── Sessions ──────────────────────────────────────────────────────────────────

export const sessions = {
  current: () => request<Session | null>("/sessions/current"),
  start: () => request<Session>("/sessions/start", { method: "POST" }),
  end: (notes?: string) =>
    request<void>("/sessions/end", {
      method: "POST",
      body: JSON.stringify({ notes }),
    }),
  work: (issueId: number) =>
    request<void>(`/sessions/work/${issueId}`, { method: "POST" }),
};

// ── Milestones ────────────────────────────────────────────────────────────────

export const milestones = {
  list: () => request<MilestoneDetail[]>("/milestones"),
  get: (id: number) => request<MilestoneDetail>(`/milestones/${id}`),
  create: (data: { title: string; description?: string }) =>
    request<MilestoneDetail>("/milestones", { method: "POST", body: JSON.stringify(data) }),
  assign: (id: number, issueId: number) =>
    request<void>(`/milestones/${id}/assign`, {
      method: "POST",
      body: JSON.stringify({ issue_id: issueId }),
    }),
  close: (id: number) =>
    request<void>(`/milestones/${id}/close`, { method: "POST" }),
};

// ── Knowledge ─────────────────────────────────────────────────────────────────

export const knowledge = {
  list: () => request<KnowledgePage[]>("/knowledge"),
  get: (slug: string) => request<KnowledgePage>(`/knowledge/${encodeURIComponent(slug)}`),
  create: (data: Omit<KnowledgePage, "created_at" | "updated_at">) =>
    request<KnowledgePage>("/knowledge", { method: "POST", body: JSON.stringify(data) }),
  search: (q: string) =>
    request<KnowledgePage[]>(`/knowledge/search?q=${encodeURIComponent(q)}`),
};

// ── Agents ────────────────────────────────────────────────────────────────────

export const agents = {
  list: () => request<Agent[]>("/agents"),
  get: (id: string) => request<Agent>(`/agents/${encodeURIComponent(id)}`),
  getStatus: (id: string) =>
    request<{ status: string; report?: string }>(`/agents/${encodeURIComponent(id)}/status`),
};

// ── Locks ─────────────────────────────────────────────────────────────────────

export const locks = {
  list: () => request<Lock[]>("/locks"),
  stale: () => request<Lock[]>("/locks/stale"),
};

// ── Sync ──────────────────────────────────────────────────────────────────────

export const sync = {
  status: () => request<SyncStatus>("/sync/status"),
  fetch: () => request<void>("/sync/fetch", { method: "POST" }),
  push: () => request<void>("/sync/push", { method: "POST" }),
};

// ── Config ────────────────────────────────────────────────────────────────────

export const config = {
  get: () => request<Config>("/config"),
  update: (data: Partial<Config>) =>
    request<Config>("/config", { method: "PATCH", body: JSON.stringify(data) }),
};

// ── Orchestrator ──────────────────────────────────────────────────────────────

export const orchestrator = {
  decompose: (document: string) =>
    request<OrchestratorPlan>("/orchestrator/decompose", {
      method: "POST",
      body: JSON.stringify({ document }),
    }),
  getPlan: () => request<OrchestratorPlan | null>("/orchestrator/plan"),
  execute: () => request<void>("/orchestrator/execute", { method: "POST" }),
  pause: () => request<void>("/orchestrator/pause", { method: "POST" }),
  status: () =>
    request<{ status: string; progress_pct: number }>("/orchestrator/status"),
};
