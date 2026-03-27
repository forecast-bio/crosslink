use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::db::Database;
use crate::hydration::hydrate_to_sqlite;
use crate::identity::AgentConfig;
use crate::shared_writer::SharedWriter;
use crate::sync::{SignatureVerification, SyncManager};
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

#[derive(Debug, Serialize, Deserialize)]
struct PromotionLogEntry {
    timestamp: String,
    old_local_id: i64,
    old_display: String,
    new_display_id: i64,
    title: String,
    agent_id: String,
}

fn append_promotion_log(
    crosslink_dir: &Path,
    promoted: &[(i64, i64, String)],
    agent_id: &str,
) -> Result<()> {
    let log_path = crosslink_dir.join("promotion-log.json");
    let mut entries: Vec<PromotionLogEntry> = if log_path.exists() {
        let content = std::fs::read_to_string(&log_path)?;
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    let now = Utc::now().to_rfc3339();
    for (neg_id, new_id, title) in promoted {
        entries.push(PromotionLogEntry {
            timestamp: now.clone(),
            old_local_id: *neg_id,
            old_display: if *neg_id != 0 {
                format!("L{}", neg_id.unsigned_abs())
            } else {
                "unknown".to_string()
            },
            new_display_id: *new_id,
            title: title.clone(),
            agent_id: agent_id.to_string(),
        });
    }

    let json = serde_json::to_string_pretty(&entries)?;
    std::fs::write(&log_path, json)?;
    Ok(())
}

/// `crosslink locks list` — show current lock state
pub fn list(crosslink_dir: &Path, db: &Database, json_output: bool) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    let locks_file = sync.read_locks_auto()?;

    if json_output {
        let json = serde_json::to_string_pretty(&locks_file)?;
        println!("{}", json);
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
            .map(|i| truncate(&i.title, 40))
            .unwrap_or_else(|| "(unknown issue)".to_string());

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
            println!("         branch: {}", branch);
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
                println!("  Branch: {}", branch);
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

    if sync.is_v2_layout() {
        let writer = SharedWriter::new(crosslink_dir)?
            .ok_or_else(|| anyhow::anyhow!("SharedWriter not available — is agent configured?"))?;
        use crate::shared_writer::LockClaimResult;
        match writer.claim_lock_v2(issue_id, branch)? {
            LockClaimResult::Claimed => {
                println!("Claimed lock on issue {}", format_issue_id(issue_id));
                if let Some(b) = branch {
                    println!("  Branch: {}", b);
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
        return Ok(());
    }

    match sync.claim_lock(&agent, issue_id, branch, crate::sync::LockMode::Normal)? {
        true => {
            println!("Claimed lock on issue {}", format_issue_id(issue_id));
            if let Some(b) = branch {
                println!("  Branch: {}", b);
            }
        }
        false => {
            println!(
                "You already hold the lock on issue {}",
                format_issue_id(issue_id)
            );
        }
    }
    Ok(())
}

/// `crosslink locks release <id>` — release a lock on an issue
pub fn release(crosslink_dir: &Path, issue_id: i64) -> Result<()> {
    let _agent = AgentConfig::load(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!("No agent configured. Run 'crosslink agent init <id>' first.")
    })?;

    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    if sync.is_v2_layout() {
        let writer = SharedWriter::new(crosslink_dir)?
            .ok_or_else(|| anyhow::anyhow!("SharedWriter not available — is agent configured?"))?;
        match writer.release_lock_v2(issue_id)? {
            true => println!("Released lock on issue {}", format_issue_id(issue_id)),
            false => println!("Issue {} was not locked.", format_issue_id(issue_id)),
        }
        return Ok(());
    }

    match sync.release_lock(&_agent, issue_id, crate::sync::LockMode::Normal)? {
        true => println!("Released lock on issue {}", format_issue_id(issue_id)),
        false => println!("Issue {} was not locked.", format_issue_id(issue_id)),
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

        if sync.is_v2_layout() {
            let writer = SharedWriter::new(crosslink_dir)?
                .ok_or_else(|| anyhow::anyhow!("SharedWriter not available"))?;
            writer.steal_lock_v2(issue_id, &existing.agent_id, None)?;
            println!(
                "Stole lock on issue {} from '{}'",
                format_issue_id(issue_id),
                existing.agent_id
            );
        } else {
            sync.claim_lock(&agent, issue_id, None, crate::sync::LockMode::Steal)?;
            println!(
                "Stole lock on issue {} from '{}'",
                format_issue_id(issue_id),
                existing.agent_id
            );
        }
    } else {
        // Not locked — just claim it
        if sync.is_v2_layout() {
            let writer = SharedWriter::new(crosslink_dir)?
                .ok_or_else(|| anyhow::anyhow!("SharedWriter not available"))?;
            use crate::shared_writer::LockClaimResult;
            match writer.claim_lock_v2(issue_id, None)? {
                LockClaimResult::Claimed | LockClaimResult::AlreadyHeld => {}
                LockClaimResult::Contended { winner_agent_id } => {
                    anyhow::bail!("Lock contended — won by '{}'", winner_agent_id);
                }
            }
        } else {
            sync.claim_lock(&agent, issue_id, None, crate::sync::LockMode::Normal)?;
        }
        println!(
            "Claimed lock on issue {} (was not locked)",
            format_issue_id(issue_id)
        );
    }

    Ok(())
}

/// `crosslink sync` — fetch latest locks, hydrate issues, and verify signatures
pub fn sync_cmd(crosslink_dir: &Path, db: &Database) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    // Ensure the agent's key is published (may have been skipped during
    // agent init if the hub cache didn't exist yet). Must happen before
    // configure_signing to avoid the chicken-and-egg signing problem.
    match sync.ensure_agent_key_published(crosslink_dir) {
        Ok(true) => println!("Published agent key to hub (deferred from agent init)."),
        Ok(false) => {}
        Err(e) => tracing::warn!("could not publish agent key: {}", e),
    }

    if let Err(e) = sync.configure_signing(crosslink_dir) {
        tracing::warn!("could not configure commit signing: {e} — commits will be unsigned");
    }

    // Upgrade v1 layouts to v2 if needed (migrates inline comments to standalone files)
    match sync.upgrade_to_v2() {
        Ok(0) => {} // already v2 or nothing to migrate
        Ok(n) => println!("Upgraded hub layout to v2 ({n} comment files migrated)."),
        Err(e) => tracing::warn!("layout upgrade failed: {e}"),
    }

    // Auto-cleanup stale V1 layout files on V2 hubs (#478)
    match sync.cleanup_stale_layout_files() {
        Ok(0) => {}
        Ok(n) => println!("Cleaned up {n} stale V1 layout file(s)."),
        Err(e) => tracing::warn!("layout cleanup failed: {e}"),
    }

    // Hydrate local SQLite from JSON issue files on the coordination branch
    let stats = hydrate_to_sqlite(sync.cache_path(), db)?;
    // Record the hub ref so lazy auto-hydration knows we're current (#500)
    crate::hydration::record_hydrated_ref(crosslink_dir);
    if stats.issues > 0 {
        println!(
            "Hydrated {} issue(s), {} comment(s), {} dep(s), {} relation(s), {} milestone(s).",
            stats.issues, stats.comments, stats.dependencies, stats.relations, stats.milestones
        );
    }

    // Attempt to promote offline issues (display_id: null → real IDs)
    if let Some(writer) = SharedWriter::new(crosslink_dir)? {
        let promoted = writer.promote_offline_issues(db)?;
        if !promoted.is_empty() {
            println!("\nPromoted {} offline issue(s):", promoted.len());
            for (neg_id, new_id, title) in &promoted {
                if *neg_id != 0 {
                    println!("  L{} -> #{}: {}", neg_id.unsigned_abs(), new_id, title);
                } else {
                    println!("  -> #{}: {}", new_id, title);
                }
            }

            // Rewrite Lx references in comments, descriptions, session notes
            let rewrite_stats = writer.rewrite_local_references(db, &promoted)?;
            if rewrite_stats.total() > 0 {
                println!(
                    "Updated {} reference(s) (comments: {}, descriptions: {}, sessions: {})",
                    rewrite_stats.total(),
                    rewrite_stats.comments_updated,
                    rewrite_stats.descriptions_updated,
                    rewrite_stats.sessions_updated,
                );
            }

            // Append to promotion log
            let agent_id = writer.agent_id();
            append_promotion_log(crosslink_dir, &promoted, agent_id)?;

            println!();
        }
    }

    println!("Cache: {}", sync.cache_path().display());

    // Verify commit signature (SSH or GPG)
    let verification = sync.verify_locks_signature()?;
    match &verification {
        SignatureVerification::Valid {
            commit,
            fingerprint,
            principal,
        } => {
            println!(
                "Locks synced. Signature valid (commit {}).",
                &commit[..7.min(commit.len())]
            );
            if let Some(who) = principal {
                println!("  Signer: {}", who);
            }
            if let Some(fp) = fingerprint {
                println!("  Fingerprint: {}", fp);
                // Check against allowed_signers (preferred) or legacy keyring
                let trusted = if let Ok(signers) = sync.read_allowed_signers() {
                    if !signers.entries.is_empty() {
                        let is_trusted = principal
                            .as_ref()
                            .map(|p| signers.is_trusted(p))
                            .unwrap_or(false);
                        if is_trusted {
                            println!("  Signer is trusted (allowed_signers).");
                        } else {
                            println!("  WARNING: Signer not in allowed_signers!");
                        }
                        true // we checked
                    } else {
                        false
                    }
                } else {
                    false
                };
                // Fall back to legacy keyring
                if !trusted {
                    if let Ok(Some(keyring)) = sync.read_keyring() {
                        if keyring.is_trusted(fp) {
                            println!("  Key is in trusted keyring.");
                        } else {
                            println!("  WARNING: Signer not in trusted keyring!");
                        }
                    }
                }
            }
        }
        SignatureVerification::Unsigned { commit } => {
            println!(
                "Locks synced. WARNING: Latest commit ({}) is NOT signed.",
                &commit[..7.min(commit.len())]
            );
        }
        SignatureVerification::Invalid { commit, reason } => {
            println!(
                "Locks synced. WARNING: Signature verification failed on {}: {}",
                &commit[..7.min(commit.len())],
                reason
            );
        }
        SignatureVerification::NoCommits => {
            println!("Locks branch has no commits yet.");
        }
    }

    let locks_file = sync.read_locks_auto()?;
    println!("{} active lock(s).", locks_file.locks.len());

    let stale = sync.find_stale_locks()?;
    if !stale.is_empty() {
        println!("{} stale lock(s) detected:", stale.len());
        for (id, agent) in &stale {
            println!("  {} (held by {})", format_issue_id(*id), agent);
        }
    }

    // Signing enforcement check
    let enforcement = read_signing_enforcement(crosslink_dir);
    if enforcement != "disabled" {
        let results = sync.verify_recent_commits(5)?;
        let unsigned: Vec<_> = results
            .iter()
            .filter(|(_, v)| matches!(v, SignatureVerification::Unsigned { .. }))
            .collect();
        let invalid: Vec<_> = results
            .iter()
            .filter(|(_, v)| matches!(v, SignatureVerification::Invalid { .. }))
            .collect();

        if !unsigned.is_empty() || !invalid.is_empty() {
            let msg = format!(
                "{} unsigned, {} invalid signature(s) in last {} commit(s)",
                unsigned.len(),
                invalid.len(),
                results.len()
            );
            if enforcement == "enforced" {
                anyhow::bail!("Signing enforcement FAILED: {}", msg);
            } else {
                // audit mode
                println!("Signing audit: {}", msg);
            }
        } else if !results.is_empty() {
            println!(
                "Signing audit: all {} recent commit(s) are signed.",
                results.len()
            );
        }

        // Per-entry signature verification
        let (verified, failed, entry_unsigned) = sync.verify_entry_signatures()?;
        let total_entries = verified + failed + entry_unsigned;
        if total_entries > 0 {
            if failed > 0 {
                let msg = format!(
                    "{} verified, {} FAILED, {} unsigned entry signature(s)",
                    verified, failed, entry_unsigned
                );
                if enforcement == "enforced" {
                    anyhow::bail!("Entry signing enforcement FAILED: {}", msg);
                } else {
                    println!("Entry signing audit: {}", msg);
                }
            } else if verified > 0 {
                println!(
                    "Entry signing audit: {} verified, {} unsigned entry signature(s).",
                    verified, entry_unsigned
                );
            }
        }
    }

    Ok(())
}

/// Read the `signing_enforcement` setting from hook-config.json.
///
/// Returns `"disabled"`, `"audit"`, or `"enforced"`. Defaults to `"disabled"`.
fn read_signing_enforcement(crosslink_dir: &Path) -> String {
    let config_path = crosslink_dir.join("hook-config.json");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return "disabled".to_string(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return "disabled".to_string(),
    };
    parsed
        .get("signing_enforcement")
        .and_then(|v| v.as_str())
        .unwrap_or("disabled")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_append_promotion_log_creates_file() {
        let dir = tempdir().unwrap();
        let promoted = vec![(-1i64, 5i64, "Fix auth".to_string())];
        append_promotion_log(dir.path(), &promoted, "agent-1").unwrap();

        let log_path = dir.path().join("promotion-log.json");
        assert!(log_path.exists());

        let content = std::fs::read_to_string(&log_path).unwrap();
        let entries: Vec<PromotionLogEntry> = serde_json::from_str(&content).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].old_local_id, -1);
        assert_eq!(entries[0].old_display, "L1");
        assert_eq!(entries[0].new_display_id, 5);
        assert_eq!(entries[0].title, "Fix auth");
        assert_eq!(entries[0].agent_id, "agent-1");
    }

    #[test]
    fn test_append_promotion_log_appends() {
        let dir = tempdir().unwrap();
        let batch1 = vec![(-1i64, 5i64, "First".to_string())];
        append_promotion_log(dir.path(), &batch1, "agent-1").unwrap();

        let batch2 = vec![(-2i64, 6i64, "Second".to_string())];
        append_promotion_log(dir.path(), &batch2, "agent-1").unwrap();

        let content = std::fs::read_to_string(dir.path().join("promotion-log.json")).unwrap();
        let entries: Vec<PromotionLogEntry> = serde_json::from_str(&content).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].new_display_id, 5);
        assert_eq!(entries[1].new_display_id, 6);
    }

    #[test]
    fn test_append_promotion_log_zero_neg_id() {
        let dir = tempdir().unwrap();
        let promoted = vec![(0i64, 5i64, "Unknown origin".to_string())];
        append_promotion_log(dir.path(), &promoted, "agent-1").unwrap();

        let content = std::fs::read_to_string(dir.path().join("promotion-log.json")).unwrap();
        let entries: Vec<PromotionLogEntry> = serde_json::from_str(&content).unwrap();
        assert_eq!(entries[0].old_display, "unknown");
    }
}
