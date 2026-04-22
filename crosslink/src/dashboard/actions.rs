//! Shell-out primitive for write operations.
//!
//! Every dashboard write — close issue, add comment, claim lock,
//! whatever — lands here. We invoke the real `crosslink` CLI with
//! `Command::new("crosslink").current_dir(<project workspace>)` so
//! writes flow through exactly the same code path as a user typing
//! the command directly. Zero drift, zero duplicated logic.
//!
//! Each invocation gets a row in the `actions` audit table capturing
//! the actor (driver fingerprint), verb, subject, args, outcome, and
//! timing. The actor comes from the `user.signingkey` git config on
//! the project's workspace — we don't take a per-user config; we
//! assume the user already configured crosslink.
//!
//! Returned text is whatever the CLI wrote to stdout. Handlers can
//! pass it straight to the frontend for user-visible confirmation.

use anyhow::Result;
use chrono::Utc;
use rusqlite::params;
use std::path::Path;
use tokio::process::Command;

use super::db::DashboardDb;
use super::projects::Project;

/// Resolved outcome of an action invocation.
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub stdout: String,
    pub stderr: String,
}

/// Look up a project by slug. Returns `None` if the slug isn't tracked.
///
/// # Errors
/// Propagates `SQLite` errors from the lookup.
pub fn find_project_by_slug(db: &DashboardDb, slug: &str) -> Result<Option<Project>> {
    let mut stmt = db.conn.prepare(
        "SELECT id, slug, clone_path, default_branch, hub_sha, hub_fetched_at,
                status, added_at, last_activity_at, pinned
         FROM projects WHERE slug = ?1",
    )?;
    let row = stmt
        .query_row([slug], |row| {
            Ok(Project {
                id: row.get(0)?,
                slug: row.get(1)?,
                clone_path: std::path::PathBuf::from(row.get::<_, String>(2)?),
                default_branch: row.get(3)?,
                hub_sha: row.get(4)?,
                hub_fetched_at: row.get(5)?,
                status: row.get(6)?,
                added_at: row.get(7)?,
                last_activity_at: row.get(8)?,
                pinned: row.get::<_, i64>(9)? != 0,
            })
        })
        .ok();
    Ok(row)
}

/// Run the `crosslink` CLI in a project's workspace and record an
/// audit row regardless of outcome.
///
/// `verb` and `subject` are the audit-log key; `args` is the actual
/// argv passed to `crosslink`. The workspace's git config / agent
/// identity are what sign any commits the CLI produces — the
/// dashboard doesn't invent a new identity.
///
/// # Errors
/// Returns an error if the subprocess fails to spawn, exits non-zero,
/// or the audit INSERT fails. The audit row is written in both
/// success and failure paths.
pub async fn run_cli(
    db_path: &Path,
    project: &Project,
    verb: &str,
    subject: Option<&str>,
    args: &[&str],
) -> Result<ActionResult> {
    let requested_at = Utc::now().to_rfc3339();
    let payload_json = serde_json::to_string(&serde_json::json!({
        "args": args,
        "cwd": project.clone_path.to_string_lossy(),
    }))
    .unwrap_or_else(|_| "{}".to_string());

    let actor = resolve_actor(&project.clone_path).unwrap_or_else(|| "unknown".to_string());

    // Resolve the `crosslink` binary via PATH first (canonical for
    // installed setups — reinstalling the CLI updates the dashboard
    // automatically), then CROSSLINK_BIN override, then self-exe as
    // a dev fallback. See `projects::resolve_crosslink_bin`.
    let cmd_name = super::projects::resolve_crosslink_bin();
    let output = Command::new(&cmd_name)
        .current_dir(&project.clone_path)
        .args(args)
        .output()
        .await;

    let completed_at = Utc::now().to_rfc3339();
    let (outcome, error, stdout, stderr) = match &output {
        Ok(out) if out.status.success() => (
            "success",
            None::<String>,
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ),
        Ok(out) => (
            "failed",
            Some(format!(
                "crosslink exited {}: {}",
                out.status
                    .code()
                    .map_or_else(|| "signal".into(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ),
        Err(e) => (
            "failed",
            Some(format!("failed to spawn crosslink: {e}")),
            String::new(),
            String::new(),
        ),
    };

    // Best-effort audit write — don't let DB errors mask the subprocess result.
    let project_id = project.id;
    let verb_owned = verb.to_string();
    let subject_owned = subject.map(str::to_string);
    let error_owned = error.clone();
    let db_path_owned = db_path.to_path_buf();
    let audit_res = tokio::task::spawn_blocking(move || -> Result<()> {
        let db = DashboardDb::open(&db_path_owned)?;
        db.conn.execute(
            "INSERT INTO actions
               (project_id, actor, verb, subject, payload_json,
                requested_at, completed_at, outcome, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                project_id,
                actor,
                verb_owned,
                subject_owned,
                payload_json,
                requested_at,
                completed_at,
                outcome,
                error_owned,
            ],
        )?;
        Ok(())
    })
    .await;
    if let Err(e) = audit_res {
        tracing::warn!("audit insert failed for {verb} on {}: {e}", project.slug);
    } else if let Ok(Err(e)) = audit_res {
        tracing::warn!("audit write failed for {verb} on {}: {e}", project.slug);
    }

    if let Some(e) = error {
        anyhow::bail!("{e}");
    }

    // After a successful write, run `crosslink sync` in the same
    // workspace to promote any still-uncommitted hub-branch state
    // into actual commits + push to origin. Without this the CLI's
    // "close issue" writes the new issue file to the hub-cache
    // working tree but only commits it on the next explicit sync —
    // meaning the dashboard's reader (and any other client polling
    // origin) never sees the close until someone runs `crosslink
    // sync` manually (#701).
    //
    // Best-effort: sync failures are logged but don't poison the
    // primary action's success. The user already got their close;
    // worst case the next 5-s poll tick tries again via a different
    // path.
    let sync_out = Command::new(&cmd_name)
        .current_dir(&project.clone_path)
        .args(["sync", "-q"])
        .output()
        .await;
    if let Ok(out) = sync_out {
        if !out.status.success() {
            tracing::warn!(
                "post-{verb} `crosslink sync` in {} exited {}: {}",
                project.slug,
                out.status
                    .code()
                    .map_or_else(|| "signal".into(), |c| c.to_string()),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }

    // Fast-forward the hub-cache worktree so the frontend's
    // mutation-triggered refetch (which fires within milliseconds)
    // reads the fresh hub tip, not the pre-mutation one. Without
    // this, close/release/steal etc. all have a ≤5s latency until
    // the poll loop's own `ensure_hub_cache_worktree` runs — and
    // the user sees "nothing happened" in the meantime (#716).
    //
    // Skip the reset if the worktree has uncommitted state —
    // something else (concurrent CLI call) is mid-write; don't
    // wipe their progress. Same safety as poll::ensure_hub_cache_worktree.
    let hub_cache = project.clone_path.join(".crosslink").join(".hub-cache");
    if hub_cache.is_dir() {
        let porcelain = Command::new("git")
            .arg("-C")
            .arg(&hub_cache)
            .args(["status", "--porcelain"])
            .output()
            .await;
        let is_dirty = matches!(
            porcelain,
            Ok(out) if out.status.success() && !out.stdout.is_empty()
        );
        if !is_dirty {
            let _ = Command::new("git")
                .arg("-C")
                .arg(&hub_cache)
                .args(["reset", "--hard", "--quiet", "crosslink/hub"])
                .status()
                .await;
        }
    }

    Ok(ActionResult { stdout, stderr })
}

/// Read `user.signingkey` from the workspace's git config so audit
/// rows can record who initiated each action. Falls back to `None`
/// if the config isn't set — the audit row still lands with
/// `actor = "unknown"`.
fn resolve_actor(clone_path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(clone_path)
        .args(["config", "user.signingkey"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        None
    } else {
        Some(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    fn temp_env() -> (tempfile::TempDir, std::path::PathBuf, Project) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("dashboard.db");
        let db = DashboardDb::open(&db_path).unwrap();

        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        StdCommand::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q"])
            .status()
            .unwrap();

        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES ('owner/repo', ?1, 'main', 'active', '2026-04-20T00:00:00Z')",
                [repo.to_string_lossy().as_ref()],
            )
            .unwrap();
        let project_id = db.conn.last_insert_rowid();

        let project = find_project_by_slug(&db, "owner/repo")
            .unwrap()
            .expect("just-inserted project should load");
        assert_eq!(project.id, project_id);
        (dir, db_path, project)
    }

    #[tokio::test]
    async fn test_run_cli_records_action_even_on_failure() {
        let (_dir, db_path, project) = temp_env();
        // Deliberately pass a subcommand that will fail (no .crosslink/
        // in the fake repo, so any real crosslink subcommand will
        // error out). We care about: does the audit row land?
        let result = run_cli(
            &db_path,
            &project,
            "close_issue",
            Some("issue:1"),
            &["issue", "close", "1"],
        )
        .await;
        assert!(
            result.is_err(),
            "expected the CLI to fail in a non-crosslink repo"
        );

        let db = DashboardDb::open(&db_path).unwrap();
        let row: (String, String, Option<String>, String) = db
            .conn
            .query_row(
                "SELECT verb, outcome, error, subject FROM actions
                 WHERE project_id = ?1 ORDER BY id DESC LIMIT 1",
                [project.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(row.0, "close_issue");
        assert_eq!(row.1, "failed");
        assert!(row.2.is_some(), "failure should record an error message");
        assert_eq!(row.3, "issue:1");
    }

    #[test]
    fn test_find_project_by_slug_returns_none_for_missing() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("dashboard.db");
        let db = DashboardDb::open(&db_path).unwrap();
        assert!(find_project_by_slug(&db, "nope/missing").unwrap().is_none());
    }
}
