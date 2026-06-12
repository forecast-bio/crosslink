use anyhow::Result;
use std::path::Path;

use crate::db::Database;
use crate::hydration::hydrate_to_sqlite;
use crate::identity::AgentConfig;
use crate::shared_writer::SharedWriter;
use crate::sync::SyncManager;
use crate::utils::{format_issue_id, truncate};
use crate::LocksCommands;

pub fn run(command: LocksCommands, crosslink_dir: &Path, db: &Database, json: bool) -> Result<()> {
    match command {
        LocksCommands::List => list(crosslink_dir, db, json),
        LocksCommands::Check { id } => check(crosslink_dir, id),
        LocksCommands::Claim { id, branch } => claim(crosslink_dir, id, branch.as_deref()),
        LocksCommands::Release { id } => release(crosslink_dir, id),
        LocksCommands::Steal { id } => steal(crosslink_dir, id),
    }
}

/// `crosslink locks list` — show current lock state
pub fn list(crosslink_dir: &Path, db: &Database, json_output: bool) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    let locks_file = sync.read_locks_auto()?;

    if json_output {
        let json = serde_json::to_string_pretty(&locks_file)?;
        println!("{json}");
        return Ok(());
    }

    if locks_file.locks.is_empty() {
        println!("No active locks.");
        return Ok(());
    }

    let stale = sync.find_stale_locks()?;
    let stale_ids: Vec<i64> = stale.iter().map(|(id, _)| *id).collect();

    println!("Active locks:");
    for (&issue_id, lock) in &locks_file.locks {
        let title = db
            .get_issue(issue_id)?
            .map_or_else(|| "(unknown issue)".to_string(), |i| truncate(&i.title, 40));

        let stale_marker = if stale_ids.contains(&issue_id) {
            " [STALE]"
        } else {
            ""
        };

        println!(
            "  {:<5} {} -- claimed by {} on {}{}",
            format_issue_id(issue_id),
            title,
            lock.agent_id,
            lock.claimed_at.format("%Y-%m-%d %H:%M"),
            stale_marker
        );
        if let Some(branch) = &lock.branch {
            println!("         branch: {branch}");
        }
    }
    Ok(())
}

/// `crosslink locks check <id>` — check if an issue is available
pub fn check(crosslink_dir: &Path, issue_id: i64) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    let locks_file = sync.read_locks_auto()?;

    match locks_file.get_lock(issue_id) {
        Some(lock) => {
            println!(
                "Issue {} is locked by '{}' (claimed {})",
                format_issue_id(issue_id),
                lock.agent_id,
                lock.claimed_at.format("%Y-%m-%d %H:%M")
            );
            if let Some(branch) = &lock.branch {
                println!("  Branch: {branch}");
            }
            // Check if stale
            let stale = sync.find_stale_locks()?;
            if stale.iter().any(|(id, _)| *id == issue_id) {
                println!("  Warning: this lock appears STALE (no recent heartbeat)");
            }
        }
        None => {
            println!(
                "Issue {} is not locked. Available for claiming.",
                format_issue_id(issue_id)
            );
        }
    }
    Ok(())
}

/// `crosslink locks claim <id>` — claim a lock on an issue
pub fn claim(crosslink_dir: &Path, issue_id: i64, branch: Option<&str>) -> Result<()> {
    let agent = AgentConfig::load(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!("No agent configured. Run 'crosslink agent init <id>' first.")
    })?;

    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;
    let _ = &agent; // identity validated above; the v3 writer carries its own.

    // Locks are pure events on the per-agent ref (#754, REQ-5). The SharedWriter
    // event path handles v3; on a legacy v2 hub it refuses with the migrate
    // prompt. The old `locks.json` write path is gone.
    let writer = SharedWriter::new(crosslink_dir)?
        .ok_or_else(|| anyhow::anyhow!("SharedWriter not available — is agent configured?"))?;
    use crate::shared_writer::LockClaimResult;
    match writer.claim_lock_v2(issue_id, branch)? {
        LockClaimResult::Claimed => {
            println!("Claimed lock on issue {}", format_issue_id(issue_id));
            if let Some(b) = branch {
                println!("  Branch: {b}");
            }
        }
        LockClaimResult::AlreadyHeld => {
            println!(
                "You already hold the lock on issue {}",
                format_issue_id(issue_id)
            );
        }
        LockClaimResult::Contended { winner_agent_id } => {
            anyhow::bail!(
                "Lock on issue {} was won by agent '{}'",
                format_issue_id(issue_id),
                winner_agent_id
            );
        }
    }
    Ok(())
}

/// `crosslink locks release <id>` — release a lock on an issue
pub fn release(crosslink_dir: &Path, issue_id: i64) -> Result<()> {
    let agent = AgentConfig::load(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!("No agent configured. Run 'crosslink agent init <id>' first.")
    })?;

    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;
    let _ = &agent; // identity validated above; the v3 writer carries its own.

    // Event-based release (v3). A legacy v2 hub refuses with the migrate prompt.
    let writer = SharedWriter::new(crosslink_dir)?
        .ok_or_else(|| anyhow::anyhow!("SharedWriter not available — is agent configured?"))?;
    if writer.release_lock_v2(issue_id)? {
        println!("Released lock on issue {}", format_issue_id(issue_id));
    } else {
        println!("Issue {} was not locked.", format_issue_id(issue_id));
    }
    Ok(())
}

/// `crosslink locks steal <id>` — steal a stale lock from another agent
pub fn steal(crosslink_dir: &Path, issue_id: i64) -> Result<()> {
    let agent = AgentConfig::load(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!("No agent configured. Run 'crosslink agent init <id>' first.")
    })?;

    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    // Check if the lock is actually stale before allowing steal
    let locks = sync.read_locks_auto()?;
    if let Some(existing) = locks.get_lock(issue_id) {
        if existing.agent_id == agent.agent_id {
            println!(
                "You already hold the lock on issue {}",
                format_issue_id(issue_id)
            );
            return Ok(());
        }

        let stale_locks = sync.find_stale_locks()?;
        let is_stale = stale_locks.iter().any(|(id, _)| *id == issue_id);

        if !is_stale {
            tracing::warn!(
                "Lock on {} held by '{}' is NOT stale. Stealing anyway.",
                format_issue_id(issue_id),
                existing.agent_id
            );
        }

        // Event-based steal (v3); a legacy v2 hub refuses with the migrate prompt.
        let writer = SharedWriter::new(crosslink_dir)?
            .ok_or_else(|| anyhow::anyhow!("SharedWriter not available"))?;
        writer.steal_lock_v2(issue_id, &existing.agent_id, None)?;
        println!(
            "Stole lock on issue {} from '{}'",
            format_issue_id(issue_id),
            existing.agent_id
        );
    } else {
        // Not locked — just claim it (event-based; v2 refuses).
        let writer = SharedWriter::new(crosslink_dir)?
            .ok_or_else(|| anyhow::anyhow!("SharedWriter not available"))?;
        use crate::shared_writer::LockClaimResult;
        match writer.claim_lock_v2(issue_id, None)? {
            LockClaimResult::Claimed | LockClaimResult::AlreadyHeld => {}
            LockClaimResult::Contended { winner_agent_id } => {
                anyhow::bail!("Lock contended — won by '{winner_agent_id}'");
            }
        }
        println!(
            "Claimed lock on issue {} (was not locked)",
            format_issue_id(issue_id)
        );
    }

    Ok(())
}

/// `crosslink sync` — reconcile hub state, hydrate issues, and report locks.
///
/// Routes by hub mode (#754):
///
/// - v3: fetch adopts every agent ref + the checkpoint and refreshes the local
///   checkpoint; hydrate `SQLite` from the reduced
///   [`crate::checkpoint::CheckpointState`]; poll this agent's control requests;
///   report locks.
/// - v2 (frozen / pre-migration hub): a READ-ONLY mirror fetch + hydrate from the
///   worktree JSON files for inspection, plus a single migrate hint. No writes,
///   no signing-enforcement bail (the v2 branch is frozen).
pub fn sync_cmd(crosslink_dir: &Path, db: &Database) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    if !sync.hub_mode().is_v3() {
        return sync_v2_readonly(crosslink_dir, db, &sync);
    }

    // Ensure the agent's key is published to allowed_signers if it was deferred
    // during agent init (the v3 meta ref carries the trust store).
    match sync.ensure_agent_key_published(crosslink_dir) {
        Ok(true) => println!("Published agent key to hub (deferred from agent init)."),
        Ok(false) => {}
        Err(e) => tracing::warn!("could not publish agent key: {}", e),
    }
    if let Err(e) = sync.configure_signing(crosslink_dir) {
        tracing::warn!("could not configure commit signing: {e} — commits will be unsigned");
    }

    // Hydrate SQLite from the reduced checkpoint state (fetch already adopted
    // refs + refreshed the checkpoint).
    let source = crate::hub_source::RefHubSource::new(sync.cache_path())?;
    let outcome = crate::compaction::reduce(&source)?;
    let stats = crate::hydration::hydrate_from_state(&outcome.state, db)?;
    crate::hydration::record_hydrated_ref(crosslink_dir);
    if stats.issues > 0 {
        println!(
            "Hydrated {} issue(s), {} comment(s), {} dep(s), {} relation(s), {} milestone(s).",
            stats.issues, stats.comments, stats.dependencies, stats.relations, stats.milestones
        );
    }

    // Process pending agent control requests for this agent (every sync tick
    // gets a poll pass so pause / resume / kill / reprioritise take effect in
    // <= one sync interval).
    if let (Ok(Some(writer)), Ok(Some(cfg))) = (
        SharedWriter::new(crosslink_dir),
        crate::identity::AgentConfig::load(crosslink_dir),
    ) {
        match crate::agent_requests::poll::process_pending(&writer, crosslink_dir, &cfg.agent_id) {
            Ok(result) if !result.acted.is_empty() => {
                println!(
                    "Processed {} agent request(s) for {}.",
                    result.acted.len(),
                    cfg.agent_id
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("agent request poll failed: {e}"),
        }
    }

    println!("Cache: {}", sync.cache_path().display());

    report_locks(&sync)?;
    Ok(())
}

/// Read-only `crosslink sync` against a frozen v2 hub: hydrate from the worktree
/// JSON files for inspection and print a single migrate hint. No writes.
fn sync_v2_readonly(crosslink_dir: &Path, db: &Database, sync: &SyncManager) -> Result<()> {
    let stats = hydrate_to_sqlite(sync.cache_path(), db)?;
    crate::hydration::record_hydrated_ref(crosslink_dir);
    if stats.issues > 0 {
        println!(
            "Hydrated {} issue(s), {} comment(s), {} dep(s), {} relation(s), {} milestone(s) \
             (read-only v2 inspection).",
            stats.issues, stats.comments, stats.dependencies, stats.relations, stats.milestones
        );
    }
    println!("Cache: {}", sync.cache_path().display());
    println!(
        "This hub uses the legacy v2 layout (read-only). Run `crosslink migrate hub-v3` to \
         migrate to the per-agent-ref layout; mutations are refused until you do."
    );
    report_locks(sync)?;
    Ok(())
}

/// Print the active and stale lock summary.
fn report_locks(sync: &SyncManager) -> Result<()> {
    let locks_file = sync.read_locks_auto()?;
    println!("{} active lock(s).", locks_file.locks.len());

    let stale = sync.find_stale_locks()?;
    if !stale.is_empty() {
        println!("{} stale lock(s) detected:", stale.len());
        for (id, agent) in &stale {
            println!("  {} (held by {})", format_issue_id(*id), agent);
        }
    }
    Ok(())
}
