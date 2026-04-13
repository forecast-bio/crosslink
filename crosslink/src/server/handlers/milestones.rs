//! Handlers for milestone management endpoints.
//!
//! Implements:
//! - `GET /api/v1/milestones` — list milestones (with optional status filter)
//! - `POST /api/v1/milestones` — create a new milestone
//! - `GET /api/v1/milestones/:id` — get a single milestone with progress stats
//! - `POST /api/v1/milestones/:id/assign` — assign an issue to a milestone
//! - `POST /api/v1/milestones/:id/close` — close a milestone

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::Json,
};

use crate::server::{
    errors::{internal_error, not_found},
    state::AppState,
    types::{
        ApiError, AssignMilestoneRequest, CreateMilestoneRequest, MilestoneDetail,
        MilestoneListQuery, MilestoneListResponse, OkResponse,
    },
};

/// Build a `MilestoneDetail` from a `Milestone` by looking up assigned issues.
fn build_detail(
    db: &crate::db::Database,
    milestone: crate::models::Milestone,
) -> anyhow::Result<MilestoneDetail> {
    let issues = db.get_milestone_issues(milestone.id)?;
    let issue_count = issues.len();
    let completed_count = issues
        .iter()
        .filter(|i| i.status == crate::models::IssueStatus::Closed)
        .count();
    let progress_percent = if issue_count == 0 {
        0.0
    } else {
        // Milestone issue counts are small enough to fit in u32.
        let completed = u32::try_from(completed_count).unwrap_or(u32::MAX);
        let total = u32::try_from(issue_count).unwrap_or(u32::MAX);
        f64::from(completed) / f64::from(total) * 100.0
    };
    Ok(MilestoneDetail {
        milestone,
        issue_count,
        completed_count,
        progress_percent,
    })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/milestones` — list milestones.
///
/// Query params:
/// - `?status=open|closed|all` — filter by status (default: open)
///
/// # Errors
///
/// Returns an error if the database query or detail building fails.
pub async fn list_milestones(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<MilestoneListQuery>,
) -> Result<Json<MilestoneListResponse>, (StatusCode, Json<ApiError>)> {
    let db = state.db().await;

    let milestones = db
        .list_milestones(query.status.as_deref())
        .map_err(|e| internal_error("Failed to list milestones", e))?;

    let items: Vec<MilestoneDetail> = milestones
        .into_iter()
        .map(|m| build_detail(&db, m))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| internal_error("Failed to build milestone details", e))?;

    drop(db);
    let total = items.len();
    Ok(Json(MilestoneListResponse { items, total }))
}

/// `POST /api/v1/milestones` — create a new milestone.
///
/// Body: `{"name": "<name>", "description": "<optional>"}`.
///
/// Returns the newly created milestone with progress stats.
///
/// # Errors
///
/// Returns an error if creating, fetching, or building the milestone detail fails.
pub async fn create_milestone(
    State(state): State<AppState>,
    Json(body): Json<CreateMilestoneRequest>,
) -> Result<Json<MilestoneDetail>, (StatusCode, Json<ApiError>)> {
    let db = state.db().await;

    let milestone_id = db
        .create_milestone(&body.name, body.description.as_deref())
        .map_err(|e| internal_error("Failed to create milestone", e))?;

    let milestone = db
        .get_milestone(milestone_id)
        .map_err(|e| internal_error("Failed to fetch new milestone", e))?
        .ok_or_else(|| {
            internal_error(
                "Milestone created but not found",
                format!("id={milestone_id}"),
            )
        })?;

    let detail = build_detail(&db, milestone)
        .map_err(|e| internal_error("Failed to build milestone detail", e))?;

    drop(db);
    Ok(Json(detail))
}

/// `GET /api/v1/milestones/:id` — get a single milestone with progress statistics.
///
/// # Errors
///
/// Returns an error if the milestone is not found or the detail cannot be built.
pub async fn get_milestone(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MilestoneDetail>, (StatusCode, Json<ApiError>)> {
    let db = state.db().await;

    let milestone = db
        .get_milestone(id)
        .map_err(|e| internal_error("Failed to fetch milestone", e))?
        .ok_or_else(|| not_found(format!("Milestone {id} not found")))?;

    let detail = build_detail(&db, milestone)
        .map_err(|e| internal_error("Failed to build milestone detail", e))?;

    drop(db);
    Ok(Json(detail))
}

/// `POST /api/v1/milestones/:id/assign` — assign an issue to a milestone.
///
/// Body: `{"issue_id": <id>}`.
///
/// # Errors
///
/// Returns an error if the milestone or issue is not found, or assignment fails.
pub async fn assign_milestone(
    State(state): State<AppState>,
    Path(milestone_id): Path<i64>,
    Json(body): Json<AssignMilestoneRequest>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ApiError>)> {
    let db = state.db().await;

    // Verify the milestone exists.
    db.get_milestone(milestone_id)
        .map_err(|e| internal_error("Failed to look up milestone", e))?
        .ok_or_else(|| not_found(format!("Milestone {milestone_id} not found")))?;

    // Verify the issue exists.
    db.get_issue(body.issue_id)
        .map_err(|e| internal_error("Failed to look up issue", e))?
        .ok_or_else(|| not_found(format!("Issue {} not found", body.issue_id)))?;

    db.add_issue_to_milestone(milestone_id, body.issue_id)
        .map_err(|e| internal_error("Failed to assign issue to milestone", e))?;

    drop(db);
    Ok(Json(OkResponse { ok: true }))
}

/// `POST /api/v1/milestones/:id/close` — close a milestone.
///
/// # Errors
///
/// Returns an error if the milestone is not found or closing fails.
pub async fn close_milestone(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<OkResponse>, (StatusCode, Json<ApiError>)> {
    let db = state.db().await;

    // Verify the milestone exists first.
    db.get_milestone(id)
        .map_err(|e| internal_error("Failed to look up milestone", e))?
        .ok_or_else(|| not_found(format!("Milestone {id} not found")))?;

    let closed = db
        .close_milestone(id)
        .map_err(|e| internal_error("Failed to close milestone", e))?;

    drop(db);
    if !closed {
        return Err(internal_error("close_milestone returned false", ""));
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
    async fn test_list_milestones_empty() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/milestones")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 0);
        assert!(body["items"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_create_milestone() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"name": "v1.0", "description": "First release"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["name"], "v1.0");
        assert_eq!(body["status"], "open");
        assert_eq!(body["issue_count"], 0);
        assert_eq!(body["progress_percent"], 0.0);
    }

    #[tokio::test]
    async fn test_get_milestone_not_found() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/milestones/999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_milestone_exists() {
        let (app, _dir) = test_app();
        // Create first
        let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "Sprint 1"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_i64().unwrap();

        // Fetch it
        let get_resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/v1/milestones/{id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let body = body_json(get_resp).await;
        assert_eq!(body["name"], "Sprint 1");
    }

    #[tokio::test]
    async fn test_close_milestone_not_found() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones/999/close")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_close_milestone_success() {
        let (app, _dir) = test_app();
        // Create first
        let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "To Close"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_i64().unwrap();

        // Close it
        let close_resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/v1/milestones/{id}/close"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(close_resp.status(), StatusCode::OK);
        let body = body_json(close_resp).await;
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_assign_milestone_not_found_milestone() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones/999/assign")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"issue_id": 1}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_list_milestones_with_status_filter() {
        let (app, _dir) = test_app();
        // Create a milestone and close it
        let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "Closed MS"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let id = created["id"].as_i64().unwrap();

        app.clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/v1/milestones/{id}/close"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // List open milestones — should be empty
        let open_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/milestones?status=open")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(open_resp.status(), StatusCode::OK);
        let open_body = body_json(open_resp).await;
        assert_eq!(open_body["total"], 0);

        // List all milestones — should have 1
        let all_resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/milestones?status=all")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(all_resp.status(), StatusCode::OK);
        let all_body = body_json(all_resp).await;
        assert_eq!(all_body["total"], 1);
    }

    #[tokio::test]
    async fn test_list_milestones_closed_filter() {
        let (app, _dir) = test_app();

        // Create and close one milestone, leave one open.
        let r1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "Open MS"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let _ = body_json(r1).await;

        let r2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "Closed MS2"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created2 = body_json(r2).await;
        let id2 = created2["id"].as_i64().unwrap();

        app.clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/v1/milestones/{id2}/close"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // List closed milestones — should have exactly 1.
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/milestones?status=closed")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 1);
        assert_eq!(body["items"][0]["name"], "Closed MS2");
    }

    #[tokio::test]
    async fn test_create_milestone_without_description() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "No Description"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["name"], "No Description");
        assert_eq!(body["issue_count"], 0);
        assert_eq!(body["progress_percent"], 0.0);
    }

    #[tokio::test]
    async fn test_assign_milestone_success() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");

        // Seed milestone and issue directly.
        let milestone_id = db.create_milestone("MS", None).unwrap();
        let issue_id = db.create_issue("Issue to assign", None, "medium").unwrap();

        let state = crate::server::state::AppState::new(db, dir.path().join(".crosslink"));
        let app = crate::server::routes::build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/v1/milestones/{milestone_id}/assign"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"issue_id": issue_id}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_assign_milestone_issue_not_found() {
        let (app, _dir) = test_app();

        // Create a milestone first.
        let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/milestones")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"name": "MS for assign"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let created = body_json(create_resp).await;
        let milestone_id = created["id"].as_i64().unwrap();

        // Try assigning a non-existent issue.
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!("/api/v1/milestones/{milestone_id}/assign"))
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"issue_id": 9999}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_helper_functions_directly() {
        let (status, json) = crate::server::errors::internal_error("ctx", "err detail");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "ctx");
        assert_eq!(json.detail.as_deref(), Some("err detail"));

        let (status, json) = crate::server::errors::not_found("not there");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json.error, "not found");
        assert_eq!(json.detail.as_deref(), Some("not there"));
    }

    #[tokio::test]
    async fn test_milestone_progress_with_issues() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");

        // Create a milestone and two issues, close one.
        let milestone_id = db.create_milestone("Progress MS", None).unwrap();
        let issue_id1 = db.create_issue("Open issue", None, "medium").unwrap();
        let issue_id2 = db.create_issue("Closed issue", None, "medium").unwrap();
        db.add_issue_to_milestone(milestone_id, issue_id1).unwrap();
        db.add_issue_to_milestone(milestone_id, issue_id2).unwrap();
        db.close_issue(issue_id2).unwrap();

        let state = crate::server::state::AppState::new(db, dir.path().join(".crosslink"));
        let app = crate::server::routes::build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri(format!("/api/v1/milestones/{milestone_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["issue_count"], 2);
        assert_eq!(body["completed_count"], 1);
        assert_eq!(body["progress_percent"], 50.0);
    }
}
