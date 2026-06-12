use anyhow::Result;
use std::path::Path;

use crate::db::Database;

/// `crosslink compact` — run event compaction manually.
pub fn run(crosslink_dir: &Path, db: &Database, force: bool) -> Result<()> {
    let sync = crate::sync::SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;
    let cache_dir = sync.cache_path().to_path_buf();

    // Load agent config for agent_id
    let agent = crate::identity::AgentConfig::load(crosslink_dir)?
        .ok_or_else(|| anyhow::anyhow!("No agent configured. Run 'crosslink agent init' first."))?;

    // Acquire the hub write lock before compaction and hold it through
    // hydration — prevents a concurrent write_commit_push from racing
    // compaction's materialized-file writes (#750).
    let hub_lock = sync.acquire_lock()?;

    // V3: route to compact_v3 (checkpoint ref + own-ref prune, REQ-7/REQ-11)
    // and hydrate from the reduced state — the v2 compaction path requires the
    // worktree materialized files that v3 does not maintain.
    if sync.hub_mode().is_v3() {
        let remote = if sync.remote_exists() {
            Some(sync.remote())
        } else {
            None
        };
        let result = crate::hub_v3::compact_v3(&cache_dir, &agent.agent_id, &hub_lock, remote)?;
        println!("Compaction complete (v3).");
        if result.events_processed > 0 {
            println!(
                "  Events processed: {}, events pruned: {}, checkpoint pushed: {}",
                result.events_processed, result.events_pruned, result.checkpoint_pushed
            );
        } else {
            println!("  No new events to process.");
        }
        let source = crate::hub_source::RefHubSource::new(&cache_dir)?;
        let outcome = crate::compaction::reduce(&source)?;
        crate::hydration::hydrate_from_state(&outcome.state, db)?;
        return Ok(());
    }

    match crate::compaction::compact(&cache_dir, &agent.agent_id, force, &hub_lock)? {
        Some(result) => {
            println!("Compaction complete.");
            if result.events_processed > 0 {
                println!(
                    "  Events processed: {}, issues updated: {}, locks updated: {}",
                    result.events_processed, result.issues_materialized, result.locks_materialized
                );
            } else {
                println!("  No new events to process.");
            }
            if result.skew_warnings > 0 {
                tracing::warn!(
                    "{} event clock skew warning(s) detected during compaction",
                    result.skew_warnings
                );
            }
            if result.unsigned_warnings > 0 {
                tracing::warn!(
                    "{} unsigned event(s) detected during compaction",
                    result.unsigned_warnings
                );
            }
            if result.git_skew_violations > 0 {
                tracing::warn!(
                    "{} clock skew violation(s) detected (see checkpoint/skew_warnings.json)",
                    result.git_skew_violations
                );
                let violations =
                    crate::clock_skew::read_skew_violations(&cache_dir).unwrap_or_default();
                for v in &violations {
                    tracing::warn!(
                        "agent={}, skew={}s, event={}, event_ts={}, commit_ts={}",
                        v.agent_id,
                        v.skew_seconds,
                        v.event_description,
                        v.event_timestamp.to_rfc3339(),
                        v.commit_timestamp.to_rfc3339()
                    );
                }
            }
        }
        None => {
            println!("Compaction skipped: lease held by another agent. Use --force to override.");
        }
    }

    // Re-hydrate after compaction
    crate::hydration::hydrate_to_sqlite(&cache_dir, db)?;
    Ok(())
}
