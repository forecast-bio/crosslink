#![no_main]

//! Fuzz target for CLI output functions.
//!
//! This tests the presentation layer (list, show, etc.) which handles
//! string truncation and formatting. The goal is to catch panics from
//! improper UTF-8 handling like byte slicing on multi-byte characters.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use tempfile::tempdir;

use crosslink::db::Database;

#[derive(Arbitrary, Debug)]
struct CliOutputInput {
    /// Issue title - can contain any Unicode
    title: String,
    /// Issue description - can contain any Unicode
    description: Option<String>,
    /// Number of issues to create (for list testing)
    num_issues: u8,
}

fuzz_target!(|input: CliOutputInput| {
    // Limit to reasonable number of issues
    let num_issues = (input.num_issues % 20).max(1);

    let dir = match tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let db_path = dir.path().join("issues.db");

    let db = match Database::open(&db_path) {
        Ok(d) => d,
        Err(_) => return,
    };

    // Create issues with fuzzy titles
    let mut created_ids = Vec::new();
    for i in 0..num_issues {
        let title = if i == 0 {
            input.title.clone()
        } else {
            format!("{} #{}", input.title, i)
        };

        if let Ok(id) = db.create_issue(&title, input.description.as_deref(), "medium") {
            created_ids.push(id);
        }
    }

    // Test list_issues - this exercises truncation
    let _ = db.list_issues(None, None, None);
    let _ = db.list_issues(Some("open"), None, None);
    let _ = db.list_issues(None, None, Some("medium"));

    // Test get_issue - exercises show output
    for id in &created_ids {
        let _ = db.get_issue(*id);
    }

    // Test search - exercises description display
    if !input.title.is_empty() {
        let search_term: String = input.title.chars().take(10).collect();
        if !search_term.is_empty() {
            let _ = db.search_issues(&search_term);
        }
    }

    // Test blocked/ready lists
    if created_ids.len() >= 2 {
        let _ = db.add_dependency(created_ids[0], created_ids[1]);
        let _ = db.list_blocked_issues();
        let _ = db.list_ready_issues();
    }

    // Test comments with Unicode
    if let Some(id) = created_ids.first() {
        if let Some(desc) = &input.description {
            let _ = db.add_comment(*id, desc, "note");
        }
        let _ = db.get_comments(*id);
    }

    // Test labels with Unicode (should be rejected but not panic)
    if let Some(id) = created_ids.first() {
        let label: String = input.title.chars().take(20).collect();
        let _ = db.add_label(*id, &label);
        let _ = db.get_labels(*id);
    }
});
