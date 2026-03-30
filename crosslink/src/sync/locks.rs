use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};

use super::core::SyncManager;
use super::HUB_BRANCH;
use crate::identity::AgentConfig;
use crate::locks::LocksFile;

/// Parse a V2 agent heartbeat file and return the heartbeat timestamp.
///
/// Reads `agents/{agent_id}/heartbeat.json` and extracts the `timestamp`
/// field (RFC 3339). Returns `None` if the file doesn't exist, is
/// unreadable, contains invalid JSON, or has no parseable timestamp.
fn parse_v2_heartbeat_timestamp(
    cache_dir: &std::path::Path,
    agent_id: &str,
) -> Option<DateTime<Utc>> {
    let heartbeat_path = cache_dir
        .join("agents")
        .join(agent_id)
        .join("heartbeat.json");
    let content = std::fs::read_to_string(&heartbeat_path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    let ts = val.get("timestamp")?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Whether a lock operation should acquire normally or steal from another agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Normal acquisition — fail if another agent holds the lock.
    Normal,
    /// Steal the lock from the current holder.
    Steal,
}

impl SyncManager {
    /// Read the current locks file from the cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the locks file exists but cannot be read or parsed.
    pub fn read_locks(&self) -> Result<LocksFile> {
        let path = self.cache_dir.join("locks.json");
        if !path.exists() {
            return Ok(LocksFile::empty());
        }
        LocksFile::load(&path)
    }

    /// Read locks from V2 per-issue lock files at `locks/*.json`.
    ///
    /// Converts to `LocksFile` format for backward compatibility with existing code.
    ///
    /// # Errors
    ///
    /// Returns an error if the locks directory cannot be read or any lock file is malformed.
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
            locks.insert(lock_v2.issue_id, lock);
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
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying lock files cannot be read or parsed.
    pub fn read_locks_auto(&self) -> Result<LocksFile> {
        let meta_dir = self.cache_dir.join("meta");
        let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
        if version >= 2 {
            self.read_locks_v2()
        } else {
            self.read_locks()
        }
    }

    /// Claim a lock on an issue for the given agent.
    ///
    /// Writes the lock to `locks.json`, commits, and pushes with retry.
    /// After a push conflict, re-reads locks to verify another agent didn't
    /// claim the same lock during the race window.
    /// Returns `Ok(true)` if newly claimed, `Ok(false)` if already held by self.
    /// Fails if locked by another agent (unless `mode` is `LockMode::Steal`).
    ///
    /// # Errors
    ///
    /// Returns an error if the issue is locked by another agent (in `Normal` mode),
    /// or if reading/writing locks or pushing fails after retries.
    pub fn claim_lock(
        &self,
        agent: &AgentConfig,
        issue_id: i64,
        branch: Option<&str>,
        mode: LockMode,
    ) -> Result<bool> {
        if self.is_v2_layout() {
            tracing::warn!("claim_lock called on V2 hub — prefer SharedWriter::claim_lock_v2");
        }
        // Retry loop: re-check lock ownership after push conflicts
        for attempt in 0..3 {
            let mut locks = self.read_locks()?;

            // Check existing lock
            if let Some(existing) = locks.get_lock(issue_id) {
                if existing.agent_id == agent.agent_id {
                    return Ok(false); // Already held by self
                }
                if mode == LockMode::Normal {
                    bail!(
                        "Issue {} is locked by '{}' (claimed {}). \
                         Use 'crosslink locks steal {}' if the lock is stale.",
                        crate::utils::format_issue_id(issue_id),
                        existing.agent_id,
                        existing.claimed_at.format("%Y-%m-%d %H:%M"),
                        issue_id
                    );
                }
                // LockMode::Steal: take the lock from the current holder
            }

            let lock = crate::locks::Lock {
                agent_id: agent.agent_id.clone(),
                branch: branch.map(std::string::ToString::to_string),
                claimed_at: Utc::now(),
                signed_by: agent
                    .ssh_fingerprint
                    .clone()
                    .unwrap_or_else(|| agent.agent_id.clone()),
            };

            locks.locks.insert(issue_id, lock);
            locks.save(&self.cache_dir.join("locks.json"))?;

            match self
                .commit_and_push_locks(&format!("{}: claim lock on #{}", agent.agent_id, issue_id))
            {
                Ok(()) => {
                    // Verify our claim survived any rebase during push (#458).
                    // If overwritten, fall through to retry instead of bailing —
                    // the system should self-heal, not require manual intervention.
                    let verified = LocksFile::load(&self.cache_dir.join("locks.json"))?;
                    match verified.get_lock(issue_id) {
                        Some(lock) if lock.agent_id == agent.agent_id => {
                            return Ok(true);
                        }
                        Some(lock) => {
                            tracing::warn!(
                                "lock claim for issue {} was overwritten by '{}', retrying",
                                crate::utils::format_issue_id(issue_id),
                                lock.agent_id
                            );
                            // Fall through to retry
                        }
                        None => {
                            tracing::warn!(
                                "lock claim for issue {} was lost during push, retrying",
                                crate::utils::format_issue_id(issue_id)
                            );
                            // Fall through to retry
                        }
                    }
                }
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Push failed after") && attempt < 2 {
                        // Pull to sync before retry (#473). If pull fails,
                        // health check and retry pull — don't push stale state.
                        if self
                            .git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH])
                            .is_err()
                        {
                            self.hub_health_check();
                            self.git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH])?;
                        }
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        bail!("Failed to claim lock on #{issue_id} after 3 attempts due to concurrent updates")
    }

    /// Release a lock on an issue.
    ///
    /// Returns `Ok(true)` if released, `Ok(false)` if not locked.
    /// Fails if locked by a different agent (unless `mode` is `LockMode::Steal`).
    ///
    /// # Errors
    ///
    /// Returns an error if the lock is held by a different agent (in `Normal` mode),
    /// or if reading/writing locks or pushing fails.
    pub fn release_lock(&self, agent: &AgentConfig, issue_id: i64, mode: LockMode) -> Result<bool> {
        if self.is_v2_layout() {
            tracing::warn!("release_lock called on V2 hub — prefer SharedWriter::release_lock_v2");
        }
        let locks = self.read_locks()?;

        match locks.get_lock(issue_id) {
            None => return Ok(false),
            Some(existing) => {
                if existing.agent_id != agent.agent_id && mode == LockMode::Normal {
                    bail!(
                        "Issue {} is locked by '{}', not by you ('{}').",
                        crate::utils::format_issue_id(issue_id),
                        existing.agent_id,
                        agent.agent_id
                    );
                }
            }
        }

        // Retry release if push conflict re-introduces the lock (#458)
        let mut released = false;
        for release_attempt in 0..3 {
            let mut current_locks = self.read_locks()?;
            current_locks.locks.remove(&issue_id);
            current_locks.save(&self.cache_dir.join("locks.json"))?;

            self.commit_and_push_locks(&format!(
                "{}: release lock on #{}",
                agent.agent_id, issue_id
            ))?;

            // Verify the release survived any rebase during push
            let verified = LocksFile::load(&self.cache_dir.join("locks.json"))?;
            if verified.get_lock(issue_id).is_none() {
                released = true;
                break; // Release confirmed
            }
            if release_attempt < 2 {
                tracing::warn!(
                    "lock release for issue {} was undone during push, retrying",
                    crate::utils::format_issue_id(issue_id)
                );
            } else {
                tracing::warn!(
                    "lock release for issue {} failed after 3 attempts",
                    crate::utils::format_issue_id(issue_id)
                );
            }
        }

        Ok(released)
    }

    /// Find locks that have gone stale (no heartbeat within the timeout).
    ///
    /// Auto-dispatches based on hub layout version:
    /// - V2: uses per-agent heartbeat timestamps at `agents/{id}/heartbeat.json`
    ///   with the same configurable `stale_lock_timeout_minutes` as V1.
    /// - V1: uses the legacy `heartbeats/` directory with `stale_lock_timeout_minutes`
    ///
    /// # Errors
    ///
    /// Returns an error if locks or heartbeats cannot be read.
    pub fn find_stale_locks(&self) -> Result<Vec<(i64, String)>> {
        if self.is_v2_layout() {
            // Use the configurable timeout from locks settings, consistent with V1
            let locks = self.read_locks_auto()?;
            let timeout =
                chrono::Duration::minutes(locks.settings.stale_lock_timeout_minutes.cast_signed());
            return self.find_stale_locks_v2(timeout);
        }

        let locks = self.read_locks_auto()?;
        let heartbeats = self.read_heartbeats()?;
        let timeout =
            chrono::Duration::minutes(locks.settings.stale_lock_timeout_minutes.cast_signed());
        let now = Utc::now();

        let mut stale = Vec::new();
        for (issue_id, lock) in &locks.locks {
            let has_fresh_heartbeat = heartbeats.iter().any(|hb| {
                hb.agent_id == lock.agent_id
                    && now
                        .signed_duration_since(hb.last_heartbeat)
                        .max(chrono::Duration::zero())
                        < timeout
            });
            if !has_fresh_heartbeat {
                stale.push((*issue_id, lock.agent_id.clone()));
            }
        }
        Ok(stale)
    }

    /// Find stale locks using agent heartbeat timestamps (V2 layout).
    ///
    /// A lock is considered stale if the holding agent's heartbeat is older than
    /// `threshold`, or if no heartbeat file exists.
    ///
    /// # Errors
    ///
    /// Returns an error if V2 lock files cannot be read.
    pub fn find_stale_locks_v2(&self, threshold: chrono::Duration) -> Result<Vec<(i64, String)>> {
        let locks = self.read_locks_v2()?;
        let now = Utc::now();
        let mut stale = Vec::new();

        for (issue_id, lock) in &locks.locks {
            let is_stale = parse_v2_heartbeat_timestamp(&self.cache_dir, &lock.agent_id)
                .is_none_or(|heartbeat_time| {
                    let age = now
                        .signed_duration_since(heartbeat_time)
                        .max(chrono::Duration::zero());
                    age > threshold
                });

            if is_stale {
                stale.push((*issue_id, lock.agent_id.clone()));
            }
        }

        Ok(stale)
    }

    /// Find stale locks with their age in minutes.
    ///
    /// Returns `(issue_id, agent_id, stale_minutes)` for each stale lock.
    /// Auto-dispatches based on hub layout version.
    ///
    /// # Errors
    ///
    /// Returns an error if locks or heartbeats cannot be read.
    pub fn find_stale_locks_with_age(&self) -> Result<Vec<(i64, String, u64)>> {
        if self.is_v2_layout() {
            return self.find_stale_locks_with_age_v2();
        }

        let locks = self.read_locks_auto()?;
        let heartbeats = self.read_heartbeats()?;
        let timeout =
            chrono::Duration::minutes(locks.settings.stale_lock_timeout_minutes.cast_signed());
        let now = Utc::now();

        let mut stale = Vec::new();
        for (issue_id, lock) in &locks.locks {
            let latest_heartbeat = heartbeats
                .iter()
                .filter(|hb| hb.agent_id == lock.agent_id)
                .map(|hb| hb.last_heartbeat)
                .max();

            let age = latest_heartbeat.map_or_else(
                || {
                    now.signed_duration_since(lock.claimed_at)
                        .max(chrono::Duration::zero())
                },
                |hb_time| {
                    now.signed_duration_since(hb_time)
                        .max(chrono::Duration::zero())
                },
            );

            if age >= timeout {
                stale.push((
                    *issue_id,
                    lock.agent_id.clone(),
                    age.num_minutes().cast_unsigned(),
                ));
            }
        }
        Ok(stale)
    }

    fn find_stale_locks_with_age_v2(&self) -> Result<Vec<(i64, String, u64)>> {
        let locks = self.read_locks_v2()?;
        let now = Utc::now();
        // Use configurable timeout from locks settings, consistent with V1
        let all_locks = self.read_locks_auto()?;
        let threshold =
            chrono::Duration::minutes(all_locks.settings.stale_lock_timeout_minutes.cast_signed());
        let mut stale = Vec::new();

        for (issue_id, lock) in &locks.locks {
            let age_minutes = parse_v2_heartbeat_timestamp(&self.cache_dir, &lock.agent_id).map_or(
                Some(u64::MAX),
                |hb_time| {
                    let age = now
                        .signed_duration_since(hb_time)
                        .max(chrono::Duration::zero());
                    if age > threshold {
                        Some(age.num_minutes().cast_unsigned())
                    } else {
                        None
                    }
                },
            );

            if let Some(mins) = age_minutes {
                stale.push((*issue_id, lock.agent_id.clone(), mins));
            }
        }
        Ok(stale)
    }
}
