/**
 * Crosslink web dashboard — shared TypeScript types.
 *
 * Every type here corresponds to a Rust type in
 * `crosslink/src/server/types.rs`. When the Rust API changes, update
 * both files together. All timestamps are ISO 8601 strings.
 *
 * @module types
 */

// ---------------------------------------------------------------------------
// Core domain types (mirror of crosslink/src/models.rs + locks.rs)
// ---------------------------------------------------------------------------

export interface Issue {
  id: number;
  title: string;
  description: string | null;
  status: IssueStatus;
  priority: IssuePriority;
  parent_id: number | null;
  created_at: string; // ISO 8601
  updated_at: string;
  closed_at: string | null;
}

export type IssueStatus = "open" | "closed" | "archived";
export type IssuePriority = "low" | "medium" | "high" | "critical";

export interface Comment {
  id: number;
  issue_id: number;
  content: string;
  created_at: string;
  kind: CommentKind;
  trigger_type: string | null;
  intervention_context: string | null;
  driver_key_fingerprint: string | null;
}

export type CommentKind =
  | "note"
  | "plan"
  | "decision"
  | "observation"
  | "blocker"
  | "resolution"
  | "result"
  | "intervention";

export interface Session {
  id: number;
  started_at: string;
  ended_at: string | null;
  active_issue_id: number | null;
  handoff_notes: string | null;
  last_action: string | null;
  agent_id: string | null;
}

export interface Milestone {
  id: number;
  name: string;
  description: string | null;
  status: "open" | "closed";
  created_at: string;
  closed_at: string | null;
}

export interface Heartbeat {
  agent_id: string;
  last_heartbeat: string;
  active_issue_id: number | null;
  machine_id: string;
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

export interface HealthResponse {
  status: string;
  version: string;
}

// ---------------------------------------------------------------------------
// Issues — request types
// ---------------------------------------------------------------------------

export interface CreateIssueRequest {
  title: string;
  description?: string;
  priority?: IssuePriority;
  parent_id?: number;
}

export interface UpdateIssueRequest {
  title?: string;
  description?: string;
  priority?: IssuePriority;
}

export interface CreateSubissueRequest {
  title: string;
  description?: string;
  priority?: IssuePriority;
}

// ---------------------------------------------------------------------------
// Issues — response types
// ---------------------------------------------------------------------------

/** Fully hydrated issue returned by GET /api/v1/issues/:id */
export interface IssueDetail extends Issue {
  labels: string[];
  comments: Comment[];
  blockers: number[];
  blocking: number[];
  subissues: Issue[];
  milestone: MilestoneSummary | null;
}

/** Lightweight item in the issue list */
export interface IssueSummary extends Issue {
  labels: string[];
  blocker_count: number;
}

export interface IssueListResponse {
  items: IssueSummary[];
  total: number;
}

export interface IssueListQuery {
  status?: IssueStatus | "all";
  label?: string;
  priority?: IssuePriority;
  search?: string;
  parent_id?: number;
}

// ---------------------------------------------------------------------------
// Comments — request types
// ---------------------------------------------------------------------------

export interface CreateCommentRequest {
  content: string;
  kind?: CommentKind;
  trigger_type?: string;
  intervention_context?: string;
}

// ---------------------------------------------------------------------------
// Labels — request types
// ---------------------------------------------------------------------------

export interface AddLabelRequest {
  label: string;
}

// ---------------------------------------------------------------------------
// Dependencies — request types
// ---------------------------------------------------------------------------

export interface AddBlockerRequest {
  blocker_id: number;
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

export interface StartSessionRequest {
  agent_id?: string;
}

export interface EndSessionRequest {
  notes?: string;
}

export interface WorkOnIssueRequest {
  agent_id?: string;
}

export interface SessionResponse {
  id: number;
  started_at: string;
  ended_at: string | null;
  active_issue_id: number | null;
  handoff_notes: string | null;
  last_action: string | null;
  agent_id: string | null;
}

// ---------------------------------------------------------------------------
// Milestones
// ---------------------------------------------------------------------------

export interface MilestoneSummary {
  id: number;
  name: string;
  status: "open" | "closed";
}

export interface CreateMilestoneRequest {
  name: string;
  description?: string;
}

export interface AssignMilestoneRequest {
  issue_id: number;
}

export interface MilestoneDetail extends Milestone {
  issue_count: number;
  completed_count: number;
  /** Percentage 0–100 */
  progress_percent: number;
}

export interface MilestoneListResponse {
  items: MilestoneDetail[];
  total: number;
}

// ---------------------------------------------------------------------------
// Knowledge pages
// ---------------------------------------------------------------------------

export interface KnowledgeSource {
  url: string;
  title: string;
  accessed_at?: string;
}

export interface KnowledgePage {
  slug: string;
  title: string;
  tags: string[];
  sources: KnowledgeSource[];
  contributors: string[];
  created: string;
  updated: string;
  content: string;
}

export interface KnowledgePageSummary {
  slug: string;
  title: string;
  tags: string[];
  updated: string;
}

export interface CreateKnowledgePageRequest {
  slug: string;
  title: string;
  content: string;
  tags?: string[];
  sources?: KnowledgeSource[];
}

export interface KnowledgeSearchMatch {
  slug: string;
  title: string;
  line_number: number;
  context_lines: [number, string][];
}

// ---------------------------------------------------------------------------
// Agents and monitoring
// ---------------------------------------------------------------------------

export type AgentStatus = "running" | "active" | "idle" | "stale" | "done" | "failed" | "unknown";

/** Heartbeat record attached to an agent (object form used by the frontend). */
export interface AgentHeartbeat {
  agent_id: string;
  timestamp: string; // ISO 8601
  issue_id: number | null;
  session_id: number | null;
  message: string | null;
}

/** Lock entry as returned inside an agent detail response. */
export interface AgentLockEntry {
  issue_id: number;
  claimed_at: string; // ISO 8601
  age_seconds: number;
  stale: boolean;
}

/** API-contract-level lock entry (mirrors Rust LockEntry). */
export interface LockEntry {
  issue_id: number;
  agent_id: string;
  branch: string | null;
  claimed_at: string;
  signed_by: string;
  age_seconds: number;
  is_stale: boolean;
}

/** API-contract-level agent summary (mirrors Rust AgentSummary). */
export interface AgentSummary {
  agent_id: string;
  machine_id: string;
  description: string | null;
  status: AgentStatus;
  last_heartbeat: string;
  active_issue_id: number | null;
  branch: string | null;
  worktree_path: string | null;
  locks: number[];
}

/** API-contract-level agent detail (mirrors Rust AgentDetail). */
export interface AgentDetailContract extends AgentSummary {
  heartbeat_history: string[];
  kickoff_status: string | null;
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

export interface SyncStatusResponse {
  hub_initialized: boolean;
  hub_branch: string;
  remote: string;
  last_fetch_at: string | null;
  active_lock_count: number;
  stale_lock_count: number;
}

export interface SyncActionResponse {
  success: boolean;
  message: string;
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

export type TrackingMode = "strict" | "normal" | "relaxed";
export type SigningEnforcement = "audit" | "required" | "disabled";

export interface ConfigResponse {
  tracking_mode: TrackingMode;
  stale_lock_timeout_minutes: number;
  remote: string;
  signing_enforcement: SigningEnforcement;
  intervention_tracking: boolean;
  auto_steal_stale_locks: boolean;
}

export interface UpdateConfigRequest {
  tracking_mode?: TrackingMode;
  stale_lock_timeout_minutes?: number;
  remote?: string;
  signing_enforcement?: SigningEnforcement;
  intervention_tracking?: boolean;
  auto_steal_stale_locks?: boolean;
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

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
  complexity_hours: number;
  /** Runtime execution state — present during/after execution */
  status?: StageStatus;
  /** Agent assigned to this stage during execution */
  agent_id?: string;
}

export interface OrchestratorPhase {
  id: string;
  title: string;
  description: string;
  stages: OrchestratorStage[];
  gate_criteria: string[];
}

export interface OrchestratorPlan {
  id: string;
  title?: string;
  document_slug: string;
  phases: OrchestratorPhase[];
  created_at: string;
  total_stages: number;
  estimated_hours: number;
}

export interface DecomposeRequest {
  document: string;
  slug?: string;
}

export type StageStatus =
  | "pending"
  | "running"
  | "done"
  | "failed"
  | "skipped"
  | "blocked";

export type ExecutionState =
  | "idle"
  | "running"
  | "paused"
  | "done"
  | "failed";

export interface ExecutionStatus {
  plan_id: string;
  state: ExecutionState;
  current_phase_id: string | null;
  progress_percent: number;
  started_at: string | null;
  completed_at: string | null;
  /** Map from stage_id → StageStatus */
  stage_statuses: Record<string, StageStatus>;
  /** Map from stage_id → agent_id for running stages */
  stage_agents: Record<string, string>;
}

// ---------------------------------------------------------------------------
// Type aliases (canonical short names for API response types)
// ---------------------------------------------------------------------------

/** Alias: AgentSummary is the canonical agent list item type */
export type Agent = AgentSummary;

/**
 * Full agent detail returned by GET /api/v1/agents/:id.
 * Richer than AgentSummary — includes full heartbeat object, lock entries,
 * tmux session, and kickoff report fields needed by the detail page.
 */
export interface AgentDetailResponse {
  agent_id: string;
  machine_id: string;
  description: string | null;
  status: AgentStatus;
  /** Latest heartbeat as a rich object (null if no heartbeat recorded). */
  last_heartbeat: AgentHeartbeat | null;
  active_issue_id: number | null;
  branch: string | null;
  worktree_path: string | null;
  tmux_session: string | null;
  /** Full lock entries for the held-locks display. */
  locks: AgentLockEntry[];
  /** ISO timestamps of heartbeats in the last 24h, oldest first. */
  heartbeat_history: string[];
  kickoff_status: string | null;
  kickoff_report: string | null;
}

/** Alias: ConfigResponse is the canonical config type */
export type Config = ConfigResponse;
/** Alias: LockEntry is the canonical lock type */
export type Lock = LockEntry;
/** Alias: SyncStatusResponse is the canonical sync status type */
export type SyncStatus = SyncStatusResponse;

// ---------------------------------------------------------------------------
// WebSocket messages
// ---------------------------------------------------------------------------

/** Discriminated union of all messages that can arrive over /ws */
export type WsMessage =
  | WsHeartbeatEvent
  | WsAgentStatusEvent
  | WsIssueUpdatedEvent
  | WsLockChangedEvent
  | WsExecutionProgressEvent;

/** Server → Client: new agent heartbeat received */
export interface WsHeartbeatEvent {
  type: "heartbeat";
  agent_id: string;
  timestamp: string;
  active_issue_id: number | null;
}

/** Server → Client: agent's derived status changed */
export interface WsAgentStatusEvent {
  type: "agent_status";
  agent_id: string;
  status: AgentStatus;
}

/** Server → Client: an issue was created, updated, or closed */
export interface WsIssueUpdatedEvent {
  type: "issue_updated";
  issue_id: number;
  field: string;
}

/** Server → Client: a lock was claimed or released */
export interface WsLockChangedEvent {
  type: "lock_changed";
  issue_id: number;
  action: "claimed" | "released";
  agent_id: string;
}

/** Server → Client: orchestration stage progress changed */
export interface WsExecutionProgressEvent {
  type: "execution_progress";
  plan_id: string;
  phase_id: string;
  stage_id: string;
  status: StageStatus;
  agent_id: string | null;
}

/** Client → Server: subscribe to specific event channels */
export interface WsSubscribeMessage {
  type: "subscribe";
  /** Valid channels: "agents" | "issues" | "locks" | "execution" */
  channels: WsChannel[];
}

export type WsChannel = "agents" | "issues" | "locks" | "execution";

/** Alias: messages sent from client to server */
export type WsClientMessage = WsSubscribeMessage;
/** Alias: messages received from server */
export type WsServerMessage = WsMessage;

// ---------------------------------------------------------------------------
// Token usage & cost tracking
// ---------------------------------------------------------------------------

/** A single token-usage record as stored in the `token_usage` table. */
export interface TokenUsageRecord {
  id: number;
  agent_id: string;
  session_id: number | null;
  timestamp: string; // ISO 8601
  input_tokens: number;
  output_tokens: number;
  model: string;
  cost_estimate: number;
}

/** Aggregated usage totals returned by GET /api/v1/usage/summary. */
export interface UsageSummary {
  total_input_tokens: number;
  total_output_tokens: number;
  total_cost: number;
  by_agent: AgentUsageSummary[];
  by_model: ModelUsageSummary[];
  daily: DailyUsage[];
}

/** Per-agent usage totals. */
export interface AgentUsageSummary {
  agent_id: string;
  input_tokens: number;
  output_tokens: number;
  cost_estimate: number;
  interaction_count: number;
}

/** Per-model usage totals. */
export interface ModelUsageSummary {
  model: string;
  input_tokens: number;
  output_tokens: number;
  cost_estimate: number;
}

/** Per-day usage totals for time-series charts. */
export interface DailyUsage {
  date: string; // YYYY-MM-DD
  input_tokens: number;
  output_tokens: number;
  cost_estimate: number;
}

/** Budget thresholds configured by the operator. */
export interface BudgetConfig {
  daily_limit: number | null;
  monthly_limit: number | null;
  alert_threshold_percent: number; // 0–100, triggers warning at this % of limit
}

/** Query parameters for GET /api/v1/usage. */
export interface UsageQuery {
  agent_id?: string;
  from?: string; // ISO 8601
  to?: string; // ISO 8601
}

// ---------------------------------------------------------------------------
// Generic API wrapper
// ---------------------------------------------------------------------------

export interface ApiError {
  error: string;
  detail?: string;
}

export interface OkResponse {
  ok: boolean;
}

