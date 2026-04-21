// Wire types mirroring the Rust serde output of `/api/v1/dashboard/*`.
// Keep these in sync with crosslink/src/dashboard/api.rs —
// eventually replaced by ts-rs-generated types (deferred follow-up).

export interface ProjectCountersView {
  open_issues: number;
  overdue_issues: number;
  due_soon_issues: number;
  blocked_issues: number;
  active_agents: number;
  stale_locks: number;
  ci_status: string | null;
  updated_at: string | null;
}

export interface ProjectListItem {
  slug: string;
  status: string;
  pinned: boolean;
  hub_sha: string | null;
  hub_fetched_at: string | null;
  last_activity_at: string | null;
  added_at: string;
  counters: ProjectCountersView;
  /**
   * Whether the tracked workspace is initialised enough for the
   * dashboard's write actions (close issue, release lock, etc.) to
   * succeed. `"ready"` → all clear. `"agent_missing"` →
   * `crosslink init` ran but `crosslink agent init` didn't.
   * `"not_initialized"` → neither ran; clone is bare.
   */
  write_capability: WriteCapability;
}

export type WriteCapability = "ready" | "agent_missing" | "not_initialized";

export interface IssueFile {
  uuid: string;
  display_id: number | null;
  title: string;
  description?: string | null;
  status: "open" | "closed" | "archived";
  priority: "low" | "medium" | "high" | "critical";
  parent_uuid?: string | null;
  created_by: string;
  created_at: string;
  updated_at: string;
  closed_at?: string | null;
  scheduled_at?: string | null;
  due_at?: string | null;
  /**
   * Backend uses `#[serde(skip_serializing_if = "Vec::is_empty")]` on
   * these list fields — they're omitted from JSON when empty. Callers
   * must default to `[]` at read time (use `labels ?? []`).
   */
  labels?: string[];
  blockers?: string[];
  related?: string[];
  milestone_uuid?: string | null;
}

export interface AgentHeartbeat {
  agent_id: string;
  last_heartbeat: string;
  active_issue_id: number | null;
  machine_id: string;
}

export interface LockEntry {
  issue_id: number;
  agent_id: string;
  branch: string | null;
  claimed_at: string;
  signed_by: string;
}

export type AgentRequestKind = "kill" | "pause" | "resume" | "reprioritise";

export interface AgentRequestAck {
  ack_at: string;
  acted: boolean;
  result: string;
  notes: string | null;
}

export interface AgentRequest {
  request_id: string;
  kind: AgentRequestKind;
  subject_issue: number | null;
  requested_by: string;
  requested_at: string;
  reason: string | null;
  ack: AgentRequestAck | null;
}

export interface AgentRequestsForAgent {
  agent_id: string;
  requests: AgentRequest[];
}

export interface CiStatus {
  sha: string;
  /** "passing" | "failing" | "pending" — pipeline-defined */
  state: string;
  url?: string | null;
}

export type SignatureState = "valid" | "unsigned" | "invalid" | "unknown";

export interface ProjectDetail {
  slug: string;
  status: string;
  pinned: boolean;
  hub_sha: string | null;
  hub_fetched_at: string | null;
  last_activity_at: string | null;
  added_at: string;
  counters: ProjectCountersView;
  issues: IssueFile[];
  agents: AgentHeartbeat[];
  locks: LockEntry[];
  layout_version: number;
  agent_requests: AgentRequestsForAgent[];
  ci_status: CiStatus | null;
  signature_state: SignatureState;
  /** Same semantics as `ProjectListItem.write_capability`. */
  write_capability: WriteCapability;
}

export interface TrackAllOrgArgs {
  org: string;
  cloneRoot?: string;
  /** When true, server runs crosslink init + agent init in each freshly-cloned repo. */
  init?: boolean;
  /** Required when `init` is true. Alphanumeric + hyphens + underscores. */
  agentId?: string;
}

export interface InitProjectBody {
  agent_id: string;
}

export interface CloneRepoArgs {
  url: string;
  slug?: string;
  cloneRoot?: string;
  init?: boolean;
  agentId?: string;
}

export interface CloneRepoOutcome {
  slug: string;
  clone_path: string;
  initialized: boolean;
}

export interface ApiError {
  error: string;
  status: number;
}

export interface PtySession {
  id: string;
  project_slug: string;
  command: string;
  started_at: string;
  exit_code: number | null;
}

export interface PtySpawnRequest {
  project_slug: string;
  command: string;
  args?: string[];
  rows?: number;
  cols?: number;
}

export type AlertSeverity = "info" | "warning" | "critical";

export interface AlertItem {
  id: number;
  project_slug: string;
  kind: string;
  severity: AlertSeverity;
  subject_ref: string | null;
  detail: string | null;
  opened_at: string;
  resolved_at: string | null;
  acknowledged_at: string | null;
}

export interface GithubConfigView {
  token_present: boolean;
  token_fingerprint: string | null;
  default_org: string | null;
  /**
   * Where the effective token comes from.
   * - `"stored"`: encrypted PAT in the dashboard DB (primary path)
   * - `"gh-cli"`: `gh auth token` fallback (no stored PAT configured)
   * - `null`: no token available from either source
   */
  token_source: "stored" | "gh-cli" | null;
}

export interface GithubConfigUpdate {
  /** `""` deletes the stored token; `undefined` leaves it unchanged. */
  token?: string;
  /** `null` clears the default org; `undefined` leaves it unchanged. */
  default_org?: string | null;
}

export interface GithubRepoHit {
  owner: string;
  repo: string;
  full_name: string;
  default_branch: string;
  ssh_url: string;
  https_url: string;
  has_hub_branch: boolean;
}

export interface GithubTrackAllOutcome {
  tracked: string[];
  skipped: { slug: string; reason: string }[];
}

export interface WebhooksView {
  urls: string[];
}

export interface SetWebhooksBody {
  urls: string[];
}
