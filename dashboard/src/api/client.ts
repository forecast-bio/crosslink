import type {
  Agent,
  AgentDetailResponse,
  AgentUsageSummary,
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
  ModelUsageSummary,
  OrchestratorPlan,
  Session,
  SyncStatus,
  TokenUsageRecord,
  UsageSummary,
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

export const issues = {
  list: (params?: IssueListParams) => {
    const q = new URLSearchParams(
      Object.entries(params ?? {}).filter(([, v]) => v !== undefined) as [
        string,
        string,
      ][],
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
      Object.entries(params ?? {}).filter(([, v]) => v !== undefined) as [
        string,
        string,
      ][],
    ).toString();
    return requestList<TokenUsageRecord>(`/usage${q ? `?${q}` : ""}`);
  },

  summary: async (params?: UsageListParams): Promise<UsageSummary> => {
    const q = new URLSearchParams(
      Object.entries(params ?? {}).filter(([, v]) => v !== undefined) as [
        string,
        string,
      ][],
    ).toString();
    const raw = await request<{
      items: Array<{
        agent_id: string;
        model: string;
        request_count: number;
        total_input_tokens: number;
        total_output_tokens: number;
        total_cost: number;
      }>;
      total_input_tokens: number;
      total_output_tokens: number;
      total_cost: number;
    }>(`/usage/summary${q ? `?${q}` : ""}`);

    // Aggregate items by agent
    const agentMap = new Map<string, AgentUsageSummary>();
    for (const r of raw.items) {
      const existing = agentMap.get(r.agent_id);
      if (existing) {
        existing.input_tokens += r.total_input_tokens;
        existing.output_tokens += r.total_output_tokens;
        existing.cost_estimate += r.total_cost;
        existing.interaction_count += r.request_count;
      } else {
        agentMap.set(r.agent_id, {
          agent_id: r.agent_id,
          input_tokens: r.total_input_tokens,
          output_tokens: r.total_output_tokens,
          cost_estimate: r.total_cost,
          interaction_count: r.request_count,
        });
      }
    }

    // Aggregate items by model
    const modelMap = new Map<string, ModelUsageSummary>();
    for (const r of raw.items) {
      const existing = modelMap.get(r.model);
      if (existing) {
        existing.input_tokens += r.total_input_tokens;
        existing.output_tokens += r.total_output_tokens;
        existing.cost_estimate += r.total_cost;
      } else {
        modelMap.set(r.model, {
          model: r.model,
          input_tokens: r.total_input_tokens,
          output_tokens: r.total_output_tokens,
          cost_estimate: r.total_cost,
        });
      }
    }

    return {
      total_input_tokens: raw.total_input_tokens,
      total_output_tokens: raw.total_output_tokens,
      total_cost: raw.total_cost,
      by_agent: [...agentMap.values()],
      by_model: [...modelMap.values()],
      daily: [],
    };
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
