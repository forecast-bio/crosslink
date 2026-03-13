// Smoke tests for multi-agent coordination: event sourcing, compaction, lock
// contention, and integrity checks.

use super::harness::SmokeHarness;

/// Initialize an agent identity and hub cache so the SharedWriter, locks, and
/// compact commands work.  Uses `--no-key` to skip SSH key generation.
fn init_agent_and_sync(h: &SmokeHarness, agent_id: &str) {
    h.run_ok(&["agent", "init", agent_id, "--no-key"]);
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
        first.stdout_contains("Compaction complete")
            || first.stdout_contains("compaction"),
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
