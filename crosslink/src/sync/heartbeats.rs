use anyhow::Result;
use chrono::Utc;

use super::core::SyncManager;
use crate::identity::AgentConfig;
use crate::locks::Heartbeat;

impl SyncManager {
    /// Write and push this agent's heartbeat.
    ///
    /// v3 is the only mode that writes (754b): the heartbeat is written to the
    /// agent's OWN ref (`refs/heads/crosslink/agents/<id>`, sibling-preserving,
    /// single-writer) and that ref is pushed. There is no worktree file and no
    /// v2 commit flow — the v2 `crosslink/hub` branch is frozen.
    ///
    /// On a v2-only / uninitialized hub there is nothing to write (the v2 write
    /// path is gone); the call is a no-op so callers on legacy hubs still
    /// succeed without error.
    ///
    /// # Errors
    ///
    /// Returns an error if the heartbeat cannot be written to or pushed from the
    /// agent ref.
    pub fn push_heartbeat(&self, agent: &AgentConfig, active_issue_id: Option<i64>) -> Result<()> {
        // Acquire the hub write lock to serialize with other cache mutations (#352).
        let _lock_guard = self.acquire_lock()?;

        if !self.hub_mode.get().is_v3() {
            // v2 hub is frozen — no heartbeat write path remains. (Inspection of
            // a pre-migration hub's last heartbeats still works via read.)
            return Ok(());
        }

        let heartbeat = Heartbeat {
            agent_id: agent.agent_id.clone(),
            last_heartbeat: Utc::now(),
            active_issue_id,
            machine_id: agent.machine_id.clone(),
        };

        // Write the heartbeat to the agent's OWN REF (sibling-preserving,
        // single-writer) and push the ref.
        crate::hub_v3::write_heartbeat_to_ref(&self.cache_dir, &agent.agent_id, &heartbeat)?;
        if self.remote_exists() {
            match crate::hub_v3::push_agent_ref(&self.cache_dir, &self.remote, &agent.agent_id)? {
                crate::hub_v3::PushOutcome::Pushed | crate::hub_v3::PushOutcome::NoRemote => {}
                other => {
                    tracing::warn!(
                        "v3 heartbeat push for '{}' did not complete: {other:?}",
                        agent.agent_id
                    );
                }
            }
        }
        Ok(())
    }

    /// Read heartbeats for the resolved hub mode.
    ///
    /// - v3: each agent ref's `heartbeat.json` (the single source — a v3 hub has
    ///   no worktree heartbeat files).
    /// - v2 (inspection of a frozen / pre-migration hub): the V2 layout
    ///   `agents/{id}/heartbeat.json` worktree files.
    ///
    /// The legacy V1 `heartbeats/*.json` directory is gone (754b): it was only
    /// ever written by the deleted v2 write path and is not part of the v3
    /// migration genesis.
    ///
    /// # Errors
    ///
    /// Returns an error if heartbeat files cannot be read.
    pub fn read_heartbeats_auto(&self) -> Result<Vec<Heartbeat>> {
        if self.hub_mode.get().is_v3() {
            return Ok(crate::hub_v3::read_heartbeats_from_refs(&self.cache_dir)?
                .into_iter()
                .map(|(_, hb)| hb)
                .collect());
        }
        self.read_heartbeats_v2()
    }

    /// Read heartbeats from the V2 layout (`agents/{id}/heartbeat.json`).
    ///
    /// Retained for inspecting a frozen / pre-migration v2 hub. V2 heartbeat
    /// files use `timestamp` (RFC 3339) instead of `last_heartbeat` and may lack
    /// `active_issue_id` / `machine_id`; this converts them to [`Heartbeat`].
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
            // Try native Heartbeat format first, then V2 JSON format.
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
}
