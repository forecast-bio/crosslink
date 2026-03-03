#![no_main]

//! Fuzz target for milestone operations.
//!
//! Tests create_milestone, close_milestone, delete_milestone,
//! add/remove issues to milestones, and listing operations.

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use tempfile::tempdir;

use crosslink::db::Database;

#[derive(Arbitrary, Debug, Clone)]
enum MilestoneOp {
    CreateMilestone { name: String, description: Option<String> },
    CloseMilestone { idx: usize },
    DeleteMilestone { idx: usize },
    AddIssue { milestone_idx: usize, issue_idx: usize },
    RemoveIssue { milestone_idx: usize, issue_idx: usize },
    ListMilestones,
    GetMilestoneIssues { idx: usize },
    GetIssueMilestone { issue_idx: usize },
    CreateIssue { title: String },
}

#[derive(Arbitrary, Debug)]
struct MilestoneInput {
    ops: Vec<MilestoneOp>,
}

fuzz_target!(|input: MilestoneInput| {
    let dir = match tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let db_path = dir.path().join("issues.db");

    let db = match Database::open(&db_path) {
        Ok(d) => d,
        Err(_) => return,
    };

    let mut milestone_ids: Vec<i64> = Vec::new();
    let mut issue_ids: Vec<i64> = Vec::new();

    for op in input.ops.iter().take(50) {
        match op {
            MilestoneOp::CreateMilestone { name, description } => {
                if let Ok(id) = db.create_milestone(name, description.as_deref()) {
                    milestone_ids.push(id);
                }
            }
            MilestoneOp::CloseMilestone { idx } => {
                if !milestone_ids.is_empty() {
                    let id = milestone_ids[*idx % milestone_ids.len()];
                    let _ = db.close_milestone(id);
                }
            }
            MilestoneOp::DeleteMilestone { idx } => {
                if !milestone_ids.is_empty() {
                    let idx_val = *idx % milestone_ids.len();
                    let id = milestone_ids[idx_val];
                    if db.delete_milestone(id).is_ok() {
                        milestone_ids.remove(idx_val);
                    }
                }
            }
            MilestoneOp::AddIssue { milestone_idx, issue_idx } => {
                if !milestone_ids.is_empty() && !issue_ids.is_empty() {
                    let mid = milestone_ids[*milestone_idx % milestone_ids.len()];
                    let iid = issue_ids[*issue_idx % issue_ids.len()];
                    let _ = db.add_issue_to_milestone(mid, iid);
                }
            }
            MilestoneOp::RemoveIssue { milestone_idx, issue_idx } => {
                if !milestone_ids.is_empty() && !issue_ids.is_empty() {
                    let mid = milestone_ids[*milestone_idx % milestone_ids.len()];
                    let iid = issue_ids[*issue_idx % issue_ids.len()];
                    let _ = db.remove_issue_from_milestone(mid, iid);
                }
            }
            MilestoneOp::ListMilestones => {
                let _ = db.list_milestones(None);
                let _ = db.list_milestones(Some("open"));
                let _ = db.list_milestones(Some("closed"));
            }
            MilestoneOp::GetMilestoneIssues { idx } => {
                if !milestone_ids.is_empty() {
                    let id = milestone_ids[*idx % milestone_ids.len()];
                    let _ = db.get_milestone_issues(id);
                }
            }
            MilestoneOp::GetIssueMilestone { issue_idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*issue_idx % issue_ids.len()];
                    let _ = db.get_issue_milestone(id);
                }
            }
            MilestoneOp::CreateIssue { title } => {
                if let Ok(id) = db.create_issue(title, None, "medium") {
                    issue_ids.push(id);
                }
            }
        }
    }

    // Final consistency checks
    let _ = db.list_milestones(None);
    let _ = db.list_issues(None, None, None);
});
