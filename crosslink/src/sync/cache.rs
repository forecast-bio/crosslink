use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::core::SyncManager;
use super::HUB_BRANCH;
use crate::locks::LocksFile;

// ---------------------------------------------------------------------------
// Hub cache write lock — serializes ALL mutations to the hub cache worktree.
//
// Used by fetch(), upgrade_to_v2(), and write_commit_push() to prevent
// concurrent git operations from racing (#457, #459).
// ---------------------------------------------------------------------------

/// RAII guard for the hub cache write lock.
///
/// Holds the lock file handle open so the OS releases it on crash.
/// On normal drop, removes the lock file.
pub struct HubWriteLock {
    path: PathBuf,
    _file: std::fs::File,
}

impl Drop for HubWriteLock {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "failed to release hub write lock {}: {}",
                    self.path.display(),
                    e
                );
            }
        }
    }
}

/// Try to atomically create the lock file and write our PID.
/// Returns the guard on success, or the IO error on failure.
fn try_create_lock(lock_path: &Path) -> std::io::Result<HubWriteLock> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)?;
    writeln!(f, "{}", std::process::id())?;
    Ok(HubWriteLock {
        path: lock_path.to_path_buf(),
        _file: f,
    })
}

/// Acquire the hub cache write lock at the given path.
///
/// Blocks up to 30 seconds, checking for stale locks via PID liveness.
/// Returns an RAII guard that releases the lock on drop.
pub fn acquire_hub_lock(lock_path: &Path) -> Result<HubWriteLock> {
    let max_wait = Duration::from_secs(30);
    let poll_interval = Duration::from_millis(100);
    let start = std::time::Instant::now();

    loop {
        match try_create_lock(lock_path) {
            Ok(guard) => return Ok(guard),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Lock file exists — check if the holder is still alive.
                let holder_alive = std::fs::read_to_string(lock_path)
                    .ok()
                    .and_then(|content| content.trim().parse::<u32>().ok())
                    .is_some_and(|pid| {
                        std::process::Command::new("kill")
                            .args(["-0", &pid.to_string()])
                            .output()
                            .is_ok_and(|o| o.status.success())
                    });

                if !holder_alive {
                    // Stale lock — remove and immediately re-attempt in the same
                    // iteration to minimize the TOCTOU window (#347).
                    let _ = std::fs::remove_file(lock_path);
                    if let Ok(guard) = try_create_lock(lock_path) {
                        return Ok(guard);
                    }
                    // Another process won the race — fall through to retry loop
                }

                if start.elapsed() > max_wait {
                    // Force-remove after timeout
                    let _ = std::fs::remove_file(lock_path);
                    match try_create_lock(lock_path) {
                        Ok(guard) => return Ok(guard),
                        Err(_) => bail!(
                            "Hub lock held for >30s and could not be acquired after force-removal"
                        ),
                    }
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

impl SyncManager {
    /// Acquire the hub cache write lock.
    ///
    /// All code that mutates the hub cache worktree (fetch, upgrade,
    /// `write_commit_push`) must hold this lock to prevent races (#457, #459).
    pub(crate) fn acquire_lock(&self) -> Result<HubWriteLock> {
        let lock_path = self.cache_dir.join(".hub-write-lock");
        acquire_hub_lock(&lock_path)
    }
    /// Ensure the hub cache has a `.gitignore` that excludes runtime files.
    ///
    /// `.hub-write-lock` is a PID lock file created and deleted every sync
    /// cycle. If tracked, it causes a dirty-state recovery loop that diverges
    /// the cache from origin (#528). This method:
    ///
    /// 1. Creates or updates `.gitignore` with the exclusion entry.
    /// 2. Untracks the file via `git rm --cached` if it was previously tracked.
    ///
    /// Safe to call multiple times — idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error if writing `.gitignore` or git operations fail.
    pub fn ensure_hub_gitignore(&self) -> Result<()> {
        if !self.cache_dir.exists() {
            return Ok(());
        }
        let gitignore_path = self.cache_dir.join(".gitignore");
        let entry = ".hub-write-lock";

        let needs_write = std::fs::read_to_string(&gitignore_path).map_or(true, |content| {
            !content.lines().any(|line| line.trim() == entry)
        });

        if needs_write {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&gitignore_path)?;
            writeln!(f, "{entry}")?;
        }

        // Untrack the lock file if git is currently tracking it
        let _ = self.git_in_cache(&["rm", "--cached", "-f", entry]);

        Ok(())
    }

    /// Initialize the hub cache directory.
    ///
    /// If the `crosslink/hub` branch exists on the remote, fetches it and
    /// creates a worktree. If not, creates an orphan branch with an empty
    /// locks.json.
    ///
    /// # Errors
    ///
    /// Returns an error if git operations (fetch, worktree, commit) fail.
    pub fn init_cache(&self) -> Result<()> {
        // Auto-migrate from old crosslink/locks branch if needed
        self.migrate_from_locks_branch()?;

        if self.cache_dir.exists() {
            return Ok(());
        }

        // Check if remote branch exists
        let has_remote = self
            .git_in_repo(&["ls-remote", "--heads", &self.remote, HUB_BRANCH])
            .is_ok_and(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty());

        if has_remote {
            // Fetch the remote branch
            self.git_in_repo(&["fetch", &self.remote, HUB_BRANCH])?;

            // Check if a local branch already exists
            let has_local = self
                .git_in_repo(&["rev-parse", "--verify", HUB_BRANCH])
                .is_ok();

            if has_local {
                self.git_in_repo(&["worktree", "add", &self.cache_path_str(), HUB_BRANCH])?;
            } else {
                // Create local branch tracking remote
                let remote_ref = format!("{}/{}", self.remote, HUB_BRANCH);
                self.git_in_repo(&[
                    "worktree",
                    "add",
                    "-b",
                    HUB_BRANCH,
                    &self.cache_path_str(),
                    &remote_ref,
                ])?;
            }
        } else {
            // No remote branch — create orphan branch with worktree
            self.git_in_repo(&[
                "worktree",
                "add",
                "--orphan",
                "-b",
                HUB_BRANCH,
                &self.cache_path_str(),
            ])?;

            // Initialize with empty locks.json and directory structure
            let locks = LocksFile::empty();
            locks.save(&self.cache_dir.join("locks.json"))?;
            std::fs::create_dir_all(self.cache_dir.join("heartbeats"))?;
            std::fs::create_dir_all(self.cache_dir.join("trust"))?;
            std::fs::create_dir_all(self.cache_dir.join("issues"))?;
            std::fs::create_dir_all(self.cache_dir.join("meta").join("milestones"))?;
            std::fs::create_dir_all(self.cache_dir.join("locks"))?;

            // Write v2 layout version marker for new hubs
            let meta_dir = self.cache_dir.join("meta");
            crate::issue_file::write_layout_version(
                &meta_dir,
                crate::issue_file::CURRENT_LAYOUT_VERSION,
            )?;

            // Exclude runtime files from tracking before first commit (#528)
            self.ensure_hub_gitignore()?;

            // Write initial bootstrap state so it's included in the first commit.
            // This marks the hub as being in the bootstrap phase (#644).
            super::bootstrap::write_bootstrap_state(
                &self.cache_dir,
                &super::bootstrap::BootstrapState {
                    status: "pending".to_string(),
                    completed_at: None,
                },
            )?;

            // Commit the initial state so the branch has at least one commit.
            // Without this, `git log` and other commands fail on the empty orphan.
            self.git_in_cache(&["add", "-A"])?;
            // Ensure git identity before first commit — CI/containers may lack
            // a global gitconfig.
            self.ensure_cache_git_identity()?;
            self.git_commit_in_cache(&["-m", "Initialize crosslink/hub branch"])?;
        }

        // Also ensure identity for the has_remote path so callers that commit
        // in the cache (e.g. bootstrap step 7) don't fail in CI.
        self.ensure_cache_git_identity()?;

        // Self-heal: ensure .hub-write-lock is gitignored on existing caches
        // that were initialized before this fix (#528).
        self.ensure_hub_gitignore()?;

        // Propagate .claude/hooks into the cache worktree so that PreToolUse
        // hooks (which resolve via `git rev-parse --show-toplevel`) still work
        // when an agent's CWD lands inside the hub cache.
        self.propagate_claude_hooks()?;

        Ok(())
    }

    /// Upgrade the hub cache from v1 to v2 layout.
    ///
    /// - Writes the v2 layout version marker
    /// - Migrates inline comments to standalone v2 comment files
    /// - Commits the migration if any changes were made
    ///
    /// Call this explicitly (e.g. from `crosslink sync --upgrade`) rather than
    /// automatically during `init_cache`, to avoid side-effects on hubs that
    /// intentionally use v1 layout.
    ///
    /// # Errors
    ///
    /// Returns an error if acquiring the hub lock, writing files, or committing fails.
    pub fn upgrade_to_v2(&self) -> Result<usize> {
        // Acquire the hub write lock to prevent agents from writing V1 files
        // while we're migrating to V2 (#459).
        let _lock_guard = self.acquire_lock()?;

        let meta_dir = self.cache_dir.join("meta");
        let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
        if version >= 2 {
            return Ok(0);
        }

        let migrated =
            crate::hydration::migrate_inline_comments_to_v2(&self.cache_dir).unwrap_or(0);

        // Write version marker to disk (included in the commit below).
        // If the commit fails, we DON'T leave the marker — we delete it
        // so the next sync retries the full migration (#470).
        crate::issue_file::write_layout_version(
            &meta_dir,
            crate::issue_file::CURRENT_LAYOUT_VERSION,
        )?;

        self.git_in_cache(&["add", "-A"])?;
        let has_changes = self.git_in_cache(&["diff", "--cached", "--quiet"]).is_err();
        if has_changes {
            let commit_result = self.git_in_cache(&[
                "commit",
                "-m",
                &format!(
                    "sync: upgrade hub layout v1\u{2192}v2 ({migrated} comment files migrated)"
                ),
            ]);
            if let Err(e) = commit_result {
                // Commit failed — remove the version marker so next sync
                // retries the migration instead of thinking it's done (#470).
                let version_path = meta_dir.join("version.json");
                if version_path.exists() {
                    let _ = std::fs::remove_file(&version_path);
                }
                return Err(e);
            }
        }

        Ok(migrated)
    }

    /// Automatically find and remove stale V1 flat files that have V2
    /// equivalents. Runs during every sync so layout inconsistencies are
    /// corrected without user intervention (#478).
    ///
    /// Returns the number of stale files cleaned up.
    ///
    /// # Errors
    ///
    /// Returns an error if removing stale files or committing cleanup fails.
    pub fn cleanup_stale_layout_files(&self) -> Result<usize> {
        let issues_dir = self.cache_dir.join("issues");
        if !issues_dir.is_dir() {
            return Ok(0);
        }

        let meta_dir = self.cache_dir.join("meta");
        let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
        if version < 2 {
            return Ok(0); // V1 hub — V1 files are correct
        }

        let stale_v1 = Self::find_stale_v1_files(&issues_dir);

        if stale_v1.is_empty() {
            return Ok(0);
        }

        // Remove the stale files and commit
        for path in &stale_v1 {
            std::fs::remove_file(path)?;
        }

        self.git_in_cache(&["add", "-A"])?;
        let has_changes = self.git_in_cache(&["diff", "--cached", "--quiet"]).is_err();
        if has_changes {
            self.git_in_cache(&[
                "commit",
                "-m",
                &format!(
                    "sync: auto-cleanup {} stale V1 layout file(s)",
                    stale_v1.len()
                ),
            ])?;
        }

        Ok(stale_v1.len())
    }

    /// Find V1 flat files that should be cleaned up or migrated to V2.
    ///
    /// Returns paths of V1 files that are stale (have a V2 equivalent) or
    /// that were successfully migrated to V2 format during this call.
    fn find_stale_v1_files(issues_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut stale_v1: Vec<std::path::PathBuf> = Vec::new();
        let Ok(entries) = std::fs::read_dir(issues_dir) else {
            return stale_v1;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if !path.is_file() || !name.to_ascii_lowercase().ends_with(".json") {
                continue;
            }
            let uuid = name.trim_end_matches(".json");
            let v2_dir = issues_dir.join(uuid);
            if v2_dir.join("issue.json").exists() {
                // Both V1 and V2 exist — V1 is stale
                stale_v1.push(path);
            } else if !v2_dir.exists() {
                // V1 exists without V2 on a V2 hub — migrate it
                if let Some(migrated) = Self::migrate_v1_to_v2(&path, &v2_dir) {
                    stale_v1.push(migrated);
                }
            }
        }
        stale_v1
    }

    /// Migrate a single V1 flat issue file to V2 directory layout.
    ///
    /// Returns `Some(v1_path)` if the migration succeeded (so the V1 file
    /// can be removed), or `None` if it failed.
    fn migrate_v1_to_v2(
        v1_path: &std::path::Path,
        v2_dir: &std::path::Path,
    ) -> Option<std::path::PathBuf> {
        let content = std::fs::read(v1_path).ok()?;
        std::fs::create_dir_all(v2_dir).ok()?;
        std::fs::write(v2_dir.join("issue.json"), &content).ok()?;
        Some(v1_path.to_path_buf())
    }

    /// Detect and recover from broken git states in the hub cache worktree.
    ///
    /// Checks for three failure modes that can leave the cache unusable:
    /// 0. **Stale index.lock** — removed unconditionally before other
    ///    recovery steps, since `rebase --abort` and `checkout` both need
    ///    the index. Safe because callers hold the hub write lock, so no
    ///    legitimate git process is running.
    /// 1. **Mid-rebase state** — `.git/rebase-merge/` or `.git/rebase-apply/`
    ///    directories left behind by an interrupted rebase. Cleared with
    ///    `git rebase --abort`.
    /// 2. **Detached HEAD** — HEAD is not attached to the hub branch.
    ///    Re-attached with `git checkout crosslink/hub`.
    ///
    /// All recovery operations are best-effort: if any individual check or
    /// fix fails, we log a warning and continue rather than failing the
    /// caller's operation.
    pub fn hub_health_check(&self) {
        if !self.cache_dir.exists() {
            return;
        }

        // Resolve the actual git directory for the cache worktree.
        // In a linked worktree, `.git` is a file pointing elsewhere.
        let git_dir = match self.git_in_cache(&["rev-parse", "--git-dir"]) {
            Ok(output) => {
                let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let path = std::path::PathBuf::from(&raw);
                // git rev-parse may return a relative path; resolve against cache_dir
                if path.is_absolute() {
                    path
                } else {
                    self.cache_dir.join(path)
                }
            }
            Err(_) => {
                // Cannot determine git dir — skip health checks
                return;
            }
        };

        // Fix 0: Remove index.lock FIRST — our own recovery operations
        // (rebase --abort, checkout) need the index, and a stale lock from
        // a previous crash will block them. We hold the hub write lock so
        // we know no legitimate git process is running.
        let index_lock = git_dir.join("index.lock");
        if index_lock.exists() {
            tracing::warn!("removing index.lock from hub cache before recovery");
            if let Err(e) = std::fs::remove_file(&index_lock) {
                tracing::warn!("failed to remove index.lock: {}", e);
            }
        }

        // Fix 1: Mid-rebase state (#454) — abort and verify
        let rebase_merge = git_dir.join("rebase-merge");
        let rebase_apply = git_dir.join("rebase-apply");
        if rebase_merge.exists() || rebase_apply.exists() {
            tracing::warn!("hub cache is stuck in mid-rebase state, aborting to recover");
            let _ = self.git_in_cache(&["rebase", "--abort"]);
            // Verify — if rebase state persists, force-clean it
            if rebase_merge.exists() {
                tracing::warn!("rebase --abort didn't clear rebase-merge, removing manually");
                let _ = std::fs::remove_dir_all(&rebase_merge);
            }
            if rebase_apply.exists() {
                tracing::warn!("rebase --abort didn't clear rebase-apply, removing manually");
                let _ = std::fs::remove_dir_all(&rebase_apply);
            }
            // Rebase abort may have left a new index.lock
            if index_lock.exists() {
                let _ = std::fs::remove_file(&index_lock);
            }
        }

        // Fix 2: Detached HEAD (#455) — re-attach with escalation
        if self.git_in_cache(&["symbolic-ref", "HEAD"]).is_err() {
            tracing::warn!("hub cache HEAD is detached, re-attaching to {}", HUB_BRANCH);
            // Try checkout first
            if self.git_in_cache(&["checkout", HUB_BRANCH]).is_err() {
                // Checkout failed — force-create the branch at current HEAD
                // then checkout. This handles the case where the branch ref
                // is missing or points to a different commit.
                tracing::warn!("checkout failed, force-creating branch at current HEAD");
                let _ = self.git_in_cache(&["branch", "-f", HUB_BRANCH, "HEAD"]);
                let _ = self.git_in_cache(&["checkout", HUB_BRANCH]);
            }
            // If STILL detached, try writing the ref directly
            if self.git_in_cache(&["symbolic-ref", "HEAD"]).is_err() {
                tracing::warn!("checkout still failed, writing HEAD ref directly");
                let _ = self.git_in_cache(&[
                    "symbolic-ref",
                    "HEAD",
                    &format!("refs/heads/{HUB_BRANCH}"),
                ]);
            }
        }
    }

    /// Detect and resolve dirty hub cache state.
    ///
    /// If the cache has modified/untracked files (e.g. from a failed push retry
    /// that left files staged but uncommitted), stage everything and commit it
    /// so that subsequent rebase/pull operations can proceed.
    ///
    /// Returns `true` if dirty state was found and cleaned.
    ///
    /// # Errors
    ///
    /// Returns an error if staging or committing dirty state fails.
    pub fn clean_dirty_state(&self) -> Result<bool> {
        let status = self.git_in_cache(&["status", "--porcelain"]);
        match status {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim().is_empty() {
                    return Ok(false);
                }
                // Stage and commit dirty state (#465). If staging fails,
                // escalate to git reset --hard HEAD to force-align the
                // working directory with the last commit.
                if self.git_in_cache(&["add", "-A"]).is_err() {
                    tracing::warn!(
                        "git add -A failed in dirty state cleanup — escalating to \
                         `git reset --hard HEAD`. This discards uncommitted changes \
                         in the hub cache worktree (not the user's working tree). \
                         Dirty files were: {}",
                        stdout.trim()
                    );
                    self.git_in_cache(&["reset", "--hard", "HEAD"])?;
                    return Ok(true);
                }
                let commit_result = self
                    .git_commit_in_cache(&["-m", "sync: auto-stage dirty hub state (recovery)"]);
                match commit_result {
                    Ok(_) => Ok(true),
                    Err(e) => {
                        let err_str = e.to_string();
                        if err_str.contains("nothing to commit")
                            || err_str.contains("no changes added")
                        {
                            Ok(false) // git add staged nothing — working dir is clean
                        } else {
                            Err(e)
                        }
                    }
                }
            }
            Err(_) => Ok(false), // Can't check status — don't block
        }
    }

    /// Fetch the latest state from remote and integrate changes.
    ///
    /// When local-only (unpushed) commits exist, rebases them on top of the
    /// remote to preserve close events and other mutations that haven't been
    /// pushed yet. Only resets to the remote when there are definitively no
    /// unpushed commits.
    ///
    /// # Errors
    ///
    /// Returns an error if acquiring the lock, fetching, or rebasing fails.
    pub fn fetch(&self) -> Result<()> {
        // Acquire the hub write lock to serialize with write_commit_push (#457).
        // fetch() modifies the working directory (reset, rebase) which races
        // with concurrent CLI writes if not serialized.
        let _lock_guard = self.acquire_lock()?;

        // Recover from broken git states before attempting fetch (#454, #455, #456)
        self.hub_health_check();

        // Self-heal: ensure .hub-write-lock is gitignored (#528).
        // Must run before clean_dirty_state so lock file changes don't
        // trigger spurious recovery commits.
        let _ = self.ensure_hub_gitignore();

        // Stage any untracked or modified files before fetch. Concurrent
        // agents may have written heartbeat/lock files that aren't committed
        // yet — these block rebase/reset with "untracked working tree files
        // would be overwritten by merge" (#480).
        self.clean_dirty_state()?;

        // Try fetching from remote. If no remote is configured, this is a no-op.
        let fetch_result = self.git_in_cache(&["fetch", &self.remote, HUB_BRANCH]);
        if let Err(e) = &fetch_result {
            let err_str = e.to_string();
            // If there's no remote or no network, don't fail — just use local state
            if err_str.contains("Could not resolve host")
                || err_str.contains("Could not read from remote")
                || err_str.contains("does not appear to be a git repository")
                || err_str.contains("No such remote")
                || err_str.contains("couldn't find remote ref")
            {
                return Ok(());
            }
            // For other errors, propagate
            fetch_result?;
        }

        // Check for unpushed local commits (e.g. offline-created issues).
        // If any exist, rebase instead of reset --hard to preserve them.
        let remote_ref = format!("{}/{}", self.remote, HUB_BRANCH);
        let log_result = self.git_in_cache(&["log", &format!("{remote_ref}..HEAD"), "--oneline"]);

        if let Ok(output) = &log_result {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                // Unpushed commits exist — rebase to preserve them
                self.rebase_preserving_local(&remote_ref)?;
                return Ok(());
            }
            // Output is empty — no unpushed commits, safe to reset
        } else {
            // git log failed (e.g. remote ref not yet available). We
            // cannot determine whether unpushed commits exist, so keep
            // local state to avoid discarding close events or other
            // local-only mutations. (#430)
            tracing::warn!(
                "cannot determine unpushed commit count (git log failed); \
                 keeping local state to avoid data loss"
            );
            return Ok(());
        }

        // No unpushed commits — safe to reset to match remote
        let reset_result = self.git_in_cache(&["reset", "--hard", &remote_ref]);
        if let Err(e) = &reset_result {
            let err_str = e.to_string();
            // If the remote branch doesn't exist yet, that's fine
            if err_str.contains("unknown revision") || err_str.contains("ambiguous argument") {
                return Ok(());
            }
            reset_result?;
        }

        Ok(())
    }

    /// Rebase local unpushed commits on top of the remote ref, preserving
    /// local-only mutations (close events, comments, etc.).
    ///
    /// If rebase fails due to conflict, aborts the rebase and keeps local
    /// state rather than losing data.
    fn rebase_preserving_local(&self, remote_ref: &str) -> Result<()> {
        // Bail if local has diverged too far — sign of a rebase loop
        self.check_divergence()?;

        // Clean dirty state before rebase — prevents "cannot pull
        // with rebase: You have unstaged changes" error loop
        self.clean_dirty_state()?;

        let rebase_result = self.git_in_cache(&["rebase", remote_ref]);
        if let Err(e) = &rebase_result {
            let err_str = e.to_string();
            if err_str.contains("unknown revision") || err_str.contains("ambiguous argument") {
                return Ok(());
            }
            // Rebase failed (likely a conflict). Abort to restore pre-rebase
            // state so local-only commits are preserved rather than lost.
            // The user can resolve manually or the next push will retry. (#430)
            // INTENTIONAL: rebase --abort is best-effort recovery — preserves local commits even if abort fails
            if let Err(abort_err) = self.git_in_cache(&["rebase", "--abort"]) {
                tracing::warn!("rebase --abort failed during recovery: {}", abort_err);
            }
            tracing::warn!(
                "rebase onto {} failed ({}); aborted to preserve local commits",
                remote_ref,
                err_str.lines().next().unwrap_or("unknown error")
            );
            return Ok(());
        }

        Ok(())
    }

    /// Stage locks.json, commit, and push with rebase-retry.
    pub(super) fn commit_and_push_locks(&self, message: &str) -> Result<()> {
        self.git_in_cache(&["add", "locks.json"])?;

        let commit_result = self.git_commit_in_cache(&["-m", message]);
        if let Err(e) = &commit_result {
            let err_str = e.to_string();
            if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                return Ok(());
            }
            commit_result?;
        }

        // Push with retry
        for attempt in 0..3 {
            let push_result = self.git_in_cache(&["push", &self.remote, HUB_BRANCH]);
            match push_result {
                Ok(_) => return Ok(()),
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        return Ok(()); // Offline — commit is local
                    }
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < 2 {
                            self.check_divergence()?;
                            // Pull to sync with remote before retry (#473).
                            // If pull fails, run health check and try once more.
                            if self
                                .git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH])
                                .is_err()
                            {
                                self.hub_health_check();
                                self.git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH])?;
                            }
                            continue;
                        }
                        bail!("Push failed after 3 retries for locks.json");
                    }
                    return Err(e);
                }
            }
        }
        // All 3 iterations returned early or continued — if we get here,
        // the loop completed without a definitive outcome, which shouldn't
        // happen. Treat as an error rather than silently returning Ok.
        bail!("Push loop completed without returning — this is a bug")
    }
}
