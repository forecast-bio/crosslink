use anyhow::{Context, Result};

use super::core::SyncManager;
use super::{HUB_BRANCH, OLD_BRANCH, OLD_CACHE_DIR};

impl SyncManager {
    /// Auto-migrate from the old `crosslink/locks` branch to `crosslink/hub`.
    ///
    /// Detects whether the old branch or cache directory exists and performs a
    /// one-time rename. Called automatically by `init_cache()`.
    /// Returns `Ok(true)` if migration was performed, `Ok(false)` if not needed.
    pub(crate) fn migrate_from_locks_branch(&self) -> Result<bool> {
        let old_cache = self.crosslink_dir.join(OLD_CACHE_DIR);
        let has_old_local_cache = old_cache.exists();

        let has_old_remote = self
            .git_in_repo(&["ls-remote", "--heads", &self.remote, OLD_BRANCH])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

        if !has_old_local_cache && !has_old_remote {
            return Ok(false); // Nothing to migrate
        }

        tracing::info!("Migrating coordination branch: crosslink/locks -> crosslink/hub...");

        // 1. Remove old worktree if it exists (#471)
        if has_old_local_cache {
            // Try git worktree remove first
            if self
                .git_in_repo(&[
                    "worktree",
                    "remove",
                    "--force",
                    &old_cache.to_string_lossy(),
                ])
                .is_err()
            {
                // Worktree remove failed — delete the directory directly
                if old_cache.exists() {
                    std::fs::remove_dir_all(&old_cache).with_context(|| {
                        format!(
                            "Cannot remove old hub cache at {}. \
                             Migration cannot proceed with stale worktree.",
                            old_cache.display()
                        )
                    })?;
                }
                // Clean up dangling worktree metadata
                if let Err(e) = self.git_in_repo(&["worktree", "prune"]) {
                    tracing::warn!("worktree prune failed during migration: {}", e);
                }
            }
        }

        // 2. Rename local branch (if it exists and new doesn't)
        let has_old_local_branch = self
            .git_in_repo(&["rev-parse", "--verify", OLD_BRANCH])
            .is_ok();
        let has_new_local = self
            .git_in_repo(&["rev-parse", "--verify", HUB_BRANCH])
            .is_ok();

        if has_old_local_branch && !has_new_local {
            self.git_in_repo(&["branch", "-m", OLD_BRANCH, HUB_BRANCH])?;
        } else if !has_old_local_branch && has_old_remote && !has_new_local {
            // Fetch old remote and create new local branch from it
            self.git_in_repo(&["fetch", &self.remote, OLD_BRANCH])?;
            self.git_in_repo(&[
                "branch",
                HUB_BRANCH,
                &format!("{}/{}", self.remote, OLD_BRANCH),
            ])?;
        }

        // 3. Push new branch to remote (best-effort)
        let has_new_remote = self
            .git_in_repo(&["ls-remote", "--heads", &self.remote, HUB_BRANCH])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);
        if !has_new_remote {
            if let Err(e) = self.git_in_repo(&["push", "-u", &self.remote, HUB_BRANCH]) {
                tracing::warn!("migration push failed, changes saved locally only: {}", e);
            }
        }

        // 4. Delete old remote branch (best-effort)
        if has_old_remote {
            if let Err(e) = self.git_in_repo(&["push", &self.remote, "--delete", OLD_BRANCH]) {
                tracing::warn!("failed to delete old remote branch '{}': {}", OLD_BRANCH, e);
            }
        }

        // 5. Delete old local branch if still present
        if self
            .git_in_repo(&["rev-parse", "--verify", OLD_BRANCH])
            .is_ok()
        {
            if let Err(e) = self.git_in_repo(&["branch", "-D", OLD_BRANCH]) {
                tracing::info!("could not delete old branch '{OLD_BRANCH}': {e} — you can remove it manually with `git branch -D {OLD_BRANCH}`");
            }
        }

        tracing::info!("Migration complete: coordination branch is now crosslink/hub");
        Ok(true)
    }
}
