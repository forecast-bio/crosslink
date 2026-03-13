//! Handlers for agent monitoring and lock endpoints.
//!
//! Implements:
//! - `GET /api/v1/agents` — list all agents with latest heartbeat and status
//! - `GET /api/v1/agents/:id` — single agent detail with heartbeat history, locks, kickoff status
//! - `GET /api/v1/agents/:id/status` — kickoff status for a specific agent
//! - `GET /api/v1/locks` — all current locks
//! - `GET /api/v1/locks/stale` — stale locks with age

use std::path::{Path, PathBuf};
use std::process::Command;

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::Json,
};
use chrono::{Duration, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use crate::server::{
    state::AppState,
    types::{AgentDetail, AgentStatus, AgentSummary, ApiError, LockEntry},
};
use crate::sync::SyncManager;

// ---------------------------------------------------------------------------
// Staleness thresholds
// ---------------------------------------------------------------------------

/// Heartbeat age below which an agent is considered "active".
const ACTIVE_THRESHOLD_SECS: i64 = 5 * 60;
/// Heartbeat age above which an agent is considered "stale" (between this and
/// ACTIVE_THRESHOLD is "idle").
const IDLE_THRESHOLD_SECS: i64 = 30 * 60;

// ---------------------------------------------------------------------------
// Helper types
// ---------------------------------------------------------------------------

/// Response for `GET /api/v1/agents/:id/status`.
#[derive(Debug, Serialize)]
pub struct AgentStatusResponse {
    pub agent_id: String,
    /// Content of `.kickoff-status` file, or a derived string when not present.
    pub kickoff_status: String,
    /// Absolute path of the agent's git worktree, if discoverable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    /// Whether the agent's tmux session is currently running.
    pub tmux_session_active: bool,
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Classify an agent's status from its heartbeat age in seconds.
fn classify_status(age_secs: i64) -> AgentStatus {
    if age_secs < ACTIVE_THRESHOLD_SECS {
        AgentStatus::Active
    } else if age_secs < IDLE_THRESHOLD_SECS {
        AgentStatus::Idle
    } else {
        AgentStatus::Stale
    }
}

/// Scan `.worktrees/` for a directory whose name matches the given `agent_id`.
///
/// Matching rules (tried in order):
/// 1. Exact slug match.
/// 2. The agent_id contains the slug.
/// 3. The slug contains the agent_id.
fn find_worktree_for_agent(root: &Path, agent_id: &str) -> Option<PathBuf> {
    let worktrees_dir = root.join(".worktrees");
    if !worktrees_dir.is_dir() {
        return None;
    }
    std::fs::read_dir(&worktrees_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .find(|e| {
            let slug = e.file_name().to_string_lossy().to_string();
            agent_id == slug || agent_id.contains(&slug) || slug.contains(agent_id)
        })
        .map(|e| e.path())
}

/// Read the current git branch from a linked worktree directory.
///
/// In a git linked worktree the `.git` entry is a *file* (not a directory)
/// containing `gitdir: <path>`.  We resolve that path and read the `HEAD`
/// file from it.
fn read_worktree_branch(worktree: &Path) -> Option<String> {
    let git_entry = worktree.join(".git");
    let head_content = if git_entry.is_file() {
        // Linked worktree: .git is a file with "gitdir: <path>"
        let git_file = std::fs::read_to_string(&git_entry).ok()?;
        let gitdir = git_file.strip_prefix("gitdir: ")?.trim();
        let head_path = PathBuf::from(gitdir).join("HEAD");
        std::fs::read_to_string(&head_path).ok()?
    } else if git_entry.is_dir() {
        // Bare-style: .git/HEAD
        std::fs::read_to_string(git_entry.join("HEAD")).ok()?
    } else {
        return None;
    };

    // HEAD contains either "ref: refs/heads/<branch>" or a detached SHA.
    head_content
        .strip_prefix("ref: refs/heads/")
        .map(|b| b.trim().to_string())
}

/// Return `true` if the named tmux session is currently running.
fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Derive the expected tmux session name for a worktree slug.
///
/// Mirrors the logic in `commands::kickoff::tmux_session_name`.
fn agent_tmux_session(agent_id: &str) -> String {
    // Strip common prefixes used in agent IDs / branch names
    let slug = agent_id
        .strip_prefix("feature/")
        .or_else(|| agent_id.strip_prefix("feat-"))
        .unwrap_or(agent_id);
    // Split on "--": agent IDs are "<parent>--<slug>"; we want the last part
    let wt_slug = slug.rsplit("--").next().unwrap_or(slug);
    let raw = format!("feat-{}", wt_slug);
    let sanitized: String = raw
        .chars()
        .map(|c| if c == '.' || c == ':' { '-' } else { c })
        .collect();
    if sanitized.len() > 50 {
        sanitized[..50].to_string()
    } else {
        sanitized
    }
}

/// Build an internal-server-error response from an error.
fn internal_error(context: &str, e: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: context.to_string(),
            detail: Some(e.to_string()),
        }),
    )
}

/// Build a not-found response.
fn not_found(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: "not found".to_string(),
            detail: Some(msg.into()),
        }),
    )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/agents` — list all known agents with latest heartbeat and status.
pub async fn list_agents(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let sync = SyncManager::new(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to initialise SyncManager", e))?;

    let heartbeats = sync
        .read_heartbeats_auto()
        .map_err(|e| internal_error("Failed to read heartbeats", e))?;

    let locks_file = sync
        .read_locks_auto()
        .unwrap_or_else(|_| crate::locks::LocksFile::empty());

    let now = Utc::now();
    let root = state
        .crosslink_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| state.crosslink_dir.clone());

    let agents: Vec<AgentSummary> = heartbeats
        .into_iter()
        .map(|hb| {
            let age_secs = now
                .signed_duration_since(hb.last_heartbeat)
                .max(Duration::zero())
                .num_seconds();
            let status = classify_status(age_secs);
            let agent_locks = locks_file.agent_locks(&hb.agent_id);
            let worktree = find_worktree_for_agent(&root, &hb.agent_id);
            let branch = worktree.as_deref().and_then(read_worktree_branch);
            let worktree_path = worktree.map(|p| p.to_string_lossy().into_owned());
            AgentSummary {
                agent_id: hb.agent_id,
                machine_id: hb.machine_id,
                description: None,
                status,
                last_heartbeat: hb.last_heartbeat,
                active_issue_id: hb.active_issue_id,
                branch,
                worktree_path,
                locks: agent_locks,
            }
        })
        .collect();

    let total = agents.len();
    Ok(Json(json!({
        "items": agents,
        "total": total
    })))
}

/// `GET /api/v1/agents/:id` — detailed view of a single agent.
pub async fn get_agent(
    State(state): State<AppState>,
    AxumPath(agent_id): AxumPath<String>,
) -> Result<Json<AgentDetail>, (StatusCode, Json<ApiError>)> {
    let sync = SyncManager::new(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to initialise SyncManager", e))?;

    let heartbeats = sync
        .read_heartbeats_auto()
        .map_err(|e| internal_error("Failed to read heartbeats", e))?;

    let hb = heartbeats
        .into_iter()
        .find(|h| h.agent_id == agent_id)
        .ok_or_else(|| not_found(format!("No heartbeat found for agent '{}'", agent_id)))?;

    let locks_file = sync
        .read_locks_auto()
        .unwrap_or_else(|_| crate::locks::LocksFile::empty());

    let now = Utc::now();
    let age_secs = now
        .signed_duration_since(hb.last_heartbeat)
        .max(Duration::zero())
        .num_seconds();
    let status = classify_status(age_secs);
    let agent_locks = locks_file.agent_locks(&hb.agent_id);

    let root = state
        .crosslink_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| state.crosslink_dir.clone());

    let worktree = find_worktree_for_agent(&root, &hb.agent_id);
    let branch = worktree.as_deref().and_then(read_worktree_branch);
    let worktree_path = worktree.as_ref().map(|p| p.to_string_lossy().into_owned());

    let kickoff_status = worktree.as_ref().and_then(|wt| {
        let path = wt.join(".kickoff-status");
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
    });

    // The hub only stores the latest heartbeat per agent; expose it as a
    // single-entry history.  A future version can persist a rolling log.
    let heartbeat_history = vec![hb.last_heartbeat];

    let summary = AgentSummary {
        agent_id: hb.agent_id,
        machine_id: hb.machine_id,
        description: None,
        status,
        last_heartbeat: hb.last_heartbeat,
        active_issue_id: hb.active_issue_id,
        branch,
        worktree_path,
        locks: agent_locks,
    };

    Ok(Json(AgentDetail {
        summary,
        heartbeat_history,
        kickoff_status,
    }))
}

/// `GET /api/v1/agents/:id/status` — kickoff status for a specific agent.
///
/// Reads the `.kickoff-status` file from the agent's worktree (if present)
/// and reports whether the agent's tmux session is still running.
pub async fn get_agent_status(
    State(state): State<AppState>,
    AxumPath(agent_id): AxumPath<String>,
) -> Result<Json<AgentStatusResponse>, (StatusCode, Json<ApiError>)> {
    let root = state
        .crosslink_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| state.crosslink_dir.clone());

    let worktree = find_worktree_for_agent(&root, &agent_id);

    let kickoff_status = match &worktree {
        Some(wt) => {
            let path = wt.join(".kickoff-status");
            if path.exists() {
                std::fs::read_to_string(&path)
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            } else {
                "running".to_string()
            }
        }
        None => "unknown".to_string(),
    };

    let session_name = agent_tmux_session(&agent_id);
    let tmux_session_active = tmux_session_exists(&session_name);

    Ok(Json(AgentStatusResponse {
        agent_id,
        kickoff_status,
        worktree_path: worktree.map(|p| p.to_string_lossy().into_owned()),
        tmux_session_active,
    }))
}

/// `GET /api/v1/locks` — all active locks with derived metadata.
pub async fn list_locks(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let sync = SyncManager::new(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to initialise SyncManager", e))?;

    let locks_file = sync
        .read_locks_auto()
        .map_err(|e| internal_error("Failed to read locks", e))?;

    let now = Utc::now();
    let stale_timeout = Duration::minutes(locks_file.settings.stale_lock_timeout_minutes as i64);

    let entries: Vec<LockEntry> = locks_file
        .locks
        .iter()
        .filter_map(|(id_str, lock)| {
            let issue_id = id_str.parse::<i64>().ok()?;
            let age = now
                .signed_duration_since(lock.claimed_at)
                .max(Duration::zero());
            let is_stale = age >= stale_timeout;
            Some(LockEntry {
                issue_id,
                agent_id: lock.agent_id.clone(),
                branch: lock.branch.clone(),
                claimed_at: lock.claimed_at,
                signed_by: lock.signed_by.clone(),
                age_seconds: age.num_seconds(),
                is_stale,
            })
        })
        .collect();

    let total = entries.len();
    Ok(Json(json!({
        "items": entries,
        "total": total
    })))
}

/// `GET /api/v1/locks/stale` — locks whose holding agent has gone stale.
///
/// Uses `SyncManager::find_stale_locks_with_age` which accounts for the
/// agent's heartbeat freshness, not just lock claimed-at time.
pub async fn list_stale_locks(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let sync = SyncManager::new(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to initialise SyncManager", e))?;

    let stale = sync
        .find_stale_locks_with_age()
        .map_err(|e| internal_error("Failed to read stale locks", e))?;

    // Re-read the full locks file to get branch/claimed_at/signed_by.
    let locks_file = sync
        .read_locks_auto()
        .unwrap_or_else(|_| crate::locks::LocksFile::empty());

    let now = Utc::now();
    let entries: Vec<LockEntry> = stale
        .into_iter()
        .filter_map(|(issue_id, _agent_id_from_stale, _age_minutes)| {
            let lock = locks_file.get_lock(issue_id)?;
            let age_secs = now
                .signed_duration_since(lock.claimed_at)
                .max(Duration::zero())
                .num_seconds();
            Some(LockEntry {
                issue_id,
                agent_id: lock.agent_id.clone(),
                branch: lock.branch.clone(),
                claimed_at: lock.claimed_at,
                signed_by: lock.signed_by.clone(),
                age_seconds: age_secs,
                is_stale: true,
            })
        })
        .collect();

    let total = entries.len();
    Ok(Json(json!({
        "items": entries,
        "total": total
    })))
}

// ---------------------------------------------------------------------------
// Lock change notification
// ---------------------------------------------------------------------------

/// Request body for `POST /api/v1/locks/notify`.
#[derive(serde::Deserialize)]
pub struct LockNotifyRequest {
    pub issue_id: i64,
    pub action: String,
    pub agent_id: String,
}

/// `POST /api/v1/locks/notify` — broadcast a lock change event over WebSocket.
///
/// Agents call this after claiming or releasing a lock so that all connected
/// WebSocket clients are notified in real time.
pub async fn notify_lock_changed(
    State(state): State<AppState>,
    Json(body): Json<LockNotifyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let action = match body.action.as_str() {
        "claimed" => crate::server::types::LockAction::Claimed,
        "released" => crate::server::types::LockAction::Released,
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(ApiError {
                    error: format!(
                        "Invalid lock action '{}'. Must be 'claimed' or 'released'",
                        other
                    ),
                    detail: None,
                }),
            ));
        }
    };

    let _ = state.ws_tx.send(crate::server::ws::WsEvent::LockChanged(
        crate::server::types::WsLockChangedEvent {
            event_type: "lock_changed",
            issue_id: body.issue_id,
            action,
            agent_id: body.agent_id,
        },
    ));

    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_status_active() {
        assert_eq!(classify_status(0), AgentStatus::Active);
        assert_eq!(classify_status(60), AgentStatus::Active);
        assert_eq!(
            classify_status(ACTIVE_THRESHOLD_SECS - 1),
            AgentStatus::Active
        );
    }

    #[test]
    fn test_classify_status_idle() {
        assert_eq!(classify_status(ACTIVE_THRESHOLD_SECS), AgentStatus::Idle);
        assert_eq!(classify_status(IDLE_THRESHOLD_SECS - 1), AgentStatus::Idle);
    }

    #[test]
    fn test_classify_status_stale() {
        assert_eq!(classify_status(IDLE_THRESHOLD_SECS), AgentStatus::Stale);
        assert_eq!(
            classify_status(IDLE_THRESHOLD_SECS + 3600),
            AgentStatus::Stale
        );
    }

    #[test]
    fn test_agent_tmux_session_basic() {
        let name = agent_tmux_session("add-auth-feature");
        assert_eq!(name, "feat-add-auth-feature");
    }

    #[test]
    fn test_agent_tmux_session_strips_feature_prefix() {
        let name = agent_tmux_session("feature/add-auth");
        assert_eq!(name, "feat-add-auth");
    }

    #[test]
    fn test_agent_tmux_session_sanitizes_dots() {
        let name = agent_tmux_session("fix.auth.bug");
        assert_eq!(name, "feat-fix-auth-bug");
    }

    #[test]
    fn test_agent_tmux_session_truncates() {
        let long = "a".repeat(100);
        let name = agent_tmux_session(&long);
        assert!(name.len() <= 50);
    }

    #[test]
    fn test_find_worktree_exact_match() {
        let dir = tempfile::tempdir().unwrap();
        let worktrees = dir.path().join(".worktrees");
        std::fs::create_dir_all(worktrees.join("my-agent")).unwrap();

        let result = find_worktree_for_agent(dir.path(), "my-agent");
        assert!(result.is_some());
        assert!(result.unwrap().ends_with("my-agent"));
    }

    #[test]
    fn test_find_worktree_no_match() {
        let dir = tempfile::tempdir().unwrap();
        let worktrees = dir.path().join(".worktrees");
        std::fs::create_dir_all(worktrees.join("other-agent")).unwrap();

        let result = find_worktree_for_agent(dir.path(), "nonexistent-xyz");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_worktree_no_worktrees_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_worktree_for_agent(dir.path(), "my-agent");
        assert!(result.is_none());
    }

    #[test]
    fn test_read_worktree_branch_from_file() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate a linked worktree: .git is a file pointing to a real gitdir
        let gitdir = dir.path().join("gitdir");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/feature/my-branch\n").unwrap();
        std::fs::write(
            dir.path().join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        let branch = read_worktree_branch(dir.path());
        assert_eq!(branch, Some("feature/my-branch".to_string()));
    }

    #[test]
    fn test_read_worktree_branch_detached() {
        let dir = tempfile::tempdir().unwrap();
        let gitdir = dir.path().join("gitdir");
        std::fs::create_dir_all(&gitdir).unwrap();
        // Detached HEAD — just a SHA, no "ref: refs/heads/" prefix
        std::fs::write(
            gitdir.join("HEAD"),
            "abc123def456abc123def456abc123def456abc1\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();

        // Detached HEAD has no branch name — should return None
        let branch = read_worktree_branch(dir.path());
        assert!(branch.is_none());
    }

    #[test]
    fn test_read_worktree_branch_bare_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let branch = read_worktree_branch(dir.path());
        assert_eq!(branch, Some("main".to_string()));
    }

    #[test]
    fn test_read_worktree_branch_no_git() {
        let dir = tempfile::tempdir().unwrap();
        let branch = read_worktree_branch(dir.path());
        assert!(branch.is_none());
    }

    #[test]
    fn test_agent_tmux_session_double_dash_split() {
        let name = agent_tmux_session("parent--child-slug");
        assert_eq!(name, "feat-child-slug");
    }

    #[test]
    fn test_agent_tmux_session_feat_prefix() {
        let name = agent_tmux_session("feat-my-task");
        assert_eq!(name, "feat-my-task");
    }

    #[test]
    fn test_agent_tmux_session_colons_sanitized() {
        let name = agent_tmux_session("fix:auth:bug");
        assert_eq!(name, "feat-fix-auth-bug");
    }

    #[test]
    fn test_find_worktree_partial_match_agent_contains_slug() {
        let dir = tempfile::tempdir().unwrap();
        let worktrees = dir.path().join(".worktrees");
        std::fs::create_dir_all(worktrees.join("short")).unwrap();

        // agent_id "long-short-name" contains slug "short"
        let result = find_worktree_for_agent(dir.path(), "long-short-name");
        assert!(result.is_some());
    }

    #[test]
    fn test_find_worktree_partial_match_slug_contains_agent() {
        let dir = tempfile::tempdir().unwrap();
        let worktrees = dir.path().join(".worktrees");
        std::fs::create_dir_all(worktrees.join("my-agent-extended")).unwrap();

        // slug "my-agent-extended" contains agent_id "my-agent"
        let result = find_worktree_for_agent(dir.path(), "my-agent");
        assert!(result.is_some());
    }

    #[test]
    fn test_classify_status_boundary_values() {
        // Exactly at active threshold -> idle
        assert_eq!(classify_status(ACTIVE_THRESHOLD_SECS), AgentStatus::Idle);
        // One below active threshold -> active
        assert_eq!(
            classify_status(ACTIVE_THRESHOLD_SECS - 1),
            AgentStatus::Active
        );
        // Exactly at idle threshold -> stale
        assert_eq!(classify_status(IDLE_THRESHOLD_SECS), AgentStatus::Stale);
        // One below idle threshold -> idle
        assert_eq!(classify_status(IDLE_THRESHOLD_SECS - 1), AgentStatus::Idle);
        // Negative age (clock skew) -> active
        assert_eq!(classify_status(-10), AgentStatus::Active);
    }

    // -----------------------------------------------------------------------
    // Handler integration tests
    // -----------------------------------------------------------------------

    use crate::db::Database;
    use crate::server::{routes::build_router, state::AppState};
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
    };
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

    /// Create a test app with a heartbeat file seeded in the hub cache.
    fn test_app_with_heartbeat(agent_id: &str) -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Seed a heartbeat file in the hub cache
        let heartbeats_dir = crosslink_dir.join(".hub-cache").join("heartbeats");
        std::fs::create_dir_all(&heartbeats_dir).unwrap();
        let hb = serde_json::json!({
            "agent_id": agent_id,
            "last_heartbeat": chrono::Utc::now().to_rfc3339(),
            "active_issue_id": null,
            "machine_id": "test-machine"
        });
        std::fs::write(
            heartbeats_dir.join(format!("{}.json", agent_id)),
            serde_json::to_string(&hb).unwrap(),
        )
        .unwrap();

        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir)
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_list_agents_empty() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/agents")
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
    async fn test_list_agents_with_heartbeat() {
        let (app, _dir) = test_app_with_heartbeat("test-agent-1");
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/agents")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 1);
        let items = body["items"].as_array().unwrap();
        assert_eq!(items[0]["agent_id"], "test-agent-1");
        assert_eq!(items[0]["machine_id"], "test-machine");
        assert_eq!(items[0]["status"], "active");
    }

    #[tokio::test]
    async fn test_get_agent_not_found() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/agents/nonexistent-agent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_agent_found() {
        let (app, _dir) = test_app_with_heartbeat("my-agent");
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/agents/my-agent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // AgentDetail uses #[serde(flatten)] on summary, so fields are top-level
        assert_eq!(body["agent_id"], "my-agent");
        assert_eq!(body["status"], "active");
        assert!(body["heartbeat_history"].as_array().unwrap().len() == 1);
    }

    #[tokio::test]
    async fn test_get_agent_status_unknown() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/agents/unknown-agent/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["agent_id"], "unknown-agent");
        assert_eq!(body["kickoff_status"], "unknown");
        assert_eq!(body["tmux_session_active"], false);
    }

    #[tokio::test]
    async fn test_list_locks_empty() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/locks")
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
    async fn test_list_stale_locks_empty() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/locks/stale")
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
    async fn test_notify_lock_changed_claimed() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/locks/notify")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "issue_id": 1,
                            "action": "claimed",
                            "agent_id": "agent-1"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_notify_lock_changed_released() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/locks/notify")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "issue_id": 42,
                            "action": "released",
                            "agent_id": "agent-2"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["ok"], true);
    }

    #[tokio::test]
    async fn test_notify_lock_changed_invalid_action() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/locks/notify")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "issue_id": 1,
                            "action": "stolen",
                            "agent_id": "agent-1"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(body["error"]
            .as_str()
            .unwrap()
            .contains("Invalid lock action"));
    }

    #[tokio::test]
    async fn test_get_agent_status_with_worktree_no_kickoff_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Create a worktree directory matching the agent name
        let worktrees_dir = dir.path().join(".worktrees").join("my-wt-agent");
        std::fs::create_dir_all(&worktrees_dir).unwrap();

        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/agents/my-wt-agent/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["agent_id"], "my-wt-agent");
        // No .kickoff-status file → defaults to "running"
        assert_eq!(body["kickoff_status"], "running");
    }

    #[tokio::test]
    async fn test_get_agent_status_with_worktree_and_kickoff_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Create a worktree directory matching the agent name
        let worktrees_dir = dir.path().join(".worktrees").join("my-wt-agent2");
        std::fs::create_dir_all(&worktrees_dir).unwrap();
        // Write a .kickoff-status file
        std::fs::write(worktrees_dir.join(".kickoff-status"), "completed\n").unwrap();

        let state = AppState::new(db, crosslink_dir);
        let app = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/agents/my-wt-agent2/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["agent_id"], "my-wt-agent2");
        assert_eq!(body["kickoff_status"], "completed");
        assert!(body["worktree_path"].as_str().is_some());
    }

    #[test]
    fn test_internal_error_helper() {
        let (status, json) = super::internal_error("ctx", "boom");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "ctx");
        assert_eq!(json.detail.as_deref(), Some("boom"));
    }

    #[test]
    fn test_not_found_helper() {
        let (status, json) = super::not_found("missing");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json.error, "not found");
        assert_eq!(json.detail.as_deref(), Some("missing"));
    }

    /// Test app with a seeded heartbeat AND a worktree directory that contains
    /// a .kickoff-status file, so get_agent returns kickoff_status.
    fn test_app_with_heartbeat_and_kickoff(
        agent_id: &str,
        kickoff_status: &str,
    ) -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Seed a heartbeat file in the hub cache.
        let heartbeats_dir = crosslink_dir.join(".hub-cache").join("heartbeats");
        std::fs::create_dir_all(&heartbeats_dir).unwrap();
        let hb = serde_json::json!({
            "agent_id": agent_id,
            "last_heartbeat": chrono::Utc::now().to_rfc3339(),
            "active_issue_id": null,
            "machine_id": "test-machine"
        });
        std::fs::write(
            heartbeats_dir.join(format!("{}.json", agent_id)),
            serde_json::to_string(&hb).unwrap(),
        )
        .unwrap();

        // Create a matching worktree with a .kickoff-status file.
        let worktrees_dir = dir.path().join(".worktrees").join(agent_id);
        std::fs::create_dir_all(&worktrees_dir).unwrap();
        std::fs::write(
            worktrees_dir.join(".kickoff-status"),
            format!("{}\n", kickoff_status),
        )
        .unwrap();

        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir)
    }

    #[tokio::test]
    async fn test_get_agent_with_kickoff_status() {
        let (app, _dir) = test_app_with_heartbeat_and_kickoff("kickoff-agent", "completed");
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/agents/kickoff-agent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["agent_id"], "kickoff-agent");
        // kickoff_status should be populated from the .kickoff-status file
        assert_eq!(body["kickoff_status"], "completed");
    }

    /// Seed a locks.json file in the hub cache with one active lock.
    fn test_app_with_lock(agent_id: &str, issue_id: i64) -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        let hub_cache = crosslink_dir.join(".hub-cache");
        std::fs::create_dir_all(&hub_cache).unwrap();

        let locks_json = serde_json::json!({
            "version": 1,
            "locks": {
                issue_id.to_string(): {
                    "agent_id": agent_id,
                    "branch": "feature/test",
                    "claimed_at": chrono::Utc::now().to_rfc3339(),
                    "signed_by": ""
                }
            },
            "settings": {
                "stale_lock_timeout_minutes": 30
            }
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
    async fn test_list_locks_with_one_lock() {
        let (app, _dir) = test_app_with_lock("lock-agent", 42);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/locks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 1);
        let items = body["items"].as_array().unwrap();
        assert_eq!(items[0]["issue_id"], 42);
        assert_eq!(items[0]["agent_id"], "lock-agent");
        assert_eq!(items[0]["branch"], "feature/test");
        assert_eq!(items[0]["is_stale"], false);
    }

    /// Build a test app where the hub cache has a lock and a stale heartbeat so
    /// that `list_stale_locks` returns at least one entry (exercises lines 407-417).
    fn test_app_with_stale_lock(
        agent_id: &str,
        issue_id: i64,
    ) -> (axum::Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let crosslink_dir = dir.path().join(".crosslink");
        let hub_cache = crosslink_dir.join(".hub-cache");
        std::fs::create_dir_all(hub_cache.join("heartbeats")).unwrap();

        // A heartbeat that is 120 minutes old → agent is stale (threshold is 30 min)
        let old_time = chrono::Utc::now() - chrono::Duration::minutes(120);
        let hb = serde_json::json!({
            "agent_id": agent_id,
            "last_heartbeat": old_time.to_rfc3339(),
            "active_issue_id": issue_id,
            "machine_id": "test-machine"
        });
        std::fs::write(
            hub_cache
                .join("heartbeats")
                .join(format!("{}.json", agent_id)),
            serde_json::to_string(&hb).unwrap(),
        )
        .unwrap();

        // A lock entry claimed at the same old time
        let locks_json = serde_json::json!({
            "version": 1,
            "locks": {
                issue_id.to_string(): {
                    "agent_id": agent_id,
                    "branch": "feature/stale-test",
                    "claimed_at": old_time.to_rfc3339(),
                    "signed_by": ""
                }
            },
            "settings": {
                "stale_lock_timeout_minutes": 30
            }
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
    async fn test_list_stale_locks_with_stale_entry() {
        let (app, _dir) = test_app_with_stale_lock("stale-agent", 77);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/locks/stale")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // There should be at least one stale lock entry
        let total = body["total"].as_u64().unwrap_or(0);
        assert!(
            total >= 1,
            "expected at least one stale lock, got {}",
            total
        );
        let items = body["items"].as_array().unwrap();
        let entry = &items[0];
        assert_eq!(entry["issue_id"], 77);
        assert_eq!(entry["agent_id"], "stale-agent");
        assert_eq!(entry["branch"], "feature/stale-test");
        assert_eq!(entry["is_stale"], true);
        // age_seconds should be positive
        assert!(entry["age_seconds"].as_i64().unwrap_or(0) > 0);
    }

    #[test]
    fn test_internal_error_helper_detail_none_via_display() {
        // Verify internal_error formats any Display type correctly
        let (status, json) = super::internal_error("db error", std::io::Error::other("disk full"));
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "db error");
        assert!(json.detail.as_deref().unwrap().contains("disk full"));
    }

    #[test]
    fn test_not_found_helper_with_owned_string() {
        // Verify not_found accepts an owned String (exercises the Into<String> bound)
        let msg = format!("agent '{}' not found", "worker-1");
        let (status, json) = super::not_found(msg);
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json.error, "not found");
        assert!(json.detail.as_deref().unwrap().contains("worker-1"));
    }
}
