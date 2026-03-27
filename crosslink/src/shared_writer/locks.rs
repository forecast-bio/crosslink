//! V2 lock protocol: event-based lock claim, release, and steal.

use anyhow::{bail, Context, Result};

use super::core::{SharedWriter, LOCK_CONFIRM_TIMEOUT_SECS};

/// Result of a V2 lock claim attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockClaimResult {
    /// Lock successfully claimed.
    Claimed,
    /// Lock already held by this agent.
    AlreadyHeld,
    /// Another agent won the lock.
    Contended { winner_agent_id: String },
}

impl SharedWriter {
    /// Claim a lock on an issue using the V2 event-based protocol.
    ///
    /// 1. Check if already held by self -> AlreadyHeld
    /// 2. Emit LockClaimed event -> append to event log
    /// 3. Push event log (conflict-free per-agent file)
    /// 4. Compact with force=true
    /// 5. Stage + commit + push compaction output (rebase-retry)
    /// 6. Read materialized lock file
    /// 7. If winner is self -> Claimed; else -> emit LockReleased cleanup -> Contended
    pub fn claim_lock_v2(
        &self,
        issue_display_id: i64,
        branch: Option<&str>,
    ) -> Result<LockClaimResult> {
        // Check if already held
        if let Some(lock) = self.read_lock_v2(issue_display_id)? {
            if lock.agent_id == self.agent.agent_id {
                return Ok(LockClaimResult::AlreadyHeld);
            }
        }

        // Emit LockClaimed event, then compact+push with timeout guard.
        // Per design doc section 8: if compaction hasn't completed within 30s,
        // fail rather than treating a stale result as authoritative.
        let event = crate::events::Event::LockClaimed {
            issue_display_id,
            branch: branch.map(|s| s.to_string()),
        };
        let start = std::time::Instant::now();
        self.emit_compact_push(event, &format!("claim lock on #{}", issue_display_id))?;
        let elapsed = start.elapsed();
        if elapsed > std::time::Duration::from_secs(LOCK_CONFIRM_TIMEOUT_SECS) {
            bail!(
                "Lock confirmation timed out after {}s (threshold {}s) -- \
                 compaction result may be stale, not treating as authoritative",
                elapsed.as_secs(),
                LOCK_CONFIRM_TIMEOUT_SECS
            );
        }

        // Re-read materialized lock to see who won
        match self.read_lock_v2(issue_display_id)? {
            Some(lock) if lock.agent_id == self.agent.agent_id => Ok(LockClaimResult::Claimed),
            Some(lock) => {
                // We lost -- clean up by emitting LockReleased
                let release = crate::events::Event::LockReleased { issue_display_id };
                // We lost contention — emit release for our stale claim.
                // If push fails, compaction will resolve it (winner's claim wins).
                if let Err(e) = self.emit_compact_push(
                    release,
                    &format!("release lock on #{} (contention cleanup)", issue_display_id),
                ) {
                    tracing::info!("contention cleanup push deferred: {}", e);
                }
                Ok(LockClaimResult::Contended {
                    winner_agent_id: lock.agent_id,
                })
            }
            None => {
                // Lock wasn't materialized -- shouldn't happen, but treat as claimed
                Ok(LockClaimResult::Claimed)
            }
        }
    }

    /// Release a lock on an issue using the V2 event-based protocol.
    ///
    /// Returns Ok(true) if released, Ok(false) if not held.
    pub fn release_lock_v2(&self, issue_display_id: i64) -> Result<bool> {
        // Check if we actually hold it
        match self.read_lock_v2(issue_display_id)? {
            Some(lock) if lock.agent_id == self.agent.agent_id => {
                // We hold it -- release
                let event = crate::events::Event::LockReleased { issue_display_id };
                self.emit_compact_push(event, &format!("release lock on #{}", issue_display_id))?;
                Ok(true)
            }
            Some(_) => {
                // Held by someone else -- can't release
                Ok(false)
            }
            None => {
                // Not locked
                Ok(false)
            }
        }
    }

    /// Clear a stale agent's lock state: prune events, clear checkpoint,
    /// and remove the materialized lock file.
    ///
    /// Shared implementation used by both `steal_lock_v2` and
    /// `force_release_lock_v2` to avoid duplicating the cleanup sequence.
    fn clear_stale_lock_state(&self, issue_display_id: i64, stale_agent_id: &str) -> Result<()> {
        // Prune stale agent's compacted events so they don't replay
        crate::compaction::prune_events(&self.cache_dir, stale_agent_id)?;

        // Clear lock from checkpoint state
        let mut state = crate::checkpoint::read_checkpoint(&self.cache_dir)?;
        state.locks.remove(&issue_display_id);
        crate::checkpoint::write_checkpoint(&self.cache_dir, &state)?;

        // Remove materialized lock file
        let lock_path = self
            .cache_dir
            .join("locks")
            .join(format!("{}.json", issue_display_id));
        if lock_path.exists() {
            std::fs::remove_file(&lock_path)?;
        }

        Ok(())
    }

    /// Steal a lock from a stale agent using the V2 event-based protocol.
    ///
    /// Prunes the stale agent's events, clears checkpoint lock state,
    /// then claims normally.
    pub fn steal_lock_v2(
        &self,
        issue_display_id: i64,
        stale_agent_id: &str,
        branch: Option<&str>,
    ) -> Result<LockClaimResult> {
        self.clear_stale_lock_state(issue_display_id, stale_agent_id)?;
        self.claim_lock_v2(issue_display_id, branch)
    }

    /// Force-release a stale lock without re-claiming it.
    ///
    /// Used by `integrity locks --repair` to actually free stale locks.
    /// Unlike `steal_lock_v2`, this does NOT call `claim_lock_v2` afterwards.
    pub fn force_release_lock_v2(
        &self,
        issue_display_id: i64,
        stale_agent_id: &str,
    ) -> Result<bool> {
        self.clear_stale_lock_state(issue_display_id, stale_agent_id)?;

        // Emit a release event and push
        let event = crate::events::Event::LockReleased { issue_display_id };
        self.emit_compact_push(
            event,
            &format!("force-release stale lock on #{}", issue_display_id),
        )?;

        Ok(true)
    }

    /// Read a V2 lock file for a specific issue.
    ///
    /// Returns None if the lock file doesn't exist.
    pub fn read_lock_v2(
        &self,
        issue_display_id: i64,
    ) -> Result<Option<crate::issue_file::LockFileV2>> {
        let lock_path = self
            .cache_dir
            .join("locks")
            .join(format!("{}.json", issue_display_id));
        if !lock_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&lock_path)
            .with_context(|| format!("Failed to read lock file: {}", lock_path.display()))?;
        let lock: crate::issue_file::LockFileV2 = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse lock file: {}", lock_path.display()))?;
        Ok(Some(lock))
    }
}
