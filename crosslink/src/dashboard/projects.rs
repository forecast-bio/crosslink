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
///
/// Agent-readiness check: modern crosslink (post-agent.json era)
/// needs `.crosslink/agent.json` for `locks steal`, `locks release`,
/// and similar agent-scoped operations. Older workspaces initialised
/// before the agent.json migration have `driver-key.pub` but no
/// `agent.json` — dashboard writes that only need signing work fine,
/// but anything agent-scoped fails with "No agent configured". We
/// require BOTH files for `Ready` so the dashboard's Init banner
/// surfaces the gap and the retrofit endpoint can fix it.
#[must_use]
pub fn write_capability(clone_path: &Path) -> WriteCapability {
    let cl = clone_path.join(".crosslink");
    if !cl.join("issues.db").is_file() {
        return WriteCapability::NotInitialized;
    }
    let has_driver_key = cl.join("driver-key.pub").is_file();
    let has_agent_json = cl.join("agent.json").is_file();
    if !has_driver_key || !has_agent_json {
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

/// Best-effort detection of the repository's default branch name.
///
/// Cascade:
/// 1. `git symbolic-ref refs/remotes/origin/HEAD` — what the remote
///    advertised at clone time. Produces `refs/remotes/origin/main`
///    or similar. Most reliable when the remote was cloned normally.
/// 2. `git remote show origin | grep "HEAD branch:"` — re-queries
///    the remote. Works when HEAD isn't locally recorded but is
///    available over the network.
/// 3. Try `main`, then `master` as rev-parse candidates — whichever
///    the repo actually has.
///
/// Returns `None` if none of the above yield a branch name. The
/// caller falls back to `"main"` since that's the modern default
/// and usually right even when detection fails.
fn detect_default_branch(clone_path: &Path) -> Option<String> {
    // 1. symbolic-ref against the locally-recorded remote HEAD.
    let out = Command::new("git")
        .arg("-C")
        .arg(clone_path)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        // Output is e.g. "origin/main" — strip the "origin/" prefix.
        if let Some(branch) = s.strip_prefix("origin/") {
            if !branch.is_empty() {
                return Some(branch.to_string());
            }
        }
    }

    // 2. `git remote show origin` — network-scoped but authoritative.
    // Parses `HEAD branch: main` out of the human-readable output.
    let out = Command::new("git")
        .arg("-C")
        .arg(clone_path)
        .args(["remote", "show", "origin"])
        .output()
        .ok()?;
    if out.status.success() {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let t = line.trim();
            if let Some(branch) = t.strip_prefix("HEAD branch:") {
                let branch = branch.trim();
                if !branch.is_empty() && branch != "(unknown)" {
                    return Some(branch.to_string());
                }
            }
        }
    }

    // 3. Literal guesses — whichever branch exists locally.
    for candidate in ["main", "master"] {
        let ok = Command::new("git")
            .arg("-C")
            .arg(clone_path)
            .args(["rev-parse", "--verify", &format!("refs/heads/{candidate}")])
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            return Some(candidate.to_string());
        }
    }

    None
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

/// Shell out to the `crosslink` binary to initialise a workspace.
/// Used by both the CLI (`crosslink dashboard track --init`) and
/// the retrofit REST endpoint.
///
/// Recovery path for partial state: `crosslink init --defaults
/// --force` has a bug where, if `.crosslink/agent.json` exists but
/// `.crosslink/issues.db` doesn't, it skips the "Initializing
/// database" step entirely while reporting exit 0 — leaving the
/// workspace perma-partial. We detect that case (issues.db still
/// missing after a successful init) and retry once with the
/// `.crosslink/` directory (minus `.hub-cache/`) wiped clean.
///
/// # Errors
/// Returns an error describing which subprocess step failed and its
/// stderr output, or "init reported success but issues.db is still
/// missing" after the retry.
pub fn run_init_and_agent_in(workspace: &Path, agent_id: &str) -> Result<()> {
    let cmd_name = resolve_crosslink_bin();

    // First attempt.
    run_init_and_agent_inner(&cmd_name, workspace, agent_id)?;
    let issues_db = workspace.join(".crosslink").join("issues.db");
    if issues_db.is_file() {
        return Ok(());
    }

    // Partial-state recovery. `crosslink init --force` apparently
    // short-circuits the DB bootstrap when SOME artifacts already
    // exist, so we strip the artifacts it checks (everything except
    // the hub-cache worktree — deleting a git worktree outside of
    // `git worktree remove` leaves dangling admin refs).
    tracing::warn!(
        "crosslink init in {} produced no issues.db; cleaning partial \
         state and retrying",
        workspace.display()
    );
    wipe_partial_crosslink_state(workspace)?;
    run_init_and_agent_inner(&cmd_name, workspace, agent_id)?;
    if !issues_db.is_file() {
        bail!(
            "crosslink init in {} reported success but `.crosslink/issues.db` \
             is still missing after a clean retry — the workspace may be on a \
             filesystem that doesn't support SQLite, or the CLI build is \
             broken. Investigate `crosslink init --defaults` manually there.",
            workspace.display()
        );
    }
    Ok(())
}

/// Wipe `.crosslink/` entirely so `crosslink init` can rebuild from
/// scratch. `crosslink init --defaults --force` has a documented
/// short-circuit when `.crosslink/.hub-cache/` already exists — it
/// skips the `Initializing database` step and leaves `issues.db`
/// missing even though it reports exit 0. Wiping the whole dir
/// (including the hub-cache worktree, properly removed via
/// `git worktree remove --force`) is the only reliable way to
/// convince it to re-create the database.
///
/// The hub-cache gets re-materialized on the next poll tick by
/// [`super::poll::ensure_hub_cache_worktree`], so nothing is lost.
///
/// Safe to call when `.crosslink/` doesn't exist — no-op in that
/// case.
fn wipe_partial_crosslink_state(workspace: &Path) -> Result<()> {
    let dot_crosslink = workspace.join(".crosslink");
    if !dot_crosslink.is_dir() {
        return Ok(());
    }

    // If a hub-cache worktree exists, remove it via `git worktree
    // remove --force` first so git's worktree admin state stays
    // consistent. `remove --force` kills the worktree even if it has
    // uncommitted changes (which is what we want — we're recovering
    // from corruption).
    let hub_cache = dot_crosslink.join(".hub-cache");
    if hub_cache.is_dir() {
        let _ = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args([
                "worktree",
                "remove",
                "--force",
                hub_cache.to_string_lossy().as_ref(),
            ])
            .output();
        // `git worktree remove` can leave stale admin dirs in some
        // edge cases (e.g. if the branch was force-updated elsewhere).
        // `prune` sweeps those up. Best-effort.
        let _ = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(["worktree", "prune"])
            .output();
    }

    // Now the whole `.crosslink/` can be nuked — no more git admin
    // refs inside. If the hub-cache removal above left the dir
    // behind for any reason, `remove_dir_all` handles it.
    std::fs::remove_dir_all(&dot_crosslink)
        .with_context(|| format!("remove_dir_all {}", dot_crosslink.display()))?;
    Ok(())
}

/// Single init+agent-init pass. Factored out of
/// [`run_init_and_agent_in`] so the caller can retry on a cleaner
/// slate when the first attempt produced inconsistent state.
fn run_init_and_agent_inner(
    cmd_name: &std::ffi::OsStr,
    workspace: &Path,
    agent_id: &str,
) -> Result<()> {
    // `--force` lets init re-run cleanly when a previous retrofit left
    // partial state (a stray `.crosslink/agent.json` without
    // `issues.db` / `hook-config.json`). Idempotent on already-clean
    // workspaces — init --force just refreshes hooks.
    let init_out = Command::new(cmd_name)
        .current_dir(workspace)
        .args(["init", "--defaults", "-q", "--force"])
        .output()
        .with_context(|| {
            format!(
                "spawn `crosslink init` (binary: {}, workspace: {})",
                std::path::Path::new(cmd_name).display(),
                workspace.display()
            )
        })?;
    if !init_out.status.success() {
        let stderr = String::from_utf8_lossy(&init_out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&init_out.stdout).trim().to_string();
        bail!(
            "crosslink init failed (exit {}): {}{}{}",
            init_out
                .status
                .code()
                .map_or_else(|| "signal".into(), |c| c.to_string()),
            stderr,
            if !stderr.is_empty() && !stdout.is_empty() {
                "; stdout: "
            } else {
                ""
            },
            stdout,
        );
    }

    // --force is load-bearing: `crosslink init --defaults` writes a
    // placeholder `.crosslink/agent.json`, so an unforced
    // `crosslink agent init` fails with "Agent already configured".
    // We only enter this helper when `write_capability != Ready`
    // (i.e. driver-key.pub is missing), so overwriting the placeholder
    // is exactly what we want.
    // Dashboard retrofit always initialises a DRIVER identity (the
    // human's main workspace) — so hub commits sign with the
    // GitHub-registered `user.signingkey` rather than an agent-
    // scoped key GitHub doesn't know about (#718). Subagent
    // worktrees go through kickoff/swarm and get `--role agent`
    // from those flows, not this path.
    let agent_out = Command::new(cmd_name)
        .current_dir(workspace)
        .args([
            "agent",
            "init",
            agent_id,
            "-q",
            "--force",
            "--role",
            "driver",
            "--description",
            "dashboard auto-bootstrap",
        ])
        .output()
        .with_context(|| {
            format!(
                "spawn `crosslink agent init` (binary: {}, workspace: {})",
                std::path::Path::new(cmd_name).display(),
                workspace.display()
            )
        })?;
    if !agent_out.status.success() {
        let stderr = String::from_utf8_lossy(&agent_out.stderr)
            .trim()
            .to_string();
        let stdout = String::from_utf8_lossy(&agent_out.stdout)
            .trim()
            .to_string();
        bail!(
            "crosslink agent init failed (exit {}): {}{}{}",
            agent_out
                .status
                .code()
                .map_or_else(|| "signal".into(), |c| c.to_string()),
            stderr,
            if !stderr.is_empty() && !stdout.is_empty() {
                "; stdout: "
            } else {
                ""
            },
            stdout,
        );
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
    let default_branch = detect_default_branch(&canonical).unwrap_or_else(|| "main".into());
    let now = chrono::Utc::now().to_rfc3339();
    db.conn.execute(
        "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
         VALUES (?1, ?2, ?3, 'active', ?4)",
        params![
            slug,
            canonical.to_string_lossy().as_ref(),
            default_branch,
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

    // The `actions` table (audit log) references `projects(id)` with
    // no `ON DELETE` action, so a raw DELETE on `projects` FK-fails
    // whenever we've ever run a write through the dashboard for this
    // slug. Null out the reference first so the audit history
    // survives the untrack as orphan rows (project_id=NULL), then
    // drop the project row. Other dependent tables (alerts,
    // project_state, activity) have CASCADE; pty_sessions has SET
    // NULL — both fine already.
    let tx = db.conn.unchecked_transaction()?;
    tx.execute(
        "UPDATE actions SET project_id = NULL
         WHERE project_id = (SELECT id FROM projects WHERE slug = ?1)",
        [slug],
    )?;
    let rows = tx.execute("DELETE FROM projects WHERE slug = ?1", [slug])?;
    if rows == 0 {
        bail!("{slug} is not currently tracked");
    }
    tx.commit()?;
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

/// Decide which `crosslink` binary to spawn for dashboard-initiated
/// actions. Order of preference:
///
/// 1. `$CROSSLINK_BIN` env var — explicit override for operators who
///    want to pin a specific build.
/// 2. `crosslink` on `$PATH` — canonical for installed setups
///    (`cargo install crosslink`, package manager, ops runbook).
///    Reinstalling the CLI automatically updates dashboard behaviour.
/// 3. `std::env::current_exe()` — dev fallback for running the
///    dashboard straight from `target/release/crosslink` before
///    installing. Skipped when the path looks like a cargo-test
///    binary (`/deps/`).
/// 4. Bare `"crosslink"` — final fallback; if PATH is missing the
///    name, spawn fails with a clear error the user can debug.
///
/// Using PATH as the primary mechanism was the whole point of
/// `cargo install`: the user's installed binary *is* the one
/// subprocesses should invoke. Self-exe was an anti-feature that
/// coupled dashboard behaviour to an ephemeral dev path (#713).
pub fn resolve_crosslink_bin() -> std::ffi::OsString {
    if let Some(override_path) = std::env::var_os("CROSSLINK_BIN") {
        if !override_path.is_empty() {
            return override_path;
        }
    }
    if which_on_path("crosslink").is_some() {
        return "crosslink".into();
    }
    if let Ok(exe) = std::env::current_exe() {
        let looks_like_test = exe
            .components()
            .any(|c| c.as_os_str() == std::ffi::OsStr::new("deps"));
        if !looks_like_test && exe.is_file() {
            return exe.into_os_string();
        }
    }
    "crosslink".into()
}

/// Trivial `which`: walks `$PATH` looking for `name`. Returns the
/// first match. `None` if not found or `PATH` unset.
fn which_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(name);
        if candidate.is_file() {
            Some(candidate)
        } else {
            None
        }
    })
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
    fn test_write_capability_agent_missing_when_agent_json_absent() {
        // Older-layout workspace with driver-key.pub but no agent.json
        // — agent-scoped operations like `locks steal` fail with
        // "No agent configured". Must surface as AgentMissing so the
        // dashboard's InitBanner offers the retrofit.
        let dir = tempdir().unwrap();
        let cl = dir.path().join(".crosslink");
        std::fs::create_dir_all(&cl).unwrap();
        std::fs::write(cl.join("issues.db"), []).unwrap();
        std::fs::write(cl.join("driver-key.pub"), b"ssh-ed25519 AAAA...").unwrap();
        assert_eq!(write_capability(dir.path()), WriteCapability::AgentMissing);
    }

    #[test]
    fn test_write_capability_ready_when_all_present() {
        let dir = tempdir().unwrap();
        let cl = dir.path().join(".crosslink");
        std::fs::create_dir_all(&cl).unwrap();
        std::fs::write(cl.join("issues.db"), []).unwrap();
        std::fs::write(cl.join("driver-key.pub"), b"ssh-ed25519 AAAA...").unwrap();
        std::fs::write(cl.join("agent.json"), b"{\"agent_id\":\"x\"}").unwrap();
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
