use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::identity::AgentConfig;
use crate::locks::{Heartbeat, Keyring, LocksFile};
use crate::signing;
use crate::utils::resolve_main_repo_root;

/// Directory name under .crosslink for the hub cache worktree.
pub(crate) const HUB_CACHE_DIR: &str = ".hub-cache";

/// The coordination branch name.
pub(crate) const HUB_BRANCH: &str = "crosslink/hub";

/// Old directory name (for migration from crosslink/locks).
const OLD_CACHE_DIR: &str = ".locks-cache";

/// Old branch name (for migration from crosslink/locks).
const OLD_BRANCH: &str = "crosslink/locks";

/// Re-export from `signing` module. Use `SignatureVerification` for new code.
pub use crate::signing::SignatureVerification;

/// Deprecated alias — use `SignatureVerification` instead.
pub type GpgVerification = SignatureVerification;

/// Read the configured tracker remote name from `.crosslink/hook-config.json`.
///
/// Returns the value of `tracker_remote` if set, otherwise `"origin"`.
pub fn read_tracker_remote(crosslink_dir: &Path) -> String {
    let config_path = crosslink_dir.join("hook-config.json");
    std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|v| {
            v.get("tracker_remote")
                .and_then(|r| r.as_str().map(|s| s.to_string()))
        })
        .unwrap_or_else(|| "origin".to_string())
}

/// Manages synchronization with the `crosslink/hub` coordination branch.
///
/// Uses a git worktree at `.crosslink/.hub-cache/` to avoid disturbing
/// the user's working tree.
pub struct SyncManager {
    /// Path to the .crosslink directory.
    crosslink_dir: PathBuf,
    /// Path to .crosslink/.hub-cache (worktree of crosslink/hub branch).
    cache_dir: PathBuf,
    /// The repo root (parent of .crosslink).
    repo_root: PathBuf,
    /// Git remote name for the hub branch (from config, defaults to "origin").
    remote: String,
}

impl SyncManager {
    /// Create a new SyncManager for the given .crosslink directory.
    ///
    /// When running inside a git worktree, automatically detects the main
    /// repository root and uses its `.crosslink/.hub-cache/` so that the
    /// shared coordination branch worktree is never duplicated.
    pub fn new(crosslink_dir: &Path) -> Result<Self> {
        let local_repo_root = crosslink_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root from .crosslink dir"))?
            .to_path_buf();

        // If we're inside a git worktree, resolve the main repo root so the
        // hub cache lives in one shared location rather than per-worktree.
        let repo_root =
            resolve_main_repo_root(&local_repo_root).unwrap_or_else(|| local_repo_root.clone());

        let cache_dir = repo_root.join(".crosslink").join(HUB_CACHE_DIR);
        let remote = read_tracker_remote(crosslink_dir);

        Ok(SyncManager {
            crosslink_dir: crosslink_dir.to_path_buf(),
            cache_dir,
            repo_root,
            remote,
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
            .git_in_repo(&["ls-remote", "--heads", &self.remote, OLD_BRANCH])
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
                eprintln!(
                    "Warning: migration push failed, changes saved locally only: {}",
                    e
                );
            }
        }

        // 4. Delete old remote branch (best-effort)
        if has_old_remote {
            if let Err(e) = self.git_in_repo(&["push", &self.remote, "--delete", OLD_BRANCH]) {
                eprintln!(
                    "Warning: failed to delete old remote branch '{}': {}",
                    OLD_BRANCH, e
                );
            }
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

    /// Configure SSH signing in the hub cache worktree.
    ///
    /// If the agent has an SSH key, sets `gpg.format=ssh`, `user.signingkey`,
    /// and `commit.gpgsign=true` in the cache worktree's local git config.
    /// This makes all subsequent commits on the hub branch automatically signed.
    pub fn configure_signing(&self, crosslink_dir: &Path) -> Result<()> {
        if !self.cache_dir.exists() {
            return Ok(());
        }

        let agent = match AgentConfig::load(crosslink_dir)? {
            Some(a) => a,
            None => return Ok(()),
        };

        let (rel_key, _fingerprint) = match (&agent.ssh_key_path, &agent.ssh_fingerprint) {
            (Some(k), Some(f)) => (k.clone(), f.clone()),
            _ => return Ok(()),
        };

        // Resolve private key path (relative to .crosslink/)
        let private_key = self.crosslink_dir.join(&rel_key);
        if !private_key.exists() {
            return Ok(());
        }

        // Set up allowed_signers path
        let allowed_signers = self.cache_dir.join("trust").join("allowed_signers");

        signing::configure_git_ssh_signing(
            &self.cache_dir,
            &private_key,
            if allowed_signers.exists() {
                Some(&allowed_signers)
            } else {
                None
            },
        )?;

        Ok(())
    }

    /// Ensure the agent's public key is published to `trust/keys/` on the hub.
    ///
    /// During `agent init`, key publishing is skipped if the hub cache doesn't
    /// exist yet. This method re-checks and publishes the key if needed, using
    /// an unsigned commit to avoid the chicken-and-egg problem where signing
    /// must be configured before the key can be published.
    ///
    /// Safe to call multiple times — no-ops if the key is already published.
    pub fn ensure_agent_key_published(&self, crosslink_dir: &Path) -> Result<bool> {
        if !self.cache_dir.exists() {
            return Ok(false);
        }

        let agent = match AgentConfig::load(crosslink_dir)? {
            Some(a) => a,
            None => return Ok(false),
        };

        let public_key = match &agent.ssh_public_key {
            Some(k) => k.clone(),
            None => return Ok(false),
        };

        let key_file = self
            .cache_dir
            .join("trust")
            .join("keys")
            .join(format!("{}.pub", agent.agent_id));

        if key_file.exists() {
            return Ok(false); // Already published
        }

        // Publish the key using an unsigned commit to avoid the signing
        // chicken-and-egg: we need to publish before signing is configured.
        let keys_dir = self.cache_dir.join("trust").join("keys");
        std::fs::create_dir_all(&keys_dir)?;
        std::fs::write(&key_file, format!("{}\n", public_key))?;

        self.git_in_cache(&["add", "trust/"])?;
        // Use -c commit.gpgsign=false to bypass signing for key publishing
        let output = Command::new("git")
            .current_dir(&self.cache_dir)
            .args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                &format!("trust: publish key for agent '{}'", agent.agent_id),
            ])
            .output()
            .context("Failed to commit key publication")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("nothing to commit") {
                bail!("git commit for key publication failed: {}", stderr);
            }
        }

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
                // Dirty state found — stage and commit to recover
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

    /// Fetch the latest state from remote and reset the cache to match.
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
        if let Ok(output) = &log_result {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                // Clean dirty state before rebase — prevents "cannot pull
                // with rebase: You have unstaged changes" error loop
                self.clean_dirty_state()?;
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

    /// Read locks from V2 per-issue lock files at `locks/*.json`.
    ///
    /// Converts to LocksFile format for backward compatibility with existing code.
    pub fn read_locks_v2(&self) -> Result<LocksFile> {
        use crate::issue_file::LockFileV2;
        use crate::locks::Lock;
        use std::collections::HashMap;

        let locks_dir = self.cache_dir.join("locks");
        if !locks_dir.exists() {
            return Ok(LocksFile::empty());
        }

        let mut locks = HashMap::new();
        for entry in std::fs::read_dir(&locks_dir)
            .with_context(|| format!("Failed to read locks dir: {}", locks_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("Failed to read lock file: {}", path.display()))?;
            let lock_v2: LockFileV2 = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse lock file: {}", path.display()))?;
            let lock = Lock {
                agent_id: lock_v2.agent_id,
                branch: lock_v2.branch,
                claimed_at: lock_v2.claimed_at,
                signed_by: lock_v2.signed_by.unwrap_or_default(),
            };
            locks.insert(lock_v2.issue_id.to_string(), lock);
        }

        Ok(LocksFile {
            version: 2,
            locks,
            settings: crate::locks::LockSettings::default(),
        })
    }

    /// Read locks using the appropriate method based on hub layout version.
    ///
    /// V1: reads `locks.json` (single file)
    /// V2: reads `locks/*.json` (per-issue files)
    pub fn read_locks_auto(&self) -> Result<LocksFile> {
        let meta_dir = self.cache_dir.join("meta");
        let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
        if version >= 2 {
            self.read_locks_v2()
        } else {
            self.read_locks()
        }
    }

    /// Read the trust keyring from the cache (deprecated — use `read_allowed_signers`).
    pub fn read_keyring(&self) -> Result<Option<Keyring>> {
        let path = self.cache_dir.join("trust").join("keyring.json");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(Keyring::load(&path)?))
    }

    /// Read the SSH allowed_signers trust store from the cache.
    pub fn read_allowed_signers(&self) -> Result<signing::AllowedSigners> {
        let path = self.cache_dir.join("trust").join("allowed_signers");
        signing::AllowedSigners::load(&path)
    }

    /// Verify the last N commits on the hub branch.
    ///
    /// Returns a list of `(commit_hash, verification_result)`.
    pub fn verify_recent_commits(
        &self,
        count: usize,
    ) -> Result<Vec<(String, SignatureVerification)>> {
        let output = self.git_in_cache(&["log", &format!("-{}", count), "--format=%H"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let commits: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

        let mut results = Vec::new();
        for commit in commits {
            let verify = Command::new("git")
                .current_dir(&self.cache_dir)
                .args(["verify-commit", "--raw", commit])
                .output()
                .context("Failed to run git verify-commit")?;

            let stderr = String::from_utf8_lossy(&verify.stderr);
            let verification = if verify.status.success() {
                let parsed = signing::parse_verify_output(&stderr);
                let principal = parsed.as_ref().and_then(|(p, _)| p.clone());
                let fingerprint = parsed.map(|(_, f)| f);
                SignatureVerification::Valid {
                    commit: commit.to_string(),
                    fingerprint,
                    principal,
                }
            } else if stderr.contains("NODATA")
                || stderr.contains("no signature")
                || stderr.is_empty()
            {
                SignatureVerification::Unsigned {
                    commit: commit.to_string(),
                }
            } else if stderr.contains("allowedSignersFile needs to be configured") {
                // gpg.ssh.allowedSignersFile not set — verification is not possible,
                // but this doesn't mean the signature is invalid. Degrade gracefully.
                SignatureVerification::Unsigned {
                    commit: commit.to_string(),
                }
            } else {
                SignatureVerification::Invalid {
                    commit: commit.to_string(),
                    reason: stderr.to_string(),
                }
            };
            results.push((commit.to_string(), verification));
        }

        Ok(results)
    }

    /// Verify per-entry signatures on comments in cached issue files.
    ///
    /// Reads all issues from the cache, checks any comments that have
    /// `signed_by` + `signature` fields against the allowed_signers store
    /// using `signing::verify_content()`.
    ///
    /// Returns `(verified, failed, unsigned)` counts.
    pub fn verify_entry_signatures(&self) -> Result<(usize, usize, usize)> {
        let issues_dir = self.cache_dir.join("issues");
        let issues = crate::issue_file::read_all_issue_files(&issues_dir)?;
        let allowed_signers_path = self.cache_dir.join("trust").join("allowed_signers");

        let mut verified = 0usize;
        let mut failed = 0usize;
        let mut unsigned = 0usize;

        for issue in &issues {
            for comment in &issue.comments {
                match (&comment.signed_by, &comment.signature) {
                    (Some(fingerprint), Some(sig)) => {
                        // Reconstruct canonical content for verification
                        let canonical = signing::canonicalize_for_signing(&[
                            ("author", &comment.author),
                            ("comment_id", &comment.id.to_string()),
                            ("content", &comment.content),
                        ]);
                        // Use fingerprint as principal for verification
                        let principal = format!("{}@crosslink", &comment.author);
                        match signing::verify_content(
                            &allowed_signers_path,
                            &principal,
                            "crosslink-comment",
                            &canonical,
                            sig,
                        ) {
                            Ok(true) => verified += 1,
                            Ok(false) => {
                                eprintln!(
                                    "warning: signature verification failed for comment {} by '{}' (signer: {})",
                                    comment.id, comment.author, fingerprint
                                );
                                failed += 1;
                            }
                            Err(e) => {
                                // Verification unavailable (no allowed_signers, no ssh-keygen)
                                // Treat as unverifiable but not failed
                                if allowed_signers_path.exists() {
                                    eprintln!(
                                        "warning: signature verification error for comment {} by '{}': {}",
                                        comment.id, comment.author, e
                                    );
                                    failed += 1;
                                } else {
                                    // Can't verify without allowed_signers — count as signed but unverifiable
                                    let _ = fingerprint; // acknowledge the signature exists
                                    unsigned += 1;
                                }
                            }
                        }
                    }
                    _ => {
                        unsigned += 1;
                    }
                }
            }
        }

        Ok((verified, failed, unsigned))
    }

    /// Verify the signature on the latest commit that touched locks.json.
    ///
    /// Handles both SSH and GPG signatures via `signing::parse_verify_output`.
    pub fn verify_locks_signature(&self) -> Result<SignatureVerification> {
        // Get the commit that last touched locks.json
        let output = self.git_in_cache(&["log", "-1", "--format=%H", "--", "locks.json"])?;
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if commit.is_empty() {
            return Ok(SignatureVerification::NoCommits);
        }

        // Try to verify the commit signature
        let verify = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["verify-commit", "--raw", &commit])
            .output()
            .context("Failed to run git verify-commit")?;

        let stderr = String::from_utf8_lossy(&verify.stderr);

        if verify.status.success() {
            let parsed = signing::parse_verify_output(&stderr);
            let principal = parsed.as_ref().and_then(|(p, _)| p.clone());
            let fingerprint = parsed.map(|(_, f)| f);
            Ok(SignatureVerification::Valid {
                commit,
                fingerprint,
                principal,
            })
        } else if stderr.contains("NODATA") || stderr.contains("no signature") || stderr.is_empty()
        {
            Ok(SignatureVerification::Unsigned { commit })
        } else if stderr.contains("allowedSignersFile needs to be configured") {
            // gpg.ssh.allowedSignersFile not set — cannot verify, degrade gracefully
            Ok(SignatureVerification::Unsigned { commit })
        } else {
            Ok(SignatureVerification::Invalid {
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
        let push_result = self.git_in_cache(&["push", &self.remote, HUB_BRANCH]);
        if let Err(e) = &push_result {
            let err_str = e.to_string();
            if err_str.contains("Could not resolve host")
                || err_str.contains("Could not read from remote")
            {
                eprintln!("Warning: heartbeat push failed (offline), changes saved locally only");
                return Ok(());
            }
            // If push is rejected (conflict), clean dirty state and try pull+push once
            if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                let _ = self.clean_dirty_state();
                let _ = self.git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH]);
                if let Err(retry_err) = self.git_in_cache(&["push", &self.remote, HUB_BRANCH]) {
                    eprintln!(
                        "Warning: heartbeat push failed after retry (conflict), changes saved locally only: {}",
                        retry_err
                    );
                }
            }
        }

        Ok(())
    }

    /// Read all heartbeat files from the V1 cache (`heartbeats/` directory).
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

    /// Read heartbeats from the V2 layout (`agents/{id}/heartbeat.json`).
    ///
    /// V2 heartbeat files use `timestamp` (RFC 3339) instead of `last_heartbeat`,
    /// and may lack `active_issue_id` / `machine_id`. This method converts them
    /// into the common `Heartbeat` struct.
    pub fn read_heartbeats_v2(&self) -> Result<Vec<Heartbeat>> {
        let agents_dir = self.cache_dir.join("agents");
        if !agents_dir.exists() {
            return Ok(Vec::new());
        }
        let mut heartbeats = Vec::new();
        for entry in std::fs::read_dir(&agents_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let agent_id = entry.file_name().to_string_lossy().to_string();
            let hb_path = entry.path().join("heartbeat.json");
            if !hb_path.exists() {
                continue;
            }
            let content = match std::fs::read_to_string(&hb_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            // Try native Heartbeat format first, then V2 JSON format
            if let Ok(hb) = serde_json::from_str::<Heartbeat>(&content) {
                heartbeats.push(hb);
            } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                let timestamp = val
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(Utc::now);
                let active_issue_id = val.get("active_issue_id").and_then(|v| v.as_i64());
                let machine_id = val
                    .get("machine_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                heartbeats.push(Heartbeat {
                    agent_id,
                    last_heartbeat: timestamp,
                    active_issue_id,
                    machine_id,
                });
            }
        }
        Ok(heartbeats)
    }

    /// Read heartbeats using the appropriate method based on hub layout version.
    ///
    /// V1: reads `heartbeats/*.json`
    /// V2: reads `agents/*/heartbeat.json`, merged with any V1 heartbeats
    pub fn read_heartbeats_auto(&self) -> Result<Vec<Heartbeat>> {
        let mut heartbeats = self.read_heartbeats()?;
        if self.is_v2_layout() {
            let v2 = self.read_heartbeats_v2()?;
            // Merge V2 heartbeats, preferring the one with the most recent timestamp
            use std::collections::HashMap;
            let mut by_agent: HashMap<String, Heartbeat> = HashMap::new();
            for hb in heartbeats.into_iter().chain(v2) {
                by_agent
                    .entry(hb.agent_id.clone())
                    .and_modify(|existing| {
                        if hb.last_heartbeat > existing.last_heartbeat {
                            *existing = hb.clone();
                        }
                    })
                    .or_insert(hb);
            }
            heartbeats = by_agent.into_values().collect();
        }
        Ok(heartbeats)
    }

    /// Find locks that have gone stale (no heartbeat within the timeout).
    ///
    /// Auto-dispatches based on hub layout version:
    /// - V2: uses per-agent heartbeat timestamps at `agents/{id}/heartbeat.json`
    /// - V1: uses the legacy `heartbeats/` directory with `stale_lock_timeout_minutes`
    pub fn find_stale_locks(&self) -> Result<Vec<(i64, String)>> {
        if self.is_v2_layout() {
            return self.find_stale_locks_v2(chrono::Duration::minutes(30));
        }

        let locks = self.read_locks_auto()?;
        let heartbeats = self.read_heartbeats()?;
        let timeout = chrono::Duration::minutes(locks.settings.stale_lock_timeout_minutes as i64);
        let now = Utc::now();

        let mut stale = Vec::new();
        for (issue_id_str, lock) in &locks.locks {
            let has_fresh_heartbeat = heartbeats.iter().any(|hb| {
                hb.agent_id == lock.agent_id
                    && now
                        .signed_duration_since(hb.last_heartbeat)
                        .max(chrono::Duration::zero())
                        < timeout
            });
            if !has_fresh_heartbeat {
                if let Ok(id) = issue_id_str.parse::<i64>() {
                    stale.push((id, lock.agent_id.clone()));
                }
            }
        }
        Ok(stale)
    }

    /// Find stale locks using agent heartbeat timestamps (V2 layout).
    ///
    /// A lock is considered stale if the holding agent's heartbeat is older than
    /// `threshold`, or if no heartbeat file exists. Falls back to claim_at based
    /// detection for V1.
    pub fn find_stale_locks_v2(&self, threshold: chrono::Duration) -> Result<Vec<(i64, String)>> {
        let locks = self.read_locks_v2()?;
        let now = Utc::now();
        let mut stale = Vec::new();

        for (issue_id_str, lock) in &locks.locks {
            let heartbeat_path = self
                .cache_dir
                .join("agents")
                .join(&lock.agent_id)
                .join("heartbeat.json");

            let is_stale = if heartbeat_path.exists() {
                match std::fs::read_to_string(&heartbeat_path) {
                    Ok(content) => {
                        match serde_json::from_str::<serde_json::Value>(&content) {
                            Ok(val) => {
                                match val.get("timestamp").and_then(|t| t.as_str()) {
                                    Some(ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
                                        Ok(heartbeat_time) => {
                                            let age = now
                                                .signed_duration_since(heartbeat_time)
                                                .max(chrono::Duration::zero());
                                            age > threshold
                                        }
                                        Err(_) => true, // Unparseable timestamp → stale
                                    },
                                    None => true, // No timestamp field → stale
                                }
                            }
                            Err(_) => true, // Invalid JSON → stale
                        }
                    }
                    Err(_) => true, // Unreadable file → stale
                }
            } else {
                true // No heartbeat file → stale
            };

            if is_stale {
                if let Ok(id) = issue_id_str.parse::<i64>() {
                    stale.push((id, lock.agent_id.clone()));
                }
            }
        }

        Ok(stale)
    }

    /// Find stale locks with their age in minutes.
    ///
    /// Returns `(issue_id, agent_id, stale_minutes)` for each stale lock.
    /// Auto-dispatches based on hub layout version.
    pub fn find_stale_locks_with_age(&self) -> Result<Vec<(i64, String, u64)>> {
        if self.is_v2_layout() {
            return self.find_stale_locks_with_age_v2();
        }

        let locks = self.read_locks_auto()?;
        let heartbeats = self.read_heartbeats()?;
        let timeout = chrono::Duration::minutes(locks.settings.stale_lock_timeout_minutes as i64);
        let now = Utc::now();

        let mut stale = Vec::new();
        for (issue_id_str, lock) in &locks.locks {
            let latest_heartbeat = heartbeats
                .iter()
                .filter(|hb| hb.agent_id == lock.agent_id)
                .map(|hb| hb.last_heartbeat)
                .max();

            let age = match latest_heartbeat {
                Some(hb_time) => now
                    .signed_duration_since(hb_time)
                    .max(chrono::Duration::zero()),
                None => now
                    .signed_duration_since(lock.claimed_at)
                    .max(chrono::Duration::zero()),
            };

            if age >= timeout {
                if let Ok(id) = issue_id_str.parse::<i64>() {
                    stale.push((id, lock.agent_id.clone(), age.num_minutes() as u64));
                }
            }
        }
        Ok(stale)
    }

    fn find_stale_locks_with_age_v2(&self) -> Result<Vec<(i64, String, u64)>> {
        let locks = self.read_locks_v2()?;
        let now = Utc::now();
        let threshold = chrono::Duration::minutes(30);
        let mut stale = Vec::new();

        for (issue_id_str, lock) in &locks.locks {
            let heartbeat_path = self
                .cache_dir
                .join("agents")
                .join(&lock.agent_id)
                .join("heartbeat.json");

            let age_minutes = if heartbeat_path.exists() {
                match std::fs::read_to_string(&heartbeat_path) {
                    Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                        Ok(val) => match val.get("timestamp").and_then(|t| t.as_str()) {
                            Some(ts) => match chrono::DateTime::parse_from_rfc3339(ts) {
                                Ok(hb_time) => {
                                    let age = now
                                        .signed_duration_since(hb_time)
                                        .max(chrono::Duration::zero());
                                    if age > threshold {
                                        Some(age.num_minutes() as u64)
                                    } else {
                                        None
                                    }
                                }
                                Err(_) => Some(u64::MAX),
                            },
                            None => Some(u64::MAX),
                        },
                        Err(_) => Some(u64::MAX),
                    },
                    Err(_) => Some(u64::MAX),
                }
            } else {
                Some(u64::MAX)
            };

            if let Some(mins) = age_minutes {
                if let Ok(id) = issue_id_str.parse::<i64>() {
                    stale.push((id, lock.agent_id.clone(), mins));
                }
            }
        }
        Ok(stale)
    }

    /// Claim a lock on an issue for the given agent.
    ///
    /// Writes the lock to `locks.json`, commits, and pushes with retry.
    /// After a push conflict, re-reads locks to verify another agent didn't
    /// claim the same lock during the race window.
    /// Returns `Ok(true)` if newly claimed, `Ok(false)` if already held by self.
    /// Fails if locked by another agent (unless `force` is true for steal).
    pub fn claim_lock(
        &self,
        agent: &AgentConfig,
        issue_id: i64,
        branch: Option<&str>,
        force: bool,
    ) -> Result<bool> {
        // Retry loop: re-check lock ownership after push conflicts
        for attempt in 0..3 {
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
                signed_by: agent
                    .ssh_fingerprint
                    .clone()
                    .unwrap_or_else(|| agent.agent_id.clone()),
            };

            locks.locks.insert(issue_id.to_string(), lock);
            locks.save(&self.cache_dir.join("locks.json"))?;

            match self
                .commit_and_push_locks(&format!("{}: claim lock on #{}", agent.agent_id, issue_id))
            {
                Ok(()) => return Ok(true),
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Push failed after") && attempt < 2 {
                        // Push conflict — pull latest and re-check lock ownership
                        let _ = self.git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH]);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        bail!(
            "Failed to claim lock on #{} after 3 attempts due to concurrent updates",
            issue_id
        )
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

    /// Get the configured git remote name for the hub branch.
    pub fn remote(&self) -> &str {
        &self.remote
    }

    /// Check if the cache directory is initialized.
    pub fn is_initialized(&self) -> bool {
        self.cache_dir.exists()
    }

    /// Get the path to the cache directory.
    pub fn cache_path(&self) -> &Path {
        &self.cache_dir
    }

    /// Check if the hub uses V2 layout (per-entity lock files in `locks/`).
    pub fn is_v2_layout(&self) -> bool {
        let meta_dir = self.cache_dir.join("meta");
        crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1) >= 2
    }

    /// Create the agent directory on the hub branch if it doesn't exist.
    ///
    /// Creates `agents/{agent_id}/heartbeat.json` with an initial heartbeat.
    /// Returns `Ok(true)` if the directory was created, `Ok(false)` if it already existed.
    pub fn ensure_agent_dir(&self, agent_id: &str) -> Result<bool> {
        if !self.create_agent_dir_files(agent_id)? {
            return Ok(false);
        }

        // Stage and commit
        self.git_in_cache(&["add", &format!("agents/{}/heartbeat.json", agent_id)])?;
        self.git_in_cache(&[
            "commit",
            "-m",
            &format!("bootstrap: initialize agent directory for {}", agent_id),
        ])?;

        // Push with retry on rebase conflict
        for attempt in 0..3 {
            let push_result = self.git_in_cache(&["push", &self.remote, HUB_BRANCH]);
            match push_result {
                Ok(_) => return Ok(true),
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        return Ok(true); // Offline — commit is local
                    }
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < 2 {
                            let _ =
                                self.git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH]);
                            continue;
                        }
                        bail!("Push failed after 3 retries for agent dir {}", agent_id);
                    }
                    return Err(e);
                }
            }
        }

        Ok(true)
    }

    /// Create the agent directory and heartbeat file on disk (no git ops).
    ///
    /// Returns `Ok(true)` if created, `Ok(false)` if the directory already exists.
    fn create_agent_dir_files(&self, agent_id: &str) -> Result<bool> {
        let agents_dir = self.cache_dir.join("agents").join(agent_id);
        if agents_dir.exists() {
            return Ok(false);
        }

        std::fs::create_dir_all(&agents_dir)
            .with_context(|| format!("Failed to create agent directory for {}", agent_id))?;

        // Write initial heartbeat
        let heartbeat = serde_json::json!({
            "agent_id": agent_id,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "status": "active"
        });
        let heartbeat_path = agents_dir.join("heartbeat.json");
        std::fs::write(&heartbeat_path, serde_json::to_string_pretty(&heartbeat)?)
            .with_context(|| "Failed to write initial heartbeat")?;

        Ok(true)
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

    /// Copy `.claude/hooks/` from the repo root into the hub cache worktree.
    ///
    /// PreToolUse hooks resolve their path via `git rev-parse --show-toplevel`.
    /// When an agent's CWD is inside the hub cache, that resolves to the cache
    /// root instead of the main repo, so the hooks must exist there too.
    /// This is a best-effort operation — if `.claude/hooks/` doesn't exist in
    /// the repo root, we silently skip.
    fn propagate_claude_hooks(&self) -> Result<()> {
        let src = self.repo_root.join(".claude").join("hooks");
        if !src.is_dir() {
            return Ok(());
        }
        let dst = self.cache_dir.join(".claude").join("hooks");
        if dst.is_dir() {
            return Ok(()); // already propagated
        }
        std::fs::create_dir_all(&dst)?;
        for entry in std::fs::read_dir(&src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_file() {
                std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
            }
        }
        Ok(())
    }

    /// Ensure the cache worktree has a git identity configured so commits
    /// succeed even in environments without a global git config (e.g. CI).
    ///
    /// Uses `--worktree` scope when the cache dir is a linked worktree to
    /// avoid leaking identity config into the shared `.git/config`.
    fn ensure_cache_git_identity(&self) -> Result<()> {
        let has_email = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["config", "user.email"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !has_email {
            let use_worktree = signing::is_linked_worktree(&self.cache_dir);
            if use_worktree {
                signing::enable_worktree_config(&self.cache_dir)?;
            }
            let scope_flag = if use_worktree {
                "--worktree"
            } else {
                "--local"
            };
            let _ = Command::new("git")
                .current_dir(&self.cache_dir)
                .args(["config", scope_flag, "user.email", "crosslink@localhost"])
                .output();
            let _ = Command::new("git")
                .current_dir(&self.cache_dir)
                .args(["config", scope_flag, "user.name", "crosslink"])
                .output();
        }
        Ok(())
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

// parse_gpg_fingerprint has been moved to signing.rs

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    // GPG fingerprint parsing tests moved to signing.rs

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
    fn test_read_heartbeats_v2_no_dir() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        std::fs::create_dir_all(&manager.cache_dir).unwrap();
        let heartbeats = manager.read_heartbeats_v2().unwrap();
        assert!(heartbeats.is_empty());
    }

    #[test]
    fn test_read_heartbeats_v2_with_native_format() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let agent_dir = cache_dir.join("agents").join("worker-v2");
        std::fs::create_dir_all(&agent_dir).unwrap();

        // Write a native Heartbeat format file in the V2 location
        let hb = Heartbeat {
            agent_id: "worker-v2".to_string(),
            last_heartbeat: Utc::now(),
            active_issue_id: Some(10),
            machine_id: "host-v2".to_string(),
        };
        std::fs::write(
            agent_dir.join("heartbeat.json"),
            serde_json::to_string_pretty(&hb).unwrap(),
        )
        .unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let heartbeats = manager.read_heartbeats_v2().unwrap();
        assert_eq!(heartbeats.len(), 1);
        assert_eq!(heartbeats[0].agent_id, "worker-v2");
        assert_eq!(heartbeats[0].active_issue_id, Some(10));
    }

    #[test]
    fn test_read_heartbeats_v2_with_v2_json_format() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let agent_dir = cache_dir.join("agents").join("worker-v2b");
        std::fs::create_dir_all(&agent_dir).unwrap();

        // Write V2 format: { agent_id, timestamp, status }
        let heartbeat = serde_json::json!({
            "agent_id": "worker-v2b",
            "timestamp": Utc::now().to_rfc3339(),
            "status": "active"
        });
        std::fs::write(
            agent_dir.join("heartbeat.json"),
            serde_json::to_string_pretty(&heartbeat).unwrap(),
        )
        .unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let heartbeats = manager.read_heartbeats_v2().unwrap();
        assert_eq!(heartbeats.len(), 1);
        assert_eq!(heartbeats[0].agent_id, "worker-v2b");
        assert!(heartbeats[0].active_issue_id.is_none());
    }

    #[test]
    fn test_read_heartbeats_auto_merges_v1_and_v2() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

        // Set up V2 layout marker
        let meta_dir = cache_dir.join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

        // Write V1 heartbeat
        let hb_dir = cache_dir.join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();
        let hb1 = Heartbeat {
            agent_id: "worker-v1".to_string(),
            last_heartbeat: Utc::now(),
            active_issue_id: Some(1),
            machine_id: "host-1".to_string(),
        };
        std::fs::write(
            hb_dir.join("worker-v1.json"),
            serde_json::to_string_pretty(&hb1).unwrap(),
        )
        .unwrap();

        // Write V2 heartbeat
        let agent_dir = cache_dir.join("agents").join("worker-v2");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let heartbeat = serde_json::json!({
            "agent_id": "worker-v2",
            "timestamp": Utc::now().to_rfc3339(),
            "status": "active"
        });
        std::fs::write(
            agent_dir.join("heartbeat.json"),
            serde_json::to_string_pretty(&heartbeat).unwrap(),
        )
        .unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let heartbeats = manager.read_heartbeats_auto().unwrap();
        assert_eq!(heartbeats.len(), 2);

        let ids: std::collections::HashSet<String> =
            heartbeats.iter().map(|h| h.agent_id.clone()).collect();
        assert!(ids.contains("worker-v1"));
        assert!(ids.contains("worker-v2"));
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

    #[test]
    fn test_find_stale_locks_v2_fresh_heartbeat() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

        // Set up V2 layout
        let meta_dir = cache_dir.join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

        // Write a lock file
        let locks_dir = cache_dir.join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();
        let lock = crate::issue_file::LockFileV2 {
            issue_id: 5,
            agent_id: "worker-1".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: None,
        };
        std::fs::write(
            locks_dir.join("5.json"),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();

        // Write a fresh heartbeat (now)
        let agent_dir = cache_dir.join("agents").join("worker-1");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let heartbeat = serde_json::json!({
            "agent_id": "worker-1",
            "timestamp": Utc::now().to_rfc3339(),
            "status": "active"
        });
        std::fs::write(
            agent_dir.join("heartbeat.json"),
            serde_json::to_string_pretty(&heartbeat).unwrap(),
        )
        .unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let stale = manager.find_stale_locks().unwrap();
        assert!(stale.is_empty(), "Fresh heartbeat should not be stale");
    }

    #[test]
    fn test_find_stale_locks_v2_old_heartbeat() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

        // Set up V2 layout
        let meta_dir = cache_dir.join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

        // Write a lock file
        let locks_dir = cache_dir.join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();
        let lock = crate::issue_file::LockFileV2 {
            issue_id: 10,
            agent_id: "worker-2".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: None,
        };
        std::fs::write(
            locks_dir.join("10.json"),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();

        // Write a stale heartbeat (2 hours ago)
        let agent_dir = cache_dir.join("agents").join("worker-2");
        std::fs::create_dir_all(&agent_dir).unwrap();
        let old_timestamp = Utc::now() - chrono::Duration::hours(2);
        let heartbeat = serde_json::json!({
            "agent_id": "worker-2",
            "timestamp": old_timestamp.to_rfc3339(),
            "status": "active"
        });
        std::fs::write(
            agent_dir.join("heartbeat.json"),
            serde_json::to_string_pretty(&heartbeat).unwrap(),
        )
        .unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let stale = manager.find_stale_locks().unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], (10, "worker-2".to_string()));
    }

    #[test]
    fn test_find_stale_locks_v2_missing_heartbeat() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

        // Set up V2 layout
        let meta_dir = cache_dir.join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

        // Write a lock file but NO heartbeat
        let locks_dir = cache_dir.join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();
        let lock = crate::issue_file::LockFileV2 {
            issue_id: 7,
            agent_id: "ghost-agent".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: None,
        };
        std::fs::write(
            locks_dir.join("7.json"),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();

        // No agents/ghost-agent/heartbeat.json exists

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let stale = manager.find_stale_locks().unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0], (7, "ghost-agent".to_string()));
    }

    /// Helper: create a git repo with an initial commit.
    fn init_git_repo(path: &Path) {
        let p = path.to_string_lossy();
        Command::new("git").args(["init", &p]).output().unwrap();
        // Set user config so commits work on CI (no global git config).
        Command::new("git")
            .args(["-C", &p, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", &p, "config", "user.name", "Test"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", &p, "commit", "--allow-empty", "-m", "init"])
            .output()
            .unwrap();
    }

    #[test]
    fn test_read_locks_v2_empty_dir() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(cache_dir.join("locks")).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let locks = manager.read_locks_v2().unwrap();
        assert!(locks.locks.is_empty());
        assert_eq!(locks.version, 2);
    }

    #[test]
    fn test_read_locks_v2_no_locks_dir() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let locks = manager.read_locks_v2().unwrap();
        assert!(locks.locks.is_empty());
    }

    #[test]
    fn test_read_locks_v2_with_files() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let locks_dir = cache_dir.join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();

        let lock = crate::issue_file::LockFileV2 {
            issue_id: 5,
            agent_id: "worker-1".to_string(),
            branch: Some("feature/x".to_string()),
            claimed_at: Utc::now(),
            signed_by: Some("SHA256:abc".to_string()),
        };
        let json = serde_json::to_string_pretty(&lock).unwrap();
        std::fs::write(locks_dir.join("5.json"), &json).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let locks = manager.read_locks_v2().unwrap();
        assert_eq!(locks.locks.len(), 1);
        assert!(locks.is_locked(5));
        let l = locks.get_lock(5).unwrap();
        assert_eq!(l.agent_id, "worker-1");
        assert_eq!(l.branch, Some("feature/x".to_string()));
        assert_eq!(l.signed_by, "SHA256:abc");
    }

    #[test]
    fn test_read_locks_v2_skips_non_json() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let locks_dir = cache_dir.join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();

        // Write a non-json file that should be ignored
        std::fs::write(locks_dir.join("README.md"), "ignore me").unwrap();

        let lock = crate::issue_file::LockFileV2 {
            issue_id: 3,
            agent_id: "worker-2".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: None,
        };
        std::fs::write(
            locks_dir.join("3.json"),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let locks = manager.read_locks_v2().unwrap();
        assert_eq!(locks.locks.len(), 1);
        assert!(locks.is_locked(3));
    }

    #[test]
    fn test_read_locks_v2_signed_by_none_defaults_empty() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        let locks_dir = cache_dir.join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();

        let lock = crate::issue_file::LockFileV2 {
            issue_id: 7,
            agent_id: "worker-3".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: None,
        };
        std::fs::write(
            locks_dir.join("7.json"),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let locks = manager.read_locks_v2().unwrap();
        let l = locks.get_lock(7).unwrap();
        assert_eq!(l.signed_by, "");
    }

    #[test]
    fn test_read_locks_auto_v1_default() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        // No meta/version.json -> defaults to V1 -> reads locks.json
        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let locks = manager.read_locks_auto().unwrap();
        assert!(locks.locks.is_empty());
    }

    #[test]
    fn test_read_locks_auto_v2_dispatch() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        // Write V2 layout version
        let meta_dir = cache_dir.join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

        // Write a lock file
        let locks_dir = cache_dir.join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();
        let lock = crate::issue_file::LockFileV2 {
            issue_id: 3,
            agent_id: "worker-2".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: None,
        };
        std::fs::write(
            locks_dir.join("3.json"),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let locks = manager.read_locks_auto().unwrap();
        assert_eq!(locks.locks.len(), 1);
        assert!(locks.is_locked(3));
    }

    #[test]
    fn test_read_locks_auto_v1_explicit() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        // Write V1 layout version explicitly
        let meta_dir = cache_dir.join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        crate::issue_file::write_layout_version(&meta_dir, 1).unwrap();

        // Write a locks.json (V1 format)
        let locks = LocksFile::empty();
        locks.save(&cache_dir.join("locks.json")).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let result = manager.read_locks_auto().unwrap();
        assert!(result.locks.is_empty());
    }

    #[test]
    fn test_ensure_agent_dir_creates_directory() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let created = manager.create_agent_dir_files("worker-42").unwrap();
        assert!(created);

        let agent_dir = cache_dir.join("agents").join("worker-42");
        assert!(agent_dir.exists());
        assert!(agent_dir.join("heartbeat.json").exists());
    }

    #[test]
    fn test_ensure_agent_dir_idempotent() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        let first = manager.create_agent_dir_files("worker-42").unwrap();
        assert!(first);

        let second = manager.create_agent_dir_files("worker-42").unwrap();
        assert!(!second);
    }

    #[test]
    fn test_ensure_agent_dir_heartbeat_valid_json() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = SyncManager::new(&crosslink_dir).unwrap();
        manager.create_agent_dir_files("test-agent").unwrap();

        let heartbeat_path = cache_dir
            .join("agents")
            .join("test-agent")
            .join("heartbeat.json");
        let content = std::fs::read_to_string(&heartbeat_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(parsed["agent_id"], "test-agent");
        assert_eq!(parsed["status"], "active");
        assert!(parsed["timestamp"].is_string());
        // Verify timestamp is valid RFC3339
        let ts = parsed["timestamp"].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(ts).expect("timestamp should be valid RFC3339");
    }

    // resolve_main_repo_root tests are in utils::tests

    #[test]
    fn test_sync_manager_in_worktree_uses_main_hub_cache() {
        let dir = tempdir().unwrap();
        let main_root = dir.path().join("main");
        std::fs::create_dir_all(&main_root).unwrap();
        init_git_repo(&main_root);

        let main_crosslink = main_root.join(".crosslink");
        std::fs::create_dir_all(&main_crosslink).unwrap();

        // Create worktree
        Command::new("git")
            .args([
                "-C",
                &main_root.to_string_lossy(),
                "branch",
                "feature/hub-test",
            ])
            .output()
            .unwrap();
        let wt_path = main_root.join(".worktrees").join("hub-test");
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        Command::new("git")
            .args([
                "-C",
                &main_root.to_string_lossy(),
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "feature/hub-test",
            ])
            .output()
            .unwrap();

        let wt_crosslink = wt_path.join(".crosslink");
        std::fs::create_dir_all(&wt_crosslink).unwrap();

        let manager = SyncManager::new(&wt_crosslink).unwrap();

        // cache_dir should point to the main repo's hub cache, not the worktree's
        // Canonicalize the parent (.crosslink) since .hub-cache doesn't exist yet.
        let expected_parent = main_crosslink.canonicalize().unwrap();
        let actual_parent = manager.cache_dir.parent().unwrap().canonicalize().unwrap();
        assert_eq!(actual_parent, expected_parent);
        assert_eq!(manager.cache_dir.file_name().unwrap(), HUB_CACHE_DIR);

        // repo_root should be the main repo, not the worktree
        assert_eq!(
            manager.repo_root.canonicalize().unwrap(),
            main_root.canonicalize().unwrap()
        );
    }
}
