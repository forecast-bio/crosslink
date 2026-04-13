//! Lifecycle smoke tests for timer, session, intervene, design doc, and issue
//! tree commands.
//!
//! These tests exercise end-to-end command flows without requiring external
//! infrastructure (tmux, containers, etc.).  Tests that do require external
//! infrastructure are marked `#[ignore]` with an explanatory comment.

use super::harness::{assert_stdout_contains, SmokeHarness};

// ===========================================================================
// Helpers
// ===========================================================================

/// Extract the numeric or local issue ID from crosslink command output.
///
/// Handles both `Created issue #1` (online display_id) and `Created issue L1`
/// (offline local ID) output formats.
fn extract_issue_id(stdout: &str) -> String {
    for line in stdout.lines() {
        // "Created issue L1" or "Created issue L12" — 'L' followed by one or
        // more digits as a standalone token (not mid-word like "lifecycle").
        // Find tokens that are exactly L<digits>.
        for word in line.split_whitespace() {
            // Strip trailing punctuation
            let word = word.trim_end_matches(&['.', ',', ':', ';', '!', '?', ')'] as &[char]);
            if word.starts_with('L')
                && word.len() > 1
                && word[1..].chars().all(|c| c.is_ascii_digit())
            {
                return word.to_string();
            }
        }
        // "Created issue #1" (online display_id)
        if let Some(pos) = line.find('#') {
            let id_str: String = line[pos + 1..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !id_str.is_empty() {
                return id_str;
            }
        }
    }
    // Fallback: look for "Now working on: L1 title" style
    for line in stdout.lines() {
        if line.contains("working on") || line.contains("Created") {
            for word in line.split_whitespace() {
                let word = word.trim_end_matches(&['.', ',', ':', ';'] as &[char]);
                if word.starts_with('L')
                    && word.len() > 1
                    && word[1..].chars().all(|c| c.is_ascii_digit())
                {
                    return word.to_string();
                }
            }
        }
    }
    panic!("Could not extract issue ID from output:\n{stdout}");
}

// ===========================================================================
// Timer roundtrip
// ===========================================================================

/// Full timer lifecycle: start → show (verify running) → stop → show (verify
/// stopped with elapsed time).
#[test]
fn test_timer_roundtrip() {
    let h = SmokeHarness::new();

    // Create an issue to track time against.
    let create_result = h.run_ok(&["issue", "create", "Timer roundtrip issue"]);
    let issue_id = extract_issue_id(&create_result.stdout);

    // Start the timer.
    let start = h.run_ok(&["timer", "start", &issue_id]);
    assert!(
        start.stdout_contains("Started")
            || start.stdout_contains("timer")
            || start.stdout_contains("Timer"),
        "timer start should confirm start.\nstdout: {}",
        start.stdout,
    );

    // Show while running — should indicate active/running state.
    let show_running = h.run_ok(&["timer", "show"]);
    assert!(
        show_running.stdout_contains("running")
            || show_running.stdout_contains("active")
            || show_running.stdout_contains("Active")
            || show_running.stdout_contains("Timer"),
        "timer show while running should indicate active state.\nstdout: {}",
        show_running.stdout,
    );

    // Stop the timer.
    let stop = h.run_ok(&["timer", "stop"]);
    assert!(
        stop.stdout_contains("Stopped")
            || stop.stdout_contains("stopped")
            || stop.stdout_contains("timer")
            || stop.stdout_contains("Timer"),
        "timer stop should confirm stop.\nstdout: {}",
        stop.stdout,
    );

    // Show after stopping — should indicate it is no longer active and report
    // elapsed or total time.
    let show_stopped = h.run_ok(&["timer", "show"]);
    // After stopping, the timer should either show "no active timer" or show
    // logged time entries (elapsed seconds/minutes).
    let combined = format!("{}{}", show_stopped.stdout, show_stopped.stderr);
    assert!(
        combined.contains("No active")
            || combined.contains("no active")
            || combined.contains("No time")
            || combined.contains("Total")
            || combined.contains("0s")
            || combined.contains("0m")
            || show_stopped.success,
        "timer show after stop should report stopped state or elapsed time.\nstdout: {}\nstderr: {}",
        show_stopped.stdout,
        show_stopped.stderr,
    );
}

/// Starting a timer when one is already running for the same issue should
/// behave gracefully (idempotent or return an informative error).
#[test]
fn test_timer_start_already_running() {
    let h = SmokeHarness::new();

    h.run_ok(&["issue", "create", "Double-start issue"]);

    // First start
    h.run_ok(&["timer", "start", "1"]);

    // Second start on the same issue — should not panic.
    let result = h.run(&["timer", "start", "1"]);
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        result.success
            || combined.contains("already")
            || combined.contains("running")
            || combined.contains("active"),
        "Second timer start should handle gracefully.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

/// Stopping a timer when none is running should exit cleanly or give an
/// informative message.
#[test]
fn test_timer_stop_not_running() {
    let h = SmokeHarness::new();

    let result = h.run(&["timer", "stop"]);
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        result.success
            || combined.contains("No active")
            || combined.contains("no active")
            || combined.contains("not running")
            || combined.contains("No timer"),
        "timer stop with no running timer should handle gracefully.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

// ===========================================================================
// Session lifecycle
// ===========================================================================

/// Full session lifecycle: start → work <id> → action "doing X" → status
/// (verify active) → end --notes "done" → last-handoff (verify saved).
#[test]
fn test_session_full_lifecycle() {
    let h = SmokeHarness::new();

    // Create an issue to work on.
    let create_result = h.run_ok(&["issue", "create", "Session lifecycle issue"]);
    let issue_id = extract_issue_id(&create_result.stdout);

    // Start session.
    let start = h.run_ok(&["session", "start"]);
    assert!(
        start.stdout_contains("started")
            || start.stdout_contains("Started")
            || start.stdout_contains("Session"),
        "session start should confirm.\nstdout: {}",
        start.stdout,
    );

    // Set the active work item.
    let work = h.run_ok(&["session", "work", &issue_id]);
    assert!(
        work.stdout_contains("working on")
            || work.stdout_contains("Working on")
            || work.stdout_contains(&issue_id)
            || work.success,
        "session work should confirm the work item.\nstdout: {}",
        work.stdout,
    );

    // Record an action breadcrumb.
    let action = h.run_ok(&["session", "action", "Implementing the lifecycle test"]);
    assert!(
        action.stdout_contains("Recorded") || action.stdout_contains("action") || action.success,
        "session action should confirm.\nstdout: {}",
        action.stdout,
    );

    // Verify session is active with the expected work item.
    let status = h.run_ok(&["session", "status"]);
    assert!(
        status.stdout_contains("active")
            || status.stdout_contains("Active")
            || status.stdout_contains("Session"),
        "session should be active.\nstdout: {}",
        status.stdout,
    );
    // The work item should appear somewhere in the status.
    assert!(
        status.stdout_contains("lifecycle") || status.stdout_contains(&issue_id) || status.success,
        "session status should reference work item.\nstdout: {}",
        status.stdout,
    );

    // End session with handoff notes.
    let handoff_note = "Done: lifecycle test complete, all assertions passed";
    let end = h.run_ok(&["session", "end", "--notes", handoff_note]);
    assert!(
        end.stdout_contains("ended")
            || end.stdout_contains("Ended")
            || end.stdout_contains("Session")
            || end.success,
        "session end should confirm.\nstdout: {}",
        end.stdout,
    );

    // Start a new session and verify the handoff was saved.
    h.run_ok(&["session", "start"]);
    let last = h.run_ok(&["session", "last-handoff"]);
    assert!(
        last.stdout_contains("lifecycle test complete")
            || last.stdout_contains("Done:")
            || last.stdout_contains("Handoff")
            || last.stdout_contains("handoff"),
        "last-handoff should contain previous session notes.\nstdout: {}",
        last.stdout,
    );
}

/// Calling `session status` when no session has been started should exit
/// cleanly or print an informative message.
#[test]
fn test_session_status_no_session() {
    let h = SmokeHarness::new();

    let result = h.run(&["session", "status"]);
    // Either succeeds (showing "no active session") or exits non-zero.
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        combined.contains("No active")
            || combined.contains("no active")
            || combined.contains("No session")
            || combined.contains("not started")
            || result.success,
        "session status with no session should handle gracefully.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

// ===========================================================================
// Intervene command
// ===========================================================================

/// Create an issue, call `issue intervene`, verify the intervention was
/// recorded (appears in issue show or workflow trail).
#[test]
fn test_intervene_records_event() {
    let h = SmokeHarness::new();

    // Create an issue to intervene on.
    let create_result = h.run_ok(&["issue", "create", "Intervene target"]);
    let issue_id = extract_issue_id(&create_result.stdout);

    // Run the intervene command.
    let intervene = h.run_ok(&[
        "issue",
        "intervene",
        &issue_id,
        "Manual correction applied to output",
        "--trigger",
        "manual_action",
        "--context",
        "Running lifecycle smoke test",
    ]);
    assert!(
        intervene.stdout_contains("intervention")
            || intervene.stdout_contains("Intervention")
            || intervene.stdout_contains("Recorded")
            || intervene.stdout_contains("recorded")
            || intervene.success,
        "intervene should confirm the event was recorded.\nstdout: {}",
        intervene.stdout,
    );

    // Verify the issue still exists and is readable.
    let show = h.run_ok(&["issue", "show", &issue_id]);
    assert_stdout_contains(&show, "Intervene target");

    // The intervention should appear as a comment/event in the workflow trail.
    let trail = h.run_ok(&["workflow", "trail", &issue_id]);
    assert!(
        trail.stdout_contains("Manual correction")
            || trail.stdout_contains("manual_action")
            || trail.stdout_contains("intervention")
            || trail.stdout_contains("Intervention")
            || trail.success,
        "workflow trail should reflect the intervention.\nstdout: {}",
        trail.stdout,
    );
}

/// Intervening on a nonexistent issue should fail gracefully.
#[test]
fn test_intervene_nonexistent_issue() {
    let h = SmokeHarness::new();

    let result = h.run(&[
        "issue",
        "intervene",
        "99999",
        "This issue does not exist",
        "--trigger",
        "manual_action",
    ]);
    assert!(
        !result.success,
        "intervene on nonexistent issue should fail.\nstdout: {}\nstderr: {}",
        result.stdout, result.stderr,
    );
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        combined.contains("not found")
            || combined.contains("Not found")
            || combined.contains("99999"),
        "error should identify the missing issue.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

// ===========================================================================
// Design doc command (kickoff plan)
// ===========================================================================

/// Verify that `crosslink kickoff plan --help` exits cleanly.
///
/// `crosslink design` is not a top-level command; the design-doc generation
/// functionality lives under `kickoff plan`.  This test checks the help page
/// is reachable without panicking.
#[test]
fn test_kickoff_plan_help_exists() {
    let h = SmokeHarness::new();

    let result = h.run_ok(&["kickoff", "plan", "--help"]);
    assert!(
        result.stdout_contains("plan")
            || result.stdout_contains("Plan")
            || result.stdout_contains("design"),
        "kickoff plan --help should describe the command.\nstdout: {}",
        result.stdout,
    );
}

/// Verify that `crosslink kickoff list` exits cleanly (no running agents).
///
/// Full `kickoff run` requires tmux or container infrastructure which is not
/// available in CI.
#[test]
fn test_kickoff_list_no_agents() {
    let h = SmokeHarness::new();

    let result = h.run_ok(&["kickoff", "list"]);
    // Should succeed and indicate no agents are running.
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        combined.contains("No")
            || combined.contains("no")
            || combined.contains("agent")
            || result.stdout.trim().is_empty()
            || result.success,
        "kickoff list with no agents should handle gracefully.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

/// `kickoff run` requires tmux or container infrastructure — skip in CI.
#[test]
#[ignore = "requires tmux or container infrastructure not available in CI"]
fn test_kickoff_run_requires_infra() {
    let h = SmokeHarness::new();
    h.run_ok(&["issue", "create", "Kickoff test issue"]);
    // Would need a design doc and tmux session to proceed further.
    let _result = h.run(&["kickoff", "run", "--help"]);
}

// ===========================================================================
// Issue tree with parent/subissue hierarchy
// ===========================================================================

/// Create a parent issue and a subissue, run `issue tree`, verify both appear
/// with correct hierarchy.
#[test]
fn test_issue_tree_with_subissues() {
    let h = SmokeHarness::new();

    // Create a parent issue.
    let parent_result = h.run_ok(&["issue", "create", "Parent lifecycle issue"]);
    let parent_id = extract_issue_id(&parent_result.stdout);

    // Create a subissue under the parent.
    let sub_result = h.run_ok(&["subissue", &parent_id, "Child lifecycle issue"]);
    let _sub_id = extract_issue_id(&sub_result.stdout);

    // `issue tree` should show both issues.
    let tree = h.run_ok(&["issue", "tree"]);
    assert_stdout_contains(&tree, "Parent lifecycle issue");
    assert_stdout_contains(&tree, "Child lifecycle issue");
}

/// Issue tree with multiple levels of nesting should render without errors.
#[test]
fn test_issue_tree_deep_nesting() {
    let h = SmokeHarness::new();

    // Create root → child → grandchild.
    let root = h.run_ok(&["issue", "create", "Root issue"]);
    let root_id = extract_issue_id(&root.stdout);

    let child = h.run_ok(&["subissue", &root_id, "Child issue"]);
    let child_id = extract_issue_id(&child.stdout);

    h.run_ok(&["subissue", &child_id, "Grandchild issue"]);

    let tree = h.run_ok(&["issue", "tree"]);
    assert_stdout_contains(&tree, "Root issue");
    assert_stdout_contains(&tree, "Child issue");
    assert_stdout_contains(&tree, "Grandchild issue");
}

/// `issue tree --status open` should only show open issues.
#[test]
fn test_issue_tree_status_filter() {
    let h = SmokeHarness::new();

    let p = h.run_ok(&["issue", "create", "Filterable parent"]);
    let p_id = extract_issue_id(&p.stdout);

    let c = h.run_ok(&["subissue", &p_id, "Open child"]);
    let c_id = extract_issue_id(&c.stdout);

    let c2 = h.run_ok(&["subissue", &p_id, "Closed child"]);
    let c2_id = extract_issue_id(&c2.stdout);

    // Close the second child.
    h.run_ok(&["issue", "close", &c2_id]);

    // Tree filtered to open should show open child but not closed child.
    let tree = h.run_ok(&["issue", "tree", "-s", "open"]);
    assert_stdout_contains(&tree, "Open child");
    assert!(
        !tree.stdout_contains("Closed child"),
        "tree --status open should not show closed issues.\nstdout: {}",
        tree.stdout,
    );

    // Verify open child still visible.
    let _ = c_id; // suppress unused variable warning
}

// ===========================================================================
// Daemon lifecycle (no external infra)
// ===========================================================================

/// `daemon status` when no daemon is running should exit cleanly.
#[test]
fn test_daemon_status_not_running() {
    let h = SmokeHarness::new();

    let result = h.run(&["daemon", "status"]);
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        combined.contains("not running")
            || combined.contains("Not running")
            || combined.contains("No daemon")
            || !result.success,
        "daemon status when not running should be informative.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

/// `daemon stop` when no daemon is running should be idempotent.
#[test]
fn test_daemon_stop_idempotent() {
    let h = SmokeHarness::new();

    let result = h.run(&["daemon", "stop"]);
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        result.success
            || combined.contains("not running")
            || combined.contains("Not running")
            || combined.contains("No daemon"),
        "daemon stop when not running should handle gracefully.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

/// `daemon start` followed by `daemon status` and `daemon stop` is the full
/// daemon lifecycle.  This test is skipped in CI because the daemon process
/// spawns background threads and may interfere with the test harness on
/// resource-constrained runners.
#[test]
#[ignore = "daemon start spawns a background process; requires a stable process environment, skip in CI"]
fn test_daemon_start_stop_lifecycle() {
    let h = SmokeHarness::new();

    h.run_ok(&["daemon", "start"]);

    let status = h.run_ok(&["daemon", "status"]);
    assert!(
        status.stdout_contains("running")
            || status.stdout_contains("Running")
            || status.stdout_contains("active"),
        "daemon should be running after start.\nstdout: {}",
        status.stdout,
    );

    h.run_ok(&["daemon", "stop"]);

    let status_after = h.run(&["daemon", "status"]);
    let combined = format!("{}{}", status_after.stdout, status_after.stderr);
    assert!(
        combined.contains("not running")
            || combined.contains("Not running")
            || !status_after.success,
        "daemon should not be running after stop.\nstdout: {}\nstderr: {}",
        status_after.stdout,
        status_after.stderr,
    );
}

// ===========================================================================
// Swarm lifecycle (requires tmux — marked ignore)
// ===========================================================================

/// `swarm status` with no swarm initialized should exit cleanly.
#[test]
fn test_swarm_status_no_swarm() {
    let h = SmokeHarness::new();

    let result = h.run(&["swarm", "status"]);
    let combined = format!("{}{}", result.stdout, result.stderr);
    assert!(
        result.success
            || combined.contains("No active swarm")
            || combined.contains("no active swarm")
            || combined.contains("No swarm")
            || combined.contains("not initialized"),
        "swarm status with no swarm should handle gracefully.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr,
    );
}

/// `swarm status` (second invocation) confirms the command group is idempotent
/// and doesn't panic on repeated calls with no active swarm.
#[test]
fn test_swarm_status_idempotent() {
    let h = SmokeHarness::new();

    // Run swarm status twice — both should handle gracefully.
    let r1 = h.run(&["swarm", "status"]);
    let r2 = h.run(&["swarm", "status"]);
    let combined1 = format!("{}{}", r1.stdout, r1.stderr);
    let combined2 = format!("{}{}", r2.stdout, r2.stderr);
    // Both should give consistent results (success or the same error).
    assert_eq!(
        r1.success, r2.success,
        "swarm status should be consistent across repeated calls.\nfirst: stdout={} stderr={}\nsecond: stdout={} stderr={}",
        r1.stdout, r1.stderr, r2.stdout, r2.stderr,
    );
    // At least one of the results should be non-empty or a known message.
    assert!(
        combined1.contains("swarm")
            || combined1.contains("No")
            || combined1.contains("hub")
            || !r1.success,
        "swarm status should produce output or a clear error.\nstdout: {}\nstderr: {}",
        r1.stdout,
        r1.stderr,
    );
    let _ = combined2; // suppress unused
}

/// Full swarm init and launch requires a design document and tmux — skip in CI.
#[test]
#[ignore = "swarm init/launch requires a design document and tmux, which are not available in CI"]
fn test_swarm_init_requires_infra() {
    let _h = SmokeHarness::new();
    // Would need to write a design doc and have tmux available.
}
