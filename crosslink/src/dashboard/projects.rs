//! Tracked-project CRUD for `crosslink dashboard track / untrack / list`.
//!
//! The dashboard aggregates across repositories the user already has
//! cloned locally. `track` takes a path to an existing working copy of
//! a crosslink-managed repository — it does *not* clone; that would
//! duplicate the user's existing crosslink workspace and force us to
//! re-mint agent identities, signing config, and hub caches in our
//! private copy.
//!
//! With this arrangement:
//! - The poll loop runs `git fetch` in the user's real workspace; no
//!   fresh clone needed.
//! - The write surface (P1.8+) shells out to the real `crosslink` CLI
//!   in that same workspace — inheriting the workspace's signing
//!   config, agent identity, and hub-cache setup automatically.
//! - `untrack` simply removes the DB row; it never deletes the
//!   user's working copy.

use anyhow::{bail, Context, Result};
use rusqlite::params;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::db::DashboardDb;

/// A tracked repository, hydrated from the `projects` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: i64,
    pub slug: String,
    /// Path to the user's existing working copy of the repo (NOT a
    /// dashboard-owned clone — we never make our own).
    pub clone_path: PathBuf,
    pub default_branch: String,
    pub hub_sha: Option<String>,
    pub hub_fetched_at: Option<String>,
    pub status: String,
    pub added_at: String,
    pub last_activity_at: Option<String>,
    pub pinned: bool,
}

/// Whether a tracked workspace can accept dashboard write actions.
///
/// Write actions shell out to the real `crosslink` CLI in that
/// workspace — if the workspace is a bare `git clone` with no
/// `crosslink init` + `crosslink agent init` run, those actions fail
/// with "No agent configured" (and similar). This lets the frontend
/// show a clear "not initialized" badge before the operator wastes a
/// click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteCapability {
    /// Workspace has `.crosslink/issues.db` and `.crosslink/driver-key.pub`
    /// — write actions should succeed.
    Ready,
    /// Workspace has `.crosslink/issues.db` but no `driver-key.pub`
    /// — `crosslink init` ran but `crosslink agent init <id>` didn't.
    /// Write actions that need an agent identity will fail.
    AgentMissing,
    /// Workspace has no `.crosslink/issues.db` — `crosslink init`
    /// hasn't been run at all. Any write action will fail.
    NotInitialized,
}

impl WriteCapability {
    /// Stable machine-readable tag. Shipped on the API as
    /// `write_capability` so frontend code can branch on it without
    /// string-matching user-facing copy.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::AgentMissing => "agent_missing",
            Self::NotInitialized => "not_initialized",
        }
    }
}

/// Inspect a tracked workspace and return its write capability.
///
/// Pure filesystem check — no subprocess, no git touch. Safe to call
/// on every API serialization.
#[must_use]
pub fn write_capability(clone_path: &Path) -> WriteCapability {
    let cl = clone_path.join(".crosslink");
    if !cl.join("issues.db").is_file() {
        return WriteCapability::NotInitialized;
    }
    if !cl.join("driver-key.pub").is_file() {
        return WriteCapability::AgentMissing;
    }
    WriteCapability::Ready
}

/// Validate an `owner/repo` slug. Returns `(owner, repo)` on success.
fn parse_slug(slug: &str) -> Result<(&str, &str)> {
    let mut parts = slug.splitn(2, '/');
    let owner = parts.next().unwrap_or_default();
    let repo = parts.next().unwrap_or_default();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        bail!("slug must be in the form `owner/repo`, got: {slug}");
    }
    if owner.contains(std::path::is_separator)
        || repo.contains(std::path::is_separator)
        || owner.contains('\\')
        || repo.contains('\\')
    {
        bail!("slug must not contain path separators: {slug}");
    }
    Ok((owner, repo))
}

/// Parse `owner/repo` out of a git remote URL.
///
/// Handles the common forms:
/// - `git@github.com:forecast-bio/sigil.git`
/// - `https://github.com/forecast-bio/sigil.git`
/// - `https://github.com/forecast-bio/sigil` (no `.git`)
/// - Arbitrary hosts (e.g. `git@gitlab.example.com:group/proj.git`)
///
/// Returns `None` if the URL doesn't match an obvious "host/owner/repo"
/// shape — callers fall back to `--slug` in that case.
pub(super) fn slug_from_remote_url(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(".git");
    // ssh form: git@host:owner/repo
    if let Some((host_part, path_part)) = url.split_once(':') {
        if !path_part.is_empty() && !path_part.starts_with("//") {
            // Reject scheme:// URLs that happened to split on the
            // protocol colon (http:, https:, ssh:, git:).
            let looks_like_scheme =
                matches!(host_part, "http" | "https" | "ssh" | "git" | "ftp" | "file");
            if !looks_like_scheme {
                return extract_owner_repo(path_part);
            }
        }
    }
    // https / http / git:// form: strip scheme + host, take path
    if let Some(path_part) = url
        .split_once("://")
        .and_then(|(_s, rest)| rest.split_once('/').map(|(_host, path)| path))
    {
        return extract_owner_repo(path_part);
    }
    None
}

fn extract_owner_repo(path_part: &str) -> Option<String> {
    let parts: Vec<&str> = path_part
        .trim_start_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    // Take the LAST two segments (handles nested paths on GitLab/Gitea).
    if parts.len() < 2 {
        return None;
    }
    let owner = parts[parts.len() - 2];
    let repo = parts[parts.len() - 1];
    Some(format!("{owner}/{repo}"))
}

/// Run `git -C <path> remote get-url origin` and return the URL.
fn origin_url(clone_path: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(clone_path)
        .args(["remote", "get-url", "origin"])
        .output()
        .context("Failed to invoke git")?;
    if !out.status.success() {
        bail!(
            "`git remote get-url origin` failed for {}: {}",
            clone_path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// CLI-level wrapper: resolves the default DB path and delegates.
///
/// # Errors
/// As [`track_at_path`], plus home-dir resolution.
pub fn track(clone_path: &Path, slug_override: Option<&str>) -> Result<()> {
    let db_path = DashboardDb::default_path()?;
    track_at_path(clone_path, slug_override, &db_path)
}

/// Like [`track`], but also runs `crosslink init --defaults` +
/// `crosslink agent init <agent_id>` in the workspace (unless it's
/// already fully initialised — the helper is idempotent-on-Ready).
///
/// # Errors
/// Returns an error if the init or agent-init step fails; the project
/// is *not* registered in that case (the caller should fix the
/// workspace and re-run).
pub fn track_with_init(
    clone_path: &Path,
    slug_override: Option<&str>,
    agent_id: &str,
) -> Result<()> {
    let db_path = DashboardDb::default_path()?;
    if write_capability(clone_path) != WriteCapability::Ready {
        run_init_and_agent_in(clone_path, agent_id)?;
    }
    track_at_path(clone_path, slug_override, &db_path)
}

/// Shell out to the running `crosslink` binary to initialise a
/// workspace. Used by both the CLI (`crosslink dashboard track
/// --init`) and the retrofit REST endpoint.
///
/// # Errors
/// Returns an error describing which subprocess step failed and its
/// stderr output.
pub fn run_init_and_agent_in(workspace: &Path, agent_id: &str) -> Result<()> {
    let self_exe = std::env::current_exe().ok();
    let usable_self = self_exe.as_deref().filter(|p| {
        !p.components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new("deps"))
    });
    let cmd_name: std::ffi::OsString = usable_self
        .map_or_else(|| "crosslink".into(), |p| p.as_os_str().to_os_string());

    // `--force` lets init re-run cleanly when a previous retrofit left
    // partial state (a stray `.crosslink/agent.json` without
    // `issues.db` / `hook-config.json`). Idempotent on already-clean
    // workspaces — init --force just refreshes hooks.
    let init_out = Command::new(&cmd_name)
        .current_dir(workspace)
        .args(["init", "--defaults", "-q", "--force"])
        .output()
        .context("spawn `crosslink init`")?;
    if !init_out.status.success() {
        let stderr = String::from_utf8_lossy(&init_out.stderr);
        bail!("crosslink init failed: {}", stderr.trim());
    }

    // --force is load-bearing: `crosslink init --defaults` writes a
    // placeholder `.crosslink/agent.json`, so an unforced
    // `crosslink agent init` fails with "Agent already configured".
    // We only enter this helper when `write_capability != Ready`
    // (i.e. driver-key.pub is missing), so overwriting the placeholder
    // is exactly what we want.
    let agent_out = Command::new(&cmd_name)
        .current_dir(workspace)
        .args([
            "agent",
            "init",
            agent_id,
            "-q",
            "--force",
            "--description",
            "dashboard auto-bootstrap",
        ])
        .output()
        .context("spawn `crosslink agent init`")?;
    if !agent_out.status.success() {
        let stderr = String::from_utf8_lossy(&agent_out.stderr);
        bail!("crosslink agent init failed: {}", stderr.trim());
    }
    Ok(())
}

/// Add a repository to the tracked set.
///
/// - Validates that `clone_path` exists and is a git repository.
/// - Derives the slug from the repo's `origin` remote URL (unless
///   `slug_override` is provided).
/// - Warns (but does not error) if the repo has no `crosslink/hub`
///   branch yet — the tile will show `unreachable_project` until the
///   branch appears; useful for tracking repos that are about to be
///   initialized.
/// - Inserts a row in the `projects` table. No clone, no mutation to
///   the user's working tree.
///
/// # Errors
/// Returns an error if the path doesn't exist or isn't a git repo,
/// the slug is invalid, the slug is already tracked, or the DB insert
/// fails.
pub fn track_at_path(clone_path: &Path, slug_override: Option<&str>, db_path: &Path) -> Result<()> {
    if !clone_path.is_dir() {
        bail!(
            "path does not exist or is not a directory: {}",
            clone_path.display()
        );
    }
    if !clone_path.join(".git").exists() {
        bail!(
            "path is not a git repository (no .git found): {}",
            clone_path.display()
        );
    }

    let slug = if let Some(s) = slug_override {
        parse_slug(s)?;
        s.to_string()
    } else {
        let url = origin_url(clone_path)?;
        slug_from_remote_url(&url).ok_or_else(|| {
            anyhow::anyhow!(
                "could not derive slug from origin URL `{url}` — pass --slug owner/repo"
            )
        })?
    };
    parse_slug(&slug)?;

    // Soft-check for the hub branch — warn if missing, don't block.
    let hub_check = Command::new("git")
        .arg("-C")
        .arg(clone_path)
        .args(["rev-parse", "--verify", "crosslink/hub"])
        .output()
        .ok();
    let has_hub = hub_check.is_some_and(|o| o.status.success());
    if !has_hub {
        eprintln!(
            "warning: {slug} has no `crosslink/hub` branch yet — \
             tracking anyway, dashboard will surface this as unreachable."
        );
    }

    let db = DashboardDb::open(db_path)?;
    let existing: Option<i64> = db
        .conn
        .query_row("SELECT id FROM projects WHERE slug = ?1", [&slug], |row| {
            row.get(0)
        })
        .ok();
    if existing.is_some() {
        bail!("{slug} is already tracked");
    }

    let canonical = clone_path
        .canonicalize()
        .unwrap_or_else(|_| clone_path.to_path_buf());
    let now = chrono::Utc::now().to_rfc3339();
    db.conn.execute(
        "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
         VALUES (?1, ?2, ?3, 'active', ?4)",
        params![
            slug,
            canonical.to_string_lossy().as_ref(),
            "main", // TODO: detect actual default branch via git symbolic-ref
            now
        ],
    )?;

    println!(
        "Tracking {slug} at {}{}",
        canonical.display(),
        if has_hub {
            ""
        } else {
            " (crosslink/hub missing)"
        }
    );
    Ok(())
}

/// CLI-level wrapper: resolves the default DB path and delegates.
///
/// # Errors
/// As [`untrack_with_path`], plus home-dir resolution.
pub fn untrack(slug: &str) -> Result<()> {
    let db_path = DashboardDb::default_path()?;
    untrack_with_path(slug, &db_path)
}

/// Stop tracking a project. Deletes the `projects` row (CASCADE
/// cleans up `project_state`, `alerts`, `activity`). The user's
/// working copy is never touched — this command only affects dashboard
/// state.
///
/// # Errors
/// Returns an error if the slug isn't tracked or the DB delete fails.
pub fn untrack_with_path(slug: &str, db_path: &Path) -> Result<()> {
    parse_slug(slug)?;

    let db = DashboardDb::open(db_path)?;
    let rows = db
        .conn
        .execute("DELETE FROM projects WHERE slug = ?1", [slug])?;
    if rows == 0 {
        bail!("{slug} is not currently tracked");
    }
    println!("Untracked {slug} (local working copy left intact)");
    Ok(())
}

/// CLI-level wrapper: resolves the default DB path and delegates.
///
/// # Errors
/// As [`list_with_path`], plus home-dir resolution.
pub fn list() -> Result<()> {
    let db_path = DashboardDb::default_path()?;
    list_with_path(&db_path)
}

/// Core list implementation, parameterised on DB path.
///
/// # Errors
/// Returns an error if the DB can't be opened or the query fails.
pub fn list_with_path(db_path: &Path) -> Result<()> {
    let db = DashboardDb::open(db_path)?;

    let mut stmt = db.conn.prepare(
        "SELECT id, slug, clone_path, default_branch, hub_sha, hub_fetched_at,
                status, added_at, last_activity_at, pinned
         FROM projects
         ORDER BY pinned DESC, slug ASC",
    )?;
    let projects: Vec<Project> = stmt
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

    if projects.is_empty() {
        println!(
            "No tracked projects. Add one with \
             `crosslink dashboard track <path-to-repo>`."
        );
        return Ok(());
    }

    println!(
        "{:<5} {:<40} {:<10} {:<25} Working copy",
        "PIN", "SLUG", "STATUS", "LAST FETCH"
    );
    for p in &projects {
        let pin = if p.pinned { "●" } else { " " };
        let last_fetch = p.hub_fetched_at.as_deref().unwrap_or("—");
        println!(
            "{pin:<5} {:<40} {:<10} {:<25} {}",
            p.slug,
            p.status,
            last_fetch,
            p.clone_path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    fn temp_db() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("dashboard.db");
        DashboardDb::open(&db_path).unwrap();
        (dir, db_path)
    }

    #[test]
    fn test_write_capability_not_initialized_when_crosslink_dir_missing() {
        let dir = tempdir().unwrap();
        assert_eq!(
            write_capability(dir.path()),
            WriteCapability::NotInitialized
        );
    }

    #[test]
    fn test_write_capability_not_initialized_when_issues_db_missing() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".crosslink")).unwrap();
        // Just .crosslink/ with no issues.db → still "not initialized"
        // since init hasn't actually completed.
        assert_eq!(
            write_capability(dir.path()),
            WriteCapability::NotInitialized
        );
    }

    #[test]
    fn test_write_capability_agent_missing_when_key_absent() {
        let dir = tempdir().unwrap();
        let cl = dir.path().join(".crosslink");
        std::fs::create_dir_all(&cl).unwrap();
        std::fs::write(cl.join("issues.db"), []).unwrap();
        // No driver-key.pub → agent init never ran
        assert_eq!(write_capability(dir.path()), WriteCapability::AgentMissing);
    }

    #[test]
    fn test_write_capability_ready_when_both_present() {
        let dir = tempdir().unwrap();
        let cl = dir.path().join(".crosslink");
        std::fs::create_dir_all(&cl).unwrap();
        std::fs::write(cl.join("issues.db"), []).unwrap();
        std::fs::write(cl.join("driver-key.pub"), b"ssh-ed25519 AAAA...").unwrap();
        assert_eq!(write_capability(dir.path()), WriteCapability::Ready);
    }

    #[test]
    fn test_write_capability_as_str_is_stable() {
        assert_eq!(WriteCapability::Ready.as_str(), "ready");
        assert_eq!(WriteCapability::AgentMissing.as_str(), "agent_missing");
        assert_eq!(WriteCapability::NotInitialized.as_str(), "not_initialized");
    }

    /// Initialise a minimal git repo at the given path with a given
    /// origin URL and an empty `crosslink/hub` branch.
    fn make_fake_repo(path: &Path, origin: &str, with_hub: bool) {
        StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(["init", "-q", "-b", "main"])
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
        StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(["commit", "--allow-empty", "-q", "-m", "init"])
            .status()
            .unwrap();
        StdCommand::new("git")
            .arg("-C")
            .arg(path)
            .args(["remote", "add", "origin", origin])
            .status()
            .unwrap();
        if with_hub {
            StdCommand::new("git")
                .arg("-C")
                .arg(path)
                .args(["checkout", "-q", "--orphan", "crosslink/hub"])
                .status()
                .unwrap();
            StdCommand::new("git")
                .arg("-C")
                .arg(path)
                .args(["commit", "--allow-empty", "-q", "-m", "hub init"])
                .status()
                .unwrap();
            StdCommand::new("git")
                .arg("-C")
                .arg(path)
                .args(["checkout", "-q", "main"])
                .status()
                .unwrap();
        }
    }

    // ── slug parsing & derivation ──

    #[test]
    fn test_parse_slug_valid() {
        assert_eq!(
            parse_slug("forecast-bio/crosslink").unwrap(),
            ("forecast-bio", "crosslink")
        );
    }

    #[test]
    fn test_parse_slug_rejects_single_segment() {
        assert!(parse_slug("crosslink").is_err());
    }

    #[test]
    fn test_parse_slug_rejects_empty_owner() {
        assert!(parse_slug("/crosslink").is_err());
    }

    #[test]
    fn test_parse_slug_rejects_path_traversal() {
        assert!(parse_slug("../etc/passwd").is_err());
        assert!(parse_slug("foo\\bar").is_err());
    }

    #[test]
    fn test_slug_from_ssh_url() {
        assert_eq!(
            slug_from_remote_url("git@github.com:forecast-bio/sigil.git"),
            Some("forecast-bio/sigil".to_string())
        );
    }

    #[test]
    fn test_slug_from_https_url_with_git_suffix() {
        assert_eq!(
            slug_from_remote_url("https://github.com/forecast-bio/sigil.git"),
            Some("forecast-bio/sigil".to_string())
        );
    }

    #[test]
    fn test_slug_from_https_url_without_git_suffix() {
        assert_eq!(
            slug_from_remote_url("https://github.com/forecast-bio/sigil"),
            Some("forecast-bio/sigil".to_string())
        );
    }

    #[test]
    fn test_slug_from_nested_gitlab_path_takes_last_two() {
        // GitLab-style nested groups: take the last two path segments.
        assert_eq!(
            slug_from_remote_url("https://gitlab.example.com/group/subgroup/project"),
            Some("subgroup/project".to_string())
        );
    }

    #[test]
    fn test_slug_from_garbage_returns_none() {
        assert_eq!(slug_from_remote_url("not a url"), None);
        assert_eq!(slug_from_remote_url(""), None);
    }

    // ── track / untrack / list ──

    #[test]
    fn test_track_rejects_nonexistent_path() {
        let (_home, db_path) = temp_db();
        let err =
            track_at_path(Path::new("/definitely/not/a/real/path"), None, &db_path).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn test_track_rejects_non_git_directory() {
        let (_home, db_path) = temp_db();
        let dir = tempdir().unwrap();
        let err = track_at_path(dir.path(), None, &db_path).unwrap_err();
        assert!(err.to_string().contains("not a git repository"));
    }

    #[test]
    fn test_track_inserts_row_with_derived_slug() {
        let (_home, db_path) = temp_db();
        let repo = tempdir().unwrap();
        make_fake_repo(
            repo.path(),
            "https://github.com/forecast-bio/test-a.git",
            true,
        );

        track_at_path(repo.path(), None, &db_path).unwrap();

        let db = DashboardDb::open(&db_path).unwrap();
        let slug: String = db
            .conn
            .query_row("SELECT slug FROM projects WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(slug, "forecast-bio/test-a");
    }

    #[test]
    fn test_track_with_slug_override_wins_over_origin() {
        let (_home, db_path) = temp_db();
        let repo = tempdir().unwrap();
        make_fake_repo(
            repo.path(),
            "https://github.com/forecast-bio/test-b.git",
            true,
        );

        track_at_path(repo.path(), Some("custom/override"), &db_path).unwrap();

        let db = DashboardDb::open(&db_path).unwrap();
        let slug: String = db
            .conn
            .query_row("SELECT slug FROM projects WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(slug, "custom/override");
    }

    #[test]
    fn test_track_rejects_duplicate_slug() {
        let (_home, db_path) = temp_db();
        let repo = tempdir().unwrap();
        make_fake_repo(
            repo.path(),
            "https://github.com/forecast-bio/test-c.git",
            true,
        );

        track_at_path(repo.path(), None, &db_path).unwrap();
        let err = track_at_path(repo.path(), None, &db_path).unwrap_err();
        assert!(err.to_string().contains("already tracked"));
    }

    #[test]
    fn test_track_repo_without_hub_branch_still_succeeds() {
        // Repos missing `crosslink/hub` track with a warning. The
        // `unreachable_project` alert picks them up on first poll.
        let (_home, db_path) = temp_db();
        let repo = tempdir().unwrap();
        make_fake_repo(
            repo.path(),
            "https://github.com/forecast-bio/test-d.git",
            false,
        );

        track_at_path(repo.path(), None, &db_path).unwrap();

        let db = DashboardDb::open(&db_path).unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_untrack_removes_row_and_leaves_working_copy() {
        let (_home, db_path) = temp_db();
        let repo = tempdir().unwrap();
        make_fake_repo(
            repo.path(),
            "https://github.com/forecast-bio/test-e.git",
            true,
        );

        track_at_path(repo.path(), None, &db_path).unwrap();
        untrack_with_path("forecast-bio/test-e", &db_path).unwrap();

        // Row gone
        let db = DashboardDb::open(&db_path).unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        // Working copy left intact
        assert!(repo.path().join(".git").exists());
    }

    #[test]
    fn test_untrack_rejects_unknown_slug() {
        let (_home, db_path) = temp_db();
        let err = untrack_with_path("owner/never-tracked", &db_path).unwrap_err();
        assert!(err.to_string().contains("not currently tracked"));
    }

    #[test]
    fn test_list_on_empty_db_prints_help() {
        let (_home, db_path) = temp_db();
        // Just verifies Ok on an empty DB.
        list_with_path(&db_path).unwrap();
    }
}
