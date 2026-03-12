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
}
