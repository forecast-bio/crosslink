import type {
  Agent,
  AgentDetailResponse,
  BudgetConfig,
  Comment,
  Config,
  CreateKnowledgePageRequest,
  HealthResponse,
  Issue,
  IssueDetail,
  IssuePriority,
  KnowledgePage,
  KnowledgeSearchMatch,
  Lock,
  MilestoneDetail,
  OrchestratorPlan,
  RawUsageSummary,
  Session,
  SyncStatus,
  TokenUsageRecord,
} from "@/lib/types";

export interface ApiClientConfig {
  baseUrl?: string;
  fetchFn?: typeof globalThis.fetch;
}

let _baseUrl = "/api/v1";
let _fetchFn: typeof globalThis.fetch = globalThis.fetch.bind(globalThis);

/** Configure the API client. Call before any API calls to inject dependencies. */
export function configureApiClient(config: ApiClientConfig): void {
  if (config.baseUrl !== undefined) _baseUrl = config.baseUrl;
  if (config.fetchFn !== undefined) _fetchFn = config.fetchFn;
}

async function request<T>(
  path: string,
  options?: RequestInit,
): Promise<T> {
  const res = await _fetchFn(`${_baseUrl}${path}`, {
    headers: { "Content-Type": "application/json", ...options?.headers },
    ...options,
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${res.status} ${res.statusText}: ${body}`);
  }
  return res.json() as Promise<T>;
}

/** Unwrap paginated list responses: { items: T[], total: number } → T[] */
async function requestList<T>(
  path: string,
  options?: RequestInit,
): Promise<T[]> {
  const res = await request<{ items: T[]; total: number }>(path, options);
  return res.items;
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

function isDefined(entry: [string, unknown]): entry is [string, string] {
  return entry[1] !== undefined;
}

export const issues = {
  list: (params?: IssueListParams) => {
    const q = new URLSearchParams(
      Object.entries(params ?? {}).filter(isDefined),
    ).toString();
    return requestList<Issue>(`/issues${q ? `?${q}` : ""}`);
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
    requestList<Comment>(`/issues/${id}/comments`),

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

  getBlocked: () => requestList<Issue>("/issues/blocked"),
  getReady: () => requestList<Issue>("/issues/ready"),
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
  clearWork: () =>
    request<void>("/sessions/work", { method: "DELETE" }),
};

// ── Milestones ────────────────────────────────────────────────────────────────

export const milestones = {
  list: () => requestList<MilestoneDetail>("/milestones"),
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
  list: () => requestList<KnowledgePage>("/knowledge"),
  get: (slug: string) => request<KnowledgePage>(`/knowledge/${encodeURIComponent(slug)}`),
  create: (data: CreateKnowledgePageRequest) =>
    request<KnowledgePage>("/knowledge", { method: "POST", body: JSON.stringify(data) }),
  update: (slug: string, data: Partial<Pick<CreateKnowledgePageRequest, "title" | "content" | "tags">>) =>
    request<KnowledgePage>(`/knowledge/${encodeURIComponent(slug)}`, {
      method: "PATCH",
      body: JSON.stringify(data),
    }),
  search: (q: string) =>
    requestList<KnowledgeSearchMatch>(`/knowledge/search?q=${encodeURIComponent(q)}`),
};

// ── Agents ────────────────────────────────────────────────────────────────────

export const agents = {
  list: () => requestList<Agent>("/agents"),
  get: (id: string) => request<AgentDetailResponse>(`/agents/${encodeURIComponent(id)}`),
  getStatus: (id: string) =>
    request<{ status: string; report?: string }>(`/agents/${encodeURIComponent(id)}/status`),
};

// ── Locks ─────────────────────────────────────────────────────────────────────

export const locks = {
  list: () => requestList<Lock>("/locks"),
  stale: () => requestList<Lock>("/locks/stale"),
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

// ── Usage ────────────────────────────────────────────────────────────────────

export interface UsageListParams {
  agent_id?: string;
  from?: string;
  to?: string;
}

export const usage = {
  list: (params?: UsageListParams) => {
    const q = new URLSearchParams(
      Object.entries(params ?? {}).filter(isDefined),
    ).toString();
    return requestList<TokenUsageRecord>(`/usage${q ? `?${q}` : ""}`);
  },

  summary: (params?: UsageListParams) => {
    const q = new URLSearchParams(
      Object.entries(params ?? {}).filter(isDefined),
    ).toString();
    return request<RawUsageSummary>(`/usage/summary${q ? `?${q}` : ""}`);
  },

  budget: () => request<BudgetConfig>("/usage/budget"),

  updateBudget: (data: Partial<BudgetConfig>) =>
    request<BudgetConfig>("/usage/budget", {
      method: "PATCH",
      body: JSON.stringify(data),
    }),
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
  retryStage: (stageId: string) =>
    request<void>(`/orchestrator/stages/${encodeURIComponent(stageId)}/retry`, {
      method: "POST",
    }),
  skipStage: (stageId: string) =>
    request<void>(`/orchestrator/stages/${encodeURIComponent(stageId)}/skip`, {
      method: "POST",
    }),
};
