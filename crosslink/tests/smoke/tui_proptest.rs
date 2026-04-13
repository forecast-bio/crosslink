use super::harness::{assert_stdout_contains, SmokeHarness};

/// Count issues by parsing JSON output from `crosslink list -s <status> --json`.
///
/// This is a local helper that uses the flat CLI syntax (`list` not `issue list`)
/// to work with the current version of the binary. The harness's
/// `assert_issue_count` uses `issue list` which requires the subcommand form.
fn count_issues(h: &SmokeHarness, status: &str) -> usize {
    let result = h.run_ok(&["list", "-s", status, "--json"]);
    let parsed: serde_json::Value = serde_json::from_str(&result.stdout).unwrap_or_else(|e| {
        panic!(
            "failed to parse issue list JSON: {}\nstdout was:\n{}",
            e, result.stdout
        )
    });
    parsed
        .as_array()
        .map(|a| a.len())
        .unwrap_or_else(|| panic!("expected JSON array, got: {}", result.stdout))
}

fn assert_issue_count_flat(h: &SmokeHarness, status: &str, expected: usize) {
    let count = count_issues(h, status);
    assert_eq!(
        count, expected,
        "expected {expected} issues with status {status:?}, got {count}"
    );
}

// ==================== TUI Rendering (via CLI) ====================

#[test]
fn test_tui_help() {
    let h = SmokeHarness::new();
    let result = h.run_ok(&["tui", "--help"]);
    assert_stdout_contains(&result, "Interactive terminal dashboard");
    assert_stdout_contains(&result, "Usage:");
}

// ==================== Proptest-style Roundtrip Tests ====================

#[test]
fn test_roundtrip_create_show() {
    let h = SmokeHarness::new();
    let titles = [
        "Simple title",
        "Title with 'quotes' and \"double quotes\"",
        "Title with special chars: @#$%^&*()",
        "Unicode: cafe\u{0301} re\u{0301}sume\u{0301} nai\u{0308}ve",
        "Very long title that goes on and on and should still be stored correctly even when it contains many words and reaches a significant length",
    ];
    for (i, title) in titles.iter().enumerate() {
        h.run_ok(&["create", title]);
        let result = h.run_ok(&["show", &(i + 1).to_string()]);
        assert!(
            result.stdout_contains(title),
            "show for issue {} didn't contain title {:?}.\nGot: {}",
            i + 1,
            title,
            result.stdout
        );
    }
}

#[test]
fn test_roundtrip_label_list() {
    let h = SmokeHarness::new();

    // Create issues with different labels
    let labels = ["bug", "feature", "docs", "ci", "refactor"];
    for (i, label) in labels.iter().enumerate() {
        h.run_ok(&["create", &format!("Issue for label {label}")]);
        h.run_ok(&["issue", "label", &(i + 1).to_string(), label]);
    }

    // Verify each label filter returns the correct issue
    for label in &labels {
        let result = h.run_ok(&["list", "-l", label]);
        assert!(
            result.stdout_contains(&format!("Issue for label {label}")),
            "list with -l {} didn't contain expected issue.\nGot: {}",
            label,
            result.stdout
        );
    }

    // Verify a non-existent label returns no issues
    let result = h.run_ok(&["list", "-l", "nonexistent"]);
    assert!(
        result.stdout_contains("No issues found"),
        "list with non-existent label should show no issues.\nGot: {}",
        result.stdout
    );
}

#[test]
fn test_roundtrip_comment_trail() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Comment trail test"]);

    // Add typed comments covering the main kinds
    let comments = [
        ("Planning the approach", "plan"),
        ("Decided on strategy X", "decision"),
        ("Found a bottleneck", "observation"),
        ("Blocked on dependency", "blocker"),
        ("Resolved the bottleneck", "resolution"),
        ("Final outcome recorded", "result"),
    ];

    for (text, kind) in &comments {
        h.run_ok(&["issue", "comment", "1", text, "--kind", kind]);
    }

    // Verify workflow trail contains all comments
    let result = h.run_ok(&["workflow", "trail", "1"]);
    for (text, kind) in &comments {
        assert!(
            result.stdout_contains(text),
            "trail missing comment text {:?}.\nGot: {}",
            text,
            result.stdout
        );
        assert!(
            result.stdout_contains(&format!("[{kind}]")),
            "trail missing kind tag [{}].\nGot: {}",
            kind,
            result.stdout
        );
    }
}

#[test]
fn test_roundtrip_export_import() {
    let h = SmokeHarness::new();
    // Create issues with labels and comments
    for i in 1..=5 {
        h.run_ok(&["create", &format!("Export issue {i}"), "-p", "medium"]);
        h.run_ok(&["issue", "label", &i.to_string(), "test-label"]);
        h.run_ok(&[
            "issue",
            "comment",
            &i.to_string(),
            &format!("Comment on {i}"),
        ]);
    }

    // Export
    let export_path = h.temp_dir.path().join("export.json");
    h.run_ok(&["export", "-f", "json", "-o", export_path.to_str().unwrap()]);

    // Verify export file exists and is valid JSON
    assert!(export_path.exists(), "export file was not created");
    let content = std::fs::read_to_string(&export_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("export JSON is invalid: {e}\ncontent: {content}"));
    assert_eq!(
        parsed.as_array().map(|a| a.len()),
        Some(5),
        "expected 5 issues in export"
    );

    // Import into a fresh harness
    let h2 = SmokeHarness::new();
    let import_path = h2.temp_dir.path().join("import.json");
    std::fs::copy(&export_path, &import_path).unwrap();
    h2.run_ok(&["import", import_path.to_str().unwrap()]);

    // Verify all 5 issues exist
    assert_issue_count_flat(&h2, "all", 5);

    // Verify titles survived the roundtrip
    let list_result = h2.run_ok(&["list", "-s", "all"]);
    for i in 1..=5 {
        assert!(
            list_result.stdout_contains(&format!("Export issue {i}")),
            "imported issue {} missing from list.\nGot: {}",
            i,
            list_result.stdout
        );
    }
}

#[test]
fn test_roundtrip_milestone_issues() {
    let h = SmokeHarness::new();
    h.run_ok(&["sync"]);

    // Create a milestone
    h.run_ok(&["milestone", "create", "v1.0-test"]);

    // Create issues and assign them to the milestone
    let issue_titles = [
        "Milestone issue alpha",
        "Milestone issue beta",
        "Milestone issue gamma",
    ];
    for (i, title) in issue_titles.iter().enumerate() {
        h.run_ok(&["create", title]);
        h.run_ok(&["milestone", "add", "1", &(i + 1).to_string()]);
    }

    // Show milestone and verify all issues are listed
    let result = h.run_ok(&["milestone", "show", "1"]);
    assert_stdout_contains(&result, "v1.0-test");
    for title in &issue_titles {
        assert!(
            result.stdout_contains(title),
            "milestone show missing issue {:?}.\nGot: {}",
            title,
            result.stdout
        );
    }
    assert_stdout_contains(&result, "0/3");
}

// ==================== Edge Cases (Proptest Regression Style) ====================

#[test]
fn test_regression_empty_description() {
    let h = SmokeHarness::new();
    // Creating an issue with an empty description should succeed
    let result = h.run_ok(&["create", "Empty desc issue", "-d", ""]);
    assert_stdout_contains(&result, "Created issue #1");

    // Show should work without crashing
    let show = h.run_ok(&["show", "1"]);
    assert_stdout_contains(&show, "Empty desc issue");
}

#[test]
fn test_regression_single_char_label() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Single char label test"]);

    // Single-character label should work
    h.run_ok(&["issue", "label", "1", "a"]);

    let result = h.run_ok(&["show", "1"]);
    assert_stdout_contains(&result, "a");

    // Listing by single-char label should also work
    let list_result = h.run_ok(&["list", "-l", "a"]);
    assert_stdout_contains(&list_result, "Single char label test");
}

#[test]
fn test_regression_large_id_show() {
    let h = SmokeHarness::new();

    // Showing a non-existent large ID should fail gracefully, not panic
    let result = h.run_err(&["show", "1000"]);
    assert!(
        result.stderr_contains("not found") || result.stdout_contains("not found"),
        "expected 'not found' error for non-existent ID 1000.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr
    );
}

#[test]
fn test_regression_special_search() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Normal issue one"]);
    h.run_ok(&["create", "Normal issue two"]);

    // Searching for "%" should not return all issues via SQL LIKE injection
    let result = h.run(&["issue", "search", "%"]);
    // The search should either return no results or handle the special char safely
    // It should NOT panic
    assert!(
        result.success,
        "search with '%' should not cause a crash.\nstderr: {}",
        result.stderr
    );
}

#[test]
fn test_regression_subissue_chain() {
    let h = SmokeHarness::new();

    // Create parent -> child -> grandchild chain
    h.run_ok(&["create", "Chain parent"]);
    h.run_ok(&["issue", "create", "Chain child", "--parent", "1"]);
    h.run_ok(&["issue", "create", "Chain grandchild", "--parent", "2"]);

    // Tree should show all three with hierarchy
    let result = h.run_ok(&["issue", "tree"]);
    assert_stdout_contains(&result, "Chain parent");
    assert_stdout_contains(&result, "Chain child");
    assert_stdout_contains(&result, "Chain grandchild");

    // Verify the indentation implies hierarchy (child is indented under parent)
    let lines: Vec<&str> = result.stdout.lines().collect();
    let parent_line = lines.iter().position(|l| l.contains("Chain parent"));
    let child_line = lines
        .iter()
        .position(|l| l.contains("Chain child") && !l.contains("Chain parent"));
    let grandchild_line = lines.iter().position(|l| l.contains("Chain grandchild"));

    assert!(parent_line.is_some(), "parent not found in tree");
    assert!(child_line.is_some(), "child not found in tree");
    assert!(grandchild_line.is_some(), "grandchild not found in tree");

    // Verify ordering: parent appears before child, child before grandchild
    assert!(
        parent_line.unwrap() < child_line.unwrap(),
        "parent should appear before child in tree"
    );
    assert!(
        child_line.unwrap() < grandchild_line.unwrap(),
        "child should appear before grandchild in tree"
    );
}

// ==================== Stress/Scale Tests ====================

#[test]
fn test_scale_50_issues() {
    let h = SmokeHarness::new();
    for i in 1..=50 {
        h.run_ok(&["create", &format!("Scale test issue {i}")]);
    }
    assert_issue_count_flat(&h, "all", 50);

    // Spot-check a few specific issues exist in the list
    let result = h.run_ok(&["list", "-s", "all"]);
    assert_stdout_contains(&result, "Scale test issue 1");
    assert_stdout_contains(&result, "Scale test issue 25");
    assert_stdout_contains(&result, "Scale test issue 50");
}

#[test]
fn test_scale_many_labels() {
    let h = SmokeHarness::new();
    h.run_ok(&["create", "Many labels issue"]);

    // Add 20 different labels
    let labels: Vec<String> = (1..=20).map(|i| format!("label-{i}")).collect();
    for label in &labels {
        h.run_ok(&["issue", "label", "1", label]);
    }

    // Show should list all 20 labels
    let result = h.run_ok(&["show", "1"]);
    for label in &labels {
        assert!(
            result.stdout_contains(label),
            "show missing label {:?}.\nGot: {}",
            label,
            result.stdout
        );
    }
}

#[test]
fn test_scale_deep_subissues_10() {
    let h = SmokeHarness::new();

    // Create a 10-level deep subissue chain
    h.run_ok(&["create", "Depth level 1"]);
    for depth in 2..=10 {
        h.run_ok(&[
            "issue",
            "create",
            &format!("Depth level {depth}"),
            "--parent",
            &(depth - 1).to_string(),
        ]);
    }

    // Tree should render all 10 levels without crashing
    let result = h.run_ok(&["issue", "tree"]);
    for depth in 1..=10 {
        assert!(
            result.stdout_contains(&format!("Depth level {depth}")),
            "tree missing depth level {}.\nGot: {}",
            depth,
            result.stdout
        );
    }

    // Verify the tree has progressive indentation
    let lines: Vec<&str> = result
        .stdout
        .lines()
        .filter(|l| l.contains("Depth level"))
        .collect();
    assert_eq!(
        lines.len(),
        10,
        "expected 10 depth levels in tree, got {}",
        lines.len()
    );

    // Each successive line should have more leading whitespace than the previous
    for i in 1..lines.len() {
        let prev_indent = lines[i - 1].len() - lines[i - 1].trim_start().len();
        let curr_indent = lines[i].len() - lines[i].trim_start().len();
        assert!(
            curr_indent > prev_indent,
            "depth level {} not more indented than level {}.\nline {}: {:?}\nline {}: {:?}",
            i + 1,
            i,
            i,
            lines[i - 1],
            i + 1,
            lines[i]
        );
    }
}

#[test]
fn test_scale_comments_20() {
    let h = SmokeHarness::new();

    let kinds = [
        "note",
        "plan",
        "decision",
        "observation",
        "blocker",
        "resolution",
        "result",
    ];
    for i in 1..=20 {
        let kind = kinds[(i - 1) % kinds.len()];
        h.run_ok(&["create", &format!("Issue for comment {i}")]);
        h.run_ok(&[
            "issue",
            "comment",
            &i.to_string(),
            &format!("Comment number {i} of twenty"),
            "--kind",
            kind,
        ]);
    }

    for i in 1..=20 {
        let result = h.run_ok(&["workflow", "trail", &i.to_string()]);
        assert!(
            result.stdout_contains(&format!("Comment number {i} of twenty")),
            "trail for issue {} missing comment.\nGot: {}",
            i,
            result.stdout
        );
    }
}
