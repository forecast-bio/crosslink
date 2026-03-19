use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::process::Command;

use super::core::{
    has_conflict_markers, resolve_accept_both, KnowledgeManager, SyncOutcome, KNOWLEDGE_BRANCH,
};

impl KnowledgeManager {
    /// Initialize the knowledge cache directory.
    ///
    /// If the `crosslink/knowledge` branch exists on the remote, fetches it and
    /// creates a worktree. If not, creates an orphan branch with an initial
    /// `index.md` page.
    pub fn init_cache(&self) -> Result<()> {
        if self.cache_dir.exists() {
            return Ok(());
        }

        // Check if remote branch exists
        let has_remote = self
            .git_in_repo(&["ls-remote", "--heads", &self.remote, KNOWLEDGE_BRANCH])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

        if has_remote {
            // Fetch the remote branch
            self.git_in_repo(&["fetch", &self.remote, KNOWLEDGE_BRANCH])?;

            // Check if a local branch already exists
            let has_local = self
                .git_in_repo(&["rev-parse", "--verify", KNOWLEDGE_BRANCH])
                .is_ok();

            if has_local {
                self.git_in_repo(&["worktree", "add", &self.cache_path_str(), KNOWLEDGE_BRANCH])?;
            } else {
                // Create local branch tracking remote
                let remote_ref = format!("{}/{}", self.remote, KNOWLEDGE_BRANCH);
                self.git_in_repo(&[
                    "worktree",
                    "add",
                    "-b",
                    KNOWLEDGE_BRANCH,
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
                KNOWLEDGE_BRANCH,
                &self.cache_path_str(),
            ])?;

            // Initialize with index.md
            let now = Utc::now().format("%Y-%m-%d").to_string();
            let index_content = format!(
                "---\n\
                 title: Knowledge Index\n\
                 tags: [index]\n\
                 sources: []\n\
                 contributors: []\n\
                 created: {now}\n\
                 updated: {now}\n\
                 ---\n\
                 \n\
                 # Knowledge Index\n\
                 \n\
                 This is the shared knowledge repository for the project.\n"
            );

            std::fs::write(self.cache_dir.join("index.md"), index_content)?;

            // Commit the initial state so the branch has at least one commit.
            self.git_in_cache(&["add", "index.md"])?;
            self.git_in_cache(&["commit", "-m", "Initialize crosslink/knowledge branch"])?;
        }

        Ok(())
    }

    /// Fetch the latest state from remote and rebase local changes on top.
    ///
    /// If a rebase produces merge conflicts, falls back to an "accept both"
    /// strategy: aborts the rebase, merges instead, and resolves any remaining
    /// conflicts by keeping both versions. Returns the list of slugs that had
    /// conflicts resolved.
    pub fn sync(&self) -> Result<SyncOutcome> {
        let fetch_result = self.git_in_cache(&["fetch", &self.remote, KNOWLEDGE_BRANCH]);
        if let Err(e) = &fetch_result {
            let err_str = e.to_string();
            if err_str.contains("Could not resolve host")
                || err_str.contains("Could not read from remote")
                || err_str.contains("does not appear to be a git repository")
                || err_str.contains("No such remote")
                || err_str.contains("couldn't find remote ref")
            {
                return Ok(SyncOutcome::default());
            }
            fetch_result?;
        }

        // Check for unpushed local commits. If any exist, rebase to preserve them.
        let remote_ref = format!("{}/{}", self.remote, KNOWLEDGE_BRANCH);
        let log_result = self.git_in_cache(&["log", &format!("{}..HEAD", remote_ref), "--oneline"]);
        if let Ok(output) = &log_result {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                let rebase_result = self.git_in_cache(&["rebase", &remote_ref]);
                if let Err(e) = &rebase_result {
                    let err_str = e.to_string();
                    if err_str.contains("unknown revision")
                        || err_str.contains("ambiguous argument")
                    {
                        return Ok(SyncOutcome::default());
                    }
                    // Rebase failed — likely a conflict. Try accept-both fallback.
                    let outcome = self.handle_rebase_conflict(&remote_ref)?;
                    if !outcome.resolved_conflicts.is_empty() {
                        return Ok(outcome);
                    }
                    rebase_result?;
                }
                return Ok(SyncOutcome::default());
            }
        }

        // No unpushed commits — safe to reset to match remote
        let reset_result = self.git_in_cache(&["reset", "--hard", &remote_ref]);
        if let Err(e) = &reset_result {
            let err_str = e.to_string();
            if err_str.contains("unknown revision") || err_str.contains("ambiguous argument") {
                return Ok(SyncOutcome::default());
            }
            reset_result?;
        }

        Ok(SyncOutcome::default())
    }

    /// Push local commits to the remote.
    ///
    /// If the push is rejected (non-fast-forward), attempts a pull --rebase.
    /// If that rebase produces conflicts, falls back to "accept both" resolution.
    pub fn push(&self) -> Result<SyncOutcome> {
        let push_result = self.git_in_cache(&["push", &self.remote, KNOWLEDGE_BRANCH]);
        if let Err(e) = &push_result {
            let err_str = e.to_string();
            if err_str.contains("Could not resolve host")
                || err_str.contains("Could not read from remote")
            {
                return Ok(SyncOutcome::default());
            }
            if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                let remote_ref = format!("{}/{}", self.remote, KNOWLEDGE_BRANCH);
                // INTENTIONAL: fetch is best-effort — rebase below will use whatever state is available
                let _ = self.git_in_cache(&["fetch", &self.remote, KNOWLEDGE_BRANCH]);
                // Try rebase
                let rebase_result = self.git_in_cache(&["rebase", &remote_ref]);
                if rebase_result.is_err() {
                    // Rebase failed — try accept-both fallback
                    let outcome = self.handle_rebase_conflict(&remote_ref)?;
                    // INTENTIONAL: push after conflict resolution is best-effort — local state is consistent either way
                    let _ = self.git_in_cache(&["push", &self.remote, KNOWLEDGE_BRANCH]);
                    return Ok(outcome);
                }
                // INTENTIONAL: push after rebase is best-effort — local state is consistent either way
                let _ = self.git_in_cache(&["push", &self.remote, KNOWLEDGE_BRANCH]);
                return Ok(SyncOutcome::default());
            }
            push_result?;
        }
        Ok(SyncOutcome::default())
    }

    /// Abort a failed rebase and fall back to merge with "accept both" resolution.
    ///
    /// 1. Aborts the in-progress rebase
    /// 2. Merges the remote ref
    /// 3. If merge conflicts, resolves each .md file using accept-both
    /// 4. Stages and commits the resolution
    pub(super) fn handle_rebase_conflict(&self, remote_ref: &str) -> Result<SyncOutcome> {
        // INTENTIONAL: rebase --abort is best-effort — may have already been aborted or not started
        let _ = self.git_in_cache(&["rebase", "--abort"]);

        // Attempt a merge instead
        let merge_result = self.git_in_cache(&["merge", remote_ref, "--no-edit"]);

        let resolved = if merge_result.is_err() {
            // Merge has conflicts — resolve all .md files with accept-both
            self.resolve_conflicts_in_cache()?
        } else {
            Vec::new()
        };

        if !resolved.is_empty() {
            // Stage resolved files and commit
            self.git_in_cache(&["add", "-A"])?;
            let slugs_str = resolved.join(", ");
            self.commit(&format!(
                "knowledge: accept-both conflict resolution for {}",
                slugs_str
            ))?;
        }

        Ok(SyncOutcome {
            resolved_conflicts: resolved,
        })
    }

    /// Scan all `.md` files in the cache for conflict markers and resolve them.
    ///
    /// Returns the list of slugs that had conflicts resolved.
    pub(super) fn resolve_conflicts_in_cache(&self) -> Result<Vec<String>> {
        let mut resolved = Vec::new();

        if !self.cache_dir.exists() {
            return Ok(resolved);
        }

        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                let content = std::fs::read_to_string(&path)?;
                if has_conflict_markers(&content) {
                    let slug = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let resolved_content = resolve_accept_both(&content);
                    std::fs::write(&path, &resolved_content)?;
                    resolved.push(slug);
                }
            }
        }

        Ok(resolved)
    }

    /// Stage all changes in the knowledge worktree and commit.
    pub fn commit(&self, message: &str) -> Result<()> {
        self.git_in_cache(&["add", "-A"])?;

        let commit_result = self.git_in_cache(&["commit", "-m", message]);
        if let Err(e) = &commit_result {
            let err_str = e.to_string();
            if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                return Ok(());
            }
            commit_result?;
        }
        Ok(())
    }

    // --- Private git helpers ---

    pub(super) fn git_in_repo(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {:?}", args))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {:?} failed: {}", args, stderr);
        }
        Ok(output)
    }

    pub(super) fn git_in_cache(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {:?} in knowledge cache", args))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {:?} in knowledge cache failed: {}", args, stderr);
        }
        Ok(output)
    }
}
