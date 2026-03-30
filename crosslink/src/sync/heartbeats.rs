use anyhow::{bail, Context, Result};
use chrono::Utc;

use super::core::SyncManager;
use super::HUB_BRANCH;
use crate::identity::AgentConfig;
use crate::locks::Heartbeat;

impl SyncManager {
    /// Write and optionally push a heartbeat file for this agent.
    ///
    /// Acquires the hub write lock to prevent races with concurrent git
    /// operations (fetch, `write_commit_push`) in the same cache worktree.
    ///
    /// # Errors
    ///
    /// Returns an error if the heartbeat file cannot be written or pushed.
    pub fn push_heartbeat(&self, agent: &AgentConfig, active_issue_id: Option<i64>) -> Result<()> {
        // Acquire the hub write lock to serialize with other cache mutations (#352)
        let _lock_guard = self.acquire_lock()?;

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
        self.git_in_cache(&["add", &format!("heartbeats/{filename}")])?;

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
                tracing::warn!("heartbeat push failed (offline), changes saved locally only");
                return Ok(());
            }
            // If push is rejected (conflict), clean dirty state and try pull+push once
            if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                // Bail if local has diverged too far — sign of a rebase loop
                self.check_divergence()?;

                self.clean_dirty_state()?;
                if self
                    .git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH])
                    .is_err()
                {
                    self.hub_health_check();
                    self.git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH])?;
                }
                if let Err(retry_err) = self.git_in_cache(&["push", &self.remote, HUB_BRANCH]) {
                    tracing::warn!(
                        "heartbeat push failed after retry (conflict), changes saved locally only: {}",
                        retry_err
                    );
                }
            }
        }

        Ok(())
    }

    /// Read all heartbeat files from the V1 cache (`heartbeats/` directory).
    ///
    /// # Errors
    ///
    /// Returns an error if the heartbeats directory cannot be read.
    pub fn read_heartbeats(&self) -> Result<Vec<Heartbeat>> {
        let dir = self.cache_dir.join("heartbeats");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut heartbeats = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "json") {
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
    ///
    /// # Errors
    ///
    /// Returns an error if the agents directory cannot be read.
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
            let Ok(content) = std::fs::read_to_string(&hb_path) else {
                continue;
            };
            // Try native Heartbeat format first, then V2 JSON format
            if let Ok(hb) = serde_json::from_str::<Heartbeat>(&content) {
                heartbeats.push(hb);
            } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                let Some(timestamp) = val
                    .get("timestamp")
                    .and_then(|t| t.as_str())
                    .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
                    .map(|dt| dt.with_timezone(&Utc))
                else {
                    tracing::warn!(
                        "corrupt or missing timestamp in heartbeat for agent '{}', skipping",
                        agent_id
                    );
                    continue;
                };
                let active_issue_id = val
                    .get("active_issue_id")
                    .and_then(serde_json::Value::as_i64);
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
    ///
    /// # Errors
    ///
    /// Returns an error if heartbeat files cannot be read.
    pub fn read_heartbeats_auto(&self) -> Result<Vec<Heartbeat>> {
        use std::collections::HashMap;

        let mut heartbeats = self.read_heartbeats()?;
        if self.is_v2_layout() {
            let v2 = self.read_heartbeats_v2()?;
            // Merge V2 heartbeats, preferring the one with the most recent timestamp
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

    /// Create the agent directory on the hub branch if it doesn't exist.
    ///
    /// Creates `agents/{agent_id}/heartbeat.json` with an initial heartbeat.
    /// Returns `Ok(true)` if the directory was created, `Ok(false)` if it already existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory or heartbeat file cannot be created.
    pub fn ensure_agent_dir(&self, agent_id: &str) -> Result<bool> {
        if !self.create_agent_dir_files(agent_id)? {
            return Ok(false);
        }

        // Stage and commit
        self.git_in_cache(&["add", &format!("agents/{agent_id}/heartbeat.json")])?;
        self.git_in_cache(&[
            "commit",
            "-m",
            &format!("bootstrap: initialize agent directory for {agent_id}"),
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
                            // Bail if local has diverged too far — sign of a rebase loop
                            self.check_divergence()?;
                            if self
                                .git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH])
                                .is_err()
                            {
                                self.hub_health_check();
                                self.git_in_cache(&["pull", "--rebase", &self.remote, HUB_BRANCH])?;
                            }
                            continue;
                        }
                        bail!("Push failed after 3 retries for agent dir {agent_id}");
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
    pub(super) fn create_agent_dir_files(&self, agent_id: &str) -> Result<bool> {
        let agents_dir = self.cache_dir.join("agents").join(agent_id);
        if agents_dir.exists() {
            return Ok(false);
        }

        std::fs::create_dir_all(&agents_dir)
            .with_context(|| format!("Failed to create agent directory for {agent_id}"))?;

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
}
