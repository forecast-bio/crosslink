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

    match crate::compaction::compact(&cache_dir, &agent.agent_id, force)? {
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
                eprintln!(
                    "  Warning: {} event clock skew warning(s) detected during compaction",
                    result.skew_warnings
                );
            }
            if result.unsigned_warnings > 0 {
                eprintln!(
                    "  Warning: {} unsigned event(s) detected during compaction",
                    result.unsigned_warnings
                );
            }
            if result.git_skew_violations > 0 {
                eprintln!(
                    "  Warning: {} clock skew violation(s) detected (see checkpoint/skew_warnings.json)",
                    result.git_skew_violations
                );
                let violations =
                    crate::clock_skew::read_skew_violations(&cache_dir).unwrap_or_default();
                for v in &violations {
                    eprintln!(
                        "    - agent={}, skew={}s, event={}, event_ts={}, commit_ts={}",
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
