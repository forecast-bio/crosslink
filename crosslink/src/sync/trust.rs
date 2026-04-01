use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::core::SyncManager;
use super::SignatureVerification;
use crate::identity::AgentConfig;
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

impl SyncManager {
    /// Configure SSH signing in the hub cache worktree.
    ///
    /// If the agent has an SSH key, sets `gpg.format=ssh`, `user.signingkey`,
    /// and `commit.gpgsign=true` in the cache worktree's local git config.
    /// This makes all subsequent commits on the hub branch automatically signed.
    ///
    /// # Errors
    ///
    /// Returns an error if loading agent config or configuring git signing fails.
    pub fn configure_signing(&self, crosslink_dir: &Path) -> Result<()> {
        if !self.cache_dir.exists() {
            return Ok(());
        }

        let Some(agent) = AgentConfig::load(crosslink_dir)? else {
            return Ok(());
        };

        let (Some(rel_key), Some(_fingerprint)) = (&agent.ssh_key_path, &agent.ssh_fingerprint)
        else {
            return Ok(());
        };
        let rel_key = rel_key.clone();

        // Resolve private key path (relative to .crosslink/)
        let private_key = self.crosslink_dir.join(&rel_key);
        if !private_key.exists() {
            // Agent key is gone (e.g. worktree cleaned up). Fall back to the
            // driver's signing key so hub commits keep working (#506).
            return self.fallback_to_driver_signing();
        }

        // Ensure allowed_signers file always exists so git's verify-commit
        // correctly classifies signed commits. Without this, verify-commit
        // reports "allowedSignersFile needs to be configured" which maps
        // to Unsigned instead of Invalid (untrusted signer).
        let allowed_signers = self.cache_dir.join("trust").join("allowed_signers");
        if !allowed_signers.exists() {
            signing::AllowedSigners::default().save(&allowed_signers)?;
        }

        signing::configure_git_ssh_signing(&self.cache_dir, &private_key, Some(&allowed_signers))?;

        Ok(())
    }

    /// Fall back to the driver's signing key when the agent key is missing.
    ///
    /// Reads `user.signingkey` from the main repo's git config. If found and
    /// the key file exists, configures the hub cache worktree to use it.
    /// If no driver key is found, disables signing so commits can proceed
    /// unsigned rather than failing fatally.
    fn fallback_to_driver_signing(&self) -> Result<()> {
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
            // Expand tilde to home directory. Handle both "~/" and bare "~".
            // Uses $HOME (Unix) / $USERPROFILE (Windows) directly, same
            // approach as signing::dirs_next().
            let expanded = key_path.strip_prefix("~/").map_or_else(
                || {
                    if key_path == "~" {
                        home_dir().unwrap_or_else(|| std::path::PathBuf::from(&key_path))
                    } else {
                        std::path::PathBuf::from(&key_path)
                    }
                },
                |rest| {
                    home_dir().map_or_else(
                        || {
                            tracing::warn!(
                                "tilde expansion failed: cannot determine home directory for '{}'",
                                key_path
                            );
                            std::path::PathBuf::from(&key_path)
                        },
                        |home| home.join(rest),
                    )
                },
            );

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
                                tracing::warn!(
                                    "signature verification failed for comment {} by '{}' (signer: {})",
                                    comment.id, comment.author, fingerprint
                                );
                                failed += 1;
                            }
                            Err(e) => {
                                // Verification unavailable (no allowed_signers, no ssh-keygen)
                                // Treat as unverifiable but not failed
                                if allowed_signers_path.exists() {
                                    tracing::warn!(
                                        "signature verification error for comment {} by '{}': {}",
                                        comment.id,
                                        comment.author,
                                        e
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
