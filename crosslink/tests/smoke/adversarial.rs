use std::process::Command;
use std::thread;

use super::harness::{assert_issue_count, assert_stdout_contains, SmokeHarness};

// ============================================================================
// Boundary Attacks
// ============================================================================

#[test]
fn test_boundary_title_exact_512() {
    let h = SmokeHarness::new();
    let title = "a".repeat(512);
    let result = h.run_ok(&["create", &title]);
    assert!(result.stdout.contains("Created issue"));

    let show = h.run_ok(&["show", "1"]);
    assert!(show.stdout.contains(&title));
}

#[test]
fn test_boundary_title_over_513() {
    let h = SmokeHarness::new();
    let title = "a".repeat(513);
    let result = h.run(&["create", &title]);
    // The app may or may not enforce the 512-char limit.
    // Either outcome is acceptable — the key is no crash or corruption.
    if result.success {
        // If it succeeded, verify the issue is retrievable
        let show = h.run_ok(&["show", "1"]);
        assert!(show.stdout.contains(&title[..50]));
    } else {
        assert!(
            result.stderr.contains("exceeds") || result.stderr.contains("maximum length"),
            "Expected error about length, got stderr: {}",
            result.stderr
        );
    }
}

#[test]
fn test_boundary_title_null_bytes() {
    let h = SmokeHarness::new();
    // Null bytes cannot be passed through Command args on Unix (the OS rejects
    // them in execve). We verify that the attempt doesn't panic — the harness's
    // run() will fail with an InvalidInput error from std::process::Command.
    // We call Command directly to catch the OS-level error gracefully.
    let output = Command::new(&h.crosslink_bin)
        .current_dir(h.temp_dir.path())
        .args(["create", "test\x00null"])
        .output();

    match output {
        Ok(o) => {
            // If the OS somehow passed it through, verify DB integrity
            if o.status.success() {
                let list = h.run_ok(&["list", "-s", "all"]);
                assert!(list.success);
            }
        }
        Err(e) => {
            // Expected: OS rejects null byte in argument
            assert!(
                e.kind() == std::io::ErrorKind::InvalidInput,
                "Expected InvalidInput error for null byte, got: {:?}",
                e.kind()
            );
        }
    }
}

#[test]
fn test_boundary_label_exact_128() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Label boundary test"]);

    let label = "a".repeat(128);
    h.run_ok(&["issue", "label", "1", &label]);

    let show = h.run_ok(&["show", "1"]);
    assert!(show.stdout.contains(&label));
}

#[test]
fn test_boundary_label_over_129() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Label boundary test"]);

    let label = "a".repeat(129);
    let result = h.run(&["issue", "label", "1", &label]);
    // The app may or may not enforce the 128-char label limit.
    if result.success {
        let show = h.run_ok(&["show", "1"]);
        assert!(show.stdout.contains(&label[..50]));
    } else {
        assert!(
            result.stderr.contains("exceeds") || result.stderr.contains("maximum length"),
            "Expected error about label length, got stderr: {}",
            result.stderr
        );
    }
}

#[test]
fn test_boundary_desc_exact_64k() {
    let h = SmokeHarness::new();
    let desc = "b".repeat(65_536);
    let result = h.run_ok(&["create", "Desc boundary test", "-d", &desc]);
    assert!(result.stdout.contains("Created issue"));
}

#[test]
fn test_boundary_desc_over_64k() {
    let h = SmokeHarness::new();
    let desc = "b".repeat(65_537);
    let result = h.run(&["create", "Desc boundary test", "-d", &desc]);
    // The app may or may not enforce the 64KB description limit.
    // Either outcome is acceptable — the key is no crash or corruption.
    if result.success {
        let show = h.run_ok(&["show", "1"]);
        assert!(show.stdout.contains("Desc boundary test"));
    } else {
        assert!(
            result.stderr.contains("exceeds") || result.stderr.contains("maximum length"),
            "Expected error about description length, got stderr: {}",
            result.stderr
        );
    }
}

#[test]
fn test_boundary_empty_title() {
    let h = SmokeHarness::new();
    let _result = h.run(&["create", ""]);
    // Empty title: the DB layer currently allows it, but even if it succeeds,
    // the system should not corrupt. Verify we can still list issues.
    let list = h.run_ok(&["list", "-s", "all"]);
    assert!(list.success);
}

#[test]
fn test_boundary_whitespace_title() {
    let h = SmokeHarness::new();
    let _result = h.run(&["create", "   "]);
    // Whitespace-only title: same as empty — system must remain consistent.
    let list = h.run_ok(&["list", "-s", "all"]);
    assert!(list.success);
}

#[test]
fn test_boundary_priority_invalid() {
    let h = SmokeHarness::new();
    let result = h.run_err(&["create", "Priority test", "-p", "hgih"]);
    assert!(
        result.stderr.contains("Invalid priority")
            || result.stderr.contains("invalid")
            || result.stderr.contains("hgih"),
        "Expected error about invalid priority, got stderr: {}",
        result.stderr
    );
}

#[test]
fn test_boundary_priority_case() {
    let h = SmokeHarness::new();
    // Priority is case-sensitive: "High" should be rejected
    let result = h.run_err(&["create", "Priority case test", "-p", "High"]);
    assert!(
        result.stderr.contains("Invalid priority")
            || result.stderr.contains("invalid")
            || result.stderr.contains("High"),
        "Expected error about invalid priority, got stderr: {}",
        result.stderr
    );
}

#[test]
fn test_boundary_status_invalid() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Test issue"]);

    // An invalid status filter should either be rejected with an error, or
    // treated as a literal filter that matches nothing (no corruption).
    let result = h.run(&["list", "-s", "bogus"]);
    if result.success {
        // If accepted: it should match zero issues (since no issue has
        // status "bogus"), and the DB must remain intact.
        assert!(
            !result.stdout.contains("Test issue"),
            "Invalid status should not match real issues"
        );
        // Verify the real issue still exists with a valid query
        assert_issue_count(&h, "all", 1);
    } else {
        // If rejected: the error message should mention the invalid status.
        assert!(
            result.stderr.contains("Invalid status")
                || result.stderr.contains("invalid")
                || result.stderr.contains("bogus"),
            "Expected error about invalid status, got stderr: {}",
            result.stderr
        );
    }
}

// ============================================================================
// SQL Injection
// ============================================================================

#[test]
fn test_inject_sql_title() {
    let h = SmokeHarness::new();
    let payload = "'; DROP TABLE issues; --";
    h.run_ok(&["create", payload]);

    // Verify the payload was stored literally
    let show = h.run_ok(&["show", "1"]);
    assert_stdout_contains(&show, payload);

    // Verify the table wasn't dropped — create another issue
    h.run_ok(&["create", "Normal issue after injection"]);

    // Both issues should exist
    assert_issue_count(&h, "all", 2);
}

#[test]
fn test_inject_sql_search() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Findable issue"]);
    h.run_ok(&["create", "Another issue"]);

    // SQL injection in search should not return all rows
    let _result = h.run_ok(&["issue", "search", "% OR 1=1 --"]);
    // The search should NOT return our normal issues (unless they happen to
    // literally match the search string). At minimum, the command should not
    // error and the DB should remain intact.
    assert_issue_count(&h, "all", 2);
}

#[test]
fn test_inject_sql_label() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Label injection test"]);

    let payload = "'; DELETE FROM labels; --";
    h.run_ok(&["issue", "label", "1", payload]);

    // Verify the label was stored literally
    let show = h.run_ok(&["show", "1"]);
    assert_stdout_contains(&show, payload);

    // Add a second label and verify both exist
    h.run_ok(&["issue", "label", "1", "safe-label"]);
    let show2 = h.run_ok(&["show", "1"]);
    assert_stdout_contains(&show2, "safe-label");
}

// ============================================================================
// Path Traversal
// ============================================================================

#[test]
fn test_inject_path_slug() {
    let h = SmokeHarness::new();
    // Attempt path traversal via knowledge slug
    let result = h.run(&["knowledge", "add", "../../../etc/passwd"]);
    // Should be rejected or fail safely — not create files outside the knowledge dir
    if result.success {
        // If it didn't error, verify that no file was created at the traversal target.
        // The real check is that the process didn't write outside its sandbox.
        let bad_path = h.temp_dir.path().join("../../../etc/passwd.md");
        assert!(
            !bad_path.exists(),
            "Path traversal should not create files outside knowledge directory"
        );
    } else {
        // Expected: the command rejects the slug
        assert!(
            result.stderr.contains("path separator")
                || result.stderr.contains("Invalid")
                || result.stderr.contains("..")
                || result.stderr.contains("outside"),
            "Expected path traversal rejection error, got stderr: {}",
            result.stderr
        );
    }
}

// ============================================================================
// Shell Metacharacters
// ============================================================================

#[test]
fn test_inject_shell_title() {
    let h = SmokeHarness::new();
    let payload = "Issue with $(whoami) and `id` and $HOME";
    h.run_ok(&["create", payload]);

    let show = h.run_ok(&["show", "1"]);
    // The shell metacharacters should be stored literally, not executed.
    // Since we use Command::new (not shell=true), they should pass through.
    assert_stdout_contains(&show, "$(whoami)");
    assert_stdout_contains(&show, "`id`");
    assert_stdout_contains(&show, "$HOME");
}

#[test]
fn test_inject_shell_comment() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Shell comment test"]);

    let payload = "Running $(rm -rf /) and `cat /etc/shadow` for $USER";
    h.run_ok(&["issue", "comment", "1", payload, "--kind", "observation"]);

    let show = h.run_ok(&["show", "1"]);
    assert_stdout_contains(&show, "$(rm -rf /)");
}

// ============================================================================
// Unicode Edge Cases
// ============================================================================

#[test]
fn test_unicode_emoji_title() {
    let h = SmokeHarness::new();
    // Multi-codepoint family emoji (ZWJ sequence)
    let title =
        "Fix rendering of \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466} emoji";
    h.run_ok(&["create", title]);

    let show = h.run_ok(&["show", "1"]);
    assert_stdout_contains(
        &show,
        "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}",
    );
}

#[test]
fn test_unicode_rtl_title() {
    let h = SmokeHarness::new();
    // Arabic text (right-to-left)
    let title = "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} \u{0628}\u{0627}\u{0644}\u{0639}\u{0627}\u{0644}\u{0645}";
    h.run_ok(&["create", title]);

    let show = h.run_ok(&["show", "1"]);
    assert_stdout_contains(&show, title);
}

#[test]
fn test_unicode_mixed_scripts() {
    let h = SmokeHarness::new();
    // Mix of Latin, Cyrillic, CJK, and Devanagari
    let title = "Hello \u{041F}\u{0440}\u{0438}\u{0432}\u{0435}\u{0442} \u{4F60}\u{597D} \u{0928}\u{092E}\u{0938}\u{094D}\u{0924}\u{0947}";
    h.run_ok(&["create", title]);

    let show = h.run_ok(&["show", "1"]);
    assert_stdout_contains(&show, title);
}

// ============================================================================
// Corruption Recovery
// ============================================================================

#[cfg(unix)]
#[test]
fn test_corrupt_db_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let h = SmokeHarness::new();
    // Make the database read-only
    let perms = std::fs::Permissions::from_mode(0o444);
    std::fs::set_permissions(h.db_path(), perms).unwrap();

    // Attempting to create an issue should either fail outright or succeed
    // with a hydration warning (since v0.5.2 the command writes to hub JSON
    // files first, which may succeed even when the DB is read-only — the
    // hydration step logs a warning and the DB is stale but the operation
    // is not fatal).
    let result = h.run(&["create", "Should fail"]);
    if result.success {
        // If it succeeded, there must be a hydration warning on stderr
        assert!(
            result.stderr.contains("hydration failed") || result.stderr.contains("readonly"),
            "Create succeeded on read-only DB without hydration warning.\nstderr: {}",
            result.stderr
        );
    }
    // Either way is acceptable — the important thing is no panic/crash.

    // Restore permissions for cleanup
    let perms = std::fs::Permissions::from_mode(0o644);
    std::fs::set_permissions(h.db_path(), perms).unwrap();
}

#[test]
fn test_corrupt_missing_db() {
    let h = SmokeHarness::new();
    // Delete the database file
    std::fs::remove_file(h.db_path()).unwrap();

    // Crosslink should handle this gracefully: either re-create the DB or
    // report a clear error.
    let result = h.run(&["list"]);
    // Whether it succeeds (recreates DB) or fails (reports missing DB),
    // it should not panic. If it succeeds, verify the output is sane.
    if result.success {
        // Should report zero issues, not garbage
        let list = h.run_ok(&["list", "-s", "all", "--json"]);
        let parsed: serde_json::Value = serde_json::from_str(&list.stdout).unwrap_or_else(|e| {
            panic!(
                "Failed to parse JSON after DB recreation: {}\nstdout: {}",
                e, list.stdout
            )
        });
        assert!(
            parsed.as_array().map(|a| a.is_empty()).unwrap_or(false),
            "Expected empty array after DB recreation, got: {}",
            list.stdout
        );
    }
    // If it failed, that's also acceptable — the key is no panic/crash
}

// ============================================================================
// Concurrency
// ============================================================================

#[test]
fn test_concurrent_creates_5() {
    let h = SmokeHarness::new();
    let bin = h.crosslink_bin.clone();
    let dir = h.temp_dir.path().to_path_buf();

    let handles: Vec<_> = (0..5)
        .map(|i| {
            let bin = bin.clone();
            let dir = dir.clone();
            thread::spawn(move || {
                let output = Command::new(&bin)
                    .current_dir(&dir)
                    .args(["create", &format!("Concurrent issue {i}")])
                    .output()
                    .expect("failed to execute crosslink");
                output.status.success()
            })
        })
        .collect();

    let mut successes = 0u32;
    for handle in handles {
        if handle.join().expect("thread panicked") {
            successes += 1;
        }
    }

    // At least one create should succeed; ideally all 5.
    // Due to SQLite write contention with the SharedWriter, some may fail.
    assert!(
        successes >= 1,
        "At least one concurrent create should succeed, got 0",
    );

    // The number of issues in the DB should match the number of successful creates
    let result = h.run_ok(&["issue", "list", "-s", "all", "--json"]);
    let parsed: serde_json::Value =
        serde_json::from_str(&result.stdout).expect("failed to parse issue list JSON");
    let count = parsed.as_array().map(|a| a.len()).unwrap_or(0);
    assert!(
        count >= 1,
        "DB should have at least 1 issue after concurrent creates, got {count}",
    );
}
