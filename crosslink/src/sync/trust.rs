use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::core::SyncManager;
use super::SignatureVerification;
use crate::identity::{AgentConfig, AgentRole};
use crate::locks::Keyring;
use crate::signing;

/// Resolve the user's home directory from environment variables.
///
/// Uses `$HOME` on Unix and `$USERPROFILE` on Windows.
fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

/// Expand a leading `~/` or bare `~` in a path string against the user's home.
///
/// Returns the input unchanged if there's no tilde or home cannot be resolved.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return home_dir().map_or_else(
            || {
                tracing::warn!(
                    "tilde expansion failed: cannot determine home directory for '{}'",
                    path
                );
                PathBuf::from(path)
            },
            |home| home.join(rest),
        );
    }
    if path == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(path));
    }
    PathBuf::from(path)
}

/// Resolve an agent's SSH private key path from the relative form stored in
/// `agent.json` (`ssh_key_path`).
///
/// GH#610: new agents store their key under the *main repo's*
/// `.crosslink/keys/` so it survives `git worktree remove` of a kickoff
/// agent worktree. Legacy agents (created before that change) have their
/// key inside the worktree's own `.crosslink/keys/`. Try the host path
/// first; fall back to the worktree-local path so existing deployments
/// keep signing until the legacy worktree is cleaned up.
pub(super) fn resolve_agent_key(worktree_crosslink_dir: &Path, rel_key: &str) -> PathBuf {
    let host = crate::signing::host_crosslink_dir(worktree_crosslink_dir);
    let host_path = host.join(rel_key);
    if host_path.exists() {
        return host_path;
    }
    worktree_crosslink_dir.join(rel_key)
}

/// Best guess whether a `user.signingkey` value is a filesystem path rather
/// than literal key material (e.g. an inline `ssh-ed25519 AAAA...` line).
///
/// Only paths need existence validation; literal key material has nothing to
/// check against the filesystem.
fn signingkey_value_is_path(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Literal SSH / PGP key material — not a path.
    if trimmed.starts_with("ssh-")
        || trimmed.starts_with("ecdsa-")
        || trimmed.starts_with("sk-")
        || trimmed.starts_with("-----BEGIN")
    {
        return false;
    }
    true
}

impl SyncManager {
    /// Configure SSH signing in the hub cache worktree.
    ///
    /// **Role-aware signing** (#718):
    ///
    /// - Main / driver workspaces (`agent.json.role == Driver` or
    ///   no `agent.json`): sign hub commits with the DRIVER's SSH
    ///   signing key (from the main repo's `user.signingkey`).
    ///   That's the key the human registered on their GitHub
    ///   account; commits verify end-to-end.
    /// - Subagent worktrees (`agent.json.role == Agent`): sign with
    ///   the agent's SSH key. That's the kickoff/swarm-scoped
    ///   identity; commits attribute to the agent and verify
    ///   locally via `allowed_signers`. GitHub shows them
    ///   unverified unless the agent pub key has been registered
    ///   there too.
    ///
    /// The agent's identity always lives in `agent.json` (who
    /// initiated the action). Only the SIGNATURE bytes differ by
    /// role — and critically, we never sign a driver-workspace's
    /// commit with an agent key that GitHub doesn't know.
    ///
    /// # Errors
    /// Returns an error if configuring git signing fails.
    pub fn configure_signing(&self, crosslink_dir: &Path) -> Result<()> {
        if !self.cache_dir.exists() {
            return Ok(());
        }

        // Ensure allowed_signers file always exists so git's
        // verify-commit correctly classifies signed commits,
        // whatever key ends up signing.
        let allowed_signers = self.cache_dir.join("trust").join("allowed_signers");
        if !allowed_signers.exists() {
            signing::AllowedSigners::default().save(&allowed_signers)?;
        }

        // Determine whether this is a driver-owned workspace or an
        // agent's subagent worktree. The role lives in agent.json;
        // if agent.json is missing we default to driver (the main-
        // repo case is the common one).
        let is_agent_worktree =
            AgentConfig::load(crosslink_dir)?.is_some_and(|c| matches!(c.role, AgentRole::Agent));

        if is_agent_worktree {
            // Subagent worktree — sign with the agent's key so the
            // attribution is distinct.
            if let Some(agent) = AgentConfig::load(crosslink_dir)? {
                if let (Some(rel_key), Some(_)) = (&agent.ssh_key_path, &agent.ssh_fingerprint) {
                    let private_key = resolve_agent_key(&self.crosslink_dir, rel_key);
                    if private_key.exists() {
                        signing::configure_git_ssh_signing(
                            &self.cache_dir,
                            &private_key,
                            Some(&allowed_signers),
                        )?;
                        register_active_key_as_trusted(
                            &self.cache_dir,
                            crosslink_dir,
                            &private_key,
                            &allowed_signers,
                        )?;
                        return Ok(());
                    }
                }
            }
            // Agent worktree but key missing — fall through to
            // driver key as a recovery path; better a verified
            // driver commit than an unsigned one.
        }

        // Driver-owned workspace — or agent worktree missing its
        // key. Prefer the driver's SSH signing key.
        if let Some(driver_key) = self.driver_signing_key() {
            if driver_key.exists() {
                signing::configure_git_ssh_signing(
                    &self.cache_dir,
                    &driver_key,
                    Some(&allowed_signers),
                )?;
                register_active_key_as_trusted(
                    &self.cache_dir,
                    crosslink_dir,
                    &driver_key,
                    &allowed_signers,
                )?;
                return Ok(());
            }
            tracing::warn!(
                "driver signing key configured but not found at {}; falling back to agent key",
                driver_key.display()
            );
        }

        // Driver key unavailable — fall back to whatever key agent.json
        // knows about, so hub commits still sign even when the operator
        // hasn't set `user.signingkey` in the main repo.
        if let Some(agent) = AgentConfig::load(crosslink_dir)? {
            if let (Some(rel_key), Some(_)) = (&agent.ssh_key_path, &agent.ssh_fingerprint) {
                let private_key = resolve_agent_key(&self.crosslink_dir, rel_key);
                if private_key.exists() {
                    signing::configure_git_ssh_signing(
                        &self.cache_dir,
                        &private_key,
                        Some(&allowed_signers),
                    )?;
                    register_active_key_as_trusted(
                        &self.cache_dir,
                        crosslink_dir,
                        &private_key,
                        &allowed_signers,
                    )?;
                    return Ok(());
                }
            }
        }

        // Nothing usable — disable signing so commits still land.
        tracing::warn!(
            "no usable signing key for {} workspace; disabling hub-commit signing",
            if is_agent_worktree { "agent" } else { "driver" }
        );
        signing::disable_git_signing(&self.cache_dir)?;
        Ok(())
    }

    /// Resolve the driver's SSH signing key path from the main repo's
    /// git config. Returns the expanded absolute path if
    /// `user.signingkey` points at an existing file; `None` if unset,
    /// empty, or unparsable.
    fn driver_signing_key(&self) -> Option<std::path::PathBuf> {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["config", "user.signingkey"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if raw.is_empty() {
            return None;
        }
        Some(expand_tilde(&raw))
    }

    /// Fall back to the driver's signing key when the agent key is missing.
    ///
    /// Reads `user.signingkey` from the main repo's git config. If found and
    /// the key file exists, configures the hub cache worktree to use it.
    /// If no driver key is found, disables signing so commits can proceed
    /// unsigned rather than failing fatally.
    pub(super) fn fallback_to_driver_signing(&self) -> Result<()> {
        // Try to read the driver's signing key from the main repo config
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(["config", "user.signingkey"])
            .output();

        let driver_key = output.ok().and_then(|o| {
            if o.status.success() {
                let key = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if key.is_empty() {
                    None
                } else {
                    Some(key)
                }
            } else {
                None
            }
        });

        if let Some(key_path) = driver_key {
            let expanded = expand_tilde(&key_path);

            if expanded.exists() {
                tracing::info!(
                    "agent key missing, falling back to driver signing key: {}",
                    expanded.display()
                );
                signing::configure_git_ssh_signing(&self.cache_dir, &expanded, None)?;
            } else {
                tracing::warn!(
                    "agent key missing and driver key not found at {}, disabling signing",
                    expanded.display()
                );
                signing::disable_git_signing(&self.cache_dir)?;
            }
        } else {
            tracing::warn!(
                "agent key missing and no driver signing key configured, disabling signing"
            );
            signing::disable_git_signing(&self.cache_dir)?;
        }

        Ok(())
    }

    /// Self-heal a stale `user.signingkey` in the hub-cache worktree config.
    ///
    /// Reads the effective `user.signingkey` for `cache_dir`. If the value is
    /// a filesystem path that no longer exists (typical when an agent worktree
    /// containing the key was deleted — see GH #565), delegates to
    /// [`Self::fallback_to_driver_signing`] to rewrite `config.worktree` with
    /// the driver key (or disable signing if the driver has no key either).
    ///
    /// Returns `Ok(true)` if a repair was performed, `Ok(false)` when nothing
    /// needed repairing. Designed to be called as a best-effort preamble to
    /// every commit in the cache worktree: cheap on the happy path (one
    /// `git config` read plus a single `Path::exists()` check), self-healing
    /// on the sad path so future syncs succeed without manual intervention.
    ///
    /// Skips validation for literal key material (`ssh-ed25519 AAAA...`,
    /// `-----BEGIN ...`) that git accepts inline alongside file paths.
    ///
    /// # Errors
    ///
    /// Returns an error only if the fallback rewrite itself fails. A read
    /// failure (no signing configured, git missing, etc.) is treated as
    /// "nothing to repair" and returns `Ok(false)`.
    pub fn repair_stale_signingkey(&self) -> Result<bool> {
        if !self.cache_dir.exists() {
            return Ok(false);
        }

        let output = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["config", "user.signingkey"])
            .output();

        let Ok(output) = output else { return Ok(false) };
        if !output.status.success() {
            return Ok(false); // No signingkey configured — nothing to repair.
        }

        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !signingkey_value_is_path(&value) {
            return Ok(false); // Literal key material or empty — not our problem.
        }

        let expanded = expand_tilde(&value);
        if expanded.exists() {
            return Ok(false); // Path still valid — no repair needed.
        }

        tracing::warn!(
            "hub-cache user.signingkey points at missing file '{}' \
             (agent worktree likely deleted) — repairing (GH #565)",
            expanded.display()
        );
        self.fallback_to_driver_signing()?;
        Ok(true)
    }

    /// Ensure the agent's public key is published to `trust/keys/` on the hub.
    ///
    /// During `agent init`, key publishing is skipped if the hub cache doesn't
    /// exist yet. This method re-checks and publishes the key if needed, using
    /// an unsigned commit to avoid the chicken-and-egg problem where signing
    /// must be configured before the key can be published.
    ///
    /// Safe to call multiple times — no-ops if the key is already published.
    ///
    /// # Accepted risk: unsigned key-publication commit
    ///
    /// The commit that publishes the agent's public key is intentionally
    /// unsigned (`commit.gpgsign=false`). This is a bootstrapping trade-off:
    /// the signing key cannot be verified until it is published, so the
    /// publication commit itself cannot be signed by the key it publishes.
    /// Subsequent commits from this agent will be signed normally. Auditors
    /// can verify the key-publication commit via the git history (the key
    /// file hash is deterministic given the public key content).
    ///
    /// # Errors
    ///
    /// Returns an error if loading agent config, writing the key file, or committing fails.
    pub fn ensure_agent_key_published(&self, crosslink_dir: &Path) -> Result<bool> {
        if !self.cache_dir.exists() {
            return Ok(false);
        }

        let Some(agent) = AgentConfig::load(crosslink_dir)? else {
            return Ok(false);
        };

        let Some(public_key) = agent.ssh_public_key.clone() else {
            return Ok(false);
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
        std::fs::write(&key_file, format!("{public_key}\n"))?;

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
                bail!("git commit for key publication failed: {stderr}");
            }
        }

        Ok(true)
    }

    /// Read the trust keyring from the cache (deprecated — use `read_allowed_signers`).
    ///
    /// # Errors
    ///
    /// Returns an error if the keyring file exists but cannot be parsed.
    pub fn read_keyring(&self) -> Result<Option<Keyring>> {
        let path = self.cache_dir.join("trust").join("keyring.json");
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(Keyring::load(&path)?))
    }

    /// Read the SSH `allowed_signers` trust store from the cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the allowed signers file cannot be read or parsed.
    pub fn read_allowed_signers(&self) -> Result<signing::AllowedSigners> {
        let path = self.cache_dir.join("trust").join("allowed_signers");
        signing::AllowedSigners::load(&path)
    }

    /// Verify a single commit's signature, returning a `SignatureVerification`.
    ///
    /// Shared implementation used by both `verify_recent_commits` and
    /// `verify_locks_signature` to avoid duplicated verification logic.
    fn verify_commit_signature(&self, commit: &str) -> Result<SignatureVerification> {
        let verify = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["verify-commit", "--raw", commit])
            .output()
            .context("Failed to run git verify-commit")?;

        let stdout = String::from_utf8_lossy(&verify.stdout);
        let stderr = String::from_utf8_lossy(&verify.stderr);
        // Combine stdout+stderr: macOS ssh-keygen emits "Good" on stdout
        let combined = format!("{stdout}\n{stderr}");

        if verify.status.success() {
            let parsed = signing::parse_verify_output(&combined);
            let principal = parsed.as_ref().and_then(|(p, _)| p.clone());
            let fingerprint = parsed.map(|(_, f)| f);
            Ok(SignatureVerification::Valid {
                commit: commit.to_string(),
                fingerprint,
                principal,
            })
        } else if stderr.contains("NODATA")
            || stderr.contains("no signature")
            || stderr.is_empty()
            || stderr.contains("allowedSignersFile needs to be configured")
        {
            Ok(SignatureVerification::Unsigned {
                commit: commit.to_string(),
            })
        } else {
            Ok(SignatureVerification::Invalid {
                commit: commit.to_string(),
                reason: stderr.to_string(),
            })
        }
    }

    /// Verify the last N commits on the hub branch.
    ///
    /// Returns a list of `(commit_hash, verification_result)`.
    ///
    /// # Errors
    ///
    /// Returns an error if git log or signature verification commands fail.
    pub fn verify_recent_commits(
        &self,
        count: usize,
    ) -> Result<Vec<(String, SignatureVerification)>> {
        let output = self.git_in_cache(&["log", &format!("-{count}"), "--format=%H"])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let commits: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

        let mut results = Vec::new();
        for commit in commits {
            let verification = self.verify_commit_signature(commit)?;
            results.push((commit.to_string(), verification));
        }

        Ok(results)
    }

    /// Verify per-entry signatures on comments in cached issue files.
    ///
    /// Reads all issues from the cache, checks any comments that have
    /// `signed_by` + `signature` fields against the `allowed_signers` store
    /// using `signing::verify_content()`.
    ///
    /// Returns `(verified, failed, unsigned)` counts.
    ///
    /// # Errors
    ///
    /// Returns an error if reading issue files or the allowed signers store fails.
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
                        // Try author-based principal first (original agent signature)
                        let principal = format!("{}@crosslink", &comment.author);
                        let original_ok = signing::verify_content(
                            &allowed_signers_path,
                            &principal,
                            "crosslink-comment",
                            &canonical,
                            sig,
                        );
                        if matches!(original_ok, Ok(true)) {
                            verified += 1;
                            continue;
                        }
                        // Fallback: try backfill principal with backfill namespace.
                        // Human-attested entries use a different namespace so they
                        // can be verified without being confused with agent sigs.
                        match signing::verify_content(
                            &allowed_signers_path,
                            "backfill@crosslink",
                            "crosslink-backfill",
                            &canonical,
                            sig,
                        ) {
                            Ok(true) => {
                                verified += 1;
                            }
                            Ok(false) => {
                                tracing::warn!(
                                    "signature verification failed for comment {} by '{}' (signer: {})",
                                    comment.id, comment.author, fingerprint
                                );
                                failed += 1;
                            }
                            Err(e) => {
                                if allowed_signers_path.exists() {
                                    tracing::warn!(
                                        "signature verification error for comment {} by '{}': {}",
                                        comment.id,
                                        comment.author,
                                        e
                                    );
                                    failed += 1;
                                } else {
                                    let _ = fingerprint;
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
    ///
    /// # Errors
    ///
    /// Returns an error if git log or signature verification commands fail.
    pub fn verify_locks_signature(&self) -> Result<SignatureVerification> {
        // Get the commit that last touched locks.json
        let output = self.git_in_cache(&["log", "-1", "--format=%H", "--", "locks.json"])?;
        let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();

        if commit.is_empty() {
            return Ok(SignatureVerification::NoCommits);
        }

        self.verify_commit_signature(&commit)
    }
}

/// Ensure the public key paired with `private_key_path` is registered in
/// `allowed_signers`. Idempotent: a no-op when the key is already trusted.
///
/// When a new entry is appended, the file is saved and an **unsigned**
/// commit is made in the cache worktree. The commit must be unsigned
/// because the freshly-registered key may not yet be present in any
/// pre-existing commit's view of `allowed_signers`, so signing this
/// commit with it would fail `git verify-commit` (the chicken-and-egg
/// `publish_agent_key` also navigates). Once this commit lands,
/// subsequent commits signed with the same key verify cleanly.
///
/// GH#585: before this call existed, only `crosslink trust approve
/// <agent>` ever wrote to `allowed_signers`. The driver's own signing
/// key (selected by `configure_signing`) was never registered, so every
/// signed hub commit out of a driver workspace failed verification.
///
/// GH#738: when this function actually adds a new entry while the hub is
/// still in `bootstrap.status = "pending"`, the registration *is* the
/// trust-establishment event — morally identical to running `crosslink
/// trust approve` on the workspace's own key — so we also flip bootstrap
/// to `"complete"` atomically in the same unsigned commit. Without this,
/// `trust pending` reports nothing pending (because the key is already
/// trusted by self-registration), and the bootstrap state would remain
/// "pending" forever, blocking signing enforcement.
///
/// Returns `Ok(true)` when an entry was added (and, by implication, when
/// bootstrap may have been completed). `Ok(false)` when the key was
/// already trusted under some principal.
fn register_active_key_as_trusted(
    cache_dir: &Path,
    crosslink_dir: &Path,
    private_key_path: &Path,
    allowed_signers_path: &Path,
) -> Result<bool> {
    use crate::signing::{AllowedSignerEntry, AllowedSigners};

    // Resolve the public-key companion file (SSH convention: <key>.pub).
    let public_key_path = with_pub_extension(private_key_path);
    let public_key = match crate::signing::read_public_key(&public_key_path) {
        Ok(k) => k,
        Err(e) => {
            tracing::debug!(
                "skipping allowed_signers self-registration: cannot read pubkey at {}: {e}",
                public_key_path.display()
            );
            return Ok(false);
        }
    };

    let mut signers = AllowedSigners::load(allowed_signers_path)?;
    if signers.contains_key(&public_key) {
        return Ok(false); // Already trusted under some principal — no-op.
    }

    // Pick a principal: prefer the agent.json identity for visibility,
    // fall back to a generic role+host label when none is configured.
    let principal = AgentConfig::load(crosslink_dir)?.map_or_else(
        || "driver@crosslink".to_string(),
        |c| format!("{}@crosslink", c.agent_id),
    );

    signers.add_entry(AllowedSignerEntry {
        principal: principal.clone(),
        public_key,
        metadata_comment: Some(format!(
            "self-registered as workspace signing key at {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        )),
    });
    signers.save(allowed_signers_path)?;

    // GH#738: If the hub is still in the bootstrap "pending" state, this
    // self-registration completes bootstrap. The flag file is staged
    // alongside allowed_signers in the same atomic commit below.
    let bootstrap_completed_now =
        if let Some(state) = super::bootstrap::read_bootstrap_state(cache_dir) {
            if state.status == "pending" {
                super::bootstrap::complete_bootstrap(cache_dir)?;
                true
            } else {
                false
            }
        } else {
            false
        };

    // Commit unsigned; best-effort. If the commit fails (e.g. nothing
    // staged because of a race), the on-disk file is still correct for
    // local verification, and the next push will pick up any residue.
    if let Err(e) = commit_allowed_signers_unsigned(cache_dir, &principal) {
        tracing::warn!(
            "registered '{principal}' in allowed_signers on disk but commit failed: {e} \
             (run `crosslink sync` to recover)"
        );
    } else if bootstrap_completed_now {
        tracing::info!("bootstrap completed: self-registered '{principal}' as trusted signer");
    }

    Ok(true)
}

/// Compute the conventional public-key path for an SSH private key
/// (`<path>` → `<path>.pub`), preserving any unusual filename shape.
fn with_pub_extension(private_key_path: &Path) -> PathBuf {
    let mut s = private_key_path.as_os_str().to_owned();
    s.push(".pub");
    PathBuf::from(s)
}

/// Stage `trust/allowed_signers` (plus `meta/bootstrap.json` when present,
/// to fold a bootstrap state-flip into the same commit; see GH#738) and
/// commit it without signing.
///
/// Used only by [`register_active_key_as_trusted`]. Unsigned commit is
/// required because the just-added key isn't yet visible in any earlier
/// commit's `allowed_signers` view — signing this commit would create
/// the very verify-commit failure the parent function is meant to fix.
fn commit_allowed_signers_unsigned(cache_dir: &Path, principal: &str) -> Result<()> {
    let run = |args: &[&str]| -> Result<()> {
        let output = Command::new("git")
            .current_dir(cache_dir)
            .args(args)
            .output()
            .with_context(|| format!("failed to spawn git {args:?}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // `nothing to commit` is benign: the file content matched a
            // previous staged state. Treat it as success.
            if !stderr.contains("nothing to commit") {
                bail!("git {args:?} failed: {}", stderr.trim());
            }
        }
        Ok(())
    };

    run(&["add", "trust/allowed_signers"])?;
    // GH#738: when bootstrap was just completed (file written by the
    // caller before this commit), fold the state-flip into the same
    // atomic commit. Best-effort — if the file is absent or unchanged,
    // `git add` is a no-op and the commit still succeeds.
    if cache_dir.join("meta").join("bootstrap.json").exists() {
        let _ = run(&["add", "meta/bootstrap.json"]);
    }
    run(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-m",
        &format!("trust: register signing key for '{principal}'"),
    ])?;
    Ok(())
}
