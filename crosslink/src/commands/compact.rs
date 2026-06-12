use anyhow::Result;
use std::path::Path;

use crate::db::Database;

/// `crosslink compact` — run event compaction manually.
///
/// `force` is accepted for CLI compatibility but is a no-op: the v2 lease
/// override it once toggled is gone with the v2 compaction path (#754), and the
/// v3 checkpoint compaction always runs.
pub fn run(crosslink_dir: &Path, db: &Database, force: bool) -> Result<()> {
    let _ = force;
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

    // V2 (frozen / pre-migration hub): standalone `crosslink compact` is refused
    // (#754). Compaction of a v2 hub is now exclusively the migration's internal
    // step (`migrate hub-v3` calls `compaction::compact` directly); operating it
    // here would write worktree materialized files the v3 path never reads.
    drop(hub_lock);
    anyhow::bail!(
        "this hub uses the legacy v2 layout; `crosslink compact` is not available on it. \
         Run `crosslink migrate hub-v3` to migrate to the per-agent-ref layout."
    )
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    /// `crosslink compact` on a frozen v2 hub bails with the migrate prompt
    /// rather than running the v2 compaction (754b hygiene).
    #[test]
    fn compact_on_v2_hub_refuses_with_migrate_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(repo)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return; // git unavailable
        }
        for cfg in [["user.email", "t@t"], ["user.name", "t"]] {
            let _ = Command::new("git")
                .args(["config", cfg[0], cfg[1]])
                .current_dir(repo)
                .status();
        }
        let crosslink_dir = repo.join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("agent.json"),
            serde_json::json!({"agent_id":"agent-1","machine_id":"m"}).to_string(),
        )
        .unwrap();

        // Build an explicit v2 `crosslink/hub` worktree so the hub resolves V2.
        let cache_dir = crosslink_dir.join(".hub-cache");
        Command::new("git")
            .current_dir(repo)
            .args([
                "worktree",
                "add",
                "--orphan",
                "-b",
                "crosslink/hub",
                cache_dir.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        for cfg in [["user.email", "t@t"], ["user.name", "t"]] {
            let _ = Command::new("git")
                .current_dir(&cache_dir)
                .args(["config", cfg[0], cfg[1]])
                .status();
        }
        std::fs::write(cache_dir.join("locks.json"), "{}").unwrap();
        Command::new("git")
            .current_dir(&cache_dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&cache_dir)
            .args(["commit", "-m", "v2", "--no-gpg-sign"])
            .output()
            .unwrap();

        let db = crate::db::Database::open(&crosslink_dir.join("issues.db")).unwrap();
        let err =
            super::run(&crosslink_dir, &db, false).expect_err("compact must refuse on a v2 hub");
        assert!(
            err.to_string().contains("migrate hub-v3"),
            "refusal must point at `crosslink migrate hub-v3`; got: {err}"
        );
    }
}
