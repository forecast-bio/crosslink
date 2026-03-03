#![no_main]

//! Fuzz target for issue relation operations.
//!
//! Tests add_relation, remove_relation, get_related_issues with arbitrary
//! relation graphs including self-relations, duplicates, and relations
//! on deleted/closed issues.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use tempfile::tempdir;

use crosslink::db::Database;

#[derive(Arbitrary, Debug, Clone)]
enum RelationOp {
    CreateIssue { title: String },
    AddRelation { idx_a: usize, idx_b: usize },
    RemoveRelation { idx_a: usize, idx_b: usize },
    GetRelated { idx: usize },
    CloseIssue { idx: usize },
    DeleteIssue { idx: usize },
}

#[derive(Arbitrary, Debug)]
struct RelationInput {
    ops: Vec<RelationOp>,
}

fuzz_target!(|input: RelationInput| {
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
            RelationOp::CreateIssue { title } => {
                if let Ok(id) = db.create_issue(title, None, "medium") {
                    issue_ids.push(id);
                }
            }
            RelationOp::AddRelation { idx_a, idx_b } => {
                if issue_ids.len() >= 2 {
                    let a = issue_ids[*idx_a % issue_ids.len()];
                    let b = issue_ids[*idx_b % issue_ids.len()];
                    // Should handle self-relations and duplicates gracefully
                    let _ = db.add_relation(a, b);
                }
            }
            RelationOp::RemoveRelation { idx_a, idx_b } => {
                if issue_ids.len() >= 2 {
                    let a = issue_ids[*idx_a % issue_ids.len()];
                    let b = issue_ids[*idx_b % issue_ids.len()];
                    let _ = db.remove_relation(a, b);
                }
            }
            RelationOp::GetRelated { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.get_related_issues(id);
                    let _ = db.get_related_issue_ids(id);
                }
            }
            RelationOp::CloseIssue { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.close_issue(id);
                }
            }
            RelationOp::DeleteIssue { idx } => {
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
        let _ = db.get_related_issues(*id);
    }
});
