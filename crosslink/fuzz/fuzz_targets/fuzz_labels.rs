#![no_main]

//! Fuzz target for label operations.
//!
//! Tests add_label, remove_label, get_labels with arbitrary Unicode label
//! names including edge cases like empty strings, very long labels, and
//! special characters.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use tempfile::tempdir;

use crosslink::db::Database;

#[derive(Arbitrary, Debug)]
struct LabelInput {
    labels: Vec<String>,
    remove_indices: Vec<u8>,
}

fuzz_target!(|input: LabelInput| {
    let dir = match tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let db_path = dir.path().join("issues.db");

    let db = match Database::open(&db_path) {
        Ok(d) => d,
        Err(_) => return,
    };

    let issue_id = match db.create_issue("Fuzz label test", None, "medium") {
        Ok(id) => id,
        Err(_) => return,
    };

    // Add arbitrary labels (limit to 20 to prevent timeout)
    let labels: Vec<&String> = input.labels.iter().take(20).collect();
    for label in &labels {
        let _ = db.add_label(issue_id, label);
    }

    // Check labels after adding
    let _ = db.get_labels(issue_id);

    // Remove some labels
    for idx in input.remove_indices.iter().take(10) {
        if !labels.is_empty() {
            let label = &labels[(*idx as usize) % labels.len()];
            let _ = db.remove_label(issue_id, label);
        }
    }

    // Check labels after removal
    let _ = db.get_labels(issue_id);

    // Try adding duplicate labels
    if let Some(label) = labels.first() {
        let _ = db.add_label(issue_id, label);
        let _ = db.add_label(issue_id, label);
    }

    // Try operations on nonexistent issue
    if let Some(label) = labels.first() {
        let _ = db.add_label(999999, label);
        let _ = db.remove_label(999999, label);
    }
    let _ = db.get_labels(999999);

    // Test listing issues filtered by label
    if let Some(label) = labels.first() {
        let label_str: &str = label;
        let _ = db.list_issues(None, Some(label_str), None);
    }
});
