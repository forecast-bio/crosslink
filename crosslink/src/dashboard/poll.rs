//! Poll loop: fetches each tracked project's hub branch, reads a
//! [`crate::dashboard::reader::HubSnapshot`], and upserts the derived
//! counters into the `project_state` table.
//!
//! The loop runs as a background tokio task for the lifetime of the
//! `crosslink dashboard serve` process. Each tick (default: 5 seconds)
//! walks every active project serially — simple, avoids hammering
//! `git`/the network, and good enough for small fleets. Parallel
//! fetches can come later if the per-tick budget is ever exceeded.
//!
//! Lifecycle:
//! - Started by the `DashboardCommands::Serve` dispatch after the
//!   dashboard DB is bootstrapped and before the HTTP server binds.
//! - Cancelled via a [`tokio_util::sync::CancellationToken`] when the
//!   server shuts down.
//! - Per-project errors are logged and isolated — one broken repo
//!   must not stop the rest of the fleet from updating.

use anyhow::Result;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::alerts;
use super::alerts_db;
use super::db::DashboardDb;
use super::projects::Project;
use super::reader;
use super::webhook;
use crate::server::types::{WsDashboardAlertsEvent, WsDashboardProjectEvent, WsEventType};
use crate::server::ws::WsEvent;

/// Default tick duration between poll cycles.
pub const DEFAULT_TICK: Duration = Duration::from_secs(5);

/// Agent active-window threshold (minutes). Heartbeats older than this
/// mean the agent no longer counts toward `project_state.active_agents`.
const DEFAULT_AGENT_ACTIVE_MINUTES: i64 = 10;

/// Stale-lock threshold (minutes). Locks held longer than this count
/// toward `project_state.stale_locks`.
const DEFAULT_STALE_LOCK_MINUTES: i64 = 60;

/// Run the poll loop until cancelled.
///
/// Blocks until the cancellation token fires; intended to be spawned
/// as a tokio task. `ws_tx`, when provided, receives a
/// [`WsEvent::DashboardProjectUpdated`] after every successful
/// per-project `project_state` upsert so WebSocket clients can
/// invalidate their caches ahead of the next poll tick.
pub async fn run(
    db_path: PathBuf,
    cancel: CancellationToken,
    ws_tx: Option<tokio::sync::broadcast::Sender<WsEvent>>,
) {
    run_with_tick(db_path, DEFAULT_TICK, cancel, ws_tx).await;
}

/// Variant of [`run`] with a configurable tick duration. Split out
/// for tests; production callers use [`run`].
pub async fn run_with_tick(
    db_path: PathBuf,
    tick: Duration,
    cancel: CancellationToken,
    ws_tx: Option<tokio::sync::broadcast::Sender<WsEvent>>,
) {
    tracing::info!(
        "dashboard poll loop starting (tick = {:?}, db = {})",
        tick,
        db_path.display()
    );

    let mut interval = tokio::time::interval(tick);
    // Skip one missed tick rather than bursting — the dashboard only
    // cares about steady-state, not catching up after a stall.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::info!("dashboard poll loop cancelled");
                return;
            }
            _ = interval.tick() => {
                if let Err(e) = poll_all_projects(&db_path, ws_tx.as_ref()).await {
                    tracing::warn!("dashboard poll tick failed: {e}");
                }
            }
        }
    }
}

/// Run one pass over every active project. Per-project failures are
/// logged but do not abort the pass.
pub async fn poll_all_projects(
    db_path: &Path,
    ws_tx: Option<&tokio::sync::broadcast::Sender<WsEvent>>,
) -> Result<()> {
    let projects = load_active_projects(db_path)?;
    for project in projects {
        let slug = project.slug.clone();
        let outcome = match poll_project(db_path, &project).await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("poll failed for {slug}: {e}");
                continue;
            }
        };
        // Emit live-update notifications. Send errors only happen
        // when there are no subscribers, which is fine.
        if let Some(tx) = ws_tx {
            let _ = tx.send(WsEvent::DashboardProjectUpdated(WsDashboardProjectEvent {
                event_type: WsEventType::DashboardProjectUpdated,
                slug: slug.clone(),
            }));
            if outcome.alerts_opened > 0 || outcome.alerts_resolved > 0 {
                let _ = tx.send(WsEvent::DashboardAlertsChanged(WsDashboardAlertsEvent {
                    event_type: WsEventType::DashboardAlertsChanged,
                    slug,
                    opened: outcome.alerts_opened,
                    resolved: outcome.alerts_resolved,
                }));
            }
        }
    }
    Ok(())
}

/// Outcome of polling a single project — used to decide which WS
/// events to emit after the DB writes land.
#[derive(Debug, Clone, Copy, Default)]
pub struct PollOutcome {
    pub alerts_opened: u32,
    pub alerts_resolved: u32,
}

/// Poll a single project: fetch, read snapshot, update DB, derive +
/// reconcile alerts. Returns an outcome used by the caller to decide
/// which WS notifications to broadcast.
pub async fn poll_project(db_path: &Path, project: &Project) -> Result<PollOutcome> {
    // 1. `git fetch` the hub branch (best-effort). We don't abort on
    //    fetch failure — the snapshot reader will still observe whatever
    //    is already in the local clone.
    let _ = fetch_hub(&project.clone_path).await;

    // 2. Read snapshot off the filesystem. Blocking operation (rusqlite
    //    + sync I/O) — push to the blocking pool.
    let clone_path = project.clone_path.clone();
    let snapshot = tokio::task::spawn_blocking(move || reader::read_snapshot(&clone_path))
        .await
        .map_err(|e| anyhow::anyhow!("snapshot task panicked: {e}"))??;

    // 3. Derive counters + alerts; write everything in one blocking
    //    pass so the DB sees a consistent view per tick.
    let now = Utc::now();
    let counters = snapshot.derive_counters(
        now,
        DEFAULT_AGENT_ACTIVE_MINUTES,
        DEFAULT_STALE_LOCK_MINUTES,
    );
    let derived_alerts = alerts::derive_alerts(project, &snapshot, now);

    let project_id = project.id;
    let hub_sha = snapshot.hub_sha.clone();
    let last_commit_at = snapshot.last_commit_at.map(|dt| dt.to_rfc3339());
    let ci_state = snapshot.ci_status.as_ref().map(|c| c.state.clone());
    let db_path_owned = db_path.to_path_buf();

    // Also pull the configured webhook URLs while we're on the blocking
    // pool — one DB open, same transaction window as the reconcile.
    let (sync_stats, webhook_urls) =
        tokio::task::spawn_blocking(move || -> Result<(alerts_db::SyncStats, Vec<String>)> {
            write_project_state(
                &db_path_owned,
                project_id,
                hub_sha.as_deref(),
                last_commit_at.as_deref(),
                counters,
                ci_state.as_deref(),
            )?;
            let db = DashboardDb::open(&db_path_owned)?;
            let stats = alerts_db::sync_alerts_for_project(&db, project_id, &derived_alerts)?;
            let urls = webhook::load_urls(&db).unwrap_or_default();
            Ok((stats, urls))
        })
        .await
        .map_err(|e| anyhow::anyhow!("DB update task panicked: {e}"))??;

    // Dispatch webhooks for newly-opened alerts. Fire-and-forget per
    // URL — a stuck endpoint must not hold up the rest of the poll
    // cycle. We spawn one task per (alert × URL) pair so they overlap.
    if !webhook_urls.is_empty() && !sync_stats.opened_alerts.is_empty() {
        let slug = project.slug.clone();
        let fired_at = now;
        for alert in &sync_stats.opened_alerts {
            let notif = webhook::AlertNotification::new(alert, slug.clone(), fired_at);
            let urls = webhook_urls.clone();
            tokio::spawn(async move {
                webhook::dispatch_all(&urls, &notif).await;
            });
        }
    }

    Ok(PollOutcome {
        alerts_opened: u32::try_from(sync_stats.opened).unwrap_or(u32::MAX),
        alerts_resolved: u32::try_from(sync_stats.resolved).unwrap_or(u32::MAX),
    })
}

fn load_active_projects(db_path: &Path) -> Result<Vec<Project>> {
    let db = DashboardDb::open(db_path)?;
    let mut stmt = db.conn.prepare(
        "SELECT id, slug, clone_path, default_branch, hub_sha, hub_fetched_at,
                status, added_at, last_activity_at, pinned
         FROM projects
         WHERE status = 'active'
         ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                slug: row.get(1)?,
                clone_path: PathBuf::from(row.get::<_, String>(2)?),
                default_branch: row.get(3)?,
                hub_sha: row.get(4)?,
                hub_fetched_at: row.get(5)?,
                status: row.get(6)?,
                added_at: row.get(7)?,
                last_activity_at: row.get(8)?,
                pinned: row.get::<_, i64>(9)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Upsert `project_state` and refresh `projects.hub_sha` /
/// `projects.hub_fetched_at` / `projects.last_activity_at`.
fn write_project_state(
    db_path: &Path,
    project_id: i64,
    hub_sha: Option<&str>,
    last_commit_at: Option<&str>,
    counters: super::reader::ProjectCounters,
    ci_status: Option<&str>,
) -> Result<()> {
    let db = DashboardDb::open(db_path)?;
    let now = Utc::now().to_rfc3339();

    db.conn.execute(
        "UPDATE projects
         SET hub_sha = ?1,
             hub_fetched_at = ?2,
             last_activity_at = COALESCE(?3, last_activity_at)
         WHERE id = ?4",
        rusqlite::params![hub_sha, now, last_commit_at, project_id],
    )?;

    db.conn.execute(
        "INSERT INTO project_state
           (project_id, open_issues, overdue_issues, due_soon_issues, blocked_issues,
            active_agents, stale_locks, ci_status, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(project_id) DO UPDATE SET
           open_issues = excluded.open_issues,
           overdue_issues = excluded.overdue_issues,
           due_soon_issues = excluded.due_soon_issues,
           blocked_issues = excluded.blocked_issues,
           active_agents = excluded.active_agents,
           stale_locks = excluded.stale_locks,
           ci_status = excluded.ci_status,
           updated_at = excluded.updated_at",
        rusqlite::params![
            project_id,
            counters.open_issues,
            counters.overdue_issues,
            counters.due_soon_issues,
            counters.blocked_issues,
            counters.active_agents,
            counters.stale_locks,
            ci_status,
            now,
        ],
    )?;

    Ok(())
}

async fn fetch_hub(clone_path: &Path) -> Result<()> {
    // Fetch with an explicit refspec so the local `crosslink/hub` ref
    // is created/updated, not just the remote-tracking ref. The `+`
    // allows non-fast-forward updates (shouldn't happen in practice
    // but doesn't cost anything). Dashboard-auto-cloned repos start
    // with no local branch, and the reader looks for the local ref
    // via `git rev-parse crosslink/hub` — without this refspec, hub
    // state appears empty to the panel despite being present on the
    // remote.
    let status = Command::new("git")
        .arg("-C")
        .arg(clone_path)
        .args([
            "fetch",
            "--quiet",
            "origin",
            "+refs/heads/crosslink/hub:refs/heads/crosslink/hub",
        ])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("git fetch exited with {status}");
    }

    // Materialise the hub branch into `.crosslink/.hub-cache/` as a
    // worktree so the reader's filesystem scan actually finds the
    // issues / heartbeats / locks files. For repos that were set up
    // via `crosslink sync` this already exists (idempotent); for
    // dashboard-auto-cloned repos it doesn't and we'd otherwise read
    // from the `main` working tree which has no hub layout.
    ensure_hub_cache_worktree(clone_path).await;
    Ok(())
}

/// Ensure a `.crosslink/.hub-cache/` worktree exists pointing at the
/// local `crosslink/hub` ref. Idempotent and CLI-safe:
/// - If the canonical worktree at `<clone>/.crosslink/.hub-cache/`
///   is missing, create it with `git worktree add`.
/// - If it exists and is CLEAN, fast-forward via `git reset --hard`.
/// - If it exists and is DIRTY (e.g. `crosslink issue close` wrote
///   new issue files but hasn't committed them yet), leave it alone.
///   Resetting dirty state here would wipe the CLI's in-flight
///   changes before the reader had a chance to observe them (#701).
///
/// We deliberately do NOT touch the legacy nested worktree at
/// `<clone>/crosslink/.crosslink/.hub-cache/` — [`super::reader::resolve_hub_root`]
/// prefers the canonical outer path now, so the nested one falls
/// through. Not managing it here avoids racing against whatever
/// original tool created it.
///
/// Best-effort: failures are logged and swallowed. A broken
/// hub-cache just means the reader falls back to scanning the
/// working tree — no worse than before this function existed.
async fn ensure_hub_cache_worktree(clone_path: &Path) {
    let cache_path = clone_path.join(".crosslink").join(".hub-cache");

    if let Some(parent) = cache_path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    if cache_path.is_dir() {
        // Skip reset if the worktree has uncommitted work — `crosslink
        // issue close` et al write to the working tree and commit
        // asynchronously via `crosslink sync`. Wiping those changes
        // here would make every dashboard write invisible to the
        // reader until the next sync.
        let porcelain = Command::new("git")
            .arg("-C")
            .arg(&cache_path)
            .args(["status", "--porcelain"])
            .output()
            .await;
        let is_dirty = matches!(
            porcelain,
            Ok(out) if out.status.success() && !out.stdout.is_empty()
        );
        if is_dirty {
            return;
        }

        let status = Command::new("git")
            .arg("-C")
            .arg(&cache_path)
            .args(["reset", "--hard", "--quiet", "crosslink/hub"])
            .status()
            .await;
        if let Ok(s) = status {
            if !s.success() {
                tracing::warn!(
                    "hub-cache reset failed at {}: status {s}",
                    cache_path.display()
                );
            }
        }
        return;
    }

    // First-time setup.
    let status = Command::new("git")
        .arg("-C")
        .arg(clone_path)
        .args([
            "worktree",
            "add",
            "--force",
            "--quiet",
            cache_path.to_string_lossy().as_ref(),
            "crosslink/hub",
        ])
        .status()
        .await;
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => tracing::warn!(
            "`git worktree add` for hub-cache exited with {s} in {}",
            clone_path.display()
        ),
        Err(e) => tracing::warn!(
            "`git worktree add` for hub-cache failed in {}: {e}",
            clone_path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    /// Build a minimal git repo with a `crosslink/hub` branch populated
    /// from the given file tree. Returns the clone-shaped path that
    /// `poll_project` expects to work on.
    fn make_fake_clone(hub_files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let path = dir.path();

        StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(["init", "-q", "-b", "crosslink/hub"])
            .status()
            .unwrap();
        StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.email", "test@test.local"])
            .status()
            .unwrap();
        StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(["config", "user.name", "Test"])
            .status()
            .unwrap();

        for (rel, contents) in hub_files {
            let full = path.join(rel);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, contents).unwrap();
        }

        StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(["add", "-A"])
            .status()
            .unwrap();
        StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(["commit", "-q", "-m", "test fixture"])
            .status()
            .unwrap();

        dir
    }

    fn seed_project(db_path: &Path, slug: &str, clone_path: &Path) -> i64 {
        let db = DashboardDb::open(db_path).unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES (?1, ?2, 'main', 'active', ?3)",
                rusqlite::params![
                    slug,
                    clone_path.to_string_lossy().as_ref(),
                    Utc::now().to_rfc3339()
                ],
            )
            .unwrap();
        db.conn.last_insert_rowid()
    }

    #[tokio::test]
    async fn test_poll_project_populates_state_from_empty_hub() {
        let home = tempdir().unwrap();
        let db_path = home.path().join("dashboard.db");
        DashboardDb::open(&db_path).unwrap();
        let clone = make_fake_clone(&[("README.md", "hi")]);

        let project_id = seed_project(&db_path, "owner/repo", clone.path());
        let project = load_active_projects(&db_path).unwrap();
        let project = project.into_iter().find(|p| p.id == project_id).unwrap();

        poll_project(&db_path, &project).await.unwrap();

        let db = DashboardDb::open(&db_path).unwrap();
        let (open, overdue, blocked, agents, stale): (i64, i64, i64, i64, i64) = db
            .conn
            .query_row(
                "SELECT open_issues, overdue_issues, blocked_issues, active_agents, stale_locks
                 FROM project_state WHERE project_id = ?1",
                [project_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(open, 0);
        assert_eq!(overdue, 0);
        assert_eq!(blocked, 0);
        assert_eq!(agents, 0);
        assert_eq!(stale, 0);
    }

    #[tokio::test]
    async fn test_poll_project_counts_open_issue() {
        let home = tempdir().unwrap();
        let db_path = home.path().join("dashboard.db");
        DashboardDb::open(&db_path).unwrap();

        let issue_json = serde_json::json!({
            "uuid": "00000000-0000-0000-0000-000000000001",
            "display_id": 1,
            "title": "t",
            "status": "open",
            "priority": "medium",
            "created_by": "a",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        });
        let clone = make_fake_clone(&[(
            "issues/00000000-0000-0000-0000-000000000001/issue.json",
            &issue_json.to_string(),
        )]);

        let project_id = seed_project(&db_path, "owner/repo", clone.path());
        let project = load_active_projects(&db_path)
            .unwrap()
            .into_iter()
            .find(|p| p.id == project_id)
            .unwrap();

        poll_project(&db_path, &project).await.unwrap();

        let db = DashboardDb::open(&db_path).unwrap();
        let open: i64 = db
            .conn
            .query_row(
                "SELECT open_issues FROM project_state WHERE project_id = ?1",
                [project_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(open, 1);
    }

    #[tokio::test]
    async fn test_poll_all_projects_tolerates_one_broken() {
        let home = tempdir().unwrap();
        let db_path = home.path().join("dashboard.db");
        DashboardDb::open(&db_path).unwrap();

        // Good project
        let clone = make_fake_clone(&[("README.md", "hi")]);
        seed_project(&db_path, "good/one", clone.path());

        // Broken project (clone_path doesn't exist)
        let db = DashboardDb::open(&db_path).unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES ('broken/one', '/nonexistent/path', 'main', 'active', ?1)",
                [Utc::now().to_rfc3339()],
            )
            .unwrap();
        drop(db);

        // Should return Ok — per-project errors are logged, not fatal.
        poll_all_projects(&db_path, None).await.unwrap();

        // The good project still got its project_state populated.
        let db = DashboardDb::open(&db_path).unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM project_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "only the healthy project should have project_state"
        );
    }

    #[tokio::test]
    async fn test_run_cancels_cleanly() {
        let home = tempdir().unwrap();
        let db_path = home.path().join("dashboard.db");
        DashboardDb::open(&db_path).unwrap();

        let cancel = CancellationToken::new();
        let handle = tokio::spawn({
            let cancel = cancel.clone();
            let path = db_path.clone();
            async move { run_with_tick(path, Duration::from_millis(50), cancel, None).await }
        });

        // Let the loop tick once before cancelling.
        tokio::time::sleep(Duration::from_millis(120)).await;
        cancel.cancel();
        // Must terminate within a reasonable window.
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("poll loop did not exit after cancel")
            .expect("poll loop task panicked");
    }
}
