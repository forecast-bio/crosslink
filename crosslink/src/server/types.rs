//! Serde-serializable request and response types for the crosslink REST and WebSocket API.
//!
//! These types define the full API contract between the `crosslink serve` backend
//! and the web dashboard frontend. Every type here has a corresponding TypeScript
//! equivalent in `dashboard/src/lib/types.ts`.
//!
//! # Design principles
//!
//! - Response types derive `Serialize` (server → client).
//! - Request types derive `Deserialize` (client → server).
//! - Types that flow in both directions derive both.
//! - All timestamps are ISO 8601 strings (RFC 3339) to avoid JSON number precision issues.
//! - Optional fields use `#[serde(skip_serializing_if = "Option::is_none")]`.
//! - Enums are serialized as lowercase strings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Re-exports — core domain types already have Serialize + Deserialize
// ---------------------------------------------------------------------------

pub use crate::models::{Comment, Issue, Milestone, Session};

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

/// Response for `GET /api/v1/health`.
#[derive(Debug, Clone, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

// ---------------------------------------------------------------------------
// Issues — request types
// ---------------------------------------------------------------------------

/// Valid priority values for issue creation/update.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ApiPriority {
    Low,
    #[default]
    Medium,
    High,
}

impl std::fmt::Display for ApiPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

/// Request body for `POST /api/v1/issues`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateIssueRequest {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: ApiPriority,
    #[serde(default)]
    pub parent_id: Option<i64>,
}

/// Request body for `PATCH /api/v1/issues/:id`.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateIssueRequest {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: Option<ApiPriority>,
}

/// Request body for `POST /api/v1/issues/:id/subissue`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSubissueRequest {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: ApiPriority,
}

// ---------------------------------------------------------------------------
// Issues — response types
// ---------------------------------------------------------------------------

/// A fully hydrated issue with labels, comments, and dependency info.
///
/// Returned by `GET /api/v1/issues/:id`.
#[derive(Debug, Clone, Serialize)]
pub struct IssueDetail {
    #[serde(flatten)]
    pub issue: Issue,
    pub labels: Vec<String>,
    pub comments: Vec<Comment>,
    pub blockers: Vec<i64>,
    pub blocking: Vec<i64>,
    pub subissues: Vec<Issue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone: Option<MilestoneSummary>,
}

/// Lightweight list item returned by `GET /api/v1/issues`.
#[derive(Debug, Clone, Serialize)]
pub struct IssueSummary {
    #[serde(flatten)]
    pub issue: Issue,
    pub labels: Vec<String>,
    pub blocker_count: usize,
}

/// Paginated issue list response.
#[derive(Debug, Clone, Serialize)]
pub struct IssueListResponse {
    pub items: Vec<IssueSummary>,
    pub total: usize,
}

/// Query parameters for `GET /api/v1/issues`.
#[derive(Debug, Clone, Deserialize)]
pub struct IssueListQuery {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    #[serde(default)]
    pub parent_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Comments — request types
// ---------------------------------------------------------------------------

/// Valid comment kind values.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CommentKind {
    #[default]
    Note,
    Plan,
    Decision,
    Observation,
    Blocker,
    Resolution,
    Result,
    Intervention,
}

impl std::fmt::Display for CommentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Note => write!(f, "note"),
            Self::Plan => write!(f, "plan"),
            Self::Decision => write!(f, "decision"),
            Self::Observation => write!(f, "observation"),
            Self::Blocker => write!(f, "blocker"),
            Self::Resolution => write!(f, "resolution"),
            Self::Result => write!(f, "result"),
            Self::Intervention => write!(f, "intervention"),
        }
    }
}

/// Request body for `POST /api/v1/issues/:id/comments`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateCommentRequest {
    pub content: String,
    #[serde(default)]
    pub kind: CommentKind,
    /// For `kind = "intervention"` comments.
    #[serde(default)]
    pub trigger_type: Option<String>,
    #[serde(default)]
    pub intervention_context: Option<String>,
}

// ---------------------------------------------------------------------------
// Labels — request types
// ---------------------------------------------------------------------------

/// Request body for `POST /api/v1/issues/:id/labels`.
#[derive(Debug, Clone, Deserialize)]
pub struct AddLabelRequest {
    pub label: String,
}

// ---------------------------------------------------------------------------
// Dependencies — request types
// ---------------------------------------------------------------------------

/// Request body for `POST /api/v1/issues/:id/block`.
#[derive(Debug, Clone, Deserialize)]
pub struct AddBlockerRequest {
    /// ID of the issue that blocks `:id`.
    pub blocker_id: i64,
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// Request body for `POST /api/v1/sessions/start`.
#[derive(Debug, Clone, Deserialize)]
pub struct StartSessionRequest {
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// Request body for `POST /api/v1/sessions/end`.
#[derive(Debug, Clone, Deserialize)]
pub struct EndSessionRequest {
    #[serde(default)]
    pub notes: Option<String>,
}

/// Request body for `POST /api/v1/sessions/work/:id`.
///
/// The issue ID is taken from the URL path parameter.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkOnIssueRequest {
    /// Optional: `agent_id` to scope the session lookup.
    #[serde(default)]
    pub agent_id: Option<String>,
}

/// Response for session endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct SessionResponse {
    #[serde(flatten)]
    pub session: Session,
}

// ---------------------------------------------------------------------------
// Milestones
// ---------------------------------------------------------------------------

/// Compact milestone info embedded in `IssueDetail`.
#[derive(Debug, Clone, Serialize)]
pub struct MilestoneSummary {
    pub id: i64,
    pub name: String,
    pub status: crate::models::IssueStatus,
}

/// Request body for `POST /api/v1/milestones`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateMilestoneRequest {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

/// Request body for `POST /api/v1/milestones/:id/assign`.
#[derive(Debug, Clone, Deserialize)]
pub struct AssignMilestoneRequest {
    pub issue_id: i64,
}

/// Milestone with progress statistics.
#[derive(Debug, Clone, Serialize)]
pub struct MilestoneDetail {
    #[serde(flatten)]
    pub milestone: Milestone,
    pub issue_count: usize,
    pub completed_count: usize,
    /// Percentage of issues closed (0–100).
    pub progress_percent: f64,
}

/// Paginated milestone list response.
#[derive(Debug, Clone, Serialize)]
pub struct MilestoneListResponse {
    pub items: Vec<MilestoneDetail>,
    pub total: usize,
}

/// Query parameters for `GET /api/v1/milestones`.
#[derive(Debug, Clone, Deserialize)]
pub struct MilestoneListQuery {
    #[serde(default)]
    pub status: Option<String>,
}

// ---------------------------------------------------------------------------
// Knowledge pages
// ---------------------------------------------------------------------------

/// Source reference within a knowledge page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeSource {
    pub url: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessed_at: Option<String>,
}

/// Full knowledge page with content.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgePage {
    pub slug: String,
    pub title: String,
    pub tags: Vec<String>,
    pub sources: Vec<KnowledgeSource>,
    pub contributors: Vec<String>,
    pub created: String,
    pub updated: String,
    pub content: String,
}

/// Lightweight knowledge page summary for list views.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgePageSummary {
    pub slug: String,
    pub title: String,
    pub tags: Vec<String>,
    pub updated: String,
}

/// Request body for `POST /api/v1/knowledge`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateKnowledgePageRequest {
    pub slug: String,
    pub title: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub sources: Vec<KnowledgeSource>,
}

/// Query parameters for `GET /api/v1/knowledge/search`.
#[derive(Debug, Clone, Deserialize)]
pub struct KnowledgeSearchQuery {
    pub q: String,
}

/// A single search match within a knowledge page.
#[derive(Debug, Clone, Serialize)]
pub struct KnowledgeSearchMatch {
    pub slug: String,
    pub title: String,
    pub line_number: usize,
    pub context_lines: Vec<(usize, String)>,
}

// ---------------------------------------------------------------------------
// Agents and monitoring
// ---------------------------------------------------------------------------

/// Agent status computed from heartbeat staleness.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// Heartbeat received within the active window (< 5 minutes).
    Active,
    /// Heartbeat received 5–30 minutes ago.
    Idle,
    /// Heartbeat received more than 30 minutes ago.
    Stale,
    /// No heartbeat file found.
    Unknown,
}

/// Per-agent summary returned in the agent list.
#[derive(Debug, Clone, Serialize)]
pub struct AgentSummary {
    pub agent_id: String,
    pub machine_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: AgentStatus,
    pub last_heartbeat: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_issue_id: Option<i64>,
    /// Git branch the agent is working on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Path to the agent's git worktree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// Issues currently locked by this agent.
    pub locks: Vec<i64>,
}

/// Detailed agent view with heartbeat history.
#[derive(Debug, Clone, Serialize)]
pub struct AgentDetail {
    #[serde(flatten)]
    pub summary: AgentSummary,
    /// Recent heartbeat timestamps (newest first, up to 24h).
    pub heartbeat_history: Vec<DateTime<Utc>>,
    /// Content of the agent's `.kickoff-status` file, if present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kickoff_status: Option<String>,
}

/// Lock entry as returned by the API, with derived age.
#[derive(Debug, Clone, Serialize)]
pub struct LockEntry {
    /// Crosslink issue ID this lock is held on.
    pub issue_id: i64,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub claimed_at: DateTime<Utc>,
    pub signed_by: String,
    /// Seconds since the lock was claimed.
    pub age_seconds: i64,
    pub is_stale: bool,
}

// ---------------------------------------------------------------------------
// Sync
// ---------------------------------------------------------------------------

/// Response for `GET /api/v1/sync/status`.
#[derive(Debug, Clone, Serialize)]
pub struct SyncStatusResponse {
    pub hub_initialized: bool,
    pub hub_branch: String,
    pub remote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_fetch_at: Option<DateTime<Utc>>,
    pub active_lock_count: usize,
    pub stale_lock_count: usize,
}

/// Response for `POST /api/v1/sync/fetch` and `POST /api/v1/sync/push`.
#[derive(Debug, Clone, Serialize)]
pub struct SyncActionResponse {
    pub success: bool,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Full config as returned by `GET /api/v1/config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigResponse {
    pub tracking_mode: String,
    pub stale_lock_timeout_minutes: u64,
    pub remote: String,
    pub signing_enforcement: String,
    pub intervention_tracking: bool,
    pub auto_steal_stale_locks: bool,
}

/// Partial config update for `PATCH /api/v1/config`.
///
/// All fields are optional — only provided fields are updated.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateConfigRequest {
    #[serde(default)]
    pub tracking_mode: Option<String>,
    #[serde(default)]
    pub stale_lock_timeout_minutes: Option<u64>,
    #[serde(default)]
    pub remote: Option<String>,
    #[serde(default)]
    pub signing_enforcement: Option<String>,
    #[serde(default)]
    pub intervention_tracking: Option<bool>,
    #[serde(default)]
    pub auto_steal_stale_locks: Option<bool>,
}

// ---------------------------------------------------------------------------
// Token usage
// ---------------------------------------------------------------------------

pub use crate::db::UsageSummaryRow;
pub use crate::models::TokenUsage;

/// Request body for `POST /api/v1/usage`.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTokenUsageRequest {
    pub agent_id: String,
    #[serde(default)]
    pub session_id: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: Option<i64>,
    #[serde(default)]
    pub cache_creation_tokens: Option<i64>,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default)]
    pub cost_estimate: Option<f64>,
}

fn default_model() -> String {
    "unknown".to_string()
}

/// Query parameters for `GET /api/v1/usage`.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenUsageListQuery {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<i64>,
    #[serde(default)]
    pub model: Option<String>,
    /// ISO 8601 timestamp — return records from this time onwards.
    #[serde(default)]
    pub from: Option<String>,
    /// ISO 8601 timestamp — return records up to this time.
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Query parameters for `GET /api/v1/usage/summary`.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenUsageSummaryQuery {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

/// Paginated token usage list response.
#[derive(Debug, Clone, Serialize)]
pub struct TokenUsageListResponse {
    pub items: Vec<TokenUsage>,
    pub total: usize,
}

/// Aggregated usage summary response.
#[derive(Debug, Clone, Serialize)]
pub struct TokenUsageSummaryResponse {
    pub items: Vec<UsageSummaryRow>,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
}

// ---------------------------------------------------------------------------
// Orchestrator — re-exported from canonical definitions in orchestrator::models (#480)
// ---------------------------------------------------------------------------

pub use crate::orchestrator::models::OrchestratorPlan;

/// Request body for `POST /api/v1/orchestrator/decompose`.
#[derive(Debug, Clone, Deserialize)]
pub struct DecomposeRequest {
    /// Markdown content of the design document to decompose.
    pub document: String,
    /// Optional slug to identify the document.
    #[serde(default)]
    pub slug: Option<String>,
}

/// Per-stage execution status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StageStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
    /// Reserved for dashboard display. The orchestrator engine uses `Pending`
    /// combined with DAG dependency checking instead of an explicit Blocked
    /// state, but the frontend uses this variant for visualization (#488).
    Blocked,
}

/// Overall orchestration execution status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionState {
    Idle,
    Running,
    Paused,
    Done,
    Failed,
}

/// Real-time execution progress returned by `GET /api/v1/orchestrator/status`.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionStatus {
    pub plan_id: String,
    pub state: ExecutionState,
    pub current_phase_id: Option<String>,
    pub progress_percent: f64,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Map from `stage_id` → `StageStatus`.
    pub stage_statuses: std::collections::HashMap<String, StageStatus>,
    /// Map from `stage_id` → `agent_id` for running stages.
    pub stage_agents: std::collections::HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// WebSocket messages
// ---------------------------------------------------------------------------

/// All messages sent over the `/ws` WebSocket connection use this envelope.
///
/// The `type` field determines which variant is present. Matching TypeScript
/// discriminated union: `WsMessage` in `dashboard/src/lib/types.ts`.
/// Discriminant for WebSocket event types.
///
/// Used as the `type` field in all `WsEvent` structs so the event type is
/// derived from the enum variant rather than a hand-written string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WsEventType {
    Heartbeat,
    AgentStatus,
    IssueUpdated,
    LockChanged,
    ExecutionProgress,
}

/// Server → Client: a new agent heartbeat was received.
#[derive(Debug, Clone, Serialize)]
pub struct WsHeartbeatEvent {
    /// Always serializes as `"heartbeat"`.
    #[serde(rename = "type")]
    pub event_type: WsEventType,
    pub agent_id: String,
    pub timestamp: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_issue_id: Option<i64>,
}

/// Server → Client: an agent's derived status changed.
#[derive(Debug, Clone, Serialize)]
pub struct WsAgentStatusEvent {
    /// Always serializes as `"agent_status"`.
    #[serde(rename = "type")]
    pub event_type: WsEventType,
    pub agent_id: String,
    pub status: AgentStatus,
}

/// Server → Client: an issue was created, updated, or closed.
#[derive(Debug, Clone, Serialize)]
pub struct WsIssueUpdatedEvent {
    /// Always serializes as `"issue_updated"`.
    #[serde(rename = "type")]
    pub event_type: WsEventType,
    pub issue_id: i64,
    /// Which field changed, e.g. "status", "title", "labels".
    pub field: String,
}

/// Server → Client: a lock was claimed or released.
#[derive(Debug, Clone, Serialize)]
pub struct WsLockChangedEvent {
    /// Always serializes as `"lock_changed"`.
    #[serde(rename = "type")]
    pub event_type: WsEventType,
    pub issue_id: i64,
    pub action: LockAction,
    pub agent_id: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LockAction {
    Claimed,
    Released,
}

/// Server → Client: orchestration stage progress changed.
#[derive(Debug, Clone, Serialize)]
pub struct WsExecutionProgressEvent {
    /// Always serializes as `"execution_progress"`.
    #[serde(rename = "type")]
    pub event_type: WsEventType,
    pub plan_id: String,
    pub phase_id: String,
    pub stage_id: String,
    pub status: StageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// Client → Server: subscribe to specific event channels.
///
/// Send this after connecting to filter which events are received.
/// Omitting this message means the client receives all events.
#[derive(Debug, Clone, Deserialize)]
pub struct WsSubscribeMessage {
    #[serde(rename = "type")]
    pub message_type: String, // always "subscribe"
    /// Channels to subscribe to. Valid values: "agents", "issues", "locks", "execution".
    pub channels: Vec<String>,
}

// ---------------------------------------------------------------------------
// Generic API wrapper
// ---------------------------------------------------------------------------

/// Standard API error response.
#[derive(Debug, Clone, Serialize)]
pub struct ApiError {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Standard success response for mutations that don't return data.
#[derive(Debug, Clone, Serialize)]
pub struct OkResponse {
    pub ok: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_health_response_serializes() {
        let r = HealthResponse {
            status: "ok".to_string(),
            version: "0.4.0".to_string(),
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"status\":\"ok\""));
        assert!(json.contains("\"version\":\"0.4.0\""));
    }

    #[test]
    fn test_create_issue_request_deserializes() {
        let json = r#"{"title": "Fix bug", "priority": "high"}"#;
        let req: CreateIssueRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.title, "Fix bug");
        assert_eq!(req.priority, ApiPriority::High);
        assert!(req.description.is_none());
        assert!(req.parent_id.is_none());
    }

    #[test]
    fn test_create_issue_request_default_priority() {
        let json = r#"{"title": "Fix bug"}"#;
        let req: CreateIssueRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.priority, ApiPriority::Medium);
    }

    #[test]
    fn test_update_issue_request_all_optional() {
        let json = r#"{}"#;
        let req: UpdateIssueRequest = serde_json::from_str(json).unwrap();
        assert!(req.title.is_none());
        assert!(req.description.is_none());
        assert!(req.priority.is_none());
    }

    #[test]
    fn test_agent_status_serializes_lowercase() {
        let json = serde_json::to_string(&AgentStatus::Active).unwrap();
        assert_eq!(json, "\"active\"");
        let json = serde_json::to_string(&AgentStatus::Stale).unwrap();
        assert_eq!(json, "\"stale\"");
    }

    #[test]
    fn test_stage_status_round_trip() {
        let statuses = [
            StageStatus::Pending,
            StageStatus::Running,
            StageStatus::Done,
            StageStatus::Failed,
        ];
        for status in &statuses {
            let json = serde_json::to_string(status).unwrap();
            let parsed: StageStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, &parsed);
        }
    }

    #[test]
    fn test_ws_heartbeat_event_serializes() {
        let event = WsHeartbeatEvent {
            event_type: WsEventType::Heartbeat,
            agent_id: "worker-1".to_string(),
            timestamp: Utc::now(),
            active_issue_id: Some(42),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"type\":\"heartbeat\""));
        assert!(json.contains("\"agent_id\":\"worker-1\""));
        assert!(json.contains("\"active_issue_id\":42"));
    }

    #[test]
    fn test_ws_heartbeat_event_skips_null_issue() {
        let event = WsHeartbeatEvent {
            event_type: WsEventType::Heartbeat,
            agent_id: "worker-1".to_string(),
            timestamp: Utc::now(),
            active_issue_id: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("active_issue_id"));
    }

    #[test]
    fn test_lock_action_serializes() {
        assert_eq!(
            serde_json::to_string(&LockAction::Claimed).unwrap(),
            "\"claimed\""
        );
        assert_eq!(
            serde_json::to_string(&LockAction::Released).unwrap(),
            "\"released\""
        );
    }

    #[test]
    fn test_orchestrator_plan_round_trip() {
        let plan = OrchestratorPlan {
            id: "plan-1".to_string(),
            document_slug: "my-doc".to_string(),
            phases: vec![],
            created_at: Utc::now(),
            total_stages: 0,
            estimated_hours: 0.0,
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: OrchestratorPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "plan-1");
        assert_eq!(parsed.document_slug, "my-doc");
    }

    #[test]
    fn test_api_error_serializes() {
        let err = ApiError {
            error: "not found".to_string(),
            detail: Some("Issue #999 does not exist".to_string()),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"error\":\"not found\""));
        assert!(json.contains("\"detail\""));
    }

    #[test]
    fn test_api_error_skips_null_detail() {
        let err = ApiError {
            error: "bad request".to_string(),
            detail: None,
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(!json.contains("detail"));
    }

    #[test]
    fn test_config_response_round_trip() {
        let config = ConfigResponse {
            tracking_mode: "strict".to_string(),
            stale_lock_timeout_minutes: 60,
            remote: "origin".to_string(),
            signing_enforcement: "audit".to_string(),
            intervention_tracking: true,
            auto_steal_stale_locks: false,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ConfigResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tracking_mode, "strict");
        assert_eq!(parsed.stale_lock_timeout_minutes, 60);
    }

    #[test]
    fn test_create_comment_request_default_kind() {
        let json = r#"{"content": "A comment"}"#;
        let req: CreateCommentRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.kind, CommentKind::Note);
    }

    #[test]
    fn test_knowledge_source_round_trip() {
        let source = KnowledgeSource {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            accessed_at: Some("2026-03-01".to_string()),
        };
        let json = serde_json::to_string(&source).unwrap();
        let parsed: KnowledgeSource = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.url, "https://example.com");
        assert_eq!(parsed.accessed_at, Some("2026-03-01".to_string()));
    }

    #[test]
    fn test_ws_subscribe_message_deserializes() {
        let json = r#"{"type": "subscribe", "channels": ["agents", "issues"]}"#;
        let msg: WsSubscribeMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.message_type, "subscribe");
        assert_eq!(msg.channels, vec!["agents", "issues"]);
    }
}
