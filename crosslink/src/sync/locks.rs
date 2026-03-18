use anyhow::{bail, Context, Result};
use chrono::Utc;

use super::core::SyncManager;
use super::HUB_BRANCH;
use crate::identity::AgentConfig;
use crate::locks::LocksFile;

impl SyncManager {
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
                                        Err(_) => true, // Unparseable timestamp -> stale
                                    },
                                    None => true, // No timestamp field -> stale
                                }
                            }
                            Err(_) => true, // Invalid JSON -> stale
                        }
                    }
                    Err(_) => true, // Unreadable file -> stale
                }
            } else {
                true // No heartbeat file -> stale
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
}
