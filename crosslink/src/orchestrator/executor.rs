//! DAG execution engine — manages the lifecycle of an orchestrated plan.
//!
//! [`OrchestratorExecutor`] drives plan execution:
//! 1. Creates crosslink issues for each stage
//! 2. Sets up parent/child relationships (phase → stages)
//! 3. Sets up blocking dependencies between stages
//! 4. Creates milestones for each phase
//! 5. For each ready stage: launches an agent via `kickoff run`
//! 6. Monitors agent heartbeats and `.kickoff-status` for completion
//! 7. When a stage completes: advances the DAG and launches newly-unblocked stages
//! 8. When all stages in a phase complete: checks the phase gate
//! 9. Supports pause/resume — stops launching new stages but lets running ones finish
//!
//! Execution state is persisted to `.crosslink/orchestrator/execution.json` so it
//! survives process restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::db::Database;
use crate::orchestrator::dag::{Dag, DagNode};
use crate::orchestrator::models::{OrchestratorPlan, OrchestratorStage};
use crate::server::types::{
    ExecutionState, ExecutionStatus, StageStatus, WsExecutionProgressEvent,
};
use crate::server::ws::WsEvent;
use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::orchestrator::models::ORCHESTRATOR_DIR;
/// Filename for the persisted execution state.
const EXECUTION_FILE: &str = "execution.json";
/// Filename for the active plan.
const PLAN_FILE: &str = "plan.json";

/// Persisted execution state — serialized to disk and loaded on resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    /// ID of the plan being executed.
    pub plan_id: String,
    /// Overall execution state.
    pub state: ExecutionState,
    /// The full DAG with current node statuses.
    pub dag: Dag,
    /// Phase milestones: `phase_id` → `milestone_id`.
    pub phase_milestones: HashMap<String, i64>,
    /// Phase parent issues: `phase_id` → `issue_id` (parent issue for stage subissues).
    pub phase_issues: HashMap<String, i64>,
    /// When execution started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    /// When execution completed (if terminal).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    /// The ID of the phase currently executing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_phase_id: Option<String>,
}

/// The orchestrator execution engine.
///
/// Holds references to the database, the `.crosslink` directory (for file I/O),
/// and an optional WebSocket broadcast sender for real-time event push.
pub struct OrchestratorExecutor {
    /// Path to the `.crosslink` directory.
    crosslink_dir: PathBuf,
    /// Current execution snapshot (loaded from disk or created fresh).
    snapshot: ExecutionSnapshot,
}

impl OrchestratorExecutor {
    /// Directory where orchestrator state files are stored.
    fn state_dir(crosslink_dir: &Path) -> PathBuf {
        crosslink_dir.join(ORCHESTRATOR_DIR)
    }

    /// Path to the execution state file.
    fn execution_path(crosslink_dir: &Path) -> PathBuf {
        Self::state_dir(crosslink_dir).join(EXECUTION_FILE)
    }

    /// Path to the plan file.
    fn plan_path(crosslink_dir: &Path) -> PathBuf {
        Self::state_dir(crosslink_dir).join(PLAN_FILE)
    }

    /// Initialize a new execution from a decomposed plan.
    ///
    /// This builds the DAG from the plan's phases and stages, creates crosslink
    /// issues and milestones for each, and persists the initial state.
    ///
    /// # Errors
    ///
    /// Returns an error if creating issues, milestones, or the DAG fails,
    /// or if persisting the initial state fails.
    pub fn init(crosslink_dir: &Path, db: &Database, plan: &OrchestratorPlan) -> Result<Self> {
        let state_dir = Self::state_dir(crosslink_dir);
        std::fs::create_dir_all(&state_dir)
            .context("Failed to create orchestrator state directory")?;

        // Save the plan for reference.
        let plan_json = serde_json::to_string_pretty(plan)?;
        std::fs::write(Self::plan_path(crosslink_dir), plan_json)?;

        // Build DAG nodes from all stages across all phases.
        let mut dag_nodes = Vec::new();
        for phase in &plan.phases {
            for stage in &phase.stages {
                dag_nodes.push(DagNode {
                    id: stage.id.clone(),
                    title: stage.title.clone(),
                    status: StageStatus::Pending,
                    depends_on: stage.depends_on.clone(),
                    issue_id: None,
                    agent_id: None,
                    phase_id: phase.id.clone(),
                });
            }
        }

        let dag = Dag::from_nodes(&dag_nodes).context("Failed to build execution DAG from plan")?;

        // Create milestones and parent issues for each phase.
        let mut phase_milestones = HashMap::new();
        let mut phase_issues = HashMap::new();

        for phase in &plan.phases {
            // Create a milestone for the phase.
            let milestone_id = db
                .create_milestone(
                    &format!("[Orchestrator] {}", phase.title),
                    Some(&phase.description),
                )
                .context("Failed to create phase milestone")?;
            phase_milestones.insert(phase.id.clone(), milestone_id);

            // Create a parent issue for the phase.
            let phase_issue_id = db
                .create_issue(
                    &format!("[Phase] {}", phase.title),
                    Some(&phase.description),
                    "high",
                )
                .context("Failed to create phase parent issue")?;
            if let Err(e) = db.add_label(phase_issue_id, "orchestrator") {
                tracing::warn!("could not label phase issue #{phase_issue_id}: {e}");
            }
            if let Err(e) = db.add_label(phase_issue_id, "phase") {
                tracing::warn!("could not label phase issue #{phase_issue_id}: {e}");
            }
            phase_issues.insert(phase.id.clone(), phase_issue_id);
        }

        // Create sub-issues for each stage and set up dependencies.
        let mut dag = dag;
        Self::create_stage_issues_and_deps(db, plan, &phase_issues, &phase_milestones, &mut dag)?;

        let snapshot = ExecutionSnapshot {
            plan_id: plan.id.clone(),
            state: ExecutionState::Idle,
            dag,
            phase_milestones,
            phase_issues,
            started_at: None,
            completed_at: None,
            current_phase_id: None,
        };

        let executor = Self {
            crosslink_dir: crosslink_dir.to_path_buf(),
            snapshot,
        };

        executor.persist()?;
        Ok(executor)
    }

    /// Create sub-issues for each stage, assign them to milestones, and set
    /// up blocking dependencies in the database.
    fn create_stage_issues_and_deps(
        db: &Database,
        plan: &OrchestratorPlan,
        phase_issues: &HashMap<String, i64>,
        phase_milestones: &HashMap<String, i64>,
        dag: &mut Dag,
    ) -> Result<()> {
        let mut stage_issue_map: HashMap<String, i64> = HashMap::new();

        for phase in &plan.phases {
            let phase_issue_id = phase_issues[&phase.id];

            for stage in &phase.stages {
                let description = build_stage_description(stage);
                let issue_id = db
                    .create_subissue(
                        phase_issue_id,
                        &format!("[Stage] {}", stage.title),
                        Some(&description),
                        "high",
                    )
                    .context("Failed to create stage subissue")?;

                if let Err(e) = db.add_label(issue_id, "orchestrator") {
                    tracing::warn!("could not label stage issue #{issue_id}: {e}");
                }
                if let Err(e) = db.add_label(issue_id, "stage") {
                    tracing::warn!("could not label stage issue #{issue_id}: {e}");
                }

                let milestone_id = phase_milestones[&phase.id];
                if let Err(e) = db.add_issue_to_milestone(milestone_id, issue_id) {
                    tracing::warn!(
                        "could not add stage issue #{issue_id} to milestone #{milestone_id}: {e}"
                    );
                }

                stage_issue_map.insert(stage.id.clone(), issue_id);
                dag.set_issue_id(&stage.id, issue_id)?;
            }
        }

        for phase in &plan.phases {
            for stage in &phase.stages {
                let blocked_id = stage_issue_map[&stage.id];
                for dep_id in &stage.depends_on {
                    if let Some(&blocker_id) = stage_issue_map.get(dep_id) {
                        if let Err(e) = db.add_dependency(blocked_id, blocker_id) {
                            tracing::warn!(
                                "could not set dependency #{blocker_id} -> #{blocked_id}: {e}"
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Load a previously persisted execution state from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the execution state file cannot be read or parsed.
    pub fn load(crosslink_dir: &Path) -> Result<Self> {
        let path = Self::execution_path(crosslink_dir);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let snapshot: ExecutionSnapshot = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;

        Ok(Self {
            crosslink_dir: crosslink_dir.to_path_buf(),
            snapshot,
        })
    }

    /// Check whether an execution state file exists.
    #[must_use]
    pub fn exists(crosslink_dir: &Path) -> bool {
        Self::execution_path(crosslink_dir).exists()
    }

    /// Load the plan from disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the plan file cannot be read or parsed.
    pub fn load_plan(crosslink_dir: &Path) -> Result<OrchestratorPlan> {
        let path = Self::plan_path(crosslink_dir);
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Persist the current execution state to disk.
    fn persist(&self) -> Result<()> {
        let state_dir = Self::state_dir(&self.crosslink_dir);
        std::fs::create_dir_all(&state_dir)?;
        let json = serde_json::to_string_pretty(&self.snapshot)?;
        std::fs::write(Self::execution_path(&self.crosslink_dir), json)?;
        Ok(())
    }

    /// Get the current execution state.
    #[must_use]
    pub const fn state(&self) -> &ExecutionState {
        &self.snapshot.state
    }

    /// Get a reference to the DAG.
    #[must_use]
    pub const fn dag(&self) -> &Dag {
        &self.snapshot.dag
    }

    /// Get the plan ID.
    #[must_use]
    pub fn plan_id(&self) -> &str {
        &self.snapshot.plan_id
    }

    /// Build an [`ExecutionStatus`] response for the API.
    #[must_use]
    pub fn status(&self) -> ExecutionStatus {
        ExecutionStatus {
            plan_id: self.snapshot.plan_id.clone(),
            state: self.snapshot.state.clone(),
            current_phase_id: self.snapshot.current_phase_id.clone(),
            progress_percent: self.snapshot.dag.progress() * 100.0,
            started_at: self.snapshot.started_at,
            completed_at: self.snapshot.completed_at,
            stage_statuses: self.snapshot.dag.status_map(),
            stage_agents: self.snapshot.dag.agent_map(),
        }
    }

    /// Start execution. Changes state from Idle to Running and returns the list
    /// of stage IDs that are immediately ready to launch.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state does not allow starting, or if persisting fails.
    pub fn start(&mut self) -> Result<Vec<String>> {
        match self.snapshot.state {
            ExecutionState::Idle | ExecutionState::Paused => {}
            ref other => bail!("Cannot start execution — current state is {other:?}"),
        }

        self.snapshot.state = ExecutionState::Running;
        if self.snapshot.started_at.is_none() {
            self.snapshot.started_at = Some(Utc::now());
        }

        // Determine current phase from topological order.
        if self.snapshot.current_phase_id.is_none() {
            if let Ok(topo) = self.snapshot.dag.topological_sort() {
                if let Some(first_id) = topo.first() {
                    if let Some(node) = self.snapshot.dag.get(first_id) {
                        self.snapshot.current_phase_id = Some(node.phase_id.clone());
                    }
                }
            }
        }

        let ready = self.snapshot.dag.ready_nodes();
        self.persist()?;
        Ok(ready)
    }

    /// Pause execution. Running stages continue but no new ones are launched.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state is not `Running`, or if persisting fails.
    pub fn pause(&mut self) -> Result<()> {
        if self.snapshot.state != ExecutionState::Running {
            bail!("Cannot pause — current state is {:?}", self.snapshot.state);
        }
        self.snapshot.state = ExecutionState::Paused;
        self.persist()?;
        Ok(())
    }

    /// Resume a paused execution. Returns the list of stages ready to launch.
    ///
    /// # Errors
    ///
    /// Returns an error if the current state is not `Paused`, or if persisting fails.
    pub fn resume(&mut self) -> Result<Vec<String>> {
        if self.snapshot.state != ExecutionState::Paused {
            bail!("Cannot resume — current state is {:?}", self.snapshot.state);
        }
        self.snapshot.state = ExecutionState::Running;
        let ready = self.snapshot.dag.ready_nodes();
        self.persist()?;
        Ok(ready)
    }

    /// Build a `WsExecutionProgressEvent` for the given stage and status (#481).
    ///
    /// Centralizes event construction so executor methods don't couple directly
    /// to the WebSocket event shape.
    fn build_progress_event(
        &self,
        stage_id: &str,
        status: StageStatus,
    ) -> WsExecutionProgressEvent {
        let node = self.snapshot.dag.get(stage_id);
        WsExecutionProgressEvent {
            event_type: crate::server::types::WsEventType::ExecutionProgress,
            plan_id: self.snapshot.plan_id.clone(),
            phase_id: node.map(|n| n.phase_id.clone()).unwrap_or_default(),
            stage_id: stage_id.to_string(),
            status,
            agent_id: node.and_then(|n| n.agent_id.clone()),
        }
    }

    /// Verify that execution is in a state that allows stage mutations.
    ///
    /// Returns an error if the execution is not Running (#486).
    fn require_running_state(&self, action: &str) -> Result<()> {
        if self.snapshot.state != ExecutionState::Running {
            bail!(
                "Cannot {} — execution state is {:?}, must be Running",
                action,
                self.snapshot.state
            );
        }
        Ok(())
    }

    /// Record that a stage has been launched with the given agent ID.
    ///
    /// Returns an event to broadcast over WebSocket.
    ///
    /// # Errors
    ///
    /// Returns an error if the execution is not running, the stage is not found, or persisting fails.
    pub fn mark_stage_running(
        &mut self,
        stage_id: &str,
        agent_id: &str,
    ) -> Result<WsExecutionProgressEvent> {
        self.require_running_state("mark stage running")?;
        self.snapshot.dag.mark_running(stage_id, agent_id)?;

        // Update current phase if needed.
        if let Some(node) = self.snapshot.dag.get(stage_id) {
            self.snapshot.current_phase_id = Some(node.phase_id.clone());
        }

        self.persist()?;

        Ok(self.build_progress_event(stage_id, StageStatus::Running))
    }

    /// Record that a stage has completed successfully.
    ///
    /// Returns: (`newly_unblocked_stage_ids`, `ws_event`, `is_execution_complete`)
    ///
    /// # Errors
    ///
    /// Returns an error if the execution is not running, the stage transition is invalid, or persisting fails.
    pub fn mark_stage_done(
        &mut self,
        stage_id: &str,
        db: &Database,
    ) -> Result<(Vec<String>, WsExecutionProgressEvent, bool)> {
        self.require_running_state("mark stage done")?;
        let phase_id = self
            .snapshot
            .dag
            .get(stage_id)
            .map(|n| n.phase_id.clone())
            .unwrap_or_default();

        // Close the stage's crosslink issue.
        if let Some(issue_id) = self.snapshot.dag.get(stage_id).and_then(|n| n.issue_id) {
            if let Err(e) = db.close_issue(issue_id) {
                tracing::warn!("could not close stage issue #{issue_id}: {e}");
            }
        }

        let newly_ready = self.snapshot.dag.mark_done(stage_id)?;

        // Check if the phase is complete (all stages in this phase are done).
        let phase_complete = self.check_phase_complete(&phase_id);
        if phase_complete {
            if let Some(&milestone_id) = self.snapshot.phase_milestones.get(&phase_id) {
                if let Err(e) = db.close_milestone(milestone_id) {
                    tracing::warn!("could not close phase milestone #{milestone_id}: {e}");
                }
            }
            if let Some(&phase_issue_id) = self.snapshot.phase_issues.get(&phase_id) {
                if let Err(e) = db.close_issue(phase_issue_id) {
                    tracing::warn!("could not close phase issue #{phase_issue_id}: {e}");
                }
            }
        }

        // Check if the entire execution is complete.
        let execution_complete = self.snapshot.dag.is_complete();
        if execution_complete {
            self.snapshot.state = if self.snapshot.dag.has_failures() {
                ExecutionState::Failed
            } else {
                ExecutionState::Done
            };
            self.snapshot.completed_at = Some(Utc::now());
        }

        self.persist()?;

        let event = self.build_progress_event(stage_id, StageStatus::Done);

        Ok((newly_ready, event, execution_complete))
    }

    /// Record that a stage has failed.
    ///
    /// Returns a WebSocket event and whether the entire execution is now complete.
    ///
    /// # Errors
    ///
    /// Returns an error if the execution is not running, the stage transition is invalid, or persisting fails.
    pub fn mark_stage_failed(
        &mut self,
        stage_id: &str,
    ) -> Result<(WsExecutionProgressEvent, bool)> {
        self.require_running_state("mark stage failed")?;

        self.snapshot.dag.mark_failed(stage_id)?;

        // Check if the entire execution is now complete.
        let execution_complete = self.snapshot.dag.is_complete();
        if execution_complete {
            self.snapshot.state = ExecutionState::Failed;
            self.snapshot.completed_at = Some(Utc::now());
        }

        self.persist()?;

        let event = self.build_progress_event(stage_id, StageStatus::Failed);

        Ok((event, execution_complete))
    }

    /// Skip a stage (e.g. after a failure, to unblock downstream stages).
    ///
    /// # Errors
    ///
    /// Returns an error if the stage transition is invalid or persisting fails.
    pub fn skip_stage(
        &mut self,
        stage_id: &str,
    ) -> Result<(Vec<String>, WsExecutionProgressEvent)> {
        // Use mark_skipped_and_unblock which shares the same unblocking logic
        // as mark_done via find_newly_unblocked (#483).
        let newly_ready = self.snapshot.dag.mark_skipped_and_unblock(stage_id)?;

        self.persist()?;

        let event = self.build_progress_event(stage_id, StageStatus::Skipped);

        Ok((newly_ready, event))
    }

    /// Retry a failed stage by resetting it to pending.
    /// Returns the stage ID if it's immediately ready to launch.
    ///
    /// # Errors
    ///
    /// Returns an error if the stage is not found, is not in `Failed` state, or persisting fails.
    pub fn retry_stage(&mut self, stage_id: &str) -> Result<Option<String>> {
        let node = self
            .snapshot
            .dag
            .get_mut(stage_id)
            .ok_or_else(|| anyhow::anyhow!("Stage '{stage_id}' not found"))?;

        if node.status != StageStatus::Failed {
            bail!(
                "Cannot retry stage '{}' — status is {:?}, must be Failed",
                stage_id,
                node.status
            );
        }

        node.status = StageStatus::Pending;
        node.agent_id = None;

        // If execution was Failed due to this stage, reset to Running.
        if self.snapshot.state == ExecutionState::Failed {
            self.snapshot.state = ExecutionState::Running;
            self.snapshot.completed_at = None;
        }

        self.persist()?;

        // Check if it's immediately ready.
        let ready = self.snapshot.dag.ready_nodes();
        if ready.contains(&stage_id.to_string()) {
            Ok(Some(stage_id.to_string()))
        } else {
            Ok(None)
        }
    }

    /// Check the status of running agents by reading `.kickoff-status` files
    /// from their worktrees.
    ///
    /// Returns a list of (`stage_id`, `completion_status`) for stages whose agents
    /// have written a status file. The `completion_status` is the content of the file
    /// (e.g. "DONE", "`CI_FAILED`").
    #[must_use]
    pub fn poll_agent_status(&self, repo_root: &Path) -> Vec<(String, String)> {
        let mut completed = Vec::new();

        for stage_id in self.snapshot.dag.running_nodes() {
            if let Some(node) = self.snapshot.dag.get(&stage_id) {
                if let Some(ref agent_id) = node.agent_id {
                    // The agent's worktree is at <repo_root>/.worktrees/<slug>
                    // The agent_id has the format "parent--slug", extract the slug part.
                    let slug = agent_id.rsplit("--").next().unwrap_or(agent_id);
                    let status_file = repo_root
                        .join(".worktrees")
                        .join(slug)
                        .join(".kickoff-status");

                    if status_file.exists() {
                        if let Ok(content) = std::fs::read_to_string(&status_file) {
                            let status = content.trim().to_string();
                            if !status.is_empty() {
                                completed.push((stage_id.clone(), status));
                            }
                        }
                    }
                }
            }
        }

        completed
    }

    /// Broadcast a WebSocket event through the given sender (#493/#494).
    ///
    /// Import is scoped to the method body since it is the only consumer of
    /// `tokio::sync::broadcast` in this module.
    pub fn broadcast_event(
        tx: &tokio::sync::broadcast::Sender<WsEvent>,
        event: WsExecutionProgressEvent,
    ) {
        // INTENTIONAL: broadcast failure is harmless when no WebSocket subscribers are connected
        let _ = tx.send(WsEvent::ExecutionProgress(event));
    }

    /// Check whether all stages in a given phase are in a terminal state.
    fn check_phase_complete(&self, phase_id: &str) -> bool {
        let by_phase = self.snapshot.dag.stages_by_phase();
        by_phase.get(phase_id).is_none_or(|stage_ids| {
            stage_ids.iter().all(|id| {
                self.snapshot.dag.get(id).is_none_or(|n| {
                    matches!(
                        n.status,
                        StageStatus::Done | StageStatus::Failed | StageStatus::Skipped
                    )
                })
            })
        })
    }

    /// Get a snapshot of the current execution state (for serialization/API).
    #[must_use]
    pub const fn snapshot(&self) -> &ExecutionSnapshot {
        &self.snapshot
    }
}

/// Build a description string for a stage issue from the orchestrator stage definition.
fn build_stage_description(stage: &OrchestratorStage) -> String {
    use std::fmt::Write;
    let mut desc = stage.description.clone();
    if !stage.tasks.is_empty() {
        desc.push_str("\n\n## Tasks\n");
        for task in &stage.tasks {
            let _ = writeln!(desc, "- **{}**: {}", task.title, task.description);
        }
    }
    if !stage.depends_on.is_empty() {
        let _ = write!(
            desc,
            "\n## Dependencies\nBlocked by: {}\n",
            stage.depends_on.join(", ")
        );
    }
    let _ = write!(
        desc,
        "\n## Estimates\n- Complexity: {:.1} agent-hours\n- Suggested agents: {}\n",
        stage.complexity_hours, stage.agent_count
    );
    desc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::models::{OrchestratorPhase, OrchestratorStage, OrchestratorTask};
    use tempfile::TempDir;

    fn make_test_plan() -> OrchestratorPlan {
        OrchestratorPlan {
            id: "test-plan-1".to_string(),
            document_slug: "test-doc".to_string(),
            phases: vec![
                OrchestratorPhase {
                    id: "phase-1".to_string(),
                    title: "Skeleton".to_string(),
                    description: "Set up project skeleton".to_string(),
                    stages: vec![
                        OrchestratorStage {
                            id: "p1-server".to_string(),
                            title: "Rust axum server".to_string(),
                            description: "Create the axum server".to_string(),
                            tasks: vec![OrchestratorTask {
                                id: "t1".to_string(),
                                title: "Add Cargo deps".to_string(),
                                description: "Add axum, tower-http".to_string(),
                                complexity_hours: 0.5,
                            }],
                            depends_on: vec![],
                            agent_count: 1,
                            complexity_hours: 2.0,
                        },
                        OrchestratorStage {
                            id: "p1-frontend".to_string(),
                            title: "React scaffold".to_string(),
                            description: "Create React app".to_string(),
                            tasks: vec![],
                            depends_on: vec![],
                            agent_count: 1,
                            complexity_hours: 2.0,
                        },
                    ],
                    gate_criteria: vec!["Server boots".to_string()],
                },
                OrchestratorPhase {
                    id: "phase-2".to_string(),
                    title: "Agent Dashboard".to_string(),
                    description: "Build agent monitoring".to_string(),
                    stages: vec![
                        OrchestratorStage {
                            id: "p2-backend".to_string(),
                            title: "Agent REST endpoints".to_string(),
                            description: "Build agent API".to_string(),
                            tasks: vec![],
                            depends_on: vec!["p1-server".to_string()],
                            agent_count: 1,
                            complexity_hours: 2.0,
                        },
                        OrchestratorStage {
                            id: "p2-frontend".to_string(),
                            title: "Agent list view".to_string(),
                            description: "Build agent UI".to_string(),
                            tasks: vec![],
                            depends_on: vec!["p1-frontend".to_string(), "p2-backend".to_string()],
                            agent_count: 1,
                            complexity_hours: 3.0,
                        },
                    ],
                    gate_criteria: vec!["Agent cards render".to_string()],
                },
            ],
            created_at: Utc::now(),
            total_stages: 4,
            estimated_hours: 9.0,
        }
    }

    fn make_test_db(tmp: &TempDir) -> Database {
        let db_path = tmp.path().join("test.db");
        Database::open(&db_path).expect("Failed to open test database")
    }

    #[test]
    fn test_init_creates_files_and_issues() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();

        // State file should exist.
        assert!(OrchestratorExecutor::execution_path(&crosslink_dir).exists());
        assert!(OrchestratorExecutor::plan_path(&crosslink_dir).exists());

        // DAG should have 4 nodes.
        assert_eq!(executor.dag().len(), 4);

        // State should be idle.
        assert_eq!(executor.state(), &ExecutionState::Idle);

        // Phase milestones should be created.
        assert_eq!(executor.snapshot().phase_milestones.len(), 2);
        assert_eq!(executor.snapshot().phase_issues.len(), 2);

        // Issues should exist in the database.
        let issues = db.list_issues(Some("open"), None, None).unwrap();
        // 2 phase parent issues + 4 stage subissues = 6
        assert_eq!(issues.len(), 6);

        // Check that stage issues have the "orchestrator" label.
        for issue in &issues {
            let labels = db.get_labels(issue.id).unwrap();
            assert!(
                labels.contains(&"orchestrator".to_string()),
                "Issue #{} missing 'orchestrator' label",
                issue.id
            );
        }
    }

    #[test]
    fn test_init_sets_up_dependencies() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();

        // p2-backend depends on p1-server
        let p2_backend_issue = executor.dag().get("p2-backend").unwrap().issue_id.unwrap();
        let blockers = db.get_blockers(p2_backend_issue).unwrap();
        let p1_server_issue = executor.dag().get("p1-server").unwrap().issue_id.unwrap();
        assert!(blockers.contains(&p1_server_issue));

        // p2-frontend depends on p1-frontend and p2-backend
        let p2_frontend_issue = executor.dag().get("p2-frontend").unwrap().issue_id.unwrap();
        let blockers = db.get_blockers(p2_frontend_issue).unwrap();
        assert_eq!(blockers.len(), 2);
    }

    #[test]
    fn test_start_and_ready_stages() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();

        let ready = executor.start().unwrap();
        assert_eq!(executor.state(), &ExecutionState::Running);

        // p1-server and p1-frontend have no deps, so they're ready.
        assert_eq!(ready.len(), 2);
        assert!(ready.contains(&"p1-server".to_string()));
        assert!(ready.contains(&"p1-frontend".to_string()));
    }

    #[test]
    fn test_stage_lifecycle() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        // Mark p1-server running.
        let event = executor.mark_stage_running("p1-server", "agent-1").unwrap();
        assert_eq!(event.status, StageStatus::Running);
        assert_eq!(event.agent_id, Some("agent-1".to_string()));

        // Mark p1-server done → p2-backend should become ready.
        let (newly_ready, event, complete) = executor.mark_stage_done("p1-server", &db).unwrap();
        assert_eq!(event.status, StageStatus::Done);
        assert!(newly_ready.contains(&"p2-backend".to_string()));
        assert!(!complete);

        // p2-frontend is NOT ready yet (needs p1-frontend and p2-backend).
        assert!(!newly_ready.contains(&"p2-frontend".to_string()));
    }

    #[test]
    fn test_full_execution_to_completion() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        // Phase 1: launch and complete both stages.
        executor.mark_stage_running("p1-server", "agent-1").unwrap();
        executor
            .mark_stage_running("p1-frontend", "agent-2")
            .unwrap();

        let (_, _, _) = executor.mark_stage_done("p1-server", &db).unwrap();
        let (_, _, _) = executor.mark_stage_done("p1-frontend", &db).unwrap();

        // Phase 2: p2-backend is ready now.
        executor
            .mark_stage_running("p2-backend", "agent-3")
            .unwrap();
        let (newly_ready, _, _) = executor.mark_stage_done("p2-backend", &db).unwrap();
        assert!(newly_ready.contains(&"p2-frontend".to_string()));

        executor
            .mark_stage_running("p2-frontend", "agent-4")
            .unwrap();
        let (_, _, complete) = executor.mark_stage_done("p2-frontend", &db).unwrap();

        assert!(complete);
        assert_eq!(executor.state(), &ExecutionState::Done);
        assert!(executor.snapshot().completed_at.is_some());
        assert!((executor.status().progress_percent - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_pause_and_resume() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        executor.pause().unwrap();
        assert_eq!(executor.state(), &ExecutionState::Paused);

        let ready = executor.resume().unwrap();
        assert_eq!(executor.state(), &ExecutionState::Running);
        assert_eq!(ready.len(), 2); // p1-server and p1-frontend still ready
    }

    #[test]
    fn test_stage_failure_and_retry() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        executor.mark_stage_running("p1-server", "agent-1").unwrap();
        let (event, _) = executor.mark_stage_failed("p1-server").unwrap();
        assert_eq!(event.status, StageStatus::Failed);
        assert!(executor.dag().has_failures());

        // Retry the failed stage.
        let ready = executor.retry_stage("p1-server").unwrap();
        assert_eq!(ready, Some("p1-server".to_string()));
        assert_eq!(
            executor.dag().get("p1-server").unwrap().status,
            StageStatus::Pending
        );
    }

    #[test]
    fn test_skip_stage_unblocks_dependents() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        // Skip p1-server → p2-backend should eventually be unblockable
        let (newly_ready, event) = executor.skip_stage("p1-server").unwrap();
        assert_eq!(event.status, StageStatus::Skipped);

        // p2-backend depends only on p1-server, so it should now be ready
        assert!(newly_ready.contains(&"p2-backend".to_string()));
    }

    #[test]
    fn test_persist_and_reload() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();
        executor.mark_stage_running("p1-server", "agent-1").unwrap();

        // Reload from disk.
        let reloaded = OrchestratorExecutor::load(&crosslink_dir).unwrap();
        assert_eq!(reloaded.state(), &ExecutionState::Running);
        assert_eq!(reloaded.dag().len(), 4);
        assert_eq!(
            reloaded.dag().get("p1-server").unwrap().status,
            StageStatus::Running
        );
        assert_eq!(
            reloaded.dag().get("p1-server").unwrap().agent_id,
            Some("agent-1".to_string())
        );
    }

    #[test]
    fn test_status_api_response() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        let status = executor.status();
        assert_eq!(status.plan_id, "test-plan-1");
        assert_eq!(status.state, ExecutionState::Running);
        assert!(status.progress_percent < f64::EPSILON);
        assert_eq!(status.stage_statuses.len(), 4);
        assert!(status.stage_agents.is_empty());
    }

    #[test]
    fn test_poll_agent_status_finds_done_file() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();
        executor
            .mark_stage_running("p1-server", "driver--rust-axum-server")
            .unwrap();

        // Simulate the agent writing .kickoff-status
        let worktree = tmp.path().join(".worktrees").join("rust-axum-server");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".kickoff-status"), "DONE").unwrap();

        let completions = executor.poll_agent_status(tmp.path());
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].0, "p1-server");
        assert_eq!(completions[0].1, "DONE");
    }

    #[test]
    fn test_execution_fails_when_all_stages_terminal_with_failure() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);

        // Simple plan with two independent stages
        let plan = OrchestratorPlan {
            id: "fail-plan".to_string(),
            document_slug: "fail-doc".to_string(),
            phases: vec![OrchestratorPhase {
                id: "p1".to_string(),
                title: "Phase 1".to_string(),
                description: "Test phase".to_string(),
                stages: vec![
                    OrchestratorStage {
                        id: "s1".to_string(),
                        title: "Stage 1".to_string(),
                        description: "First".to_string(),
                        tasks: vec![],
                        depends_on: vec![],
                        agent_count: 1,
                        complexity_hours: 1.0,
                    },
                    OrchestratorStage {
                        id: "s2".to_string(),
                        title: "Stage 2".to_string(),
                        description: "Second".to_string(),
                        tasks: vec![],
                        depends_on: vec![],
                        agent_count: 1,
                        complexity_hours: 1.0,
                    },
                ],
                gate_criteria: vec![],
            }],
            created_at: Utc::now(),
            total_stages: 2,
            estimated_hours: 2.0,
        };

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        executor.mark_stage_running("s1", "agent-1").unwrap();
        executor.mark_stage_running("s2", "agent-2").unwrap();

        let (_, _) = executor.mark_stage_failed("s1").unwrap();
        let (_, _, complete) = executor.mark_stage_done("s2", &db).unwrap();

        assert!(complete);
        assert_eq!(executor.state(), &ExecutionState::Failed);
    }

    #[test]
    fn test_build_stage_description() {
        let stage = OrchestratorStage {
            id: "test".to_string(),
            title: "Test Stage".to_string(),
            description: "Base description".to_string(),
            tasks: vec![OrchestratorTask {
                id: "t1".to_string(),
                title: "Task 1".to_string(),
                description: "Do something".to_string(),
                complexity_hours: 0.5,
            }],
            depends_on: vec!["dep-1".to_string()],
            agent_count: 2,
            complexity_hours: 3.0,
        };

        let desc = build_stage_description(&stage);
        assert!(desc.contains("Base description"));
        assert!(desc.contains("Task 1"));
        assert!(desc.contains("dep-1"));
        assert!(desc.contains("3.0 agent-hours"));
        assert!(desc.contains("Suggested agents: 2"));
    }

    #[test]
    fn test_cannot_start_if_already_done() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);

        let plan = OrchestratorPlan {
            id: "simple".to_string(),
            document_slug: "doc".to_string(),
            phases: vec![OrchestratorPhase {
                id: "p1".to_string(),
                title: "Only phase".to_string(),
                description: "Single stage".to_string(),
                stages: vec![OrchestratorStage {
                    id: "s1".to_string(),
                    title: "Only stage".to_string(),
                    description: "Do it".to_string(),
                    tasks: vec![],
                    depends_on: vec![],
                    agent_count: 1,
                    complexity_hours: 1.0,
                }],
                gate_criteria: vec![],
            }],
            created_at: Utc::now(),
            total_stages: 1,
            estimated_hours: 1.0,
        };

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();
        executor.mark_stage_running("s1", "agent-1").unwrap();
        executor.mark_stage_done("s1", &db).unwrap();

        assert_eq!(executor.state(), &ExecutionState::Done);
        assert!(executor.start().is_err());
    }

    #[test]
    fn test_build_stage_description_no_tasks_no_deps() {
        let stage = OrchestratorStage {
            id: "test".to_string(),
            title: "Simple Stage".to_string(),
            description: "Just a description".to_string(),
            tasks: vec![],
            depends_on: vec![],
            agent_count: 1,
            complexity_hours: 1.0,
        };

        let desc = build_stage_description(&stage);
        assert!(desc.contains("Just a description"));
        // Should NOT contain Tasks or Dependencies sections
        assert!(!desc.contains("## Tasks"));
        assert!(!desc.contains("## Dependencies"));
        // Should always contain Estimates
        assert!(desc.contains("## Estimates"));
        assert!(desc.contains("1.0 agent-hours"));
        assert!(desc.contains("Suggested agents: 1"));
    }

    #[test]
    fn test_build_stage_description_multiple_tasks() {
        let stage = OrchestratorStage {
            id: "multi".to_string(),
            title: "Multi-task".to_string(),
            description: "Has tasks".to_string(),
            tasks: vec![
                OrchestratorTask {
                    id: "t1".to_string(),
                    title: "First".to_string(),
                    description: "Do first".to_string(),
                    complexity_hours: 1.0,
                },
                OrchestratorTask {
                    id: "t2".to_string(),
                    title: "Second".to_string(),
                    description: "Do second".to_string(),
                    complexity_hours: 2.0,
                },
            ],
            depends_on: vec![],
            agent_count: 2,
            complexity_hours: 3.0,
        };

        let desc = build_stage_description(&stage);
        assert!(desc.contains("## Tasks"));
        assert!(desc.contains("**First**: Do first"));
        assert!(desc.contains("**Second**: Do second"));
    }

    #[test]
    fn test_build_stage_description_multiple_deps() {
        let stage = OrchestratorStage {
            id: "deps".to_string(),
            title: "Dependent".to_string(),
            description: "Has dependencies".to_string(),
            tasks: vec![],
            depends_on: vec!["dep-a".to_string(), "dep-b".to_string()],
            agent_count: 1,
            complexity_hours: 2.0,
        };

        let desc = build_stage_description(&stage);
        assert!(desc.contains("## Dependencies"));
        assert!(desc.contains("dep-a, dep-b"));
    }

    #[test]
    fn test_pause_when_not_running_errors() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();

        // Should fail when idle
        let result = executor.pause();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot pause"));
    }

    #[test]
    fn test_resume_when_not_paused_errors() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        // Should fail when running (not paused)
        let result = executor.resume();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Cannot resume"));
    }

    #[test]
    fn test_retry_nonexistent_stage_errors() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        let result = executor.retry_stage("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_retry_non_failed_stage_errors() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        // p1-server is pending, not failed
        let result = executor.retry_stage("p1-server");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be Failed"));
    }

    #[test]
    fn test_retry_resets_execution_state_from_failed() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);

        // Simple single-stage plan
        let plan = OrchestratorPlan {
            id: "retry-test".to_string(),
            document_slug: "doc".to_string(),
            phases: vec![OrchestratorPhase {
                id: "p1".to_string(),
                title: "Phase 1".to_string(),
                description: "Test".to_string(),
                stages: vec![OrchestratorStage {
                    id: "s1".to_string(),
                    title: "Stage 1".to_string(),
                    description: "Do it".to_string(),
                    tasks: vec![],
                    depends_on: vec![],
                    agent_count: 1,
                    complexity_hours: 1.0,
                }],
                gate_criteria: vec![],
            }],
            created_at: Utc::now(),
            total_stages: 1,
            estimated_hours: 1.0,
        };

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();
        executor.mark_stage_running("s1", "agent-1").unwrap();
        let (_, execution_complete) = executor.mark_stage_failed("s1").unwrap();

        // The single stage failed, so the execution is now complete.
        assert!(execution_complete);
        assert_eq!(executor.state(), &ExecutionState::Failed);

        let ready = executor.retry_stage("s1").unwrap();
        assert_eq!(ready, Some("s1".to_string()));
        assert_eq!(
            executor.dag().get("s1").unwrap().status,
            StageStatus::Pending
        );
        // Agent should be cleared
        assert!(executor.dag().get("s1").unwrap().agent_id.is_none());
    }

    #[test]
    fn test_retry_blocked_stage_returns_none() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        // Fail p2-backend (which depends on p1-server)
        // First we need to mark it running to fail it. But it's blocked.
        // Mark it failed directly via dag manipulation.
        let (_, _) = executor.mark_stage_failed("p2-backend").unwrap();

        // Retry it - should return None since p1-server is still pending
        let ready = executor.retry_stage("p2-backend").unwrap();
        assert_eq!(ready, None);
    }

    #[test]
    fn test_exists_false_for_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        assert!(!OrchestratorExecutor::exists(&crosslink_dir));
    }

    #[test]
    fn test_exists_true_after_init() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        assert!(OrchestratorExecutor::exists(&crosslink_dir));
    }

    #[test]
    fn test_load_nonexistent_errors() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let result = OrchestratorExecutor::load(&crosslink_dir);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_plan_from_disk() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();

        let loaded_plan = OrchestratorExecutor::load_plan(&crosslink_dir).unwrap();
        assert_eq!(loaded_plan.id, "test-plan-1");
        assert_eq!(loaded_plan.phases.len(), 2);
        assert_eq!(loaded_plan.total_stages, 4);
    }

    #[test]
    fn test_poll_agent_status_no_running_stages() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        // No stages marked as running
        let completions = executor.poll_agent_status(tmp.path());
        assert!(completions.is_empty());
    }

    #[test]
    fn test_poll_agent_status_no_status_file() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();
        executor
            .mark_stage_running("p1-server", "driver--rust-server")
            .unwrap();

        // Worktree dir exists but no .kickoff-status file
        let worktree = tmp.path().join(".worktrees").join("rust-server");
        std::fs::create_dir_all(&worktree).unwrap();

        let completions = executor.poll_agent_status(tmp.path());
        assert!(completions.is_empty());
    }

    #[test]
    fn test_poll_agent_status_empty_status_file() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();
        executor
            .mark_stage_running("p1-server", "driver--rust-server")
            .unwrap();

        // Worktree dir with empty .kickoff-status
        let worktree = tmp.path().join(".worktrees").join("rust-server");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".kickoff-status"), "").unwrap();

        let completions = executor.poll_agent_status(tmp.path());
        assert!(completions.is_empty());
    }

    #[test]
    fn test_poll_agent_status_agent_id_without_double_dash() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();
        // Agent ID without "--" separator
        executor
            .mark_stage_running("p1-server", "simple-agent")
            .unwrap();

        let worktree = tmp.path().join(".worktrees").join("simple-agent");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".kickoff-status"), "DONE").unwrap();

        let completions = executor.poll_agent_status(tmp.path());
        assert_eq!(completions.len(), 1);
        assert_eq!(completions[0].1, "DONE");
    }

    #[test]
    fn test_mark_stage_running_updates_current_phase() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        executor.mark_stage_running("p1-server", "agent-1").unwrap();
        assert_eq!(
            executor.snapshot().current_phase_id,
            Some("phase-1".to_string())
        );
    }

    #[test]
    fn test_skip_stage_with_no_dependents() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);

        // Plan with a leaf stage (no dependents)
        let plan = OrchestratorPlan {
            id: "skip-test".to_string(),
            document_slug: "doc".to_string(),
            phases: vec![OrchestratorPhase {
                id: "p1".to_string(),
                title: "Phase 1".to_string(),
                description: "Test".to_string(),
                stages: vec![OrchestratorStage {
                    id: "s1".to_string(),
                    title: "Leaf stage".to_string(),
                    description: "No deps depend on me".to_string(),
                    tasks: vec![],
                    depends_on: vec![],
                    agent_count: 1,
                    complexity_hours: 1.0,
                }],
                gate_criteria: vec![],
            }],
            created_at: Utc::now(),
            total_stages: 1,
            estimated_hours: 1.0,
        };

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        let (newly_ready, event) = executor.skip_stage("s1").unwrap();
        assert!(newly_ready.is_empty());
        assert_eq!(event.status, StageStatus::Skipped);
    }

    #[test]
    fn test_start_from_paused_state() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();
        executor.pause().unwrap();

        // start() should also work from Paused state
        let ready = executor.start().unwrap();
        assert_eq!(executor.state(), &ExecutionState::Running);
        assert_eq!(ready.len(), 2);
    }

    #[test]
    fn test_plan_id_accessor() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        assert_eq!(executor.plan_id(), "test-plan-1");
    }

    #[test]
    fn test_phase_complete_check_via_skip() {
        let tmp = TempDir::new().unwrap();
        let crosslink_dir = tmp.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let db = make_test_db(&tmp);
        let plan = make_test_plan();

        let mut executor = OrchestratorExecutor::init(&crosslink_dir, &db, &plan).unwrap();
        executor.start().unwrap();

        // Complete phase 1 by running one and skipping the other
        executor.mark_stage_running("p1-server", "agent-1").unwrap();
        executor.mark_stage_done("p1-server", &db).unwrap();
        executor.skip_stage("p1-frontend").unwrap();

        // Phase 1 should be complete now (both stages terminal)
        // Verify via the status - phase milestones should reflect this
        // (internal check_phase_complete is called by mark_stage_done)
        let status = executor.status();
        // Progress should be 50% (2 of 4 stages done)
        assert!((status.progress_percent - 50.0).abs() < f64::EPSILON);
    }
}
