use std::fs;
use std::path::Path;

use super::harness::{assert_stdout_contains, SmokeHarness};

// ==================== Import/Export Tests ====================

#[test]
fn test_export_empty_db() {
    let h = SmokeHarness::new();
    let export_path = h.temp_dir.path().join("export.json");
    let result = h.run_ok(&["export", "-o", export_path.to_str().unwrap(), "-f", "json"]);

    let content = fs::read_to_string(&export_path).expect("Failed to read export file");
    let parsed: serde_json::Value =
        serde_json::from_str(&content).expect("Export is not valid JSON");
    let arr = parsed.as_array().expect("Export should be a JSON array");
    assert_eq!(arr.len(), 0, "Empty DB should export as empty array []");
    // Verify the output mentions 0 issues (may be on stdout or stderr)
    assert!(
        result.stdout.contains("0 issues") || result.stderr.contains("0 issues"),
        "Expected export to mention 0 issues.\nstdout: {}\nstderr: {}",
        result.stdout,
        result.stderr
    );
}

#[test]
fn test_export_json_format() {
    let h = SmokeHarness::new();

    // Create a few issues with different properties
    h.run_ok(&["create", "First issue", "-p", "high"]);
    h.run_ok(&["create", "Second issue", "-d", "Has a description"]);
    h.run_ok(&["issue", "label", "1", "bug"]);
    h.run_ok(&["issue", "comment", "1", "A comment on issue 1"]);

    let export_path = h.temp_dir.path().join("export.json");
    h.run_ok(&["export", "-o", export_path.to_str().unwrap(), "-f", "json"]);

    let content = fs::read_to_string(&export_path).expect("Failed to read export file");
    let parsed: Vec<serde_json::Value> =
        serde_json::from_str(&content).expect("Export is not valid JSON array");

    assert_eq!(parsed.len(), 2, "Should export 2 issues");

    // Find the first issue by title (order is not guaranteed, and empty Vec fields
    // are omitted by skip_serializing_if)
    let first = parsed
        .iter()
        .find(|i| i["title"].as_str() == Some("First issue"))
        .expect("Should find 'First issue' in export");

    // Check required fields are present
    assert!(first.get("uuid").is_some(), "Issue should have uuid field");
    assert!(
        first.get("title").is_some(),
        "Issue should have title field"
    );
    assert!(
        first.get("priority").is_some(),
        "Issue should have priority field"
    );
    assert!(
        first.get("status").is_some(),
        "Issue should have status field"
    );
    assert!(
        first.get("created_at").is_some(),
        "Issue should have created_at field"
    );
    assert!(
        first.get("updated_at").is_some(),
        "Issue should have updated_at field"
    );

    // Verify specific values
    assert_eq!(first["priority"].as_str().unwrap(), "high");

    // Check labels (present because non-empty)
    let labels = first["labels"]
        .as_array()
        .expect("First issue should have labels field (non-empty)");
    assert!(
        labels.iter().any(|l| l.as_str() == Some("bug")),
        "First issue should have 'bug' label"
    );

    // Check comments (present because non-empty)
    let comments = first["comments"]
        .as_array()
        .expect("First issue should have comments field (non-empty)");
    assert!(
        !comments.is_empty(),
        "First issue should have at least one comment"
    );

    // Check second issue — labels/comments may be omitted when empty
    let second = parsed
        .iter()
        .find(|i| i["title"].as_str() == Some("Second issue"))
        .expect("Should find 'Second issue' in export");
    assert!(
        second.get("uuid").is_some(),
        "Second issue should have uuid"
    );
    assert_eq!(second["status"].as_str().unwrap(), "open");
}

#[test]
fn test_export_markdown_format() {
    let h = SmokeHarness::new();

    h.run_ok(&["create", "Open issue", "-p", "high"]);
    let _ = h.run_ok(&["create", "Closed issue"]);
    h.run_ok(&["close", "2"]);

    let export_path = h.temp_dir.path().join("export.md");
    h.run_ok(&[
        "export",
        "-o",
        export_path.to_str().unwrap(),
        "-f",
        "markdown",
    ]);

    let content = fs::read_to_string(&export_path).expect("Failed to read export file");

    assert!(
        content.contains("# Crosslink Issues Export"),
        "Markdown should have title header"
    );
    assert!(
        content.contains("## Open Issues"),
        "Markdown should have Open Issues section"
    );
    assert!(
        content.contains("## Closed Issues"),
        "Markdown should have Closed Issues section"
    );
    assert!(
        content.contains("Open issue"),
        "Markdown should contain issue title"
    );
    assert!(
        content.contains("Closed issue"),
        "Markdown should contain closed issue title"
    );
    assert!(
        content.contains("**Priority:**"),
        "Markdown should contain priority field"
    );
}

#[test]
fn test_import_boundary_10mb() {
    let h = SmokeHarness::new();

    // Build a JSON array that is just under 10MB (10 * 1024 * 1024 = 10485760 bytes).
    // Each issue must stay under the 64KB description limit, so we use many issues
    // with moderate-length descriptions (~2KB each) to reach ~10MB total.
    // ~10MB / ~2.5KB per issue ~ 4000 issues.
    let desc = "x".repeat(2000); // 2KB description per issue, well under 64KB limit
    let mut issues = String::from("[\n");
    let target_size: usize = 10 * 1024 * 1024 - 4096; // Just under 10MB
    let mut count = 0u32;

    loop {
        if count > 0 {
            issues.push_str(",\n");
        }
        let entry = format!(
            r#"  {{
    "uuid": "00000000-0000-0000-0000-{:012x}",
    "display_id": {},
    "title": "Boundary issue {}",
    "description": "{}",
    "priority": "medium",
    "status": "open",
    "labels": [],
    "comments": [],
    "blockers": [],
    "related": [],
    "milestone_uuid": null,
    "time_entries": [],
    "parent_uuid": null,
    "created_by": "test",
    "created_at": "2026-01-01T00:00:00Z",
    "updated_at": "2026-01-01T00:00:00Z",
    "closed_at": null
  }}"#,
            count + 1,
            count + 1,
            count + 1,
            desc
        );
        if issues.len() + entry.len() + 4 > target_size {
            // Adding this entry would exceed target — add it and stop
            issues.push_str(&entry);
            break;
        }
        issues.push_str(&entry);
        count += 1;
    }
    issues.push_str("\n]");

    // Verify our JSON is under 10MB but substantial
    assert!(
        issues.len() < 10 * 1024 * 1024,
        "Test file should be under 10MB, got {} bytes",
        issues.len()
    );
    assert!(
        issues.len() > 9 * 1024 * 1024,
        "Test file should be close to 10MB, got {} bytes",
        issues.len()
    );

    let import_path = h.temp_dir.path().join("big_import.json");
    fs::write(&import_path, &issues).unwrap();

    let result = h.run_ok(&["import", import_path.to_str().unwrap()]);
    assert_stdout_contains(&result, "imported");
}

#[test]
fn test_import_boundary_over() {
    let h = SmokeHarness::new();

    // Create a file just over 10MB
    let over_size = 10 * 1024 * 1024 + 1024;
    let desc_padding = "x".repeat(over_size);
    let import_json = format!(
        r#"[{{
  "uuid": "00000000-0000-0000-0000-000000000001",
  "display_id": 1,
  "title": "Too big",
  "description": "{}",
  "priority": "medium",
  "status": "open",
  "labels": [],
  "comments": [],
  "blockers": [],
  "related": [],
  "milestone_uuid": null,
  "time_entries": [],
  "parent_uuid": null,
  "created_by": "test",
  "created_at": "2026-01-01T00:00:00Z",
  "updated_at": "2026-01-01T00:00:00Z",
  "closed_at": null
}}]"#,
        desc_padding
    );

    let import_path = h.temp_dir.path().join("too_big.json");
    fs::write(&import_path, &import_json).unwrap();

    let result = h.run_err(&["import", import_path.to_str().unwrap()]);
    assert!(
        result.stderr.contains("limit") || result.stderr.contains("exceeding"),
        "Should mention size limit in error, got stderr: {}",
        result.stderr
    );
}

#[test]
fn test_import_malformed_json() {
    let h = SmokeHarness::new();

    // Truncated JSON — valid start but ends abruptly
    let malformed = r#"[{"uuid": "00000000-0000-0000-0000-000000000001", "title": "Trunc"#;
    let import_path = h.temp_dir.path().join("malformed.json");
    fs::write(&import_path, malformed).unwrap();

    let result = h.run_err(&["import", import_path.to_str().unwrap()]);
    assert!(
        result.stderr.contains("parse")
            || result.stderr.contains("JSON")
            || result.stderr.contains("error"),
        "Should indicate JSON parse error, got stderr: {}",
        result.stderr
    );
}

#[test]
fn test_import_legacy_format() {
    let h = SmokeHarness::new();

    // Legacy ExportData envelope format
    let legacy_json = r#"{
  "version": 1,
  "exported_at": "2026-01-01T00:00:00Z",
  "issues": [
    {
      "id": 1,
      "title": "Legacy issue one",
      "description": null,
      "status": "open",
      "priority": "medium",
      "parent_id": null,
      "labels": ["legacy"],
      "comments": [],
      "created_at": "2026-01-01T00:00:00Z",
      "updated_at": "2026-01-01T00:00:00Z",
      "closed_at": null
    },
    {
      "id": 2,
      "title": "Legacy issue two",
      "description": "With a description",
      "status": "closed",
      "priority": "high",
      "parent_id": null,
      "labels": [],
      "comments": [{"content": "A legacy comment", "created_at": "2026-01-01T00:00:00Z"}],
      "created_at": "2026-01-01T00:00:00Z",
      "updated_at": "2026-01-01T00:00:00Z",
      "closed_at": "2026-01-02T00:00:00Z"
    }
  ]
}"#;

    let import_path = h.temp_dir.path().join("legacy.json");
    fs::write(&import_path, legacy_json).unwrap();

    let result = h.run_ok(&["import", import_path.to_str().unwrap()]);
    assert_stdout_contains(&result, "legacy");

    // Verify issues were actually imported
    let list_result = h.run_ok(&["list", "-s", "all"]);
    assert!(
        list_result.stdout.contains("Legacy issue one"),
        "Should have imported first legacy issue"
    );
    assert!(
        list_result.stdout.contains("Legacy issue two"),
        "Should have imported second legacy issue"
    );
}

#[test]
fn test_import_export_roundtrip() {
    let h = SmokeHarness::new();

    // Create 10 issues with labels and comments
    for i in 1..=10 {
        h.run_ok(&["create", &format!("Roundtrip issue {}", i), "-p", "medium"]);
    }
    // Add labels to some
    h.run_ok(&["issue", "label", "1", "bug"]);
    h.run_ok(&["issue", "label", "2", "feature"]);
    h.run_ok(&["issue", "label", "3", "bug"]);
    // Add comments to some
    h.run_ok(&["issue", "comment", "1", "Comment on issue 1"]);
    h.run_ok(&["issue", "comment", "2", "Comment on issue 2"]);
    h.run_ok(&["issue", "comment", "5", "Comment on issue 5"]);
    // Close a few
    h.run_ok(&["close", "4"]);
    h.run_ok(&["close", "7"]);

    // Export round 1
    let export1_path = h.temp_dir.path().join("export1.json");
    h.run_ok(&["export", "-o", export1_path.to_str().unwrap(), "-f", "json"]);
    let export1 = fs::read_to_string(&export1_path).unwrap();
    let issues1: Vec<serde_json::Value> = serde_json::from_str(&export1).unwrap();
    assert_eq!(issues1.len(), 10);

    // Delete the database and reinitialize
    let db_path = h.db_path();
    fs::remove_file(&db_path).expect("Failed to remove database");

    // Reinitialize (need init again since we deleted the DB)
    h.run_ok(&["init"]);

    // Import
    h.run_ok(&["import", export1_path.to_str().unwrap()]);

    // Export round 2
    let export2_path = h.temp_dir.path().join("export2.json");
    h.run_ok(&["export", "-o", export2_path.to_str().unwrap(), "-f", "json"]);
    let export2 = fs::read_to_string(&export2_path).unwrap();
    let issues2: Vec<serde_json::Value> = serde_json::from_str(&export2).unwrap();

    // Same count
    assert_eq!(
        issues1.len(),
        issues2.len(),
        "Roundtrip should preserve issue count"
    );

    // Verify titles match (order may differ, so collect and sort)
    let mut titles1: Vec<String> = issues1
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_string())
        .collect();
    let mut titles2: Vec<String> = issues2
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_string())
        .collect();
    titles1.sort();
    titles2.sort();
    assert_eq!(titles1, titles2, "Roundtrip should preserve issue titles");

    // Verify statuses match
    let mut statuses1: Vec<String> = issues1
        .iter()
        .map(|i| i["status"].as_str().unwrap().to_string())
        .collect();
    let mut statuses2: Vec<String> = issues2
        .iter()
        .map(|i| i["status"].as_str().unwrap().to_string())
        .collect();
    statuses1.sort();
    statuses2.sort();
    assert_eq!(
        statuses1, statuses2,
        "Roundtrip should preserve issue statuses"
    );

    // Verify labels are preserved
    let find_by_title = |issues: &[serde_json::Value], title: &str| -> serde_json::Value {
        issues
            .iter()
            .find(|i| i["title"].as_str() == Some(title))
            .cloned()
            .unwrap()
    };

    let i1_round2 = find_by_title(&issues2, "Roundtrip issue 1");
    let labels: Vec<String> = i1_round2["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    assert!(
        labels.contains(&"bug".to_string()),
        "Roundtrip should preserve labels"
    );
}

#[test]
fn test_import_orphan_blockers() {
    let h = SmokeHarness::new();

    // Import an issue whose blocker UUID doesn't exist in the import set.
    // The import should handle this gracefully (skip the orphan blocker dependency).
    let import_json = r#"[
  {
    "uuid": "aaaaaaaa-0000-0000-0000-000000000001",
    "display_id": 1,
    "title": "Issue with orphan blocker",
    "description": null,
    "priority": "medium",
    "status": "open",
    "labels": [],
    "comments": [],
    "blockers": ["bbbbbbbb-0000-0000-0000-000000000099"],
    "related": [],
    "milestone_uuid": null,
    "time_entries": [],
    "parent_uuid": null,
    "created_by": "test",
    "created_at": "2026-01-01T00:00:00Z",
    "updated_at": "2026-01-01T00:00:00Z",
    "closed_at": null
  }
]"#;

    let import_path = h.temp_dir.path().join("orphan_blockers.json");
    fs::write(&import_path, import_json).unwrap();

    // This should succeed — orphan blockers are silently skipped
    let result = h.run_ok(&["import", import_path.to_str().unwrap()]);
    assert_stdout_contains(&result, "imported");

    // Verify the issue was created
    let list_result = h.run_ok(&["list"]);
    assert!(
        list_result.stdout.contains("Issue with orphan blocker"),
        "Issue should have been imported despite orphan blocker"
    );
}

// ==================== Archive Tests ====================

#[test]
fn test_archive_lifecycle() {
    let h = SmokeHarness::new();

    // Create and close an issue
    h.run_ok(&["create", "Archive me"]);
    h.run_ok(&["close", "1"]);

    // Archive it
    let result = h.run_ok(&["archive", "add", "1"]);
    assert_stdout_contains(&result, "Archived");

    // List archived — should show our issue
    let list_result = h.run_ok(&["archive", "list"]);
    assert!(
        list_result.stdout.contains("Archive me"),
        "Archived issue should appear in archive list"
    );

    // Should not appear in open or closed lists (archived is a separate status)
    let open_list = h.run_ok(&["list", "-s", "open"]);
    assert!(
        !open_list.stdout.contains("Archive me"),
        "Archived issue should not appear in open list"
    );
    let closed_list_before = h.run_ok(&["list", "-s", "closed"]);
    assert!(
        !closed_list_before.stdout.contains("Archive me"),
        "Archived issue should not appear in closed list"
    );

    // Unarchive it
    let unarchive_result = h.run_ok(&["archive", "remove", "1"]);
    assert_stdout_contains(&unarchive_result, "Unarchived");

    // Should now appear in closed list again
    let closed_list = h.run_ok(&["list", "-s", "closed"]);
    assert!(
        closed_list.stdout.contains("Archive me"),
        "Unarchived issue should appear in closed list"
    );

    // Should not appear in archive list anymore
    let archive_list = h.run_ok(&["archive", "list"]);
    assert!(
        !archive_list.stdout.contains("Archive me"),
        "Unarchived issue should not appear in archive list"
    );
}

#[test]
fn test_archive_open_issue_fails() {
    let h = SmokeHarness::new();

    h.run_ok(&["create", "Open issue"]);

    // Trying to archive an open issue should fail
    let result = h.run_err(&["archive", "add", "1"]);
    assert!(
        result.stderr.contains("closed")
            || result.stderr.contains("only archive closed")
            || result.stderr.contains("Can only archive"),
        "Should indicate only closed issues can be archived, got stderr: {}",
        result.stderr
    );
}

#[test]
fn test_archive_older() {
    let h = SmokeHarness::new();

    // Create and close several issues
    for i in 1..=5 {
        h.run_ok(&["create", &format!("Issue {}", i)]);
    }
    for i in 1..=5 {
        h.run_ok(&["close", &i.to_string()]);
    }

    // Archive issues closed more than 0 days ago — should archive all of them
    let result = h.run_ok(&["archive", "older", "0"]);
    assert!(
        result.stdout.contains("Archived") || result.stdout.contains("archived"),
        "Should indicate issues were archived, got: {}",
        result.stdout
    );

    // Verify all are archived
    let archive_list = h.run_ok(&["archive", "list"]);
    for i in 1..=5 {
        assert!(
            archive_list.stdout.contains(&format!("Issue {}", i)),
            "Issue {} should be in archive list",
            i
        );
    }

    // Open and closed lists should be empty (all issues are now archived)
    let open_list = h.run_ok(&["list", "-s", "open"]);
    assert!(
        open_list.stdout.contains("No issues found"),
        "Open list should show no issues after archiving all, got: {}",
        open_list.stdout
    );
    let closed_list = h.run_ok(&["list", "-s", "closed"]);
    assert!(
        closed_list.stdout.contains("No issues found"),
        "Closed list should show no issues after archiving all, got: {}",
        closed_list.stdout
    );
}

#[test]
fn test_unarchive_not_archived() {
    let h = SmokeHarness::new();

    // Create an issue (open, not archived)
    h.run_ok(&["create", "Not archived"]);

    // Try to unarchive it — should fail
    let result = h.run_err(&["archive", "remove", "1"]);
    assert!(
        result.stderr.contains("not found or not archived")
            || result.stderr.contains("not archived"),
        "Should indicate issue is not archived, got stderr: {}",
        result.stderr
    );
}

// ==================== Knowledge Tests ====================

#[test]
fn test_knowledge_lifecycle() {
    let h = SmokeHarness::new();

    // Add a knowledge page
    let add_result = h.run_ok(&[
        "knowledge",
        "add",
        "test-page",
        "--tag",
        "testing",
        "--content",
        "This is test content about Rust programming.",
    ]);
    assert_stdout_contains(&add_result, "Created knowledge page");

    // Show the page
    let show_result = h.run_ok(&["knowledge", "show", "test-page"]);
    assert!(
        show_result.stdout.contains("test content")
            || show_result.stdout.contains("Rust programming"),
        "Show should display page content, got: {}",
        show_result.stdout
    );

    // List pages — should include our page
    let list_result = h.run_ok(&["knowledge", "list"]);
    assert!(
        list_result.stdout.contains("test-page"),
        "List should include our page, got: {}",
        list_result.stdout
    );

    // Edit the page — append content
    let edit_result = h.run_ok(&[
        "knowledge",
        "edit",
        "test-page",
        "--append",
        "Additional notes about testing.",
    ]);
    assert_stdout_contains(&edit_result, "Updated knowledge page");

    // Search for content
    let search_result = h.run_ok(&["knowledge", "search", "Rust programming"]);
    assert!(
        search_result.stdout.contains("test-page"),
        "Search should find our page by content, got: {}",
        search_result.stdout
    );

    // Remove the page
    let remove_result = h.run_ok(&["knowledge", "remove", "test-page"]);
    assert_stdout_contains(&remove_result, "Removed knowledge page");

    // Verify it's gone
    let show_after = h.run(&["knowledge", "show", "test-page"]);
    assert!(!show_after.success, "Showing removed page should fail");
}

#[test]
fn test_knowledge_slug_traversal() {
    let h = SmokeHarness::new();

    // Attempt path traversal via slug — should be rejected or sanitized
    let traversal_slugs = [
        "../../../etc/passwd",
        "..%2f..%2fetc%2fpasswd",
        "foo/../../bar",
        "test\\..\\..\\etc",
    ];

    for slug in &traversal_slugs {
        let result = h.run(&["knowledge", "add", slug, "--content", "malicious content"]);
        // Should either fail outright or sanitize the slug
        if result.success {
            // If it succeeded, the slug must have been sanitized (no path traversal)
            // Verify no file was written outside the knowledge cache
            let etc_passwd = Path::new("/etc/passwd");
            let content_before = fs::read_to_string(etc_passwd).unwrap_or_default();
            assert!(
                !content_before.contains("malicious content"),
                "Path traversal should not write to /etc/passwd"
            );
        }
        // If it failed, that's the expected behavior for invalid slugs
    }
}

#[test]
fn test_knowledge_edit_append() {
    let h = SmokeHarness::new();

    // Create a page
    h.run_ok(&[
        "knowledge",
        "add",
        "append-test",
        "--content",
        "Original content.",
    ]);

    // Append to it
    h.run_ok(&[
        "knowledge",
        "edit",
        "append-test",
        "--append",
        "Appended section.",
    ]);

    // Verify both original and appended content are present
    let show_result = h.run_ok(&["knowledge", "show", "append-test"]);
    assert!(
        show_result.stdout.contains("Original content"),
        "Should still have original content, got: {}",
        show_result.stdout
    );
    assert!(
        show_result.stdout.contains("Appended section"),
        "Should have appended content, got: {}",
        show_result.stdout
    );
}

#[test]
fn test_knowledge_search_basic() {
    let h = SmokeHarness::new();

    // Create pages with different content
    h.run_ok(&[
        "knowledge",
        "add",
        "alpha-page",
        "--content",
        "Alpha content about quantum mechanics.",
    ]);
    h.run_ok(&[
        "knowledge",
        "add",
        "beta-page",
        "--content",
        "Beta content about classical physics.",
    ]);

    // Search for a term that only appears in one page
    let result = h.run_ok(&["knowledge", "search", "quantum"]);
    assert!(
        result.stdout.contains("alpha-page"),
        "Search should find alpha-page for 'quantum', got: {}",
        result.stdout
    );
    assert!(
        !result.stdout.contains("beta-page"),
        "Search should not find beta-page for 'quantum', got: {}",
        result.stdout
    );
}

#[test]
fn test_knowledge_search_no_match() {
    let h = SmokeHarness::new();

    // Create a page
    h.run_ok(&[
        "knowledge",
        "add",
        "lonely-page",
        "--content",
        "Some ordinary content.",
    ]);

    // Search for something that doesn't exist
    let result = h.run_ok(&["knowledge", "search", "xyzzy_nonexistent_term_42"]);
    assert!(
        result.stdout.contains("No knowledge pages match")
            || result.stdout.contains("no match")
            || !result.stdout.contains("lonely-page"),
        "Search with no matches should indicate no results, got: {}",
        result.stdout
    );
}

#[test]
fn test_knowledge_import_dir() {
    let h = SmokeHarness::new();

    // Create a temp directory with .md files
    let import_dir = h.temp_dir.path().join("md_import");
    fs::create_dir(&import_dir).unwrap();

    fs::write(
        import_dir.join("first-doc.md"),
        "# First Document\n\nContent of the first document.",
    )
    .unwrap();
    fs::write(
        import_dir.join("second-doc.md"),
        "# Second Document\n\nContent of the second document.",
    )
    .unwrap();
    fs::write(
        import_dir.join("not-markdown.txt"),
        "This should be ignored.",
    )
    .unwrap();

    // Import the directory
    let result = h.run_ok(&[
        "knowledge",
        "import",
        import_dir.to_str().unwrap(),
        "--tag",
        "imported",
    ]);

    // Should report import results
    assert!(
        result.stdout.contains("Imported")
            || result.stdout.contains("imported")
            || result.stdout.contains("2"),
        "Should report imported files, got: {}",
        result.stdout
    );

    // Verify the pages exist
    let list_result = h.run_ok(&["knowledge", "list"]);
    assert!(
        list_result.stdout.contains("first-doc"),
        "first-doc should be listed, got: {}",
        list_result.stdout
    );
    assert!(
        list_result.stdout.contains("second-doc"),
        "second-doc should be listed, got: {}",
        list_result.stdout
    );
}

#[test]
fn test_knowledge_remove_nonexistent() {
    let h = SmokeHarness::new();

    // Ensure the knowledge cache is initialized by adding then removing a page,
    // or just try to remove a nonexistent page directly
    let result = h.run(&["knowledge", "remove", "does-not-exist"]);
    assert!(
        !result.success,
        "Removing nonexistent page should fail, got stdout: {} stderr: {}",
        result.stdout, result.stderr
    );
    assert!(
        result.stderr.contains("not found") || result.stderr.contains("Not found"),
        "Error should mention page not found, got stderr: {}",
        result.stderr
    );
}
