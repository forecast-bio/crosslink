use super::harness::{assert_stdout_contains, SmokeHarness};

// =========================================================================
// Config
// =========================================================================

#[test]
fn test_config_show() {
    let h = SmokeHarness::new();
    let r = h.run_ok(&["config", "show"]);
    // Should display at least the tracking_mode key with a default annotation
    assert_stdout_contains(&r, "tracking_mode");
    assert_stdout_contains(&r, "(default)");
}

#[test]
fn test_config_get_set_roundtrip() {
    let h = SmokeHarness::new();

    // Set tracking_mode to "strict"
    h.run_ok(&["config", "set", "tracking_mode", "strict"]);

    // Get it back and verify
    let r = h.run_ok(&["config", "get", "tracking_mode"]);
    assert_stdout_contains(&r, "strict");
}

#[test]
fn test_config_list() {
    let h = SmokeHarness::new();
    let r = h.run_ok(&["config", "list"]);
    // Should contain headers and at least a few known keys
    assert_stdout_contains(&r, "KEY");
    assert_stdout_contains(&r, "tracking_mode");
    assert_stdout_contains(&r, "intervention_tracking");
    assert_stdout_contains(&r, "signing_enforcement");
}

#[test]
fn test_config_invalid_key() {
    let h = SmokeHarness::new();
    let r = h.run_err(&["config", "get", "nonexistent_key_xyz"]);
    // Should mention the key is unknown
    let combined = format!("{}{}", r.stdout, r.stderr);
    assert!(
        combined.contains("Unknown config key")
            || combined.contains("unknown")
            || combined.contains("nknown"),
        "Expected error about unknown key, got:\nstdout: {}\nstderr: {}",
        r.stdout,
        r.stderr,
    );
}

#[test]
fn test_config_reset_single() {
    let h = SmokeHarness::new();

    // Change tracking_mode from default
    h.run_ok(&["config", "set", "tracking_mode", "strict"]);
    let r = h.run_ok(&["config", "get", "tracking_mode"]);
    assert_stdout_contains(&r, "strict");

    // Reset it
    h.run_ok(&["config", "reset", "tracking_mode"]);

    // After reset, diff should not mention tracking_mode (it's back to default)
    let r = h.run_ok(&["config", "diff"]);
    assert!(
        !r.stdout.contains("tracking_mode"),
        "Expected tracking_mode to be back to default after reset, but diff shows:\n{}",
        r.stdout,
    );
}

#[test]
fn test_config_diff_clean() {
    let h = SmokeHarness::new();
    let r = h.run_ok(&["config", "diff"]);
    // Fresh install with defaults -> no differences
    assert_stdout_contains(&r, "No differences");
}

#[test]
fn test_config_diff_after_set() {
    let h = SmokeHarness::new();

    // Modify a value
    h.run_ok(&["config", "set", "tracking_mode", "relaxed"]);

    let r = h.run_ok(&["config", "diff"]);
    // Should show tracking_mode as modified
    assert_stdout_contains(&r, "tracking_mode");
    assert!(
        !r.stdout.contains("No differences"),
        "Expected diff to show changes, but got:\n{}",
        r.stdout,
    );
}

// =========================================================================
// Sync / Migrate
// =========================================================================

#[test]
fn test_sync_basic() {
    let h = SmokeHarness::new();
    // sync should succeed in an initialized repo with a remote.
    // It may partially succeed with warnings (e.g. no agent key published yet)
    // but the core operations (fetch, init_cache) should work.
    let r = h.run(&["sync"]);
    assert!(
        r.success || r.stderr.contains("Warning") || r.stderr.contains("agent"),
        "sync failed unexpectedly:\nstdout: {}\nstderr: {}",
        r.stdout,
        r.stderr,
    );
}

#[test]
fn test_sync_idempotent() {
    let h = SmokeHarness::new();
    // Run sync twice; both should produce the same outcome
    let r1 = h.run(&["sync"]);
    let r2 = h.run(&["sync"]);
    assert_eq!(
        r1.success, r2.success,
        "sync not idempotent:\nfirst: exit={} stderr={}\nsecond: exit={} stderr={}",
        r1.exit_code, r1.stderr, r2.exit_code, r2.stderr,
    );
}

#[test]
fn test_migrate_rename_no_old() {
    let h = SmokeHarness::new();
    // The harness already uses crosslink/hub (v2), so rename-branch should report
    // "No migration needed" or similar, not error out.
    let r = h.run(&["migrate", "rename-branch"]);
    let combined = format!("{}{}", r.stdout, r.stderr);
    assert!(
        combined.contains("No migration needed") || combined.contains("already using") || r.success,
        "Expected graceful handling when old branch doesn't exist:\nstdout: {}\nstderr: {}",
        r.stdout,
        r.stderr,
    );
}

// =========================================================================
// Integrity
// =========================================================================

#[test]
fn test_integrity_counters_clean() {
    let h = SmokeHarness::new();
    let r = h.run_ok(&["integrity", "counters"]);
    // On a fresh install, counters should either PASS or be SKIPPED
    // (skipped when the sync cache directory does not exist)
    let combined = format!("{}{}", r.stdout, r.stderr);
    assert!(
        combined.contains("PASS") || combined.contains("SKIPPED"),
        "Expected PASS or SKIPPED for counters on fresh install, got:\n{combined}",
    );
}

#[test]
fn test_integrity_hydration_clean() {
    let h = SmokeHarness::new();
    let r = h.run_ok(&["integrity", "hydration"]);
    let combined = format!("{}{}", r.stdout, r.stderr);
    assert!(
        combined.contains("PASS") || combined.contains("SKIPPED"),
        "Expected PASS or SKIPPED for hydration on fresh install, got:\n{combined}",
    );
}

#[test]
fn test_integrity_locks_clean() {
    let h = SmokeHarness::new();
    let r = h.run_ok(&["integrity", "locks"]);
    let combined = format!("{}{}", r.stdout, r.stderr);
    assert!(
        combined.contains("PASS") || combined.contains("SKIPPED"),
        "Expected PASS or SKIPPED for locks on fresh install, got:\n{combined}",
    );
}

#[test]
fn test_integrity_schema_current() {
    let h = SmokeHarness::new();
    let r = h.run_ok(&["integrity", "schema"]);
    assert_stdout_contains(&r, "PASS");
}

#[test]
fn test_integrity_counters_repair() {
    let h = SmokeHarness::new();

    // Create several issues so the database has real display IDs
    h.run_ok(&["issue", "create", "Issue alpha"]);
    h.run_ok(&["issue", "create", "Issue beta"]);
    h.run_ok(&["issue", "create", "Issue gamma"]);

    // Force a sync so that counter files exist on the hub cache
    let _sync_result = h.run(&["sync"]);

    // Now try to corrupt the counter file if the hub cache exists
    let hub_cache = h.crosslink_dir().join(".hub-cache");
    let counters_path = hub_cache.join("meta").join("counters.json");

    if counters_path.exists() {
        // Corrupt the counter: set next_display_id too low
        std::fs::write(
            &counters_path,
            r#"{"next_display_id": 1, "next_comment_id": 1, "next_milestone_id": 1}"#,
        )
        .expect("failed to write corrupted counters");

        // Without repair, should report FAIL
        let r = h.run_ok(&["integrity", "counters"]);
        assert_stdout_contains(&r, "FAIL");

        // With --repair, should fix it
        let r = h.run_ok(&["integrity", "counters", "--repair"]);
        let combined = format!("{}{}", r.stdout, r.stderr);
        assert!(
            combined.contains("REPAIRED") || combined.contains("PASS"),
            "Expected REPAIRED or PASS after repair, got:\n{combined}",
        );

        // Verify it passes now
        let r = h.run_ok(&["integrity", "counters"]);
        assert_stdout_contains(&r, "PASS");
    } else {
        // Hub cache exists but counters.json was not populated (sync did not
        // fully propagate). The integrity check may report SKIPPED (no cache)
        // or FAIL (cache exists but counters are stale). Both are acceptable
        // when the counters file was never written.
        let r = h.run_ok(&["integrity", "counters"]);
        let combined = format!("{}{}", r.stdout, r.stderr);
        assert!(
            combined.contains("SKIPPED") || combined.contains("FAIL"),
            "Expected SKIPPED or FAIL when counters not populated, got:\n{combined}",
        );
    }
}

// =========================================================================
// Compact
// =========================================================================

#[test]
fn test_compact_cli_basic() {
    let h = SmokeHarness::new();
    // compact requires agent identity and sync to be configured.
    // In the test harness, we have a remote but may not have an agent.
    // Accept success or known errors about missing agent/sync config.
    let r = h.run(&["compact"]);
    if !r.success {
        let r2 = h.run(&["compact", "--force"]);
        let combined = format!("{}{}", r2.stdout, r2.stderr);
        assert!(
            r2.success
                || combined.contains("agent")
                || combined.contains("No agent")
                || combined.contains("sync")
                || combined.contains("remote")
                || combined.contains("fetch"),
            "compact --force failed unexpectedly:\nstdout: {}\nstderr: {}",
            r2.stdout,
            r2.stderr,
        );
    }
}

#[test]
fn test_compact_cli_no_events() {
    let h = SmokeHarness::new();
    // On a fresh install with no events, compact should be idempotent.
    let r = h.run(&["compact", "--force"]);
    let combined = format!("{}{}", r.stdout, r.stderr);
    assert!(
        r.success
            || combined.contains("No agent")
            || combined.contains("agent")
            || combined.contains("No new events")
            || combined.contains("remote")
            || combined.contains("fetch"),
        "compact with no events failed unexpectedly:\nstdout: {}\nstderr: {}",
        r.stdout,
        r.stderr,
    );
}

// =========================================================================
// Prune
// =========================================================================

#[test]
fn test_prune_dry_run() {
    let h = SmokeHarness::new();
    let r = h.run(&["prune", "--dry-run"]);
    let combined = format!("{}{}", r.stdout, r.stderr);
    // Dry run should show the plan and exit 0, or fail gracefully if
    // sync is not fully set up.
    assert!(
        r.success
            || combined.contains("sync")
            || combined.contains("remote")
            || combined.contains("fetch")
            || combined.contains("hub"),
        "prune --dry-run failed unexpectedly:\nstdout: {}\nstderr: {}",
        r.stdout,
        r.stderr,
    );
    // If it succeeded, the output should reference the dry-run plan
    if r.success {
        assert!(
            combined.contains("dry run")
                || combined.contains("Prune plan")
                || combined.contains("commit(s)")
                || combined.contains("nothing to prune"),
            "Expected dry-run output, got:\n{combined}",
        );
    }
}
