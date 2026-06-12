//! Handlers for hub synchronization endpoints.
//!
//! Implements:
//! - `GET /api/v1/sync/status` — hub init state, remote, lock counts
//! - `POST /api/v1/sync/fetch` — fetch latest state from remote
//! - `POST /api/v1/sync/push` — push local hub state to remote

use axum::{extract::State, http::StatusCode, response::Json};

use crate::server::{
    errors::internal_error,
    state::AppState,
    types::{ApiError, SyncActionResponse, SyncStatusResponse},
};
use crate::sync::SyncManager;

/// Try to construct a `SyncManager` from the shared crosslink dir.
fn sync_manager(state: &AppState) -> Result<SyncManager, (StatusCode, Json<ApiError>)> {
    SyncManager::new(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to initialize SyncManager", e))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/sync/status` — return hub initialization state, remote, and lock counts.
///
/// # Errors
///
/// Returns an error if the sync manager cannot be initialized or lock
/// data cannot be read.
pub async fn sync_status(
    State(state): State<AppState>,
) -> Result<Json<SyncStatusResponse>, (StatusCode, Json<ApiError>)> {
    let sm = sync_manager(&state)?;

    let hub_initialized = sm.is_initialized();
    let remote = sm.remote().to_string();

    // Lock and stale-lock counts (default to 0 if hub is not initialized)
    let (active_lock_count, stale_lock_count) = if hub_initialized {
        let locks = sm
            .read_locks_auto()
            .map_err(|e| internal_error("Failed to read locks", e))?;
        let active = locks.locks.len();

        let stale_count = sm.find_stale_locks_with_age().map_or(0, |v| v.len());

        (active, stale_count)
    } else {
        (0, 0)
    };

    // Derive last_fetch_at from the hub cache directory's modification time.
    let last_fetch_at = if hub_initialized {
        std::fs::metadata(sm.cache_path())
            .ok()
            .and_then(|m| m.modified().ok())
            .map(chrono::DateTime::<chrono::Utc>::from)
    } else {
        None
    };

    Ok(Json(SyncStatusResponse {
        hub_initialized,
        hub_branch: "crosslink/hub".to_string(),
        remote,
        last_fetch_at,
        active_lock_count,
        stale_lock_count,
    }))
}

/// `POST /api/v1/sync/fetch` — fetch the latest hub state from remote.
///
/// # Errors
///
/// Returns an error if the hub is not initialized or the fetch operation fails.
pub async fn sync_fetch(
    State(state): State<AppState>,
) -> Result<Json<SyncActionResponse>, (StatusCode, Json<ApiError>)> {
    let sm = sync_manager(&state)?;

    if !sm.is_initialized() {
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "Hub not initialized".to_string(),
                detail: Some(
                    "Run `crosslink sync` from the CLI to initialize the hub cache first."
                        .to_string(),
                ),
            }),
        ));
    }

    sm.fetch().map_err(|e| internal_error("Fetch failed", e))?;

    Ok(Json(SyncActionResponse {
        success: true,
        message: "Fetched latest hub state from remote".to_string(),
    }))
}

/// `POST /api/v1/sync/push` — synchronize local hub state with the remote.
///
/// Under the v3 hub (#754) there is no separate "push the hub branch" step: each
/// agent's mutations push that agent's own ref at write time, and compaction
/// pushes the checkpoint. The server holds no agent ref of its own, so this
/// endpoint reduces to a ref fetch + checkpoint refresh — the same reconciliation
/// `crosslink sync` performs. A frozen v2 hub is never written.
///
/// # Errors
///
/// Returns an error if the hub is not initialized or the fetch operation fails.
pub async fn sync_push(
    State(state): State<AppState>,
) -> Result<Json<SyncActionResponse>, (StatusCode, Json<ApiError>)> {
    let sm = sync_manager(&state)?;

    if !sm.is_initialized() {
        return Err((
            StatusCode::CONFLICT,
            Json(ApiError {
                error: "Hub not initialized".to_string(),
                detail: Some(
                    "Run `crosslink sync` from the CLI to initialize the hub cache first."
                        .to_string(),
                ),
            }),
        ));
    }

    sm.fetch().map_err(|e| internal_error("Sync failed", e))?;

    Ok(Json(SyncActionResponse {
        success: true,
        message: "Synchronized hub state with remote (v3 refs)".to_string(),
    }))
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
    use serde_json::Value;
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
    async fn test_sync_status_no_hub() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/sync/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["hub_initialized"], false);
        assert_eq!(body["hub_branch"], "crosslink/hub");
        assert_eq!(body["active_lock_count"], 0);
        assert_eq!(body["stale_lock_count"], 0);
    }

    #[tokio::test]
    async fn test_sync_status_has_remote_field() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/sync/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // When no hook-config.json exists, defaults to "origin"
        assert_eq!(body["remote"], "origin");
    }

    #[tokio::test]
    async fn test_sync_fetch_no_hub_returns_conflict() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sync/fetch")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("not initialized"));
    }

    #[tokio::test]
    async fn test_sync_push_no_hub_returns_conflict() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sync/push")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = body_json(resp).await;
        assert!(body["error"].as_str().unwrap().contains("not initialized"));
    }

    /// Build a test app where the hub cache directory exists (making
    /// `SyncManager::is_initialized()` return true), but contains no real
    /// git data so network-touching operations will fail gracefully.
    fn test_app_with_hub() -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        // Create the hub cache dir so is_initialized() returns true.
        let hub_cache = crosslink_dir.join(".hub-cache");
        std::fs::create_dir_all(&hub_cache).unwrap();
        // Write a minimal locks.json so read_locks_auto doesn't error.
        let locks_json = serde_json::json!({
            "version": 1,
            "locks": {},
            "settings": {"stale_lock_timeout_minutes": 30},
            "updated_at": chrono::Utc::now().to_rfc3339()
        });
        std::fs::write(
            hub_cache.join("locks.json"),
            serde_json::to_string(&locks_json).unwrap(),
        )
        .unwrap();
        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir)
    }

    #[tokio::test]
    async fn test_sync_status_with_hub_initialized() {
        let (app, _dir) = test_app_with_hub();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/sync/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // Hub is initialized now
        assert_eq!(body["hub_initialized"], true);
        assert_eq!(body["hub_branch"], "crosslink/hub");
        // No locks seeded
        assert_eq!(body["active_lock_count"], 0);
        assert_eq!(body["stale_lock_count"], 0);
    }

    #[tokio::test]
    async fn test_sync_fetch_with_hub_initialized_offline() {
        // When hub is initialized but remote is unreachable (offline env),
        // the fetch attempt should not panic — it either succeeds or returns
        // an internal error. We only assert that the request completes.
        let (app, _dir) = test_app_with_hub();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sync/fetch")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Either 200 (fetch ran, possibly offline OK) or 500 (git error)
        // — we just verify the request doesn't return 409 Conflict.
        assert_ne!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_sync_push_with_hub_initialized_offline() {
        // Similar to fetch — verifies the push handler exercises the
        // hub-initialized branch and completes without panicking.
        let (app, _dir) = test_app_with_hub();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/sync/push")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Either 200 (push ran) or 500 (git error — no remote configured)
        assert_ne!(resp.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn test_internal_error_helper() {
        let (status, json) = crate::server::errors::internal_error("sync failed", "some error");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "sync failed");
        assert_eq!(json.detail.as_deref(), Some("some error"));
    }
}
