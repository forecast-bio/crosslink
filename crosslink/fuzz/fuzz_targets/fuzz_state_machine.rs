#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use tempfile::tempdir;

use crosslink::db::Database;

#[derive(Arbitrary, Debug, Clone)]
enum StateOp {
    // Issue lifecycle
    CreateIssue { title: String, priority: String },
    CloseIssue { idx: usize },
    ReopenIssue { idx: usize },
    ArchiveIssue { idx: usize },
    UnarchiveIssue { idx: usize },
    DeleteIssue { idx: usize },
    // Session lifecycle
    StartSession,
    EndSession { notes: Option<String> },
    SetSessionIssue { idx: usize },
    // Timer lifecycle
    StartTimer { idx: usize },
    StopTimer { idx: usize },
    // Queries (should never panic)
    GetCurrentSession,
    GetActiveTimer,
    ListIssues,
    ListArchived,
}

#[derive(Arbitrary, Debug)]
struct StateMachineInput {
    ops: Vec<StateOp>,
}

fuzz_target!(|input: StateMachineInput| {
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
    let mut session_id: Option<i64> = None;

    for op in input.ops.iter().take(100) {
        match op {
            StateOp::CreateIssue { title, priority } => {
                if let Ok(id) = db.create_issue(title, None, priority) {
                    issue_ids.push(id);
                }
            }
            StateOp::CloseIssue { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.close_issue(id);
                }
            }
            StateOp::ReopenIssue { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.reopen_issue(id);
                }
            }
            StateOp::ArchiveIssue { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.archive_issue(id);
                }
            }
            StateOp::UnarchiveIssue { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.unarchive_issue(id);
                }
            }
            StateOp::DeleteIssue { idx } => {
                if !issue_ids.is_empty() {
                    let idx_val = *idx % issue_ids.len();
                    let id = issue_ids[idx_val];
                    if db.delete_issue(id).is_ok() {
                        issue_ids.remove(idx_val);
                    }
                }
            }
            StateOp::StartSession => {
                if let Ok(id) = db.start_session_with_agent(None) {
                    session_id = Some(id);
                }
            }
            StateOp::EndSession { notes } => {
                if let Some(sid) = session_id {
                    let _ = db.end_session(sid, notes.as_deref());
                    session_id = None;
                }
            }
            StateOp::SetSessionIssue { idx } => {
                if let Some(sid) = session_id {
                    if !issue_ids.is_empty() {
                        let id = issue_ids[*idx % issue_ids.len()];
                        let _ = db.set_session_issue(sid, id);
                    }
                }
            }
            StateOp::StartTimer { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.start_timer(id);
                }
            }
            StateOp::StopTimer { idx } => {
                if !issue_ids.is_empty() {
                    let id = issue_ids[*idx % issue_ids.len()];
                    let _ = db.stop_timer(id);
                }
            }
            StateOp::GetCurrentSession => {
                let _ = db.get_current_session_for_agent(None);
            }
            StateOp::GetActiveTimer => {
                let _ = db.get_active_timer();
            }
            StateOp::ListIssues => {
                let _ = db.list_issues(None, None, None);
            }
            StateOp::ListArchived => {
                let _ = db.list_archived_issues();
            }
        }
    }

    // Final consistency checks - should never panic
    let _ = db.get_current_session_for_agent(None);
    let _ = db.get_active_timer();
    let _ = db.list_issues(None, None, None);
    let _ = db.list_archived_issues();
});
