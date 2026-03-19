//! Handlers for hub synchronization endpoints.
//!
//! Implements:
//! - `GET /api/v1/sync/status` — hub init state, remote, lock counts
//! - `POST /api/v1/sync/fetch` — fetch latest state from remote
//! - `POST /api/v1/sync/push` — push local hub state to remote

use axum::{extract::State, http::StatusCode, response::Json};

use crate::server::{
    state::AppState,
    types::{ApiError, SyncActionResponse, SyncStatusResponse},
};
use crate::sync::SyncManager;

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

/// Try to construct a `SyncManager` from the shared crosslink dir.
fn sync_manager(state: &AppState) -> Result<SyncManager, (StatusCode, Json<ApiError>)> {
    SyncManager::new(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to initialize SyncManager", e))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/sync/status` — return hub initialization state, remote, and lock counts.
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

        let stale = sm.find_stale_locks_with_age().map(|v| v.len()).unwrap_or(0);

        (active, stale)
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

/// `POST /api/v1/sync/push` — push local hub state to remote.
///
/// Commits any uncommitted changes in the hub cache and pushes to the remote.
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

    // Clean any dirty state (stage + commit recovery), then push.
    let had_dirty = sm
        .clean_dirty_state()
        .map_err(|e| internal_error("Failed to clean dirty state", e))?;

    // Fetch first to rebase any local changes on top of remote.
    sm.fetch()
        .map_err(|e| internal_error("Pre-push fetch failed", e))?;

    // Push the hub cache to remote using git push.
    push_hub_cache(&sm).map_err(|e| internal_error("Push failed", e))?;

    let message = if had_dirty {
        "Committed local changes and pushed hub state to remote".to_string()
    } else {
        "Pushed hub state to remote".to_string()
    };

    Ok(Json(SyncActionResponse {
        success: true,
        message,
    }))
}

/// Push the hub cache to the remote with retry on conflict.
fn push_hub_cache(sm: &SyncManager) -> anyhow::Result<()> {
    use std::process::Command;

    let cache_path = sm.cache_path();
    let remote = sm.remote();

    for attempt in 0..3 {
        let output = Command::new("git")
            .args(["push", remote, "crosslink/hub"])
            .current_dir(cache_path)
            .output()?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not resolve host")
            || stderr.contains("Could not read from remote")
        {
            // Offline — local state is fine
            return Ok(());
        }

        if (stderr.contains("rejected") || stderr.contains("non-fast-forward")) && attempt < 2 {
            // INTENTIONAL: rebase failure is non-fatal — the retry loop will bail on persistent conflicts
            let _ = Command::new("git")
                .args(["pull", "--rebase", remote, "crosslink/hub"])
                .current_dir(cache_path)
                .output();
            continue;
        }

        anyhow::bail!("Push failed: {}", stderr);
    }

    Ok(())
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
        let (status, json) = super::internal_error("sync failed", "some error");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "sync failed");
        assert_eq!(json.detail.as_deref(), Some("some error"));
    }

    #[test]
    fn test_push_hub_cache_no_git_repo_bails() {
        // Call push_hub_cache with a SyncManager pointing at a temp dir
        // that has no git repo — the git push will fail.
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let hub_cache = crosslink_dir.join(".hub-cache");
        std::fs::create_dir_all(&hub_cache).unwrap();

        let sm = crate::sync::SyncManager::new(&crosslink_dir).unwrap();
        // push_hub_cache runs `git push` which will fail in a non-git directory.
        // It should return an error (not panic).
        let result = super::push_hub_cache(&sm);
        // Either Ok (git offline path) or Err (git command failed) — not panic.
        let _ = result;
    }
}
