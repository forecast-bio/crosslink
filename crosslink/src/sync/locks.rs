use anyhow::{Context, Result};
use chrono::Utc;

use super::core::SyncManager;
use crate::locks::LocksFile;

impl SyncManager {
    /// Read materialized lock state for the resolved hub mode.
    ///
    /// - v3: reads the LOCAL checkpoint ref's `state.json` directly (no full
    ///   reduce) so the per-tool-call hot path (`lock_check` via `PreToolUse`
    ///   hooks) stays cheap (see [`Self::read_locks_v3`]).
    /// - v2 (frozen / pre-migration hub): no live lock state — the v2 lock write
    ///   path is gone (#754), so this returns an empty [`LocksFile`]. Migration
    ///   reads locks from the compacted checkpoint, not from here.
    ///
    /// # Errors
    ///
    /// Returns an error only if the v3 checkpoint blob exists but does not parse.
    pub fn read_locks_auto(&self) -> Result<LocksFile> {
        if self.hub_mode.get().is_v3() {
            return self.read_locks_v3();
        }
        Ok(LocksFile::empty())
    }

    /// Read lock state from the LOCAL v3 checkpoint ref's `state.json`.
    ///
    /// This deliberately does NOT run a full reduction: it reads the most
    /// recently compacted checkpoint (`refs/heads/crosslink/checkpoint` -> `state.json`
    /// -> `state.locks`) and maps it into a [`LocksFile`]. The hot path here is
    /// `lock_check` (invoked per tool call by `PreToolUse` hooks), so a full
    /// `RefHubSource` reduce on every call would be too expensive.
    ///
    /// # Staleness window
    ///
    /// The returned locks reflect the last LOCAL compaction, not necessarily the
    /// latest events on every agent ref. A preceding `fetch` (which compacts)
    /// closes the window.
    ///
    /// # Errors
    ///
    /// Returns an error only if the checkpoint blob exists but does not parse.
    /// A missing checkpoint (fresh v3 hub) yields an empty [`LocksFile`].
    pub fn read_locks_v3(&self) -> Result<LocksFile> {
        let Some(tip) =
            crate::hub_v3::git_rev_parse_optional(&self.cache_dir, crate::hub_v3::CHECKPOINT_REF)?
        else {
            return Ok(LocksFile::empty());
        };
        let spec = format!("{tip}:state.json");
        let Some(bytes) = crate::hub_v3::git_cat_file_blob_optional(&self.cache_dir, &spec)? else {
            return Ok(LocksFile::empty());
        };
        let state = crate::checkpoint::CheckpointState::from_slice(&bytes)
            .context("failed to parse v3 checkpoint state.json for lock read")?;
        let mut file = LocksFile::empty();
        for (issue_id, entry) in state.locks {
            file.locks.insert(
                issue_id,
                crate::locks::Lock {
                    agent_id: entry.agent_id,
                    branch: entry.branch,
                    claimed_at: entry.claimed_at,
                    signed_by: String::new(),
                },
            );
        }
        Ok(file)
    }

    /// Find locks that have gone stale (no heartbeat within the timeout).
    ///
    /// v3-aware: lock state comes from the checkpoint and heartbeats from each
    /// agent ref's `heartbeat.json`. A frozen v2 hub has no live locks, so the
    /// stale set is empty.
    ///
    /// # Errors
    ///
    /// Returns an error if locks or heartbeats cannot be read.
    pub fn find_stale_locks(&self) -> Result<Vec<(i64, String)>> {
        let locks = self.read_locks_auto()?;
        if locks.locks.is_empty() {
            return Ok(Vec::new());
        }
        // Mode-aware heartbeats: v3 reads each agent ref's `heartbeat.json`.
        let heartbeats = self.read_heartbeats_auto()?;
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

    /// Find stale locks with their age in minutes.
    ///
    /// Returns `(issue_id, agent_id, stale_minutes)` for each stale lock.
    /// v3-aware (see [`Self::find_stale_locks`]).
    ///
    /// # Errors
    ///
    /// Returns an error if locks or heartbeats cannot be read.
    pub fn find_stale_locks_with_age(&self) -> Result<Vec<(i64, String, u64)>> {
        let locks = self.read_locks_auto()?;
        if locks.locks.is_empty() {
            return Ok(Vec::new());
        }
        let heartbeats = self.read_heartbeats_auto()?;
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
}
