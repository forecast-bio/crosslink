// Swarm data model types.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::sync::SyncManager;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Top-level swarm plan, stored at `swarm/plan.json` on the hub branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmPlan {
    pub schema_version: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_doc: Option<String>,
    pub created_at: String,
    pub phases: Vec<String>,
}

/// Definition of a single phase, stored at `swarm/phases/<name>.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhaseDefinition {
    pub name: String,
    pub status: PhaseStatus,
    pub agents: Vec<AgentEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl std::fmt::Display for PhaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhaseStatus::Pending => write!(f, "pending"),
            PhaseStatus::InProgress => write!(f, "in progress"),
            PhaseStatus::Completed => write!(f, "completed"),
            PhaseStatus::Failed => write!(f, "failed"),
        }
    }
}

/// An agent within a phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEntry {
    pub slug: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Planned,
    Running,
    Completed,
    Merged,
    Failed,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Planned => write!(f, "planned"),
            AgentStatus::Running => write!(f, "running"),
            AgentStatus::Completed => write!(f, "completed"),
            AgentStatus::Merged => write!(f, "merged"),
            AgentStatus::Failed => write!(f, "failed"),
        }
    }
}

/// Gate result recorded after all phase agents complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateResult {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_at: Option<String>,
}

/// Checkpoint snapshot after a phase (or partial phase) completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Checkpoint {
    pub phase: String,
    pub created_at: String,
    pub agents_merged: Vec<String>,
    pub agents_pending: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev_branch_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_result: Option<TestResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestResult {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
}

/// Budget configuration stored at `swarm/budget.json` on the hub branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetConfig {
    pub budget_window_s: u64,
    pub model: String,
}

/// Default: 5-hour window, opus model (#521).
impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            budget_window_s: 18000,
            model: "opus".to_string(),
        }
    }
}

/// Historical cost log stored at `swarm/history/cost-log.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CostLog {
    #[serde(default)]
    pub observations: Vec<CostObservation>,
    #[serde(default)]
    pub model_estimates: std::collections::HashMap<String, ModelEstimate>,
}

/// A single historical observation from a completed agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostObservation {
    pub agent_id: String,
    pub model: String,
    pub duration_s: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
}

/// Aggregate duration estimates for a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelEstimate {
    pub median_duration_s: u64,
    pub p90_duration_s: u64,
}

/// Budget estimation result for a phase.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetRecommendation {
    Proceed,
    ProceedWithCaution,
    Split { recommended_count: usize },
    Block { reason: String },
}

/// A budget window in a multi-window plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowAllocation {
    pub window_index: usize,
    pub phases: Vec<WindowPhase>,
    pub total_estimate_s: u64,
    pub buffer_s: u64,
    pub stop_point: String,
}

/// A phase allocated to a window, with its estimated cost.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowPhase {
    pub name: String,
    pub agent_count: usize,
    pub estimate_s: u64,
    pub fit: WindowFit,
}

/// How well a phase fits in the remaining window budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowFit {
    Fits,
    Tight,
    Overflow,
}

// ---------------------------------------------------------------------------
// Merge orchestration data model
// ---------------------------------------------------------------------------

/// Top-level merge plan, stored at `swarm/merge-plan.json` on the hub branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergePlan {
    pub target_branch: String,
    pub agents: Vec<MergeSource>,
    pub conflicts: Vec<FileConflict>,
    pub merge_order: Vec<String>, // agent slugs in application order
}

/// A single agent's worktree as a merge source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeSource {
    pub agent_slug: String,
    pub worktree_path: PathBuf,
    pub changed_files: Vec<String>,
    pub commit_count: usize,
}

/// A file conflict between multiple agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileConflict {
    pub file: String,
    pub agents: Vec<String>,
    pub conflict_type: ConflictType,
}

/// Classification of a file conflict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictType {
    /// Multiple agents modified the same file but different regions
    NonOverlapping,
    /// Multiple agents modified overlapping regions
    Overlapping,
    /// One agent created, another modified
    CreateModify,
}

// ---------------------------------------------------------------------------
// Multi-swarm context
// ---------------------------------------------------------------------------

/// Pointer to the active swarm, stored at `swarm/active.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ActiveSwarmRef {
    pub uuid: String,
    pub title: String,
    pub created_at: String,
}

/// Resolved swarm context -- holds the base path for the active swarm.
///
/// For legacy layouts (single `swarm/plan.json`), base is `"swarm"`.
/// For multi-swarm layouts, base is `"swarm/{uuid}"`.
pub(super) struct SwarmContext {
    pub base: String,
    pub is_legacy: bool,
}

impl SwarmContext {
    pub fn plan_path(&self) -> String {
        format!("{}/plan.json", self.base)
    }

    pub fn phase_path(&self, phase_name: &str) -> String {
        format!("{}/phases/{}.json", self.base, slugify_phase(phase_name))
    }

    pub fn checkpoints_dir(&self) -> String {
        format!("{}/checkpoints", self.base)
    }

    pub fn checkpoint_path(&self, slug: &str) -> String {
        format!("{}/checkpoints/{}.json", self.base, slug)
    }

    pub fn budget_path(&self) -> String {
        format!("{}/budget.json", self.base)
    }

    pub fn history_path(&self) -> String {
        format!("{}/history/cost-log.json", self.base)
    }
}

/// Resolve the active swarm context.
///
/// 1. `swarm/active.json` -> multi-swarm, base = `swarm/{uuid}`
/// 2. `swarm/plan.json` -> legacy, base = `swarm`
/// 3. Neither -> error
pub(super) fn resolve_swarm(sync: &SyncManager) -> anyhow::Result<SwarmContext> {
    use super::io::read_hub_json;
    use anyhow::bail;

    let active_path = sync.cache_path().join("swarm/active.json");
    if active_path.exists() {
        if let Ok(active) = read_hub_json::<ActiveSwarmRef>(sync, "swarm/active.json") {
            return Ok(SwarmContext {
                base: format!("swarm/{}", active.uuid),
                is_legacy: false,
            });
        }
    }

    let plan_path = sync.cache_path().join("swarm/plan.json");
    if plan_path.exists() {
        return Ok(SwarmContext {
            base: "swarm".to_string(),
            is_legacy: true,
        });
    }

    bail!("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")
}

/// Create a new swarm slot with a UUID, writing the active pointer.
pub(super) fn create_swarm_slot(sync: &SyncManager, title: &str) -> anyhow::Result<SwarmContext> {
    use super::io::write_hub_json;

    let uuid = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let active_ref = ActiveSwarmRef {
        uuid: uuid.clone(),
        title: title.to_string(),
        created_at: now,
    };

    write_hub_json(sync, "swarm/active.json", &active_ref)?;

    let base = format!("swarm/{}", uuid);
    let phases_dir = sync.cache_path().join(format!("{}/phases", base));
    std::fs::create_dir_all(&phases_dir)?;

    Ok(SwarmContext {
        base,
        is_legacy: false,
    })
}

/// Resolved runtime status of an agent, combining phase definition + worktree state.
#[derive(Serialize)]
pub(super) struct ResolvedAgent {
    pub slug: String,
    pub description: String,
    pub issue_id: Option<i64>,
    pub defined_status: AgentStatus,
    pub live_status: String,
    /// The actual branch name (e.g. `feature/<compact_name>`), if set.
    pub branch: Option<String>,
}

/// Slugify a phase name for use as a filename.
pub fn slugify_phase(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "")
}

// ---------------------------------------------------------------------------
// Review data model
// ---------------------------------------------------------------------------

/// The overall review plan stored at `swarm/review-plan.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPlan {
    pub mandate: String,
    pub mandate_prompt: String,
    pub agent_count: usize,
    pub created_at: String,
    pub agents: Vec<ReviewAgentAssignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_output: Option<PathBuf>,
}

/// Assignment of a partition to a review agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewAgentAssignment {
    pub agent_slug: String,
    pub partition_label: String,
    pub files: Vec<String>,
}

// ---------------------------------------------------------------------------
// Fix data model
// ---------------------------------------------------------------------------

/// Plan for parallel fix execution, stored at `swarm/fix-plan.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixPlan {
    pub schema_version: u32,
    pub created_at: String,
    pub issues: Vec<FixTarget>,
}

/// A single issue targeted for an agent fix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixTarget {
    pub issue_number: u64,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub agent_slug: String,
    pub status: AgentStatus,
}

/// An issue fetched from GitHub with its number, title, body, and labels.
pub(super) type LabeledIssue = (u64, String, String, Vec<String>);
