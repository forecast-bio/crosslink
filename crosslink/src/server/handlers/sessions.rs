//! Handlers for session management endpoints.
//!
//! Implements:
//! - `GET /api/v1/sessions/current` — get the active session for the calling agent
//! - `POST /api/v1/sessions/start` — start a new session
//! - `POST /api/v1/sessions/end` — end the current session
//! - `POST /api/v1/sessions/work/:id` — set the active issue for the current session

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};

use crate::server::{
    state::AppState,
    types::{ApiError, EndSessionRequest, OkResponse, SessionResponse, StartSessionRequest},
};

// ---------------------------------------------------------------------------
// Helpers
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

fn not_found(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: "not found".to_string(),
            detail: Some(msg.into()),
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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/sessions/current` — return the active (not yet ended) session.
///
/// Accepts an optional `?agent_id=` query param to scope to a specific agent.
pub async fn get_current_session(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ApiError>)> {
    let agent_id = params.get("agent_id").map(|s| s.as_str());
    let db = state.db();

    let session = db
        .get_current_session_for_agent(agent_id)
        .map_err(|e| internal_error("Failed to query current session", e))?
        .ok_or_else(|| not_found("No active session found"))?;

    Ok(Json(SessionResponse { session }))
}

/// `POST /api/v1/sessions/start` — start a new session.
///
/// Body: `{"agent_id": "<optional>"}`.
///
/// Returns the newly created session.
pub async fn start_session(
    State(state): State<AppState>,
    Json(body): Json<StartSessionRequest>,
) -> Result<Json<SessionResponse>, (StatusCode, Json<ApiError>)> {
    let db = state.db();

    let agent_id_ref = body.agent_id.as_deref();
    let session_id = db
        .start_session_with_agent(agent_id_ref)
        .map_err(|e| internal_error("Failed to start session", e))?;

    // Fetch the newly-created session to return it.
    let session = db
        .get_current_session_for_agent(agent_id_ref)
        .map_err(|e| internal_error("Failed to fetch new session", e))?
        .ok_or_else(|| {
            internal_error("Session created but not found", format!("id={session_id}"))
        })?;

    Ok(Json(SessionResponse { session }))
}

/// `POST /api/v1/sessions/end` — end the current active session.
///
/// Body: `{"notes": "<optional handoff notes>"}`.
///
/// To end a session scoped to a specific agent, pass `?agent_id=` as a query param.
pub async fn end_session(
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<EndSessionRequest>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ApiError>)> {
    let agent_id = params.get("agent_id").map(|s| s.as_str());
    let db = state.db();

    // Find the current active session so we know its ID.
    let session = db
        .get_current_session_for_agent(agent_id)
        .map_err(|e| internal_error("Failed to query current session", e))?
        .ok_or_else(|| not_found("No active session to end"))?;

    let ended = db
        .end_session(session.id, body.notes.as_deref())
        .map_err(|e| internal_error("Failed to end session", e))?;

    if !ended {
        return Err(bad_request(format!(
            "Session {} could not be ended (already ended?)",
            session.id
        )));
    }

    Ok(Json(OkResponse { ok: true }))
}

/// `POST /api/v1/sessions/work/:id` — set the active issue for the current session.
///
/// `:id` is the crosslink issue ID to mark as the current work item.
/// Accepts an optional `?agent_id=` query param to scope to a specific agent's session.
pub async fn work_on_issue(
    State(state): State<AppState>,
    Path(issue_id): Path<i64>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ApiError>)> {
    let agent_id = params.get("agent_id").map(|s| s.as_str());
    let db = state.db();

    // Verify the issue exists before updating the session.
    let issue_exists = db
        .get_issue(issue_id)
        .map_err(|e| internal_error("Failed to look up issue", e))?
        .is_some();

    if !issue_exists {
        return Err(not_found(format!("Issue {} not found", issue_id)));
    }

    // Find the current session.
    let session = db
        .get_current_session_for_agent(agent_id)
        .map_err(|e| internal_error("Failed to query current session", e))?
        .ok_or_else(|| not_found("No active session — call POST /sessions/start first"))?;

    let updated = db
        .set_session_issue(session.id, issue_id)
        .map_err(|e| internal_error("Failed to update session issue", e))?;

    if !updated {
        return Err(internal_error("set_session_issue returned false", ""));
    }

    Ok(Json(OkResponse { ok: true }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
        Router,
    };
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    use crate::db::Database;
    use crate::server::{routes::build_router, state::AppState};

    fn test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let state = AppState::new(db, dir.path().join(".crosslink"));
        (build_router(state, None), dir)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_get_current_session_no_session() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/sessions/current")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_start_session_returns_session() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/start")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body.get("id").is_some());
        assert!(body.get("started_at").is_some());
        assert!(body.get("ended_at").unwrap().is_null());
    }

    #[tokio::test]
    async fn test_start_session_with_agent_id() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/start")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"agent_id": "my-agent"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["agent_id"], "my-agent");
    }

    #[tokio::test]
    async fn test_end_session_no_active_session() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/end")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_work_on_issue_no_session() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/work/1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // No active session → 404
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_current_session_after_start() {
        let (app, _dir) = test_app();
        // Start a session
        let start_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/start")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"agent_id": "test-agent"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start_resp.status(), StatusCode::OK);

        // Now fetch the current session
        let get_resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/sessions/current?agent_id=test-agent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let body = body_json(get_resp).await;
        assert_eq!(body["agent_id"], "test-agent");
        assert!(body["ended_at"].is_null());
    }

    #[tokio::test]
    async fn test_end_session_success() {
        let (app, _dir) = test_app();
        // Start a session first
        app.clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/start")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"agent_id": "end-test-agent"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // End it
        let end_resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/end?agent_id=end-test-agent")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"notes": "done"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(end_resp.status(), StatusCode::OK);
        let body = body_json(end_resp).await;
        assert_eq!(body["ok"], true);
    }

    /// Helper that returns (Router, TempDir) with an active session and a created issue.
    fn test_app_with_session_and_issue() -> (Router, tempfile::TempDir, i64) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        // Start a session and create an issue directly via db
        db.start_session().unwrap();
        let issue_id = db.create_issue("work item", None, "medium").unwrap();
        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir, issue_id)
    }

    #[tokio::test]
    async fn test_work_on_issue_success() {
        let (app, _dir, issue_id) = test_app_with_session_and_issue();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(&format!("/api/v1/sessions/work/{}", issue_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_work_on_issue_not_found() {
        let (app, _dir, _) = test_app_with_session_and_issue();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/work/9999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = body_json(resp).await;
        assert!(body["detail"].as_str().unwrap().contains("9999"));
    }

    #[test]
    fn test_helper_functions() {
        let (status, json) = super::internal_error("ctx", "err");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "ctx");

        let (status, json) = super::not_found("gone");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json.detail.as_deref(), Some("gone"));

        let (status, json) = super::bad_request("invalid");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json.detail.as_deref(), Some("invalid"));
    }

    #[tokio::test]
    async fn test_get_current_session_with_agent_id_scoping() {
        // Start two sessions for different agents, verify current session
        // returns the right one when scoped by agent_id.
        let (app, _dir) = test_app();

        // Start session for agent-alpha
        app.clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/start")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"agent_id": "agent-alpha"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Start session for agent-beta
        app.clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/start")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"agent_id": "agent-beta"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Fetch current session for agent-beta
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/sessions/current?agent_id=agent-beta")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // SessionResponse wraps the session object
        assert_eq!(body["agent_id"], "agent-beta");
    }

    #[tokio::test]
    async fn test_end_session_with_notes() {
        // Start a session and end it with notes.
        let (app, _dir) = test_app();

        app.clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/start")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"agent_id": "note-agent"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let end_resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sessions/end?agent_id=note-agent")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"notes": "finished implementing feature X"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(end_resp.status(), StatusCode::OK);
        let body = body_json(end_resp).await;
        assert_eq!(body["ok"], true);
    }
}
