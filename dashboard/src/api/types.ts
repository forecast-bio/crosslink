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
}

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
  labels: string[];
  blockers: string[];
  related: string[];
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
}

export interface ApiError {
  error: string;
  status: number;
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
