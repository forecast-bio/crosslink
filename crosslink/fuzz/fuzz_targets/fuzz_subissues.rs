#![no_main]

//! Fuzz target for subissue (parent-child) operations.
//!
//! Tests create_subissue, get_subissues, update_parent with arbitrary
//! nesting and reparenting sequences.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use tempfile::tempdir;

use crosslink::db::Database;

#[derive(Arbitrary, Debug, Clone)]
enum SubissueOp {
    CreateRoot { title: String },
    CreateSubissue { parent_idx: usize, title: String },
    Reparent { child_idx: usize, new_parent_idx: usize },
    Unparent { child_idx: usize },
    GetSubissues { parent_idx: usize },
    CloseIssue { idx: usize },
    DeleteIssue { idx: usize },
}

#[derive(Arbitrary, Debug)]
struct SubissueInput {
    ops: Vec<SubissueOp>,
}

fuzz_target!(|input: SubissueInput| {
    let dir = match tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let db_path = dir.path().join("issues.db");

    let db = match Database::open(&db_path) {
        Ok(d) => d,
        Err(_) => return,
    };

    let mut issue_ids: Vec<i64> = Vec::new();

    for op in input.ops.iter().take(50) {
        match op {
            SubissueOp::CreateRoot { title } => {
                if let Ok(id) = db.create_issue(title, None, "medium") {
                    issue_ids.push(id);
                }
            }
            SubissueOp::CreateSubissue { parent_idx, title } => {
                if !issue_ids.is_empty() {
                    let parent_id = issue_ids[*parent_idx % issue_ids.len()];
                    if let Ok(id) = db.create_subissue(parent_id, title, None, "medium") {
                        issue_ids.push(id);
                    }
                }
            }
            SubissueOp::Reparent { child_idx, new_parent_idx } => {
                if issue_ids.len() >= 2 {
                    let child_id = issue_ids[*child_idx % issue_ids.len()];
                    let parent_id = issue_ids[*new_parent_idx % issue_ids.len()];
                    let _ = db.update_parent(child_id, Some(parent_id));
                }
            }
            SubissueOp::Unparent { child_idx } => {
                if !issue_ids.is_empty() {
                    let child_id = issue_ids[*child_idx % issue_ids.len()];
                    let _ = db.update_parent(child_id, None);
                }
            }
            SubissueOp::GetSubissues { parent_idx } => {
                if !issue_ids.is_empty() {
                    let parent_id = issue_ids[*parent_idx % issue_ids.len()];
                    let _ = db.get_subissues(parent_id);
                }
            }
            SubissueOp::CloseIssue { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.close_issue(id);
                }
            }
            SubissueOp::DeleteIssue { idx } => {
                if !issue_ids.is_empty() {
                    let idx_val = *idx % issue_ids.len();
                    let id = issue_ids[idx_val];
                    if db.delete_issue(id).is_ok() {
                        issue_ids.remove(idx_val);
                    }
                }
            }
        }
    }

    // Final consistency checks
    let _ = db.list_issues(None, None, None);
    for id in &issue_ids {
        let _ = db.get_issue(*id);
        let _ = db.get_subissues(*id);
    }
});
