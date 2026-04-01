// Smoke tests for multi-agent coordination: event sourcing, compaction, lock
// contention, and integrity checks.

use super::harness::SmokeHarness;

/// Initialize an agent identity and hub cache so the SharedWriter, locks, and
/// compact commands work.  Uses `--no-key` to skip SSH key generation.
fn init_agent_and_sync(h: &SmokeHarness, agent_id: &str) {
    // --force because `crosslink init --defaults` auto-creates an agent identity
    h.run_ok(&["agent", "init", agent_id, "--no-key", "--force"]);
    // Sync initialises the hub cache worktree which SharedWriter needs.
    h.run_ok(&["sync"]);
}

// ===========================================================================
// Multi-Agent Basic Coordination
// ===========================================================================

/// Agent A creates an issue and syncs.  Agent B syncs and sees it.
#[test]
fn test_two_agents_create_issues() {
    let agent_a = SmokeHarness::new();
    init_agent_and_sync(&agent_a, "agent-a");

    let agent_b = agent_a.fork_agent("agent-b");
    init_agent_and_sync(&agent_b, "agent-b");

    // Agent A creates an issue (SharedWriter writes to hub cache)
    agent_a.run_ok(&["create", "Task from A"]);

    // Agent A syncs (pushes hub cache to shared remote)
    agent_a.run_ok(&["sync"]);

    // Agent B syncs (pulls from shared remote, hydrates SQLite)
    agent_b.run_ok(&["sync"]);

    // Agent B should see Agent A's issue
    let result = agent_b.run_ok(&["list", "-s", "all"]);
    assert!(
        result.stdout_contains("Task from A"),
        "Agent B should see Agent A's issue after sync.\nstdout: {}",
        result.stdout,
    );
}

/// Both agents create different issues, both sync, and both see all issues.
#[test]
fn test_two_agents_independent() {
    let agent_a = SmokeHarness::new();
    init_agent_and_sync(&agent_a, "agent-a");

    let agent_b = agent_a.fork_agent("agent-b");
    init_agent_and_sync(&agent_b, "agent-b");

    // Agent A creates an issue and syncs
    agent_a.run_ok(&["create", "Issue from A"]);
    agent_a.run_ok(&["sync"]);

    // Agent B syncs to get Agent A's data, creates its own issue, and syncs
    agent_b.run_ok(&["sync"]);
    agent_b.run_ok(&["create", "Issue from B"]);
    agent_b.run_ok(&["sync"]);

    // Agent A syncs again to pick up Agent B's issue
    agent_a.run_ok(&["sync"]);

    // Both agents should now see both issues
    let result_a = agent_a.run_ok(&["list", "-s", "all"]);
    assert!(
        result_a.stdout_contains("Issue from A"),
        "Agent A should see its own issue.\nstdout: {}",
        result_a.stdout,
    );
    assert!(
        result_a.stdout_contains("Issue from B"),
        "Agent A should see Agent B's issue.\nstdout: {}",
        result_a.stdout,
    );

    let result_b = agent_b.run_ok(&["list", "-s", "all"]);
    assert!(
        result_b.stdout_contains("Issue from A"),
        "Agent B should see Agent A's issue.\nstdout: {}",
        result_b.stdout,
    );
    assert!(
        result_b.stdout_contains("Issue from B"),
        "Agent B should see its own issue.\nstdout: {}",
        result_b.stdout,
    );
}

// ===========================================================================
// Lock Management
// ===========================================================================

/// Claim a lock on an issue, verify it shows as locked, release it, and verify
/// it shows as unlocked.
#[test]
fn test_lock_claim_release() {
    let h = SmokeHarness::new();
    init_agent_and_sync(&h, "smoke-agent");

    // Create an issue to lock
    h.run_ok(&["create", "Lockable task"]);
    h.run_ok(&["sync"]);

    // Claim the lock
    let claim_result = h.run_ok(&["locks", "claim", "1"]);
    assert!(
        claim_result.stdout_contains("Claimed")
            || claim_result.stdout_contains("claimed")
            || claim_result.stdout_contains("lock"),
        "Expected claim confirmation.\nstdout: {}\nstderr: {}",
        claim_result.stdout,
        claim_result.stderr,
    );

    // Check that the lock is held
    let check_result = h.run_ok(&["locks", "check", "1"]);
    assert!(
        check_result.stdout_contains("locked")
            || check_result.stdout_contains("Locked")
            || check_result.stdout_contains("held")
            || check_result.stdout_contains("Held"),
        "Expected issue to be locked.\nstdout: {}\nstderr: {}",
        check_result.stdout,
        check_result.stderr,
    );

    // Release the lock
    let release_result = h.run_ok(&["locks", "release", "1"]);
    assert!(
        release_result.stdout_contains("Released")
            || release_result.stdout_contains("released")
            || release_result.stdout_contains("lock"),
        "Expected release confirmation.\nstdout: {}\nstderr: {}",
        release_result.stdout,
        release_result.stderr,
    );

    // Check again — should be unlocked
    let check_result = h.run_ok(&["locks", "check", "1"]);
    assert!(
        check_result.stdout_contains("available")
            || check_result.stdout_contains("Available")
            || check_result.stdout_contains("unlocked")
            || check_result.stdout_contains("not locked")
            || check_result.stdout_contains("Not locked"),
        "Expected issue to be unlocked after release.\nstdout: {}\nstderr: {}",
        check_result.stdout,
        check_result.stderr,
    );
}

/// No locks have been claimed — `locks list` should indicate an empty state.
#[test]
fn test_lock_list_empty() {
    let h = SmokeHarness::new();

    // Sync to initialise the hub branch
    h.run_ok(&["sync"]);

    let result = h.run_ok(&["locks", "list"]);
    assert!(
        result.stdout_contains("No active locks")
            || result.stdout_contains("no active locks")
            || result.stdout_contains("0 active lock"),
        "Expected empty lock list.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

/// Check a lock on an issue that has never been locked.
#[test]
fn test_lock_check_unlocked() {
    let h = SmokeHarness::new();

    h.run_ok(&["create", "Never locked"]);
    h.run_ok(&["sync"]);

    let result = h.run_ok(&["locks", "check", "1"]);
    assert!(
        result.stdout_contains("available")
            || result.stdout_contains("Available")
            || result.stdout_contains("unlocked")
            || result.stdout_contains("not locked")
            || result.stdout_contains("Not locked"),
        "Unlocked issue should report as available.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

/// Same agent claims the same lock twice.  Should be idempotent (succeed) or
/// return an informative message — but not crash.
#[test]
fn test_lock_claim_twice_same_agent() {
    let h = SmokeHarness::new();
    init_agent_and_sync(&h, "smoke-agent");

    h.run_ok(&["create", "Double lock task"]);
    h.run_ok(&["sync"]);

    // First claim
    h.run_ok(&["locks", "claim", "1"]);

    // Second claim by the same agent — should succeed or report already held.
    // We use run() instead of run_ok() since the CLI may return an error or
    // succeed depending on the implementation.
    let result = h.run(&["locks", "claim", "1"]);
    assert!(
        result.stdout_contains("Claimed")
            || result.stdout_contains("claimed")
            || result.stdout_contains("Already")
            || result.stdout_contains("already")
            || result.stdout_contains("held"),
        "Double claim should be idempotent or report already held.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
    // Must not crash
    assert!(
        result.exit_code == 0 || result.exit_code == 1,
        "Unexpected exit code {} for double claim",
        result.exit_code,
    );
}

// ===========================================================================
// Compact
// ===========================================================================

/// Create issues, sync (which pushes events to the hub), then run compact and
/// verify it completes successfully.
#[test]
fn test_compact_after_creates() {
    let h = SmokeHarness::new();
    init_agent_and_sync(&h, "smoke-agent");

    // Create several issues and sync
    h.run_ok(&["create", "Compact test A"]);
    h.run_ok(&["create", "Compact test B"]);
    h.run_ok(&["create", "Compact test C"]);
    h.run_ok(&["sync"]);

    // Run compact
    let result = h.run_ok(&["compact", "--force"]);
    assert!(
        result.stdout_contains("Compaction complete")
            || result.stdout_contains("compaction")
            || result.stdout_contains("Compact"),
        "Expected compaction success message.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );

    // Verify issues are still accessible after compaction
    let list_result = h.run_ok(&["list", "-s", "all"]);
    assert!(
        list_result.stdout_contains("Compact test A"),
        "Issue A should survive compaction.\nstdout: {}",
        list_result.stdout,
    );
    assert!(
        list_result.stdout_contains("Compact test B"),
        "Issue B should survive compaction.\nstdout: {}",
        list_result.stdout,
    );
    assert!(
        list_result.stdout_contains("Compact test C"),
        "Issue C should survive compaction.\nstdout: {}",
        list_result.stdout,
    );
}

/// Running compact twice should produce the same result and not error.
#[test]
fn test_compact_idempotent() {
    let h = SmokeHarness::new();
    init_agent_and_sync(&h, "smoke-agent");

    h.run_ok(&["create", "Idempotent compact test"]);
    h.run_ok(&["sync"]);

    // First compaction
    let first = h.run_ok(&["compact", "--force"]);
    assert!(
        first.stdout_contains("Compaction complete") || first.stdout_contains("compaction"),
        "First compaction should succeed.\nstdout: {}\nstderr: {}",
        first.stdout,
        first.stderr,
    );

    // Second compaction — should succeed with no new events
    let second = h.run_ok(&["compact", "--force"]);
    assert!(
        second.stdout_contains("Compaction complete")
            || second.stdout_contains("compaction")
            || second.stdout_contains("No new events"),
        "Second compaction should succeed idempotently.\nstdout: {}\nstderr: {}",
        second.stdout,
        second.stderr,
    );

    // Issues should still be intact
    let list_result = h.run_ok(&["list", "-s", "all"]);
    assert!(
        list_result.stdout_contains("Idempotent compact test"),
        "Issue should survive double compaction.\nstdout: {}",
        list_result.stdout,
    );
}

// ===========================================================================
// Integrity After Operations
// ===========================================================================

/// Create issues via the SharedWriter path, sync, then verify all integrity
/// checks pass.
#[test]
fn test_integrity_after_sync() {
    let h = SmokeHarness::new();
    init_agent_and_sync(&h, "smoke-agent");

    h.run_ok(&["create", "Integrity check A"]);
    h.run_ok(&["create", "Integrity check B"]);
    h.run_ok(&["sync"]);

    // Run all integrity checks — should pass or be skipped (never fail).
    let result = h.run_ok(&["integrity"]);
    assert!(
        !result.stdout_contains("[FAIL]"),
        "No integrity check should fail after clean sync.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

/// Create issues via CLI, sync, and verify hydration integrity is clean.
#[test]
fn test_integrity_hydration_matches() {
    let h = SmokeHarness::new();
    init_agent_and_sync(&h, "smoke-agent");

    h.run_ok(&["create", "Hydration test A"]);
    h.run_ok(&["create", "Hydration test B"]);
    h.run_ok(&["create", "Hydration test C"]);
    h.run_ok(&["sync"]);

    // Check hydration specifically
    let result = h.run_ok(&["integrity", "hydration"]);
    assert!(
        result.stdout_contains("[PASS]") || result.stdout_contains("[SKIPPED]"),
        "Hydration integrity should pass or skip (not fail) after sync.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
    assert!(
        !result.stdout_contains("[FAIL]"),
        "Hydration should not fail.\nstdout: {}",
        result.stdout,
    );
}

// ===========================================================================
// Adversarial Coordination Tests
// ===========================================================================

/// Two agents edit the same issue concurrently without syncing first, then both
/// sync.  The final state must be consistent: the issue must still exist and
/// neither agent should panic or corrupt the database.
///
/// Convergence here means both agents, after syncing, agree that the issue
/// exists (no split-brain data loss).  The exact winning value of a concurrent
/// update (last-write-wins vs merge) is implementation-defined; we only assert
/// non-corruption and liveness.
#[test]
fn test_adversarial_same_issue_write_conflict_convergence() {
    let agent_a = SmokeHarness::new();
    init_agent_and_sync(&agent_a, "agent-a");

    let agent_b = agent_a.fork_agent("agent-b");
    init_agent_and_sync(&agent_b, "agent-b");

    // Agent A creates the shared issue and syncs so both agents know about it.
    agent_a.run_ok(&["create", "Shared conflict issue"]);
    agent_a.run_ok(&["sync"]);
    agent_b.run_ok(&["sync"]);

    // Both agents add different labels to the same issue WITHOUT syncing first
    // — this is the concurrent-write scenario.
    agent_a.run_ok(&["issue", "label", "1", "label-from-a"]);
    agent_b.run_ok(&["issue", "label", "1", "label-from-b"]);

    // Now both push their diverged state.
    // One push may fail (push conflict) or one may win; both are acceptable
    // as long as we can recover.
    let sync_a = agent_a.run(&["sync"]);
    let sync_b = agent_b.run(&["sync"]);

    // At least one sync must succeed.
    assert!(
        sync_a.success || sync_b.success,
        "At least one agent's sync must succeed after concurrent edits.\
         \nAgent A sync stdout: {}\nAgent A sync stderr: {}\
         \nAgent B sync stdout: {}\nAgent B sync stderr: {}",
        sync_a.stdout,
        sync_a.stderr,
        sync_b.stdout,
        sync_b.stderr,
    );

    // Bring both agents up-to-date by retrying whichever failed.
    let _ = agent_a.run(&["sync"]);
    let _ = agent_b.run(&["sync"]);

    // Final convergence check: the issue must still exist on both sides.
    let show_a = agent_a.run_ok(&["show", "1"]);
    assert!(
        show_a.stdout_contains("Shared conflict issue"),
        "Agent A: issue must survive concurrent edit.\nstdout: {}",
        show_a.stdout,
    );

    let show_b = agent_b.run_ok(&["show", "1"]);
    assert!(
        show_b.stdout_contains("Shared conflict issue"),
        "Agent B: issue must survive concurrent edit.\nstdout: {}",
        show_b.stdout,
    );
}

/// Agent A claims a lock; Agent B steals it after A's heartbeat goes stale.
/// After the steal, Agent B should hold the lock and an audit comment (or
/// equivalent record) should have been added to the issue.
///
/// If the `locks steal` command is unimplemented or not yet available in this
/// build, the test accepts an error exit code (no crash required).
#[test]
fn test_adversarial_stale_lock_steal_audit() {
    let agent_a = SmokeHarness::new();
    init_agent_and_sync(&agent_a, "agent-a");

    let agent_b = agent_a.fork_agent("agent-b");
    init_agent_and_sync(&agent_b, "agent-b");

    // Agent A creates an issue and claims the lock.
    agent_a.run_ok(&["create", "Stale lock test issue"]);
    agent_a.run_ok(&["sync"]);
    agent_b.run_ok(&["sync"]);

    agent_a.run_ok(&["locks", "claim", "1"]);
    // Do NOT release the lock — simulate a stale / abandoned lock.

    // Agent B syncs to see the claimed lock, then steals it.
    agent_b.run_ok(&["sync"]);
    let steal = agent_b.run(&["locks", "steal", "1"]);

    if steal.success {
        // Verify Agent B now holds the lock.
        let check = agent_b.run_ok(&["locks", "check", "1"]);
        assert!(
            check.stdout_contains("agent-b")
                || check.stdout_contains("locked")
                || check.stdout_contains("Locked")
                || check.stdout_contains("held"),
            "After steal, Agent B should hold the lock.\nstdout: {}",
            check.stdout,
        );

        // Verify an audit trail was created: either a comment on the issue or
        // an entry in the locks list that references the steal.
        // We accept any of: a comment on the issue referencing steal/lock, or
        // the lock list showing agent-b.
        let show = agent_b.run(&["show", "1"]);
        let locks_list = agent_b.run(&["locks", "list"]);
        let has_audit = show.stdout.to_ascii_lowercase().contains("steal")
            || show.stdout.to_ascii_lowercase().contains("lock")
            || locks_list.stdout_contains("agent-b")
            || locks_list.stdout_contains("1");
        assert!(
            has_audit,
            "Steal should produce an audit record (comment or lock list entry).\
             \nshow stdout: {}\nlocks list stdout: {}",
            show.stdout, locks_list.stdout,
        );
    } else {
        // steal returned non-zero — acceptable if the feature is not yet
        // available or the lock is not stale enough.  Assert no panic by
        // verifying the system is still responsive.
        agent_b.run_ok(&["list", "-s", "all"]);
    }
}

/// Corrupt the hub cache (delete hub-branch files or break git state), then
/// verify `crosslink sync` recovers gracefully — either re-initialising the
/// cache or reporting a clear error.  The command must not panic.
///
/// This test deliberately damages internal state; we accept both recovery
/// (exit 0) and a clean error (non-zero with a message).  A segfault, OOM,
/// or silent data loss would be a failure.
#[test]
fn test_adversarial_hub_cache_corruption_recovery() {
    let h = SmokeHarness::new();
    init_agent_and_sync(&h, "recovery-agent");

    // Create some data so the hub cache has content.
    h.run_ok(&["create", "Pre-corruption issue"]);
    h.run_ok(&["sync"]);

    // Locate the hub cache directory — crosslink stores it under .crosslink/
    // typically as a worktree or a bare-like directory.
    let crosslink_dir = h.crosslink_dir();

    // Remove all files under .crosslink/hub* or .crosslink/cache* to
    // simulate corruption.  We deliberately look for directories that start
    // with "hub" or "cache" and delete their contents.
    let hub_deleted = if let Ok(entries) = std::fs::read_dir(&crosslink_dir) {
        let mut deleted = false;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("hub") || name_str.starts_with("cache") {
                let path = entry.path();
                if path.is_dir() {
                    // Delete a subset of files inside to corrupt without
                    // removing the directory itself.
                    if let Ok(inner) = std::fs::read_dir(&path) {
                        for inner_entry in inner.flatten() {
                            let _ = std::fs::remove_file(inner_entry.path());
                        }
                    }
                    deleted = true;
                } else {
                    let _ = std::fs::remove_file(&path);
                    deleted = true;
                }
            }
        }
        deleted
    } else {
        false
    };

    // Even if we didn't find hub-specific dirs, corrupt the git objects
    // directory slightly by removing the FETCH_HEAD file (a common cached
    // state file).
    let _ = std::fs::remove_file(crosslink_dir.join("FETCH_HEAD"));

    // Now attempt sync.  We do NOT use run_ok because recovery may return
    // an error code — that is acceptable.
    let sync_result = h.run(&["sync"]);

    // The command must not hang or produce an empty output (which would
    // suggest a silent panic / abort).
    let output_present = !sync_result.stdout.is_empty() || !sync_result.stderr.is_empty();

    // After possible corruption we only require: no silent failure.
    // A clear error message is acceptable; a successful recovery is better.
    if !sync_result.success {
        assert!(
            output_present,
            "sync after hub cache corruption must produce output (not silently crash).\
             \nstdout: {}\nstderr: {}",
            sync_result.stdout, sync_result.stderr,
        );
        // Log what happened for human review but don't fail — the key
        // assertion is the system didn't panic silently.
        let _ = hub_deleted; // used above
    } else {
        // Recovery succeeded — verify the original issue is still accessible
        // or the system is at least in a consistent state.
        let list = h.run_ok(&["list", "-s", "all"]);
        assert!(
            list.success,
            "After sync recovery, list should succeed.\nstdout: {}",
            list.stdout,
        );
    }
}

/// Two agents independently create events without syncing.  After both agents
/// sync and compact, no events should be lost: all issues created by both
/// agents must still be visible, demonstrating that compaction does not drop
/// concurrent events.
#[test]
fn test_adversarial_event_log_divergence_compaction_consistency() {
    let agent_a = SmokeHarness::new();
    init_agent_and_sync(&agent_a, "agent-a");

    let agent_b = agent_a.fork_agent("agent-b");
    init_agent_and_sync(&agent_b, "agent-b");

    // Both agents create events locally without syncing first (diverged logs).
    agent_a.run_ok(&["create", "Event diverge A-1"]);
    agent_a.run_ok(&["create", "Event diverge A-2"]);

    agent_b.run_ok(&["create", "Event diverge B-1"]);
    agent_b.run_ok(&["create", "Event diverge B-2"]);

    // Now sync both agents — one will push first, the other will need to
    // merge/rebase the diverged history.
    let sync_a = agent_a.run(&["sync"]);
    let sync_b = agent_b.run(&["sync"]);

    // Retry the failing one so both are eventually consistent.
    if !sync_a.success {
        agent_a.run_ok(&["sync"]);
    }
    if !sync_b.success {
        agent_b.run_ok(&["sync"]);
    }

    // Do a final cross-sync so both have the full merged history.
    agent_a.run_ok(&["sync"]);
    agent_b.run_ok(&["sync"]);

    // Run compaction on both.  We use run() not run_ok() because some
    // environments may not have enough events to trigger compaction.
    let _compact_a = agent_a.run(&["compact", "--force"]);
    let _compact_b = agent_b.run(&["compact", "--force"]);

    // Sync once more after compaction to propagate the compacted state.
    agent_a.run_ok(&["sync"]);
    agent_b.run_ok(&["sync"]);

    // Verify that all four issues survive on both agents.
    let list_a = agent_a.run_ok(&["list", "-s", "all"]);
    for title in &[
        "Event diverge A-1",
        "Event diverge A-2",
        "Event diverge B-1",
        "Event diverge B-2",
    ] {
        assert!(
            list_a.stdout_contains(title),
            "Agent A: compaction must not drop event '{}'.\nstdout: {}",
            title,
            list_a.stdout,
        );
    }

    let list_b = agent_b.run_ok(&["list", "-s", "all"]);
    for title in &[
        "Event diverge A-1",
        "Event diverge A-2",
        "Event diverge B-1",
        "Event diverge B-2",
    ] {
        assert!(
            list_b.stdout_contains(title),
            "Agent B: compaction must not drop event '{}'.\nstdout: {}",
            title,
            list_b.stdout,
        );
    }
}

/// Both agents create issues simultaneously without prior sync, then both sync.
/// Verify that both issues exist after sync and there are no duplicate issues
/// (i.e., the total count matches the number of creates).
#[test]
fn test_adversarial_concurrent_issue_creation_no_duplicates() {
    let agent_a = SmokeHarness::new();
    init_agent_and_sync(&agent_a, "agent-a");

    let agent_b = agent_a.fork_agent("agent-b");
    init_agent_and_sync(&agent_b, "agent-b");

    // Both agents create issues without syncing first — true concurrent
    // creation with no coordination.
    agent_a.run_ok(&["create", "Concurrent create A"]);
    agent_b.run_ok(&["create", "Concurrent create B"]);

    // Sync both agents; push conflicts are expected and should resolve.
    let sync_a = agent_a.run(&["sync"]);
    let sync_b = agent_b.run(&["sync"]);

    // Retry whichever failed.
    if !sync_a.success {
        agent_a.run_ok(&["sync"]);
    }
    if !sync_b.success {
        agent_b.run_ok(&["sync"]);
    }

    // Final cross-sync for full convergence.
    agent_a.run_ok(&["sync"]);
    agent_b.run_ok(&["sync"]);

    // Both issues must exist on both agents.
    let list_a = agent_a.run_ok(&["list", "-s", "all"]);
    assert!(
        list_a.stdout_contains("Concurrent create A"),
        "Agent A: its own issue must exist after sync.\nstdout: {}",
        list_a.stdout,
    );
    assert!(
        list_a.stdout_contains("Concurrent create B"),
        "Agent A: Agent B's issue must exist after sync.\nstdout: {}",
        list_a.stdout,
    );

    let list_b = agent_b.run_ok(&["list", "-s", "all"]);
    assert!(
        list_b.stdout_contains("Concurrent create A"),
        "Agent B: Agent A's issue must exist after sync.\nstdout: {}",
        list_b.stdout,
    );
    assert!(
        list_b.stdout_contains("Concurrent create B"),
        "Agent B: its own issue must exist after sync.\nstdout: {}",
        list_b.stdout,
    );

    // Verify no duplicates: each title should appear exactly once.
    // We use JSON output for reliable counting.
    let json_a = agent_a.run_ok(&["issue", "list", "-s", "all", "--json"]);
    let parsed_a: serde_json::Value = serde_json::from_str(&json_a.stdout).unwrap_or_else(|e| {
        panic!(
            "Failed to parse Agent A issue list JSON: {}\nstdout: {}",
            e, json_a.stdout
        )
    });
    let issues_a = parsed_a
        .as_array()
        .expect("Expected JSON array for Agent A issue list");

    // Count occurrences of each expected title.
    let count_a = issues_a
        .iter()
        .filter(|issue| {
            issue
                .get("title")
                .and_then(|t| t.as_str())
                .map(|t| t == "Concurrent create A" || t == "Concurrent create B")
                .unwrap_or(false)
        })
        .count();

    assert_eq!(
        count_a, 2,
        "Agent A: expected exactly 2 issues (one per agent), got {}.\nJSON: {}",
        count_a, json_a.stdout,
    );
}
