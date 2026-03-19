use anyhow::{bail, Result};

use super::core::SyncManager;
use super::HUB_BRANCH;
use crate::locks::LocksFile;

impl SyncManager {
    /// Initialize the hub cache directory.
    ///
    /// If the `crosslink/hub` branch exists on the remote, fetches it and
    /// creates a worktree. If not, creates an orphan branch with an empty
    /// locks.json.
    pub fn init_cache(&self) -> Result<()> {
        // Auto-migrate from old crosslink/locks branch if needed
        self.migrate_from_locks_branch()?;

        if self.cache_dir.exists() {
            return Ok(());
        }

        // Check if remote branch exists
        let has_remote = self
            .git_in_repo(&["ls-remote", "--heads", &self.remote, HUB_BRANCH])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

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

            // Commit the initial state so the branch has at least one commit.
            // Without this, `git log` and other commands fail on the empty orphan.
            self.git_in_cache(&["add", "locks.json"])?;
            // Ensure git identity before first commit — CI/containers may lack
            // a global gitconfig.
            self.ensure_cache_git_identity()?;
            self.git_in_cache(&["commit", "-m", "Initialize crosslink/hub branch"])?;
        }

        // Also ensure identity for the has_remote path so callers that commit
        // in the cache (e.g. bootstrap step 7) don't fail in CI.
        self.ensure_cache_git_identity()?;

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
    /// automatically during init_cache, to avoid side-effects on hubs that
    /// intentionally use v1 layout.
    pub fn upgrade_to_v2(&self) -> Result<usize> {
        let meta_dir = self.cache_dir.join("meta");
        let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
        if version >= 2 {
            return Ok(0);
        }

        let migrated =
            crate::hydration::migrate_inline_comments_to_v2(&self.cache_dir).unwrap_or(0);

        crate::issue_file::write_layout_version(
            &meta_dir,
            crate::issue_file::CURRENT_LAYOUT_VERSION,
        )?;

        // INTENTIONAL: staging is best-effort — we check for actual changes before committing
        let _ = self.git_in_cache(&["add", "-A"]);
        let has_changes = self.git_in_cache(&["diff", "--cached", "--quiet"]).is_err();
        if has_changes {
            self.git_in_cache(&[
                "commit",
                "-m",
                &format!(
                    "sync: upgrade hub layout v1\u{2192}v2 ({} comment files migrated)",
                    migrated
                ),
            ])?;
        }

        Ok(migrated)
    }

    /// Detect and resolve dirty hub cache state.
    ///
    /// If the cache has modified/untracked files (e.g. from a failed push retry
    /// that left files staged but uncommitted), stage everything and commit it
    /// so that subsequent rebase/pull operations can proceed.
    ///
    /// Returns `true` if dirty state was found and cleaned.
    pub fn clean_dirty_state(&self) -> Result<bool> {
        let status = self.git_in_cache(&["status", "--porcelain"]);
        match status {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim().is_empty() {
                    return Ok(false);
                }
                // INTENTIONAL: dirty state recovery is best-effort — if staging/commit fails, we still return Ok
                let _ = self.git_in_cache(&["add", "-A"]);
                let _ = self.git_in_cache(&[
                    "commit",
                    "-m",
                    "sync: auto-stage dirty hub state (recovery)",
                ]);
                Ok(true)
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
    pub fn fetch(&self) -> Result<()> {
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
        let log_result = self.git_in_cache(&["log", &format!("{}..HEAD", remote_ref), "--oneline"]);

        match &log_result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.trim().is_empty() {
                    // Unpushed commits exist — rebase to preserve them
                    self.rebase_preserving_local(&remote_ref)?;
                    return Ok(());
                }
                // Output is empty — no unpushed commits, safe to reset
            }
            Err(_) => {
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
            let _ = self.git_in_cache(&["rebase", "--abort"]);
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

        let commit_result = self.git_in_cache(&["commit", "-m", message]);
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
                            // Bail if local has diverged too far — sign of a rebase loop
                            self.check_divergence()?;
                            // INTENTIONAL: pull/rebase failure is non-fatal — retry loop will bail on persistent conflicts
                            let _ =
                                self.git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH]);
                            continue;
                        }
                        bail!("Push failed after 3 retries for locks.json");
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}
