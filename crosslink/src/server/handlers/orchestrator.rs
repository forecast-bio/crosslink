//! Handlers for the design document orchestration endpoints.
//!
//! Implements:
//! - `POST /api/v1/orchestrator/decompose` — LLM-assisted doc → plan breakdown
//! - `GET  /api/v1/orchestrator/plan`      — get the current plan (if any)
//! - `GET  /api/v1/orchestrator/status`    — get execution status
//! - `POST /api/v1/orchestrator/execute`   — start/resume execution
//! - `POST /api/v1/orchestrator/pause`     — pause execution
//! - `POST /api/v1/orchestrator/stages/:id/retry` — retry a failed stage
//! - `POST /api/v1/orchestrator/stages/:id/skip`  — skip a stage

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};

use crate::orchestrator::{decompose, executor::OrchestratorExecutor};
use crate::server::{
    errors::{bad_request, internal_error, not_found},
    state::AppState,
    types::{ApiError, DecomposeRequest, ExecutionStatus, OrchestratorPlan},
};

/// Convert a progress percentage (0.0..=100.0) to a `u32`, clamping negatives to 0.
fn progress_to_u32(pct: f64) -> u32 {
    format!("{:.0}", pct.round().clamp(0.0, 100.0))
        .parse::<u32>()
        .unwrap_or(0)
}

fn conflict(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::CONFLICT,
        Json(ApiError {
            error: "conflict".to_string(),
            detail: Some(msg.into()),
        }),
    )
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/decompose
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/decompose` — decompose a design document.
///
/// Accepts a JSON body with `document` (markdown string) and optional `slug`.
/// Calls the Claude CLI to produce a structured phase/stage/task breakdown,
/// stores the resulting plan on disk, and returns it.
///
/// # Errors
/// Returns an error if the document is empty or decomposition fails.
pub async fn decompose_handler(
    State(state): State<AppState>,
    Json(body): Json<DecomposeRequest>,
) -> Result<Json<OrchestratorPlan>, (StatusCode, Json<ApiError>)> {
    if body.document.trim().is_empty() {
        return Err(bad_request(
            "document field is required and must not be empty",
        ));
    }

    let slug = body.slug.as_deref();

    let plan = decompose::decompose_document(&state.crosslink_dir, &body.document, slug)
        .await
        .map_err(|e| internal_error("decomposition failed", e))?;

    Ok(Json(plan))
}

// ---------------------------------------------------------------------------
// GET /api/v1/orchestrator/plan
// ---------------------------------------------------------------------------

/// `GET /api/v1/orchestrator/plan` — get the current plan, if any.
///
/// Returns the plan JSON or `null` if no plan has been decomposed yet.
///
/// # Errors
/// Returns an error if plan loading encounters a non-missing-file error.
pub async fn get_plan(
    State(state): State<AppState>,
) -> Result<Json<Option<OrchestratorPlan>>, (StatusCode, Json<ApiError>)> {
    Ok(Json(
        OrchestratorExecutor::load_plan(&state.crosslink_dir).ok(),
    ))
}

// ---------------------------------------------------------------------------
// GET /api/v1/orchestrator/status
// ---------------------------------------------------------------------------

/// `GET /api/v1/orchestrator/status` — get execution status.
///
/// Returns progress percentage and execution state. If no execution exists,
/// returns idle status with 0% progress.
///
/// # Errors
/// Returns an error if the execution state cannot be loaded.
pub async fn get_status(
    State(state): State<AppState>,
) -> Result<Json<ExecutionStatusResponse>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Ok(Json(ExecutionStatusResponse {
            status: "idle".to_string(),
            progress_pct: 0,
            detail: None,
        }));
    }

    let executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let full_status = executor.status();
    let progress_pct = progress_to_u32(full_status.progress_percent);
    let status_str = match full_status.state {
        crate::server::types::ExecutionState::Idle => "idle",
        crate::server::types::ExecutionState::Running => "running",
        crate::server::types::ExecutionState::Paused => "paused",
        crate::server::types::ExecutionState::Done => "done",
        crate::server::types::ExecutionState::Failed => "failed",
    };

    Ok(Json(ExecutionStatusResponse {
        status: status_str.to_string(),
        progress_pct,
        detail: Some(full_status),
    }))
}

/// Simplified status response matching what the frontend expects.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExecutionStatusResponse {
    pub status: String,
    pub progress_pct: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<ExecutionStatus>,
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/execute
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/execute` — start or resume execution.
///
/// # Errors
/// Returns an error if no plan exists, the execution state cannot be loaded, or
/// starting/resuming fails.
pub async fn execute(
    State(state): State<AppState>,
) -> Result<Json<ExecutionStatusResponse>, (StatusCode, Json<ApiError>)> {
    // If no execution exists, initialize from the current plan.
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        let plan = OrchestratorExecutor::load_plan(&state.crosslink_dir)
            .map_err(|e| not_found(format!("No plan found: {e}")))?;

        let db = state.db().await;
        let mut executor = OrchestratorExecutor::init(&state.crosslink_dir, &db, &plan)
            .map_err(|e| internal_error("Failed to initialize execution", e))?;
        drop(db);

        let _ready = executor
            .start()
            .map_err(|e| internal_error("Failed to start execution", e))?;

        let full_status = executor.status();
        return Ok(Json(ExecutionStatusResponse {
            status: "running".to_string(),
            progress_pct: progress_to_u32(full_status.progress_percent),
            detail: Some(full_status),
        }));
    }

    // Otherwise load and start/resume.
    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let _ready = executor
        .start()
        .map_err(|e| conflict(format!("Cannot start execution: {e}")))?;

    let full_status = executor.status();
    Ok(Json(ExecutionStatusResponse {
        status: "running".to_string(),
        progress_pct: progress_to_u32(full_status.progress_percent),
        detail: Some(full_status),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/pause
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/pause` — pause execution.
///
/// # Errors
/// Returns an error if no execution exists, the state cannot be loaded, or pausing fails.
pub async fn pause(
    State(state): State<AppState>,
) -> Result<Json<ExecutionStatusResponse>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    executor
        .pause()
        .map_err(|e| conflict(format!("Cannot pause: {e}")))?;

    let full_status = executor.status();
    Ok(Json(ExecutionStatusResponse {
        status: "paused".to_string(),
        progress_pct: progress_to_u32(full_status.progress_percent),
        detail: Some(full_status),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/stages/:id/retry
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/stages/:id/retry` — retry a failed stage.
///
/// # Errors
/// Returns an error if no execution exists, the state cannot be loaded, or the
/// stage cannot be retried.
pub async fn retry_stage(
    State(state): State<AppState>,
    Path(stage_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let ready = executor
        .retry_stage(&stage_id)
        .map_err(|e| bad_request(format!("Cannot retry stage: {e}")))?;

    Ok(Json(serde_json::json!({
        "ok": true,
        "stage_id": stage_id,
        "ready_to_launch": ready.is_some(),
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/stages/:id/skip
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/stages/:id/skip` — skip a stage.
///
/// # Errors
/// Returns an error if no execution exists, the state cannot be loaded, or the
/// stage cannot be skipped.
pub async fn skip_stage(
    State(state): State<AppState>,
    Path(stage_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let (newly_ready, event) = executor
        .skip_stage(&stage_id)
        .map_err(|e| bad_request(format!("Cannot skip stage: {e}")))?;

    // Broadcast the skip event over WebSocket.
    OrchestratorExecutor::broadcast_event(&state.ws_tx, event);

    Ok(Json(serde_json::json!({
        "ok": true,
        "stage_id": stage_id,
        "newly_ready": newly_ready,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/orchestrator/plans
// ---------------------------------------------------------------------------

/// `GET /api/v1/orchestrator/plans` — list all stored plan IDs.
///
/// # Errors
/// Returns an error if the plan directory cannot be read.
pub async fn list_plans_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<String>>, (StatusCode, Json<ApiError>)> {
    let plans = decompose::list_plans(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to list plans", e))?;
    Ok(Json(plans))
}

// ---------------------------------------------------------------------------
// GET /api/v1/orchestrator/plans/:id
// ---------------------------------------------------------------------------

/// `GET /api/v1/orchestrator/plans/:id` — retrieve a specific stored plan.
///
/// # Errors
/// Returns an error if the plan is not found or cannot be serialized.
pub async fn get_plan_by_id(
    State(state): State<AppState>,
    Path(plan_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    let plan = decompose::load_plan(&state.crosslink_dir, &plan_id)
        .map_err(|e| not_found(format!("Plan not found: {e}")))?;
    let json =
        serde_json::to_value(plan).map_err(|e| internal_error("Failed to serialize plan", e))?;
    Ok(Json(json))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/resume
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/resume` — resume a paused execution.
///
/// # Errors
/// Returns an error if no execution exists, the state cannot be loaded, or resuming fails.
pub async fn resume_execution(
    State(state): State<AppState>,
) -> Result<Json<ExecutionStatusResponse>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let _ready = executor
        .resume()
        .map_err(|e| conflict(format!("Cannot resume: {e}")))?;

    let full_status = executor.status();
    Ok(Json(ExecutionStatusResponse {
        status: "running".to_string(),
        progress_pct: progress_to_u32(full_status.progress_percent),
        detail: Some(full_status),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/stages/:id/running
// ---------------------------------------------------------------------------

/// Request body for marking a stage as running.
#[derive(Debug, serde::Deserialize)]
pub struct MarkRunningRequest {
    pub agent_id: String,
}

/// `POST /api/v1/orchestrator/stages/:id/running` — record agent launch for a stage.
///
/// # Errors
/// Returns an error if no execution exists, the state cannot be loaded, or the
/// stage cannot be marked as running.
pub async fn mark_stage_running_handler(
    State(state): State<AppState>,
    Path(stage_id): Path<String>,
    Json(body): Json<MarkRunningRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let event = executor
        .mark_stage_running(&stage_id, &body.agent_id)
        .map_err(|e| bad_request(format!("Cannot mark stage running: {e}")))?;

    OrchestratorExecutor::broadcast_event(&state.ws_tx, event);

    Ok(Json(serde_json::json!({
        "ok": true,
        "stage_id": stage_id,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/stages/:id/done
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/stages/:id/done` — record stage completion.
///
/// # Errors
/// Returns an error if no execution exists, the state cannot be loaded, or the
/// stage cannot be marked as done.
pub async fn mark_stage_done_handler(
    State(state): State<AppState>,
    Path(stage_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let db = state.db().await;
    let (newly_ready, event, complete) = executor
        .mark_stage_done(&stage_id, &db)
        .map_err(|e| bad_request(format!("Cannot mark stage done: {e}")))?;

    OrchestratorExecutor::broadcast_event(&state.ws_tx, event);

    Ok(Json(serde_json::json!({
        "ok": true,
        "stage_id": stage_id,
        "newly_ready": newly_ready,
        "execution_complete": complete,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/stages/:id/failed
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/stages/:id/failed` — record stage failure.
///
/// # Errors
/// Returns an error if no execution exists, the state cannot be loaded, or the
/// stage cannot be marked as failed.
pub async fn mark_stage_failed_handler(
    State(state): State<AppState>,
    Path(stage_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let (event, execution_complete) = executor
        .mark_stage_failed(&stage_id)
        .map_err(|e| bad_request(format!("Cannot mark stage failed: {e}")))?;

    OrchestratorExecutor::broadcast_event(&state.ws_tx, event);

    Ok(Json(serde_json::json!({
        "ok": true,
        "stage_id": stage_id,
        "execution_complete": execution_complete,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/orchestrator/agents/poll
// ---------------------------------------------------------------------------

/// `GET /api/v1/orchestrator/agents/poll` — poll running agent status files.
///
/// # Errors
/// Returns an error if no execution exists or the state cannot be loaded.
pub async fn poll_agents(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    // Repo root is the parent of .crosslink
    let repo_root = state.crosslink_dir.parent().unwrap_or(&state.crosslink_dir);
    let statuses = executor.poll_agent_status(repo_root);

    Ok(Json(serde_json::json!({
        "agents": statuses.iter().map(|(id, status)| serde_json::json!({
            "stage_id": id,
            "status": status,
        })).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/orchestrator/snapshot
// ---------------------------------------------------------------------------

/// `GET /api/v1/orchestrator/snapshot` — full execution state export with DAG details.
///
/// # Errors
/// Returns an error if no execution exists or the state cannot be loaded.
pub async fn get_snapshot(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let snapshot = executor.snapshot();
    let dag = executor.dag();
    let plan_id = executor.plan_id();
    let exec_state = executor.state();

    // Use DAG accessor methods for rich introspection
    let node_ids = dag.node_ids();
    let running = dag.running_nodes();
    let stage_count = dag.len();

    // Build dependency graph using dependents/dependencies accessors
    let dep_graph: serde_json::Map<String, serde_json::Value> = node_ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                serde_json::json!({
                    "dependents": dag.dependents(id),
                    "dependencies": dag.dependencies(id),
                    "status": dag.get(id).map(|n| format!("{:?}", n.status)),
                }),
            )
        })
        .collect();

    // Counts by status
    let pending = dag.nodes_with_status(&crate::server::types::StageStatus::Pending);
    let done = dag.nodes_with_status(&crate::server::types::StageStatus::Done);
    let failed = dag.nodes_with_status(&crate::server::types::StageStatus::Failed);

    Ok(Json(serde_json::json!({
        "plan_id": plan_id,
        "state": format!("{:?}", exec_state),
        "stage_count": stage_count,
        "is_empty": dag.is_empty(),
        "nodes": serde_json::to_value(dag.nodes()).unwrap_or_default(),
        "running": running,
        "pending_count": pending.len(),
        "done_count": done.len(),
        "failed_count": failed.len(),
        "dependency_graph": dep_graph,
        "snapshot": serde_json::to_value(snapshot).unwrap_or_default(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::orchestrator::{
        dag::{Dag, DagNode},
        executor::ExecutionSnapshot,
        models::{OrchestratorPhase, OrchestratorStage},
    };
    use crate::server::{
        routes::build_router,
        state::AppState,
        types::{ExecutionState, OrchestratorPlan, StageStatus},
    };
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
    };
    use serde_json::Value;
    use std::collections::HashMap;
    use tower::util::ServiceExt;

    fn test_app() -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Build a minimal test plan with one phase and one stage (no dependencies).
    fn make_simple_plan() -> OrchestratorPlan {
        OrchestratorPlan {
            id: "test-plan-1".to_string(),
            document_slug: "test-doc".to_string(),
            phases: vec![OrchestratorPhase {
                id: "phase-1".to_string(),
                title: "Phase One".to_string(),
                description: "First phase".to_string(),
                stages: vec![OrchestratorStage {
                    id: "stage-a".to_string(),
                    title: "Stage A".to_string(),
                    description: "Do A".to_string(),
                    tasks: vec![],
                    depends_on: vec![],
                    agent_count: 1,
                    complexity_hours: 1.0,
                }],
                gate_criteria: vec![],
            }],
            created_at: chrono::Utc::now(),
            total_stages: 1,
            estimated_hours: 1.0,
        }
    }

    /// Write a plan to `crosslink_dir/orchestrator/plan.json` so that
    /// `OrchestratorExecutor::load_plan` can find it.
    fn write_plan_file(crosslink_dir: &std::path::Path, plan: &OrchestratorPlan) {
        let orch_dir = crosslink_dir.join("orchestrator");
        std::fs::create_dir_all(&orch_dir).unwrap();
        let json = serde_json::to_string_pretty(plan).unwrap();
        std::fs::write(orch_dir.join("plan.json"), json).unwrap();
    }

    /// Write an execution snapshot JSON directly to disk (bypasses `init` to avoid
    /// database calls, letting us control the exact execution state).
    fn write_execution_snapshot(
        crosslink_dir: &std::path::Path,
        plan: &OrchestratorPlan,
        state: ExecutionState,
    ) {
        let orch_dir = crosslink_dir.join("orchestrator");
        std::fs::create_dir_all(&orch_dir).unwrap();

        // Build a minimal DAG from the plan.
        let nodes: Vec<DagNode> = plan
            .phases
            .iter()
            .flat_map(|ph| {
                ph.stages.iter().map(|s| DagNode {
                    id: s.id.clone(),
                    title: s.title.clone(),
                    status: StageStatus::Pending,
                    depends_on: s.depends_on.clone(),
                    issue_id: None,
                    agent_id: None,
                    phase_id: ph.id.clone(),
                })
            })
            .collect();
        let dag = Dag::from_nodes(&nodes).unwrap();

        let snapshot = ExecutionSnapshot {
            plan_id: plan.id.clone(),
            state,
            dag,
            phase_milestones: HashMap::new(),
            phase_issues: HashMap::new(),
            started_at: Some(chrono::Utc::now()),
            completed_at: None,
            current_phase_id: Some("phase-1".to_string()),
        };
        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        std::fs::write(orch_dir.join("execution.json"), json).unwrap();
    }

    /// Write an execution snapshot where a specific stage is in "Running" state.
    fn write_execution_with_running_stage(
        crosslink_dir: &std::path::Path,
        plan: &OrchestratorPlan,
        running_stage_id: &str,
        agent_id: &str,
    ) {
        let orch_dir = crosslink_dir.join("orchestrator");
        std::fs::create_dir_all(&orch_dir).unwrap();

        let nodes: Vec<DagNode> = plan
            .phases
            .iter()
            .flat_map(|ph| {
                ph.stages.iter().map(|s| {
                    let (status, aid) = if s.id == running_stage_id {
                        (StageStatus::Running, Some(agent_id.to_string()))
                    } else {
                        (StageStatus::Pending, None)
                    };
                    DagNode {
                        id: s.id.clone(),
                        title: s.title.clone(),
                        status,
                        depends_on: s.depends_on.clone(),
                        issue_id: None,
                        agent_id: aid,
                        phase_id: ph.id.clone(),
                    }
                })
            })
            .collect();
        let dag = Dag::from_nodes(&nodes).unwrap();

        let snapshot = ExecutionSnapshot {
            plan_id: plan.id.clone(),
            state: ExecutionState::Running,
            dag,
            phase_milestones: HashMap::new(),
            phase_issues: HashMap::new(),
            started_at: Some(chrono::Utc::now()),
            completed_at: None,
            current_phase_id: Some("phase-1".to_string()),
        };
        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        std::fs::write(orch_dir.join("execution.json"), json).unwrap();
    }

    /// Write an execution snapshot where a specific stage is in "Failed" state.
    fn write_execution_with_failed_stage(
        crosslink_dir: &std::path::Path,
        plan: &OrchestratorPlan,
        failed_stage_id: &str,
    ) {
        let orch_dir = crosslink_dir.join("orchestrator");
        std::fs::create_dir_all(&orch_dir).unwrap();

        let nodes: Vec<DagNode> = plan
            .phases
            .iter()
            .flat_map(|ph| {
                ph.stages.iter().map(|s| {
                    let status = if s.id == failed_stage_id {
                        StageStatus::Failed
                    } else {
                        StageStatus::Pending
                    };
                    DagNode {
                        id: s.id.clone(),
                        title: s.title.clone(),
                        status,
                        depends_on: s.depends_on.clone(),
                        issue_id: None,
                        agent_id: None,
                        phase_id: ph.id.clone(),
                    }
                })
            })
            .collect();
        let dag = Dag::from_nodes(&nodes).unwrap();

        let snapshot = ExecutionSnapshot {
            plan_id: plan.id.clone(),
            state: ExecutionState::Failed,
            dag,
            phase_milestones: HashMap::new(),
            phase_issues: HashMap::new(),
            started_at: Some(chrono::Utc::now()),
            completed_at: None,
            current_phase_id: Some("phase-1".to_string()),
        };
        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        std::fs::write(orch_dir.join("execution.json"), json).unwrap();
    }

    /// Create an app where a plan file exists at `orchestrator/plan.json`.
    fn test_app_with_plan(plan: &OrchestratorPlan) -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, plan);
        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir)
    }

    /// Create an app where an execution file exists (Running state, all stages pending).
    fn test_app_with_running_execution(
        plan: &OrchestratorPlan,
    ) -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, plan);
        write_execution_snapshot(&crosslink_dir, plan, ExecutionState::Running);
        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir)
    }

    /// Create an app where an execution file exists (Paused state).
    fn test_app_with_paused_execution(
        plan: &OrchestratorPlan,
    ) -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, plan);
        write_execution_snapshot(&crosslink_dir, plan, ExecutionState::Paused);
        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir)
    }

    #[tokio::test]
    async fn test_get_plan_no_plan() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/plan")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body.is_null());
    }

    #[tokio::test]
    async fn test_get_status_no_execution() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "idle");
        assert_eq!(body["progress_pct"], 0);
    }

    #[tokio::test]
    async fn test_execute_no_plan_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/execute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_pause_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/pause")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_decompose_empty_document() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/decompose")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({"document": ""}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_decompose_whitespace_only_document() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/decompose")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"document": "   \n\t  "}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("must not be empty"));
    }

    #[tokio::test]
    async fn test_retry_stage_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/some-stage/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_skip_stage_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/some-stage/skip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_resume_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_mark_stage_running_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/some-stage/running")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"agent_id": "agent-1"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_mark_stage_done_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/some-stage/done")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_mark_stage_failed_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/some-stage/failed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_poll_agents_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/agents/poll")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_snapshot_no_execution_returns_404() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/snapshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_list_plans_empty() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/plans")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_plan_by_id_not_found() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/plans/nonexistent-plan-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_status_returns_idle_fields() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "idle");
        assert_eq!(body["progress_pct"], 0);
        // idle status should not have a detail field
        assert!(body.get("detail").is_none());
    }

    #[tokio::test]
    async fn test_get_plan_returns_null_when_empty() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/plan")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // No plan has been created yet, so the response should be null
        assert!(body.is_null());
    }

    // -----------------------------------------------------------------------
    // Happy-path tests that require a plan or execution on disk
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_get_plan_returns_plan_when_exists() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_plan(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/plan")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(!body.is_null());
        assert_eq!(body["id"], "test-plan-1");
        assert_eq!(body["document_slug"], "test-doc");
    }

    #[tokio::test]
    async fn test_get_status_with_running_execution() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "running");
        // detail should be present when execution exists
        assert!(body.get("detail").is_some());
        let detail = &body["detail"];
        assert_eq!(detail["plan_id"], "test-plan-1");
        assert_eq!(detail["state"], "running");
    }

    #[tokio::test]
    async fn test_get_status_with_paused_execution() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_paused_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "paused");
    }

    #[tokio::test]
    async fn test_get_status_with_done_execution() {
        let plan = make_simple_plan();
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, &plan);
        write_execution_snapshot(&crosslink_dir, &plan, ExecutionState::Done);
        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "done");
    }

    #[tokio::test]
    async fn test_get_status_with_failed_execution() {
        let plan = make_simple_plan();
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, &plan);
        write_execution_snapshot(&crosslink_dir, &plan, ExecutionState::Failed);
        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "failed");
    }

    #[tokio::test]
    async fn test_get_status_with_idle_execution_state() {
        let plan = make_simple_plan();
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, &plan);
        write_execution_snapshot(&crosslink_dir, &plan, ExecutionState::Idle);
        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // Execution file exists, but state is "idle"
        assert_eq!(body["status"], "idle");
        assert!(body.get("detail").is_some());
    }

    #[tokio::test]
    async fn test_execute_with_plan_starts_execution() {
        // Start execution when no execution file exists but plan file does.
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_plan(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/execute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "running");
        assert!(body.get("detail").is_some());
    }

    #[tokio::test]
    async fn test_execute_when_already_running_returns_conflict() {
        // Attempting to start when already running should return 409 Conflict.
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/execute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // The executor's start() bails when state is Running → conflict
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_execute_resumes_paused_execution() {
        // Calling execute on a paused execution should transition it to Running.
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_paused_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/execute")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "running");
    }

    #[tokio::test]
    async fn test_pause_with_running_execution() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/pause")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "paused");
    }

    #[tokio::test]
    async fn test_pause_when_not_running_returns_conflict() {
        // Pausing a paused execution should fail.
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_paused_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/pause")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_resume_paused_execution() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_paused_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["status"], "running");
    }

    #[tokio::test]
    async fn test_resume_when_not_paused_returns_conflict() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/resume")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_mark_stage_running_success() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/stage-a/running")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"agent_id": "agent-xyz"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["stage_id"], "stage-a");
    }

    #[tokio::test]
    async fn test_mark_stage_running_unknown_stage_returns_bad_request() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/nonexistent-stage/running")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"agent_id": "agent-xyz"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_mark_stage_done_success() {
        let plan = make_simple_plan();
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, &plan);
        // Stage must be Running before it can be marked Done
        write_execution_with_running_stage(&crosslink_dir, &plan, "stage-a", "agent-1");
        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/stage-a/done")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["stage_id"], "stage-a");
        assert!(body["newly_ready"].as_array().unwrap().is_empty());
        // Single stage, so marking it done completes execution
        assert_eq!(body["execution_complete"], true);
    }

    #[tokio::test]
    async fn test_mark_stage_done_wrong_state_returns_bad_request() {
        // A stage that is still Pending cannot be marked Done
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/stage-a/done")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_mark_stage_failed_success() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        // mark_failed can be called on any status (it unconditionally sets Failed)
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/stage-a/failed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["stage_id"], "stage-a");
    }

    #[tokio::test]
    async fn test_mark_stage_failed_unknown_stage_returns_bad_request() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/no-such-stage/failed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_retry_stage_when_failed_succeeds() {
        let plan = make_simple_plan();
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, &plan);
        write_execution_with_failed_stage(&crosslink_dir, &plan, "stage-a");
        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/stage-a/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["stage_id"], "stage-a");
        // stage-a has no deps so it should be immediately ready
        assert_eq!(body["ready_to_launch"], true);
    }

    #[tokio::test]
    async fn test_retry_stage_when_not_failed_returns_bad_request() {
        let plan = make_simple_plan();
        // Stage is Pending (not Failed) — retry should be rejected
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/stage-a/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_retry_stage_unknown_stage_returns_bad_request() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/no-such-stage/retry")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_skip_stage_success() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/stage-a/skip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
        assert_eq!(body["stage_id"], "stage-a");
        assert!(body["newly_ready"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_skip_stage_unknown_stage_returns_bad_request() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/stages/no-such-stage/skip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_poll_agents_with_execution_no_running_stages() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/agents/poll")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // No running stages → no agent statuses reported
        assert!(body["agents"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_poll_agents_with_running_stage_no_status_file() {
        let plan = make_simple_plan();
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, &plan);
        write_execution_with_running_stage(&crosslink_dir, &plan, "stage-a", "parent--stage-a");
        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/agents/poll")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // Stage is running but no .kickoff-status file → nothing reported
        assert!(body["agents"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_get_snapshot_with_execution() {
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/snapshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["plan_id"], "test-plan-1");
        assert_eq!(body["state"], "Running");
        assert_eq!(body["stage_count"], 1);
        assert_eq!(body["is_empty"], false);
        assert_eq!(body["pending_count"], 1);
        assert_eq!(body["done_count"], 0);
    }

    #[tokio::test]
    async fn test_list_plans_with_stored_plans() {
        // Use the decompose module's store function to seed a plan.
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Write two stored plans using the decompose module's storage format.
        let orch_dir = crosslink_dir.join("orchestrator");
        std::fs::create_dir_all(&orch_dir).unwrap();
        let stored = serde_json::json!({
            "plan": {
                "id": "plan-alpha",
                "document_slug": "alpha",
                "phases": [],
                "created_at": "2026-01-01T00:00:00Z",
                "total_stages": 0,
                "estimated_hours": 0.0
            },
            "source_document": "# Alpha",
            "stored_at": "2026-01-01T00:00:00Z"
        });
        std::fs::write(
            orch_dir.join("plan-alpha.json"),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();
        let stored2 = serde_json::json!({
            "plan": {
                "id": "plan-beta",
                "document_slug": "beta",
                "phases": [],
                "created_at": "2026-01-01T00:00:00Z",
                "total_stages": 0,
                "estimated_hours": 0.0
            },
            "source_document": "# Beta",
            "stored_at": "2026-01-01T00:00:00Z"
        });
        std::fs::write(
            orch_dir.join("plan-beta.json"),
            serde_json::to_string(&stored2).unwrap(),
        )
        .unwrap();

        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/plans")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        let ids = body.as_array().unwrap();
        // plan.json is also in the directory (the active plan file) but is not
        // listed since list_plans returns ALL .json files. Let's check our plans:
        assert!(ids.iter().any(|v| v == "plan-alpha"));
        assert!(ids.iter().any(|v| v == "plan-beta"));
    }

    #[tokio::test]
    async fn test_get_plan_by_id_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let orch_dir = crosslink_dir.join("orchestrator");
        std::fs::create_dir_all(&orch_dir).unwrap();
        let stored = serde_json::json!({
            "plan": {
                "id": "my-plan-123",
                "document_slug": "my-doc",
                "phases": [],
                "created_at": "2026-01-01T00:00:00Z",
                "total_stages": 0,
                "estimated_hours": 2.5
            },
            "source_document": "# My Doc",
            "stored_at": "2026-01-01T00:00:00Z"
        });
        std::fs::write(
            orch_dir.join("my-plan-123.json"),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();

        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/plans/my-plan-123")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["plan"]["id"], "my-plan-123");
        assert_eq!(body["plan"]["document_slug"], "my-doc");
        assert_eq!(body["source_document"], "# My Doc");
    }

    #[tokio::test]
    async fn test_decompose_missing_field_returns_bad_request() {
        // Sending a body without the required `document` field should fail.
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/decompose")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"slug": "test"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Missing required field → deserialization fails → 422 Unprocessable Entity
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn test_decompose_invalid_json_returns_bad_request() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/orchestrator/decompose")
                    .header("content-type", "application/json")
                    .body(Body::from("not json at all"))
                    .unwrap(),
            )
            .await
            .unwrap();
        // axum returns 400 for completely invalid JSON bodies
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_error_helpers_produce_correct_status_codes() {
        // Exercise the helper functions directly to verify status codes.
        let (code, Json(body)) = internal_error("ctx", "detail msg");
        assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body.error, "ctx");
        assert_eq!(body.detail.as_deref(), Some("detail msg"));

        let (code, Json(body)) = bad_request("bad");
        assert_eq!(code, StatusCode::BAD_REQUEST);
        assert_eq!(body.error, "bad request");
        assert_eq!(body.detail.as_deref(), Some("bad"));

        let (code, Json(body)) = not_found("gone");
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(body.error, "not found");
        assert_eq!(body.detail.as_deref(), Some("gone"));

        let (code, Json(body)) = conflict("clash");
        assert_eq!(code, StatusCode::CONFLICT);
        assert_eq!(body.error, "conflict");
        assert_eq!(body.detail.as_deref(), Some("clash"));
    }

    #[tokio::test]
    async fn test_execution_status_response_no_detail_for_idle() {
        // Verify the serde skip rule: `detail` omitted when None.
        let response = ExecutionStatusResponse {
            status: "idle".to_string(),
            progress_pct: 0,
            detail: None,
        };
        let json = serde_json::to_string(&response).unwrap();
        assert!(!json.contains("detail"));
    }

    #[tokio::test]
    async fn test_mark_running_request_deserializes() {
        let json = r#"{"agent_id": "agent-123"}"#;
        let req: MarkRunningRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.agent_id, "agent-123");
    }

    #[tokio::test]
    async fn test_poll_agents_with_running_stage_and_status_file() {
        // Create a worktree with a .kickoff-status file so poll_agents returns it.
        let plan = make_simple_plan();
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        write_plan_file(&crosslink_dir, &plan);
        // The agent_id used in worktrees must match the slug derivation:
        // agent_tmux_session uses "parent--stage-a" → last part is "stage-a".
        write_execution_with_running_stage(&crosslink_dir, &plan, "stage-a", "parent--stage-a");

        // Create the worktree directory with a .kickoff-status file.
        // poll_agent_status extracts slug via rsplit("--"), so "parent--stage-a" → "stage-a"
        let wt_dir = dir.path().join(".worktrees").join("stage-a");
        std::fs::create_dir_all(&wt_dir).unwrap();
        std::fs::write(wt_dir.join(".kickoff-status"), "running\n").unwrap();

        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/agents/poll")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // The running stage should now appear in agents.
        let agents = body["agents"].as_array().unwrap();
        assert!(!agents.is_empty());
        assert_eq!(agents[0]["stage_id"], "stage-a");
        assert_eq!(agents[0]["status"], "running");
    }

    #[tokio::test]
    async fn test_get_snapshot_running_with_details() {
        // Verify that the snapshot response includes all expected fields.
        let plan = make_simple_plan();
        let (app, _dir) = test_app_with_running_execution(&plan);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/orchestrator/snapshot")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // Check all the key snapshot fields.
        assert_eq!(body["plan_id"], "test-plan-1");
        assert_eq!(body["state"], "Running");
        assert_eq!(body["stage_count"], 1);
        assert_eq!(body["is_empty"], false);
        assert_eq!(body["running"].as_array().unwrap().len(), 0);
        assert_eq!(body["pending_count"], 1);
        assert_eq!(body["done_count"], 0);
        assert_eq!(body["failed_count"], 0);
        // dependency_graph should contain the stage-a entry.
        assert!(body["dependency_graph"]["stage-a"].is_object());
    }
}
