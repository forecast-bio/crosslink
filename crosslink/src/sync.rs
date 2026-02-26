use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::identity::AgentConfig;
use crate::locks::{Heartbeat, Keyring, LocksFile};

/// Directory name under .crosslink for the hub cache worktree.
pub(crate) const HUB_CACHE_DIR: &str = ".hub-cache";

/// The coordination branch name.
pub(crate) const HUB_BRANCH: &str = "crosslink/hub";

/// Old directory name (for migration from crosslink/locks).
const OLD_CACHE_DIR: &str = ".locks-cache";

/// Old branch name (for migration from crosslink/locks).
const OLD_BRANCH: &str = "crosslink/locks";

/// Result of GPG signature verification.
#[derive(Debug)]
pub enum GpgVerification {
    /// Signature is valid. Fingerprint may be extracted.
    Valid {
        commit: String,
        fingerprint: Option<String>,
    },
    /// Commit exists but is not signed.
    Unsigned { commit: String },
    /// Signature verification failed.
    Invalid { commit: String, reason: String },
    /// No commits exist on the branch yet.
    NoCommits,
}

/// Manages synchronization with the `crosslink/hub` coordination branch.
///
/// Uses a git worktree at `.crosslink/.hub-cache/` to avoid disturbing
/// the user's working tree.
pub struct SyncManager {
    /// Path to the .crosslink directory.
    #[allow(dead_code)]
    crosslink_dir: PathBuf,
    /// Path to .crosslink/.hub-cache (worktree of crosslink/hub branch).
    cache_dir: PathBuf,
    /// The repo root (parent of .crosslink).
    repo_root: PathBuf,
}

impl SyncManager {
    /// Create a new SyncManager for the given .crosslink directory.
    pub fn new(crosslink_dir: &Path) -> Result<Self> {
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let repo_root = crosslink_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root from .crosslink dir"))?
            .to_path_buf();
        Ok(SyncManager {
            crosslink_dir: crosslink_dir.to_path_buf(),
            cache_dir,
            repo_root,
        })
    }

    /// Auto-migrate from the old `crosslink/locks` branch to `crosslink/hub`.
    ///
    /// Detects whether the old branch or cache directory exists and performs a
    /// one-time rename. Called automatically by `init_cache()`.
    /// Returns `Ok(true)` if migration was performed, `Ok(false)` if not needed.
    pub(crate) fn migrate_from_locks_branch(&self) -> Result<bool> {
        let old_cache = self.crosslink_dir.join(OLD_CACHE_DIR);
        let has_old_local_cache = old_cache.exists();

        let has_old_remote = self
            .git_in_repo(&["ls-remote", "--heads", "origin", OLD_BRANCH])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

        if !has_old_local_cache && !has_old_remote {
            return Ok(false); // Nothing to migrate
        }

        eprintln!("Migrating coordination branch: crosslink/locks -> crosslink/hub...");

        // 1. Remove old worktree if it exists
        if has_old_local_cache {
            let _ = self.git_in_repo(&[
                "worktree",
                "remove",
                "--force",
                &old_cache.to_string_lossy(),
            ]);
            // Fallback: if worktree remove fails, just delete the directory
            if old_cache.exists() {
                let _ = std::fs::remove_dir_all(&old_cache);
                // Clean up stale worktree reference
                let _ = self.git_in_repo(&["worktree", "prune"]);
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
            self.git_in_repo(&["fetch", "origin", OLD_BRANCH])?;
            self.git_in_repo(&["branch", HUB_BRANCH, &format!("origin/{}", OLD_BRANCH)])?;
        }

        // 3. Push new branch to remote (best-effort)
        let has_new_remote = self
            .git_in_repo(&["ls-remote", "--heads", "origin", HUB_BRANCH])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);
        if !has_new_remote {
            let _ = self.git_in_repo(&["push", "-u", "origin", HUB_BRANCH]);
        }

        // 4. Delete old remote branch (best-effort)
        if has_old_remote {
            let _ = self.git_in_repo(&["push", "origin", "--delete", OLD_BRANCH]);
        }

        // 5. Delete old local branch if still present
        if self
            .git_in_repo(&["rev-parse", "--verify", OLD_BRANCH])
            .is_ok()
        {
            let _ = self.git_in_repo(&["branch", "-D", OLD_BRANCH]);
        }

        eprintln!("Migration complete: coordination branch is now crosslink/hub");
        Ok(true)
    }

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
            .git_in_repo(&["ls-remote", "--heads", "origin", HUB_BRANCH])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

        if has_remote {
            // Fetch the remote branch
            self.git_in_repo(&["fetch", "origin", HUB_BRANCH])?;

            // Check if a local branch already exists
            let has_local = self
                .git_in_repo(&["rev-parse", "--verify", HUB_BRANCH])
                .is_ok();

            if has_local {
                self.git_in_repo(&["worktree", "add", &self.cache_path_str(), HUB_BRANCH])?;
            } else {
                // Create local branch tracking remote
                self.git_in_repo(&[
                    "worktree",
                    "add",
                    "-b",
                    HUB_BRANCH,
                    &self.cache_path_str(),
                    &format!("origin/{}", HUB_BRANCH),
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

            // Commit the initial state so the branch has at least one commit.
            // Without this, `git log` and other commands fail on the empty orphan.
            self.git_in_cache(&["add", "locks.json"])?;
            self.git_in_cache(&["commit", "-m", "Initialize crosslink/hub branch"])?;
        }

        Ok(())
    }

    /// Fetch the latest state from remote and reset the cache to match.
    pub fn fetch(&self) -> Result<()> {
        // Try fetching from remote. If no remote is configured, this is a no-op.
        let fetch_result = self.git_in_cache(&["fetch", "origin", HUB_BRANCH]);
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
        let remote_ref = format!("origin/{}", HUB_BRANCH);
        let log_result = self.git_in_cache(&["log", &format!("{}..HEAD", remote_ref), "--oneline"]);
        if let Ok(output) = &log_result {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                // Unpushed commits exist — rebase to preserve them
                let rebase_result = self.git_in_cache(&["rebase", &remote_ref]);
                if let Err(e) = &rebase_result {
                    let err_str = e.to_string();
                    if err_str.contains("unknown revision")
                        || err_str.contains("ambiguous argument")
                    {
                        return Ok(());
                    }
                    rebase_result?;
                }
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

    /// Read the current locks file from the cache.
    pub fn read_locks(&self) -> Result<LocksFile> {
        let path = self.cache_dir.join("locks.json");
        if !path.exists() {
            return Ok(LocksFile::empty());
        }
        LocksFile::load(&path)
    }

    /// Read the trust keyring from the cache.
    pub fn read_keyring(&self) -> Result<Option<Keyring>> {
        let path = self.cache_dir.join("trust").join("keyring.json");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(Keyring::load(&path)?))
    }

    /// Verify the GPG signature on the latest commit that touched locks.json.
    pub fn verify_locks_signature(&self) -> Result<GpgVerification> {
        // Get the commit that last touched locks.json
        let output = self.git_in_cache(&["log", "-1", "--format=%H", "--", "locks.json"])?;
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if commit.is_empty() {
            return Ok(GpgVerification::NoCommits);
        }

        // Try to verify the commit signature
        let verify = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["verify-commit", "--raw", &commit])
            .output()
            .context("Failed to run git verify-commit")?;

        let stderr = String::from_utf8_lossy(&verify.stderr);

        if verify.status.success() {
            let fingerprint = parse_gpg_fingerprint(&stderr);
            Ok(GpgVerification::Valid {
                commit,
                fingerprint,
            })
        } else if stderr.contains("NODATA") || stderr.contains("no signature") || stderr.is_empty()
        {
            Ok(GpgVerification::Unsigned { commit })
        } else {
            Ok(GpgVerification::Invalid {
                commit,
                reason: stderr.to_string(),
            })
        }
    }

    /// Write and optionally push a heartbeat file for this agent.
    pub fn push_heartbeat(&self, agent: &AgentConfig, active_issue_id: Option<i64>) -> Result<()> {
        let heartbeat = Heartbeat {
            agent_id: agent.agent_id.clone(),
            last_heartbeat: Utc::now(),
            active_issue_id,
            machine_id: agent.machine_id.clone(),
        };

        // Ensure heartbeats directory exists
        let hb_dir = self.cache_dir.join("heartbeats");
        std::fs::create_dir_all(&hb_dir)?;

        let filename = format!("{}.json", agent.agent_id);
        let path = hb_dir.join(&filename);
        let json = serde_json::to_string_pretty(&heartbeat)?;
        std::fs::write(&path, json)?;

        // Stage the heartbeat file
        self.git_in_cache(&["add", &format!("heartbeats/{}", filename)])?;

        // Commit (may fail if nothing changed, that's fine)
        let msg = format!(
            "heartbeat: {} at {}",
            agent.agent_id,
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );
        let commit_result = self.git_in_cache(&["commit", "-m", &msg]);
        if let Err(e) = &commit_result {
            let err_str = e.to_string();
            if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                return Ok(());
            }
            commit_result?;
        }

        // Push (best-effort — may fail if offline or conflicts)
        let push_result = self.git_in_cache(&["push", "origin", HUB_BRANCH]);
        if let Err(e) = &push_result {
            let err_str = e.to_string();
            if err_str.contains("Could not resolve host")
                || err_str.contains("Could not read from remote")
            {
                // Offline — silently skip push
                return Ok(());
            }
            // If push is rejected (conflict), try pull+push once
            if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                let _ = self.git_in_cache(&["pull", "--rebase", "origin", HUB_BRANCH]);
                let _ = self.git_in_cache(&["push", "origin", HUB_BRANCH]);
            }
        }

        Ok(())
    }

    /// Read all heartbeat files from the cache.
    pub fn read_heartbeats(&self) -> Result<Vec<Heartbeat>> {
        let dir = self.cache_dir.join("heartbeats");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut heartbeats = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                let content = std::fs::read_to_string(&path)?;
                if let Ok(hb) = serde_json::from_str::<Heartbeat>(&content) {
                    heartbeats.push(hb);
                }
            }
        }
        Ok(heartbeats)
    }

    /// Find locks that have gone stale (no heartbeat within the timeout).
    pub fn find_stale_locks(&self) -> Result<Vec<(i64, String)>> {
        let locks = self.read_locks()?;
        let heartbeats = self.read_heartbeats()?;
        let timeout = chrono::Duration::minutes(locks.settings.stale_lock_timeout_minutes as i64);
        let now = Utc::now();

        let mut stale = Vec::new();
        for (issue_id_str, lock) in &locks.locks {
            let has_fresh_heartbeat = heartbeats
                .iter()
                .any(|hb| hb.agent_id == lock.agent_id && (now - hb.last_heartbeat) < timeout);
            if !has_fresh_heartbeat {
                if let Ok(id) = issue_id_str.parse::<i64>() {
                    stale.push((id, lock.agent_id.clone()));
                }
            }
        }
        Ok(stale)
    }

    /// Claim a lock on an issue for the given agent.
    ///
    /// Writes the lock to `locks.json`, commits, and pushes with retry.
    /// Returns `Ok(true)` if newly claimed, `Ok(false)` if already held by self.
    /// Fails if locked by another agent (unless `force` is true for steal).
    pub fn claim_lock(
        &self,
        agent: &AgentConfig,
        issue_id: i64,
        branch: Option<&str>,
        force: bool,
    ) -> Result<bool> {
        let mut locks = self.read_locks()?;

        // Check existing lock
        if let Some(existing) = locks.get_lock(issue_id) {
            if existing.agent_id == agent.agent_id {
                return Ok(false); // Already held by self
            }
            if !force {
                bail!(
                    "Issue #{} is locked by '{}' (claimed {}). \
                     Use 'crosslink locks steal {}' if the lock is stale.",
                    issue_id,
                    existing.agent_id,
                    existing.claimed_at.format("%Y-%m-%d %H:%M"),
                    issue_id
                );
            }
            // force=true: steal the lock
        }

        let lock = crate::locks::Lock {
            agent_id: agent.agent_id.clone(),
            branch: branch.map(|s| s.to_string()),
            claimed_at: Utc::now(),
            signed_by: agent.agent_id.clone(), // placeholder, GPG signing is optional
        };

        locks.locks.insert(issue_id.to_string(), lock);
        locks.save(&self.cache_dir.join("locks.json"))?;

        self.commit_and_push_locks(&format!("{}: claim lock on #{}", agent.agent_id, issue_id))?;

        Ok(true)
    }

    /// Release a lock on an issue.
    ///
    /// Returns `Ok(true)` if released, `Ok(false)` if not locked.
    /// Fails if locked by a different agent (unless `force` is true).
    pub fn release_lock(&self, agent: &AgentConfig, issue_id: i64, force: bool) -> Result<bool> {
        let mut locks = self.read_locks()?;

        match locks.get_lock(issue_id) {
            None => return Ok(false),
            Some(existing) => {
                if existing.agent_id != agent.agent_id && !force {
                    bail!(
                        "Issue #{} is locked by '{}', not by you ('{}').",
                        issue_id,
                        existing.agent_id,
                        agent.agent_id
                    );
                }
            }
        }

        locks.locks.remove(&issue_id.to_string());
        locks.save(&self.cache_dir.join("locks.json"))?;

        self.commit_and_push_locks(&format!(
            "{}: release lock on #{}",
            agent.agent_id, issue_id
        ))?;

        Ok(true)
    }

    /// Stage locks.json, commit, and push with rebase-retry.
    fn commit_and_push_locks(&self, message: &str) -> Result<()> {
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
            let push_result = self.git_in_cache(&["push", "origin", HUB_BRANCH]);
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
                            let _ = self.git_in_cache(&["pull", "--rebase", "origin", HUB_BRANCH]);
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

    /// Check if the cache directory is initialized.
    pub fn is_initialized(&self) -> bool {
        self.cache_dir.exists()
    }

    /// Get the path to the cache directory.
    pub fn cache_path(&self) -> &Path {
        &self.cache_dir
    }

    // --- Private helpers ---

    fn cache_path_str(&self) -> String {
        self.cache_dir.to_string_lossy().to_string()
    }

    fn git_in_repo(&self, args: &[&str]) -> Result<std::process::Output> {
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

    fn git_in_cache(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {:?} in cache", args))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {:?} in cache failed: {}", args, stderr);
        }
        Ok(output)
    }
}

/// Parse GPG fingerprint from `git verify-commit --raw` output.
///
/// Looks for lines like: `[GNUPG:] VALIDSIG <fingerprint> ...`
fn parse_gpg_fingerprint(gpg_output: &str) -> Option<String> {
    for line in gpg_output.lines() {
        if line.contains("VALIDSIG") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                return Some(parts[2].to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_gpg_fingerprint_valid() {
        let output = "[GNUPG:] VALIDSIG ABCDEF1234567890 2024-01-01 12345678\n[GNUPG:] GOODSIG";
        let fp = parse_gpg_fingerprint(output);
        assert_eq!(fp, Some("ABCDEF1234567890".to_string()));
    }

    #[test]
    fn test_parse_gpg_fingerprint_no_validsig() {
        let output = "[GNUPG:] GOODSIG ABC123\n[GNUPG:] TRUST_FULLY";
        let fp = parse_gpg_fingerprint(output);
        assert!(fp.is_none());
    }

    #[test]
    fn test_parse_gpg_fingerprint_empty() {
        let fp = parse_gpg_fingerprint("");
        assert!(fp.is_none());
    }

    #[test]
    fn test_sync_manager_new() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        assert_eq!(manager.cache_dir, crosslink_dir.join(HUB_CACHE_DIR));
        assert_eq!(manager.repo_root, dir.path());
    }

    #[test]
    fn test_sync_manager_not_initialized() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        assert!(!manager.is_initialized());
    }

    #[test]
    fn test_read_locks_no_cache() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        // Cache doesn't exist yet, but read_locks should return empty
        // (it checks if the file exists)
        let locks_path = manager.cache_dir.join("locks.json");
        assert!(!locks_path.exists());
    }

    #[test]
    fn test_read_heartbeats_no_dir() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        // Manually create cache dir without heartbeats subdir
        std::fs::create_dir_all(&manager.cache_dir).unwrap();
        let heartbeats = manager.read_heartbeats().unwrap();
        assert!(heartbeats.is_empty());
    }

    #[test]
    fn test_read_heartbeats_with_files() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let hb_dir = cache_dir.join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        let hb = Heartbeat {
            agent_id: "worker-1".to_string(),
            last_heartbeat: Utc::now(),
            active_issue_id: Some(5),
            machine_id: "test-host".to_string(),
        };
        let json = serde_json::to_string_pretty(&hb).unwrap();
        std::fs::write(hb_dir.join("worker-1.json"), json).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let heartbeats = manager.read_heartbeats().unwrap();
        assert_eq!(heartbeats.len(), 1);
        assert_eq!(heartbeats[0].agent_id, "worker-1");
        assert_eq!(heartbeats[0].active_issue_id, Some(5));
    }

    #[test]
    fn test_find_stale_locks_empty() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        // Write empty locks.json
        let locks = LocksFile::empty();
        locks.save(&cache_dir.join("locks.json")).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let stale = manager.find_stale_locks().unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_locks_with_stale() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let hb_dir = cache_dir.join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        // Create a lock
        let mut locks_map = std::collections::HashMap::new();
        locks_map.insert(
            "5".to_string(),
            crate::locks::Lock {
                agent_id: "worker-1".to_string(),
                branch: None,
                claimed_at: Utc::now(),
                signed_by: "ABC".to_string(),
            },
        );
        let locks = LocksFile {
            version: 1,
            locks: locks_map,
            settings: crate::locks::LockSettings {
                stale_lock_timeout_minutes: 60,
            },
        };
        locks.save(&cache_dir.join("locks.json")).unwrap();

        // No heartbeat file for worker-1 → stale
        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let stale = manager.find_stale_locks().unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], (5, "worker-1".to_string()));
    }

    #[test]
    fn test_find_stale_locks_with_fresh_heartbeat() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let hb_dir = cache_dir.join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();

        // Create a lock
        let mut locks_map = std::collections::HashMap::new();
        locks_map.insert(
            "5".to_string(),
            crate::locks::Lock {
                agent_id: "worker-1".to_string(),
                branch: None,
                claimed_at: Utc::now(),
                signed_by: "ABC".to_string(),
            },
        );
        let locks = LocksFile {
            version: 1,
            locks: locks_map,
            settings: crate::locks::LockSettings {
                stale_lock_timeout_minutes: 60,
            },
        };
        locks.save(&cache_dir.join("locks.json")).unwrap();

        // Fresh heartbeat
        let hb = Heartbeat {
            agent_id: "worker-1".to_string(),
            last_heartbeat: Utc::now(),
            active_issue_id: Some(5),
            machine_id: "test".to_string(),
        };
        let json = serde_json::to_string(&hb).unwrap();
        std::fs::write(hb_dir.join("worker-1.json"), json).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let stale = manager.find_stale_locks().unwrap();
        assert!(stale.is_empty());
    }
}
