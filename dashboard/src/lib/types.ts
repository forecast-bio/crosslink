// TypeScript types matching Rust models from crosslink core

// ── Issues ───────────────────────────────────────────────────────────────────

export type IssueStatus = "open" | "closed";
export type IssuePriority = "low" | "medium" | "high" | "critical";

export interface Issue {
  id: number;
  title: string;
  description: string | null;
  status: IssueStatus;
  priority: IssuePriority;
  created_at: string;
  updated_at: string;
  closed_at: string | null;
  parent_id: number | null;
  milestone_id: number | null;
}

export interface IssueDetail extends Issue {
  labels: string[];
  comments: Comment[];
  blocked_by: number[];
  blocking: number[];
  subissues: Issue[];
}

export interface Comment {
  id: number;
  issue_id: number;
  content: string;
  kind: CommentKind | null;
  created_at: string;
}

export type CommentKind =
  | "plan"
  | "decision"
  | "observation"
  | "blocker"
  | "resolution"
  | "result";

// ── Sessions ─────────────────────────────────────────────────────────────────

export interface Session {
  id: number;
  agent_id: string;
  started_at: string;
  ended_at: string | null;
  notes: string | null;
  active_issue_id: number | null;
}

// ── Milestones ────────────────────────────────────────────────────────────────

export interface Milestone {
  id: number;
  title: string;
  description: string | null;
  status: "open" | "closed";
  created_at: string;
  closed_at: string | null;
  issue_count: number;
  closed_issue_count: number;
}

// ── Knowledge ────────────────────────────────────────────────────────────────

export interface KnowledgePage {
  slug: string;
  title: string;
  tags: string[];
  source: string | null;
  created_at: string;
  updated_at: string;
  content: string;
}

// ── Agents & Heartbeats ──────────────────────────────────────────────────────

export type AgentStatus = "running" | "idle" | "done" | "failed" | "stale";

export interface Heartbeat {
  agent_id: string;
  timestamp: string;
  issue_id: number | null;
  session_id: number | null;
  message: string | null;
}

export interface Agent {
  id: string;
  worktree_path: string | null;
  branch: string | null;
  tmux_session: string | null;
  status: AgentStatus;
  last_heartbeat: Heartbeat | null;
  active_issue_id: number | null;
  locks: Lock[];
}

// ── Locks ────────────────────────────────────────────────────────────────────

export interface Lock {
  issue_id: number;
  agent_id: string;
  claimed_at: string;
  stale: boolean;
  age_seconds: number;
}

// ── Sync ─────────────────────────────────────────────────────────────────────

export interface SyncStatus {
  initialized: boolean;
  last_fetch_at: string | null;
  hub_branch: string;
  remote: string;
}

// ── Config ───────────────────────────────────────────────────────────────────

export interface Config {
  tracking_mode: "strict" | "normal" | "relaxed";
  hub_remote: string;
  hub_branch: string;
  knowledge_branch: string;
  agent_id: string | null;
  [key: string]: unknown;
}

// ── Orchestrator ─────────────────────────────────────────────────────────────

export interface OrchestratorTask {
  id: string;
  title: string;
  description: string;
  complexity_hours: number;
}

export interface OrchestratorStage {
  id: string;
  title: string;
  description: string;
  tasks: OrchestratorTask[];
  depends_on: string[];
  agent_count: number;
  status: "pending" | "running" | "done" | "failed" | "blocked";
  issue_id: number | null;
  agent_id: string | null;
}

export interface OrchestratorPhase {
  id: string;
  title: string;
  description: string;
  stages: OrchestratorStage[];
  milestone_id: number | null;
}

export interface OrchestratorPlan {
  id: string;
  title: string;
  source_document: string;
  phases: OrchestratorPhase[];
  created_at: string;
  updated_at: string;
}

export interface ExecutionStatus {
  plan_id: string;
  status: "idle" | "running" | "paused" | "done" | "failed";
  current_phase: string | null;
  progress_pct: number;
  started_at: string | null;
  completed_at: string | null;
}

// ── API responses ─────────────────────────────────────────────────────────────

export interface ApiError {
  error: string;
  message: string;
}

export interface HealthResponse {
  status: "ok";
  version: string;
}

// ── WebSocket messages ────────────────────────────────────────────────────────

export type WsServerMessage =
  | { type: "heartbeat"; agent_id: string; timestamp: string; issue_id?: number }
  | { type: "agent_status"; agent_id: string; status: AgentStatus }
  | { type: "issue_updated"; issue_id: number; field: string }
  | { type: "lock_changed"; issue_id: number; action: "claimed" | "released" }
  | { type: "execution_progress"; phase: string; stage: string; status: string };

export type WsClientMessage =
  | { type: "subscribe"; channels: WsChannel[] };

export type WsChannel = "agents" | "issues" | "execution";
