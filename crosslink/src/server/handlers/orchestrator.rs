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
    state::AppState,
    types::{ApiError, DecomposeRequest, ExecutionStatus, OrchestratorPlan},
};

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

fn internal_error(context: &str, e: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: context.to_string(),
            detail: Some(e.to_string()),
        }),
    )
}

fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "bad request".to_string(),
            detail: Some(msg.into()),
        }),
    )
}

fn not_found(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: "not found".to_string(),
            detail: Some(msg.into()),
        }),
    )
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
pub async fn get_plan(
    State(state): State<AppState>,
) -> Result<Json<Option<OrchestratorPlan>>, (StatusCode, Json<ApiError>)> {
    match OrchestratorExecutor::load_plan(&state.crosslink_dir) {
        Ok(plan) => Ok(Json(Some(plan))),
        Err(_) => Ok(Json(None)),
    }
}

// ---------------------------------------------------------------------------
// GET /api/v1/orchestrator/status
// ---------------------------------------------------------------------------

/// `GET /api/v1/orchestrator/status` — get execution status.
///
/// Returns progress percentage and execution state. If no execution exists,
/// returns idle status with 0% progress.
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
    let progress_pct = full_status.progress_percent.round() as u32;
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
pub async fn execute(
    State(state): State<AppState>,
) -> Result<Json<ExecutionStatusResponse>, (StatusCode, Json<ApiError>)> {
    // If no execution exists, initialize from the current plan.
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        let plan = OrchestratorExecutor::load_plan(&state.crosslink_dir)
            .map_err(|e| not_found(format!("No plan found: {e}")))?;

        let db = state.db();

        let mut executor = OrchestratorExecutor::init(&state.crosslink_dir, &db, &plan)
            .map_err(|e| internal_error("Failed to initialize execution", e))?;

        let _ready = executor
            .start()
            .map_err(|e| internal_error("Failed to start execution", e))?;

        let full_status = executor.status();
        return Ok(Json(ExecutionStatusResponse {
            status: "running".to_string(),
            progress_pct: full_status.progress_percent.round() as u32,
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
        progress_pct: full_status.progress_percent.round() as u32,
        detail: Some(full_status),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/pause
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/pause` — pause execution.
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
        progress_pct: full_status.progress_percent.round() as u32,
        detail: Some(full_status),
    }))
}

// ---------------------------------------------------------------------------
// POST /api/v1/orchestrator/stages/:id/retry
// ---------------------------------------------------------------------------

/// `POST /api/v1/orchestrator/stages/:id/retry` — retry a failed stage.
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
        progress_pct: full_status.progress_percent.round() as u32,
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
pub async fn mark_stage_done_handler(
    State(state): State<AppState>,
    Path(stage_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let db = state.db();
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
pub async fn mark_stage_failed_handler(
    State(state): State<AppState>,
    Path(stage_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ApiError>)> {
    if !OrchestratorExecutor::exists(&state.crosslink_dir) {
        return Err(not_found("No execution in progress"));
    }

    let mut executor = OrchestratorExecutor::load(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to load execution state", e))?;

    let event = executor
        .mark_stage_failed(&stage_id)
        .map_err(|e| bad_request(format!("Cannot mark stage failed: {e}")))?;

    OrchestratorExecutor::broadcast_event(&state.ws_tx, event);

    Ok(Json(serde_json::json!({
        "ok": true,
        "stage_id": stage_id,
    })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/orchestrator/agents/poll
// ---------------------------------------------------------------------------

/// `GET /api/v1/orchestrator/agents/poll` — poll running agent status files.
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
    use crate::server::{routes::build_router, state::AppState};
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
    };
    use serde_json::Value;
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
}
