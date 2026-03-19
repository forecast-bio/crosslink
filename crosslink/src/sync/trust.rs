use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use super::core::SyncManager;
use super::SignatureVerification;
use crate::identity::AgentConfig;
use crate::locks::Keyring;
use crate::signing;

impl SyncManager {
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

            let stdout = String::from_utf8_lossy(&verify.stdout);
            let stderr = String::from_utf8_lossy(&verify.stderr);
            // Combine stdout+stderr: macOS ssh-keygen emits "Good" on stdout
            let combined = format!("{}\n{}", stdout, stderr);
            let verification = if verify.status.success() {
                let parsed = signing::parse_verify_output(&combined);
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

        let stdout = String::from_utf8_lossy(&verify.stdout);
        let stderr = String::from_utf8_lossy(&verify.stderr);
        // Combine stdout+stderr: macOS ssh-keygen emits "Good" on stdout
        let combined = format!("{}\n{}", stdout, stderr);

        if verify.status.success() {
            let parsed = signing::parse_verify_output(&combined);
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
}
