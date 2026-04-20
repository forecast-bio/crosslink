use crate::db::*;
use crate::models::{IssueStatus, Priority};
use chrono::Utc;
use rusqlite::params;
use tempfile::tempdir;

fn setup_test_db() -> (Database, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Database::open(&db_path).unwrap();
    (db, dir)
}

// ==================== Issue CRUD Tests ====================

#[test]
fn test_create_and_get_issue() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();
    assert!(id > 0);

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.id, id);
    assert_eq!(issue.title, "Test issue");
    assert_eq!(issue.description, None);
    assert_eq!(issue.status, IssueStatus::Open);
    assert_eq!(issue.priority, Priority::Medium);
    assert_eq!(issue.parent_id, None);
    assert!(issue.closed_at.is_none());
}

#[test]
fn test_create_issue_with_description() {
    let (db, _dir) = setup_test_db();

    let id = db
        .create_issue("Test issue", Some("Detailed description"), "high")
        .unwrap();
    let issue = db.get_issue(id).unwrap().unwrap();

    assert_eq!(issue.title, "Test issue");
    assert_eq!(issue.description, Some("Detailed description".to_string()));
    assert_eq!(issue.priority, Priority::High);
}

#[test]
fn test_create_subissue() {
    let (db, _dir) = setup_test_db();

    let parent_id = db.create_issue("Parent issue", None, "high").unwrap();
    let child_id = db
        .create_subissue(parent_id, "Child issue", None, "medium")
        .unwrap();

    let child = db.get_issue(child_id).unwrap().unwrap();
    assert_eq!(child.parent_id, Some(parent_id));

    let subissues = db.get_subissues(parent_id).unwrap();
    assert_eq!(subissues.len(), 1);
    assert_eq!(subissues[0].id, child_id);
}

#[test]
fn test_get_nonexistent_issue() {
    let (db, _dir) = setup_test_db();
    let issue = db.get_issue(99999).unwrap();
    assert!(issue.is_none());
}

#[test]
fn test_list_issues() {
    let (db, _dir) = setup_test_db();

    db.create_issue("Issue 1", None, "low").unwrap();
    db.create_issue("Issue 2", None, "medium").unwrap();
    db.create_issue("Issue 3", None, "high").unwrap();

    let issues = db.list_issues(None, None, None).unwrap();
    assert_eq!(issues.len(), 3);
}

#[test]
fn test_list_issues_filter_by_status() {
    let (db, _dir) = setup_test_db();

    let id1 = db.create_issue("Open issue", None, "low").unwrap();
    let id2 = db.create_issue("To be closed", None, "medium").unwrap();
    db.close_issue(id2).unwrap();

    let open_issues = db.list_issues(Some("open"), None, None).unwrap();
    assert_eq!(open_issues.len(), 1);
    assert_eq!(open_issues[0].id, id1);

    let closed_issues = db.list_issues(Some("closed"), None, None).unwrap();
    assert_eq!(closed_issues.len(), 1);
    assert_eq!(closed_issues[0].id, id2);

    let all_issues = db.list_issues(Some("all"), None, None).unwrap();
    assert_eq!(all_issues.len(), 2);
}

#[test]
fn test_list_issues_filter_by_priority() {
    let (db, _dir) = setup_test_db();

    db.create_issue("Low priority", None, "low").unwrap();
    db.create_issue("High priority", None, "high").unwrap();

    let high_issues = db.list_issues(None, None, Some("high")).unwrap();
    assert_eq!(high_issues.len(), 1);
    assert_eq!(high_issues[0].priority, Priority::High);
}

#[test]
fn test_update_issue() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Original title", None, "low").unwrap();

    let updated = db
        .update_issue(
            id,
            Some("Updated title"),
            Some("New description"),
            Some("critical"),
        )
        .unwrap();
    assert!(updated);

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.title, "Updated title");
    assert_eq!(issue.description, Some("New description".to_string()));
    assert_eq!(issue.priority, Priority::Critical);
}

#[test]
fn test_update_issue_partial() {
    let (db, _dir) = setup_test_db();

    let id = db
        .create_issue("Original title", Some("Original desc"), "low")
        .unwrap();

    db.update_issue(id, Some("New title"), None, None).unwrap();

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.title, "New title");
    assert_eq!(issue.description, Some("Original desc".to_string()));
    assert_eq!(issue.priority, Priority::Low);
}

#[test]
fn test_close_and_reopen_issue() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();

    let closed = db.close_issue(id).unwrap();
    assert!(closed);

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.status, IssueStatus::Closed);
    assert!(issue.closed_at.is_some());

    let reopened = db.reopen_issue(id).unwrap();
    assert!(reopened);

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.status, IssueStatus::Open);
    assert!(issue.closed_at.is_none());
}

#[test]
fn test_close_nonexistent_issue_returns_false() {
    let (db, _dir) = setup_test_db();

    // Closing an issue that doesn't exist should return false
    let closed = db.close_issue(99999).unwrap();
    assert!(
        !closed,
        "close_issue should return false for nonexistent issue"
    );
}

#[test]
fn test_reopen_nonexistent_issue_returns_false() {
    let (db, _dir) = setup_test_db();

    // Reopening an issue that doesn't exist should return false
    let reopened = db.reopen_issue(99999).unwrap();
    assert!(
        !reopened,
        "reopen_issue should return false for nonexistent issue"
    );
}

#[test]
fn test_delete_issue() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("To delete", None, "low").unwrap();
    assert!(db.get_issue(id).unwrap().is_some());

    let deleted = db.delete_issue(id).unwrap();
    assert!(deleted);
    assert!(db.get_issue(id).unwrap().is_none());
}

#[test]
fn test_delete_nonexistent_issue() {
    let (db, _dir) = setup_test_db();
    let deleted = db.delete_issue(99999).unwrap();
    assert!(!deleted);
}

// ==================== Labels Tests ====================

#[test]
fn test_add_and_get_labels() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();

    db.add_label(id, "bug").unwrap();
    db.add_label(id, "urgent").unwrap();

    let labels = db.get_labels(id).unwrap();
    assert_eq!(labels.len(), 2);
    assert!(labels.contains(&"bug".to_string()));
    assert!(labels.contains(&"urgent".to_string()));
}

#[test]
fn test_add_duplicate_label_returns_false() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();

    // First add should return true (label was added)
    let first = db.add_label(id, "bug").unwrap();
    assert!(first, "First add_label should return true");

    // Second add should return false (duplicate, nothing inserted)
    let second = db.add_label(id, "bug").unwrap();
    assert!(!second, "Duplicate add_label should return false");

    let labels = db.get_labels(id).unwrap();
    assert_eq!(labels.len(), 1);
}

#[test]
fn test_remove_label() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();

    db.add_label(id, "bug").unwrap();
    db.add_label(id, "urgent").unwrap();

    let removed = db.remove_label(id, "bug").unwrap();
    assert!(removed);

    let labels = db.get_labels(id).unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0], "urgent");
}

#[test]
fn test_remove_nonexistent_label_returns_false() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();
    db.add_label(id, "bug").unwrap();

    // Removing a label that doesn't exist should return false
    let removed = db.remove_label(id, "nonexistent").unwrap();
    assert!(
        !removed,
        "remove_label should return false for nonexistent label"
    );
}

#[test]
fn test_list_issues_filter_by_label() {
    let (db, _dir) = setup_test_db();

    let id1 = db.create_issue("Bug issue", None, "high").unwrap();
    let id2 = db.create_issue("Feature issue", None, "medium").unwrap();

    db.add_label(id1, "bug").unwrap();
    db.add_label(id2, "feature").unwrap();

    let bug_issues = db.list_issues(None, Some("bug"), None).unwrap();
    assert_eq!(bug_issues.len(), 1);
    assert_eq!(bug_issues[0].id, id1);
}

// ==================== Comments Tests ====================

#[test]
fn test_add_and_get_comments() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();

    let comment_id = db.add_comment(id, "First comment", "note").unwrap();
    assert!(comment_id > 0);

    db.add_comment(id, "Second comment", "note").unwrap();

    let comments = db.get_comments(id).unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0].content, "First comment");
    assert_eq!(comments[1].content, "Second comment");
}

// ==================== Dependencies Tests ====================

#[test]
fn test_add_and_get_dependencies() {
    let (db, _dir) = setup_test_db();

    let blocker = db.create_issue("Blocker issue", None, "high").unwrap();
    let blocked = db.create_issue("Blocked issue", None, "medium").unwrap();

    db.add_dependency(blocked, blocker).unwrap();

    let blockers = db.get_blockers(blocked).unwrap();
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0], blocker);

    let blocking = db.get_blocking(blocker).unwrap();
    assert_eq!(blocking.len(), 1);
    assert_eq!(blocking[0], blocked);
}

#[test]
fn test_remove_dependency() {
    let (db, _dir) = setup_test_db();

    let blocker = db.create_issue("Blocker", None, "high").unwrap();
    let blocked = db.create_issue("Blocked", None, "medium").unwrap();

    db.add_dependency(blocked, blocker).unwrap();
    let removed = db.remove_dependency(blocked, blocker).unwrap();
    assert!(removed);

    let blockers = db.get_blockers(blocked).unwrap();
    assert!(blockers.is_empty());
}

#[test]
fn test_list_blocked_issues() {
    let (db, _dir) = setup_test_db();

    let blocker = db.create_issue("Blocker", None, "high").unwrap();
    let blocked = db.create_issue("Blocked", None, "medium").unwrap();
    let unblocked = db.create_issue("Unblocked", None, "low").unwrap();

    db.add_dependency(blocked, blocker).unwrap();

    let blocked_issues = db.list_blocked_issues().unwrap();
    assert_eq!(blocked_issues.len(), 1);
    assert_eq!(blocked_issues[0].id, blocked);

    // Unblocked issue should not appear
    assert!(!blocked_issues.iter().any(|i| i.id == unblocked));
}

#[test]
fn test_list_ready_issues() {
    let (db, _dir) = setup_test_db();

    let blocker = db.create_issue("Blocker", None, "high").unwrap();
    let blocked = db.create_issue("Blocked", None, "medium").unwrap();
    let ready = db.create_issue("Ready", None, "low").unwrap();

    db.add_dependency(blocked, blocker).unwrap();

    let ready_issues = db.list_ready_issues().unwrap();

    // Blocker and ready should be in ready list (not blocked by anything)
    let ready_ids: Vec<i64> = ready_issues.iter().map(|i| i.id).collect();
    assert!(ready_ids.contains(&blocker));
    assert!(ready_ids.contains(&ready));
    assert!(!ready_ids.contains(&blocked));
}

#[test]
fn test_blocked_becomes_ready_when_blocker_closed() {
    let (db, _dir) = setup_test_db();

    let blocker = db.create_issue("Blocker", None, "high").unwrap();
    let blocked = db.create_issue("Blocked", None, "medium").unwrap();

    db.add_dependency(blocked, blocker).unwrap();

    // Initially blocked
    let blocked_issues = db.list_blocked_issues().unwrap();
    assert_eq!(blocked_issues.len(), 1);

    // Close blocker
    db.close_issue(blocker).unwrap();

    // Now should be ready
    let blocked_issues = db.list_blocked_issues().unwrap();
    assert!(blocked_issues.is_empty());

    let ready_issues = db.list_ready_issues().unwrap();
    assert!(ready_issues.iter().any(|i| i.id == blocked));
}

// ==================== Sessions Tests ====================

#[test]
fn test_start_and_get_session() {
    let (db, _dir) = setup_test_db();

    let id = db.start_session().unwrap();
    assert!(id > 0);

    let session = db.get_current_session().unwrap().unwrap();
    assert_eq!(session.id, id);
    assert!(session.ended_at.is_none());
    assert!(session.active_issue_id.is_none());
}

#[test]
fn test_end_session() {
    let (db, _dir) = setup_test_db();

    let id = db.start_session().unwrap();
    db.end_session(id, Some("Handoff notes")).unwrap();

    let current = db.get_current_session().unwrap();
    assert!(current.is_none());

    let last = db.get_last_session().unwrap().unwrap();
    assert_eq!(last.id, id);
    assert!(last.ended_at.is_some());
    assert_eq!(last.handoff_notes, Some("Handoff notes".to_string()));
}

#[test]
fn test_update_comment_content() {
    let (db, _dir) = setup_test_db();
    let issue_id = db.create_issue("Test", None, "medium").unwrap();
    let comment_id = db
        .add_comment(issue_id, "See L1 for details", "note")
        .unwrap();

    let updated = db
        .update_comment_content(comment_id, "See #5 for details")
        .unwrap();
    assert!(updated);

    let comments = db.get_comments(issue_id).unwrap();
    assert_eq!(comments[0].content, "See #5 for details");
}

#[test]
fn test_update_comment_content_nonexistent() {
    let (db, _dir) = setup_test_db();
    let updated = db.update_comment_content(99999, "new content").unwrap();
    assert!(!updated);
}

#[test]
fn test_update_session_notes() {
    let (db, _dir) = setup_test_db();
    let session_id = db.start_session().unwrap();
    db.end_session(session_id, Some("Working on L1")).unwrap();

    let updated = db
        .update_session_notes(session_id, "Working on #5")
        .unwrap();
    assert!(updated);

    let session = db.get_last_session().unwrap().unwrap();
    assert_eq!(session.handoff_notes, Some("Working on #5".to_string()));
}

#[test]
fn test_get_all_sessions_with_notes() {
    let (db, _dir) = setup_test_db();

    // Session without notes
    let s1 = db.start_session().unwrap();
    db.end_session(s1, None).unwrap();

    // Session with notes
    let s2 = db.start_session().unwrap();
    db.end_session(s2, Some("Handoff for L1")).unwrap();

    // Another with notes
    let s3 = db.start_session().unwrap();
    db.end_session(s3, Some("Continuing L2 work")).unwrap();

    let sessions = db.get_all_sessions_with_notes().unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(
        sessions[0].handoff_notes,
        Some("Handoff for L1".to_string())
    );
    assert_eq!(
        sessions[1].handoff_notes,
        Some("Continuing L2 work".to_string())
    );
}

#[test]
fn test_set_session_issue() {
    let (db, _dir) = setup_test_db();

    let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
    let session_id = db.start_session().unwrap();

    db.set_session_issue(session_id, issue_id).unwrap();

    let session = db.get_current_session().unwrap().unwrap();
    assert_eq!(session.active_issue_id, Some(issue_id));
}

// ==================== Time Tracking Tests ====================

#[test]
fn test_start_and_stop_timer() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();

    let timer_id = db.start_timer(id).unwrap();
    assert!(timer_id > 0);

    let active = db.get_active_timer().unwrap();
    assert!(active.is_some());
    assert_eq!(active.unwrap().0, id);

    std::thread::sleep(std::time::Duration::from_millis(100));

    db.stop_timer(id).unwrap();

    let active = db.get_active_timer().unwrap();
    assert!(active.is_none());
}

#[test]
fn test_get_total_time() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test issue", None, "medium").unwrap();

    // No time tracked yet
    let total = db.get_total_time(id).unwrap();
    assert_eq!(total, 0);
}

// ==================== Search Tests ====================

#[test]
fn test_search_issues_by_title() {
    let (db, _dir) = setup_test_db();

    db.create_issue("Fix authentication bug", None, "high")
        .unwrap();
    db.create_issue("Add dark mode", None, "medium").unwrap();
    db.create_issue("Auth improvements", None, "low").unwrap();

    let results = db.search_issues("auth").unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn test_search_issues_by_description() {
    let (db, _dir) = setup_test_db();

    db.create_issue(
        "Feature A",
        Some("This relates to authentication"),
        "medium",
    )
    .unwrap();
    db.create_issue("Feature B", Some("Something else"), "medium")
        .unwrap();

    let results = db.search_issues("authentication").unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_search_issues_by_comment() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Some issue", None, "medium").unwrap();
    db.add_comment(id, "Found the root cause in authentication module", "note")
        .unwrap();

    let results = db.search_issues("authentication").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, id);
}

// ==================== Relations Tests ====================

#[test]
fn test_add_and_get_relations() {
    let (db, _dir) = setup_test_db();

    let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
    let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

    db.add_relation(id1, id2).unwrap();

    let related = db.get_related_issues(id1).unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].id, id2);

    // Bidirectional
    let related = db.get_related_issues(id2).unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0].id, id1);
}

#[test]
fn test_relation_to_self_fails() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Issue", None, "medium").unwrap();

    let result = db.add_relation(id, id);
    assert!(result.is_err());
}

#[test]
fn test_remove_relation() {
    let (db, _dir) = setup_test_db();

    let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
    let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

    db.add_relation(id1, id2).unwrap();
    db.remove_relation(id1, id2).unwrap();

    let related = db.get_related_issues(id1).unwrap();
    assert!(related.is_empty());
}

// ==================== Milestones Tests ====================

#[test]
fn test_create_and_get_milestone() {
    let (db, _dir) = setup_test_db();

    let id = db.create_milestone("v1.0", Some("First release")).unwrap();
    assert!(id > 0);

    let milestone = db.get_milestone(id).unwrap().unwrap();
    assert_eq!(milestone.name, "v1.0");
    assert_eq!(milestone.description, Some("First release".to_string()));
    assert_eq!(milestone.status, IssueStatus::Open);
}

#[test]
fn test_list_milestones() {
    let (db, _dir) = setup_test_db();

    db.create_milestone("v1.0", None).unwrap();
    db.create_milestone("v2.0", None).unwrap();

    let milestones = db.list_milestones(None).unwrap();
    assert_eq!(milestones.len(), 2);
}

#[test]
fn test_add_issue_to_milestone() {
    let (db, _dir) = setup_test_db();

    let milestone_id = db.create_milestone("v1.0", None).unwrap();
    let issue_id = db.create_issue("Feature", None, "medium").unwrap();

    db.add_issue_to_milestone(milestone_id, issue_id).unwrap();

    let issues = db.get_milestone_issues(milestone_id).unwrap();
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].id, issue_id);

    let milestone = db.get_issue_milestone(issue_id).unwrap().unwrap();
    assert_eq!(milestone.id, milestone_id);
}

#[test]
fn test_close_milestone() {
    let (db, _dir) = setup_test_db();

    let id = db.create_milestone("v1.0", None).unwrap();
    db.close_milestone(id).unwrap();

    let milestone = db.get_milestone(id).unwrap().unwrap();
    assert_eq!(milestone.status, IssueStatus::Closed);
    assert!(milestone.closed_at.is_some());
}

// ==================== Archive Tests ====================

#[test]
fn test_archive_closed_issue() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test", None, "medium").unwrap();
    db.close_issue(id).unwrap();

    let archived = db.archive_issue(id).unwrap();
    assert!(archived);

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.status, IssueStatus::Archived);
}

#[test]
fn test_archive_open_issue_fails() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test", None, "medium").unwrap();

    let archived = db.archive_issue(id).unwrap();
    assert!(!archived);

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.status, IssueStatus::Open);
}

#[test]
fn test_unarchive_issue() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test", None, "medium").unwrap();
    db.close_issue(id).unwrap();
    db.archive_issue(id).unwrap();

    let unarchived = db.unarchive_issue(id).unwrap();
    assert!(unarchived);

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.status, IssueStatus::Closed);
}

#[test]
fn test_list_archived_issues() {
    let (db, _dir) = setup_test_db();

    let id1 = db.create_issue("Archived", None, "medium").unwrap();
    let _id2 = db.create_issue("Open", None, "medium").unwrap();

    db.close_issue(id1).unwrap();
    db.archive_issue(id1).unwrap();

    let archived = db.list_archived_issues().unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].id, id1);
}

// ==================== Security Tests ====================

#[test]
fn test_sql_injection_in_title() {
    let (db, _dir) = setup_test_db();

    // Attempt SQL injection via title
    let malicious = "'; DROP TABLE issues; --";
    let id = db.create_issue(malicious, None, "medium").unwrap();

    // Should have created issue with literal string, not executed SQL
    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.title, malicious);

    // Database should still be intact
    let issues = db.list_issues(None, None, None).unwrap();
    assert!(!issues.is_empty());
}

#[test]
fn test_sql_injection_in_description() {
    let (db, _dir) = setup_test_db();

    let malicious = "test'); DELETE FROM issues; --";
    let id = db
        .create_issue("Normal title", Some(malicious), "medium")
        .unwrap();

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.description, Some(malicious.to_string()));
}

#[test]
fn test_sql_injection_in_label() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test", None, "medium").unwrap();
    let malicious = "bug'; DROP TABLE labels; --";

    db.add_label(id, malicious).unwrap();

    let labels = db.get_labels(id).unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0], malicious);
}

#[test]
fn test_sql_injection_in_search() {
    let (db, _dir) = setup_test_db();

    db.create_issue("Normal issue", None, "medium").unwrap();

    // Attempt injection in search
    let malicious = "%'; DROP TABLE issues; --";
    let results = db.search_issues(malicious).unwrap();

    // Should return empty results, not crash
    assert!(results.is_empty());

    // Database should still be intact
    let issues = db.list_issues(None, None, None).unwrap();
    assert!(!issues.is_empty());
}

#[test]
fn test_sql_injection_in_comment() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test", None, "medium").unwrap();
    let malicious = "comment'); DELETE FROM comments; --";

    db.add_comment(id, malicious, "note").unwrap();

    let comments = db.get_comments(id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].content, malicious);
}

#[test]
fn test_unicode_in_fields() {
    let (db, _dir) = setup_test_db();

    let title = "测试问题 🐛 αβγ";
    let description = "Description with émojis 🎉 and ñ";

    let id = db.create_issue(title, Some(description), "medium").unwrap();

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.title, title);
    assert_eq!(issue.description, Some(description.to_string()));
}

#[test]
fn test_very_long_strings() {
    let (db, _dir) = setup_test_db();

    // Within limits: should succeed
    let long_title = "a".repeat(MAX_TITLE_LEN);
    let long_desc = "b".repeat(MAX_DESCRIPTION_LEN);

    let id = db
        .create_issue(&long_title, Some(&long_desc), "medium")
        .unwrap();

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.title.len(), MAX_TITLE_LEN);
    assert_eq!(issue.description.unwrap().len(), MAX_DESCRIPTION_LEN);

    // Exceeding limits: should fail
    let too_long_title = "a".repeat(MAX_TITLE_LEN + 1);
    assert!(db.create_issue(&too_long_title, None, "medium").is_err());

    let too_long_desc = "b".repeat(MAX_DESCRIPTION_LEN + 1);
    assert!(db
        .create_issue("ok", Some(&too_long_desc), "medium")
        .is_err());
}

#[test]
fn test_null_bytes_in_strings() {
    let (db, _dir) = setup_test_db();

    let title = "test\0null\0bytes";
    let id = db.create_issue(title, None, "medium").unwrap();

    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.title, title);
}

// ==================== Cascade Delete Tests ====================

#[test]
fn test_delete_issue_cascades_labels() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test", None, "medium").unwrap();
    db.add_label(id, "bug").unwrap();
    db.add_label(id, "urgent").unwrap();

    db.delete_issue(id).unwrap();

    // Labels should be gone (via CASCADE)
    let labels = db.get_labels(id).unwrap();
    assert!(labels.is_empty());
}

#[test]
fn test_delete_issue_cascades_comments() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Test", None, "medium").unwrap();
    db.add_comment(id, "Comment 1", "note").unwrap();
    db.add_comment(id, "Comment 2", "note").unwrap();

    db.delete_issue(id).unwrap();

    let comments = db.get_comments(id).unwrap();
    assert!(comments.is_empty());
}

#[test]
fn test_delete_parent_cascades_subissues() {
    let (db, _dir) = setup_test_db();

    let parent_id = db.create_issue("Parent", None, "high").unwrap();
    let child_id = db
        .create_subissue(parent_id, "Child", None, "medium")
        .unwrap();

    db.delete_issue(parent_id).unwrap();

    // Child should be deleted too
    assert!(db.get_issue(child_id).unwrap().is_none());
}

// ==================== Edge Cases ====================

#[test]
fn test_empty_title() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("", None, "medium").unwrap();
    let issue = db.get_issue(id).unwrap().unwrap();
    assert_eq!(issue.title, "");
}

#[test]
fn test_update_parent() {
    let (db, _dir) = setup_test_db();

    let parent1 = db.create_issue("Parent 1", None, "high").unwrap();
    let parent2 = db.create_issue("Parent 2", None, "high").unwrap();
    let child = db.create_issue("Child", None, "medium").unwrap();

    db.update_parent(child, Some(parent1)).unwrap();
    let issue = db.get_issue(child).unwrap().unwrap();
    assert_eq!(issue.parent_id, Some(parent1));

    db.update_parent(child, Some(parent2)).unwrap();
    let issue = db.get_issue(child).unwrap().unwrap();
    assert_eq!(issue.parent_id, Some(parent2));

    db.update_parent(child, None).unwrap();
    let issue = db.get_issue(child).unwrap().unwrap();
    assert_eq!(issue.parent_id, None);
}

// ==================== Database Corruption Recovery ====================

#[test]
fn test_corrupted_db_file_empty() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("issues.db");

    // Create an empty file (corrupted)
    std::fs::write(&db_path, b"").unwrap();

    // SQLite treats empty files as new databases, so this should succeed
    // and the database should be usable afterward
    let result = Database::open(&db_path);
    assert!(
        result.is_ok(),
        "Empty file should be treated as new DB: {:?}",
        result.err()
    );
    let db = result.unwrap();
    let id = db
        .create_issue("Test after recovery", None, "medium")
        .unwrap();
    assert!(id > 0);
}

#[test]
fn test_corrupted_db_file_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("issues.db");

    // Write garbage data
    std::fs::write(&db_path, b"not a sqlite database at all!").unwrap();

    // Should fail gracefully with an error, not panic
    let result = Database::open(&db_path);
    assert!(result.is_err());
}

#[test]
fn test_corrupted_db_file_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("issues.db");

    // Create valid DB first
    {
        let db = Database::open(&db_path).unwrap();
        db.create_issue("Test", None, "medium").unwrap();
    }

    // Truncate it (simulate crash during write)
    let content = std::fs::read(&db_path).unwrap();
    std::fs::write(&db_path, &content[..content.len() / 2]).unwrap();

    // Truncated DB should fail to open -- SQLite detects corruption
    let result = Database::open(&db_path);
    match result {
        Err(e) => {
            let err_msg = format!("{e}");
            assert!(
                err_msg.contains("not a database")
                    || err_msg.contains("malformed")
                    || err_msg.contains("corrupt")
                    || err_msg.contains("disk image"),
                "Error should indicate corruption, got: {err_msg}"
            );
        }
        Ok(db) => {
            // If SQLite somehow recovers, verify the original data is gone
            let issues = db.list_issues(Some("all"), None, None).unwrap();
            assert!(
                issues.is_empty(),
                "Truncated DB should not retain original data"
            );
        }
    }
}

#[test]
fn test_db_readonly_location() {
    // This test only works on Unix-like systems
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("issues.db");

        // Create the file first
        std::fs::write(&db_path, b"").unwrap();

        // Make it read-only
        let mut perms = std::fs::metadata(&db_path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&db_path, perms).unwrap();

        // Should fail gracefully
        let result = Database::open(&db_path);
        assert!(result.is_err());
    }
}

// ==================== Export Metadata Tests ====================

#[test]
fn test_get_issue_export_metadata() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("Meta test", None, "medium").unwrap();

    let (uuid, created_by) = db.get_issue_export_metadata(id).unwrap();
    // create_issue auto-generates a uuid but does not set created_by
    assert!(uuid.is_some());
    assert!(!uuid.unwrap().is_empty());
    assert!(created_by.is_none());
}

#[test]
fn test_get_issue_export_metadata_with_hydrated_issue() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();
    db.insert_hydrated_issue(&HydratedIssue {
        id: 42,
        uuid: "abc-123",
        title: "Hydrated issue",
        description: Some("desc"),
        status: "open",
        priority: "high",
        parent_id: None,
        created_by: Some("agent-1"),
        created_at: &now,
        updated_at: &now,
        closed_at: None,
        scheduled_at: None,
        due_at: None,
    })
    .unwrap();

    let (uuid, created_by) = db.get_issue_export_metadata(42).unwrap();
    assert_eq!(uuid.as_deref(), Some("abc-123"));
    assert_eq!(created_by.as_deref(), Some("agent-1"));
}

#[test]
fn test_get_issue_export_metadata_nonexistent() {
    let (db, _dir) = setup_test_db();
    let result = db.get_issue_export_metadata(99999);
    assert!(result.is_err());
}

// ==================== Comments With Author Tests ====================

#[test]
fn test_get_comments_with_author_empty() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("No comments", None, "low").unwrap();

    let comments = db.get_comments_with_author(id).unwrap();
    assert!(comments.is_empty());
}

#[test]
fn test_get_comments_with_author() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("Commented issue", None, "medium").unwrap();
    db.add_comment(id, "First comment", "note").unwrap();
    db.add_comment(id, "Second comment", "plan").unwrap();

    let comments = db.get_comments_with_author(id).unwrap();
    assert_eq!(comments.len(), 2);

    // Tuple: (id, author, content, created_at, kind, trigger_type, intervention_context, driver_key_fingerprint)
    assert_eq!(comments[0].2, "First comment");
    assert_eq!(comments[0].4, "note");
    assert_eq!(comments[1].2, "Second comment");
    assert_eq!(comments[1].4, "plan");
    // author is None when added via add_comment (no author param)
    assert!(comments[0].1.is_none());
}

// ==================== Time Entries Tests ====================

#[test]
fn test_get_time_entries_for_issue_empty() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("No timer", None, "low").unwrap();

    let entries = db.get_time_entries_for_issue(id).unwrap();
    assert!(entries.is_empty());
}

#[test]
fn test_get_time_entries_for_issue() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("Timed issue", None, "medium").unwrap();

    db.start_timer(id).unwrap();
    db.stop_timer(id).unwrap();

    let entries = db.get_time_entries_for_issue(id).unwrap();
    assert_eq!(entries.len(), 1);

    // Tuple: (id, started_at, ended_at, duration_seconds)
    assert!(entries[0].0 > 0); // entry id
    assert!(entries[0].2.is_some()); // ended_at should be set
    assert!(entries[0].3.is_some()); // duration should be set
}

#[test]
fn test_get_time_entries_for_issue_active_timer() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("Active timer", None, "medium").unwrap();

    db.start_timer(id).unwrap();

    let entries = db.get_time_entries_for_issue(id).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].2.is_none()); // ended_at not set yet
    assert!(entries[0].3.is_none()); // duration not set yet
}

// ==================== Milestone UUID Tests ====================

#[test]
fn test_get_milestone_uuid_for_issue_none() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("No milestone", None, "low").unwrap();

    let uuid = db.get_milestone_uuid_for_issue(id).unwrap();
    assert!(uuid.is_none());
}

#[test]
fn test_get_milestone_uuid_for_issue_assigned() {
    let (db, _dir) = setup_test_db();
    let issue_id = db.create_issue("Milestone issue", None, "medium").unwrap();
    let ms_id = db.create_milestone("v1.0", None).unwrap();
    db.add_issue_to_milestone(ms_id, issue_id).unwrap();

    // create_milestone doesn't set uuid, so it will be None
    let uuid = db.get_milestone_uuid_for_issue(issue_id).unwrap();
    assert!(uuid.is_none());
}

#[test]
fn test_get_milestone_uuid_for_issue_hydrated() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();

    // Insert issue via hydration
    db.insert_hydrated_issue(&HydratedIssue {
        id: 10,
        uuid: "issue-uuid",
        title: "Test",
        description: None,
        status: "open",
        priority: "medium",
        parent_id: None,
        created_by: None,
        created_at: &now,
        updated_at: &now,
        closed_at: None,
        scheduled_at: None,
        due_at: None,
    })
    .unwrap();

    // Insert milestone via hydration (has uuid)
    db.insert_hydrated_milestone(&HydratedMilestone {
        id: 1,
        uuid: "ms-uuid-123",
        name: "Sprint 1",
        description: None,
        status: "open",
        created_at: &now,
        closed_at: None,
    })
    .unwrap();

    db.insert_hydrated_milestone_issue(1, 10).unwrap();

    let uuid = db.get_milestone_uuid_for_issue(10).unwrap();
    assert_eq!(uuid.as_deref(), Some("ms-uuid-123"));
}

// ==================== Related Issue IDs Tests ====================

#[test]
fn test_get_related_issue_ids_empty() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("Lonely issue", None, "low").unwrap();

    let related = db.get_related_issue_ids(id).unwrap();
    assert!(related.is_empty());
}

#[test]
fn test_get_related_issue_ids() {
    let (db, _dir) = setup_test_db();
    let id1 = db.create_issue("Issue A", None, "medium").unwrap();
    let id2 = db.create_issue("Issue B", None, "medium").unwrap();
    let id3 = db.create_issue("Issue C", None, "medium").unwrap();

    db.add_relation(id1, id2).unwrap();
    db.add_relation(id3, id1).unwrap();

    // From id1's perspective, both id2 and id3 should be related
    let mut related = db.get_related_issue_ids(id1).unwrap();
    related.sort_unstable();
    assert_eq!(related, vec![id2, id3]);

    // From id2's perspective, only id1 is related
    let related2 = db.get_related_issue_ids(id2).unwrap();
    assert_eq!(related2, vec![id1]);
}

// ==================== Session Agent-Scoped Tests ====================

#[test]
fn test_session_with_agent_id() {
    let (db, _dir) = setup_test_db();

    let sid = db.start_session_with_agent(Some("agent-alpha")).unwrap();
    assert!(sid > 0);

    // Should find it when filtering by agent
    let session = db
        .get_current_session_for_agent(Some("agent-alpha"))
        .unwrap();
    assert!(session.is_some());
    let s = session.unwrap();
    assert_eq!(s.agent_id.as_deref(), Some("agent-alpha"));

    // Should NOT find it when filtering by a different agent
    let other = db
        .get_current_session_for_agent(Some("agent-beta"))
        .unwrap();
    assert!(other.is_none());

    // None filter returns any active session
    let any = db.get_current_session_for_agent(None).unwrap();
    assert!(any.is_some());
}

#[test]
fn test_get_last_session_for_agent() {
    let (db, _dir) = setup_test_db();

    let sid = db.start_session_with_agent(Some("agent-x")).unwrap();
    db.end_session(sid, Some("done")).unwrap();

    // Should find the ended session for this agent
    let session = db.get_last_session_for_agent(Some("agent-x")).unwrap();
    assert!(session.is_some());
    assert_eq!(session.unwrap().handoff_notes.as_deref(), Some("done"));

    // Different agent should not find it
    let other = db.get_last_session_for_agent(Some("agent-y")).unwrap();
    assert!(other.is_none());

    // None filter returns any ended session
    let any = db.get_last_session_for_agent(None).unwrap();
    assert!(any.is_some());
}

#[test]
fn test_set_session_action() {
    let (db, _dir) = setup_test_db();

    let sid = db.start_session().unwrap();
    let ok = db.set_session_action(sid, "refactoring db module").unwrap();
    assert!(ok);

    let session = db.get_current_session().unwrap().unwrap();
    assert_eq!(
        session.last_action.as_deref(),
        Some("refactoring db module")
    );
}

// ==================== Hydration Tests ====================

#[test]
fn test_insert_hydrated_issue() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();

    db.insert_hydrated_issue(&HydratedIssue {
        id: 100,
        uuid: "uuid-100",
        title: "Hydrated",
        description: Some("A hydrated issue"),
        status: "open",
        priority: "critical",
        parent_id: None,
        created_by: Some("bot"),
        created_at: &now,
        updated_at: &now,
        closed_at: None,
        scheduled_at: None,
        due_at: None,
    })
    .unwrap();

    let issue = db.get_issue(100).unwrap().unwrap();
    assert_eq!(issue.title, "Hydrated");
    assert_eq!(issue.priority, Priority::Critical);
    assert_eq!(issue.status, IssueStatus::Open);
}

#[test]
fn test_insert_hydrated_issue_with_parent() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();

    db.insert_hydrated_issue(&HydratedIssue {
        id: 1,
        uuid: "parent-uuid",
        title: "Parent",
        description: None,
        status: "open",
        priority: "high",
        parent_id: None,
        created_by: None,
        created_at: &now,
        updated_at: &now,
        closed_at: None,
        scheduled_at: None,
        due_at: None,
    })
    .unwrap();

    db.insert_hydrated_issue(&HydratedIssue {
        id: 2,
        uuid: "child-uuid",
        title: "Child",
        description: None,
        status: "open",
        priority: "medium",
        parent_id: Some(1),
        created_by: None,
        created_at: &now,
        updated_at: &now,
        closed_at: None,
        scheduled_at: None,
        due_at: None,
    })
    .unwrap();

    let child = db.get_issue(2).unwrap().unwrap();
    assert_eq!(child.parent_id, Some(1));
}

#[test]
fn test_insert_hydrated_label() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("Labeled", None, "low").unwrap();

    db.insert_hydrated_label(id, "bug").unwrap();
    db.insert_hydrated_label(id, "urgent").unwrap();

    let labels = db.get_labels(id).unwrap();
    assert!(labels.contains(&"bug".to_string()));
    assert!(labels.contains(&"urgent".to_string()));
}

#[test]
fn test_insert_hydrated_label_idempotent() {
    let (db, _dir) = setup_test_db();
    let id = db.create_issue("Dup label", None, "low").unwrap();

    db.insert_hydrated_label(id, "bug").unwrap();
    db.insert_hydrated_label(id, "bug").unwrap(); // should not error (INSERT OR IGNORE)

    let labels = db.get_labels(id).unwrap();
    assert_eq!(labels.len(), 1);
}

#[test]
fn test_insert_hydrated_comment() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();
    let issue_id = db.create_issue("Commented", None, "medium").unwrap();

    db.insert_hydrated_comment(
        1000,
        issue_id,
        Some("comment-uuid"),
        Some("alice"),
        "Great work!",
        &now,
        "note",
        None,
        None,
        None,
    )
    .unwrap();

    let comments = db.get_comments_with_author(issue_id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].1.as_deref(), Some("alice"));
    assert_eq!(comments[0].2, "Great work!");
    assert_eq!(comments[0].4, "note");
}

#[test]
fn test_insert_hydrated_comment_with_intervention() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();
    let issue_id = db.create_issue("Intervened", None, "high").unwrap();

    db.insert_hydrated_comment(
        2000,
        issue_id,
        None,
        Some("bot"),
        "Intervention needed",
        &now,
        "blocker",
        Some("manual"),
        Some("context info"),
        Some("fingerprint-abc"),
    )
    .unwrap();

    let comments = db.get_comments_with_author(issue_id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].4, "blocker");
    assert_eq!(comments[0].5.as_deref(), Some("manual"));
    assert_eq!(comments[0].6.as_deref(), Some("context info"));
    assert_eq!(comments[0].7.as_deref(), Some("fingerprint-abc"));
}

#[test]
fn test_insert_hydrated_time_entry() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();
    let issue_id = db.create_issue("Timed", None, "medium").unwrap();

    db.insert_hydrated_time_entry(500, issue_id, &now, Some(&now), Some(3600))
        .unwrap();

    let entries = db.get_time_entries_for_issue(issue_id).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, 500); // entry id
    assert!(entries[0].2.is_some()); // ended_at
    assert_eq!(entries[0].3, Some(3600)); // duration_seconds
}

#[test]
fn test_insert_hydrated_time_entry_open() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();
    let issue_id = db.create_issue("Open timer", None, "medium").unwrap();

    db.insert_hydrated_time_entry(501, issue_id, &now, None, None)
        .unwrap();

    let entries = db.get_time_entries_for_issue(issue_id).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].2.is_none()); // ended_at not set
    assert!(entries[0].3.is_none()); // duration not set
}

#[test]
fn test_insert_hydrated_milestone() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();

    db.insert_hydrated_milestone(&HydratedMilestone {
        id: 50,
        uuid: "ms-uuid-50",
        name: "Release 2.0",
        description: Some("Major release"),
        status: "open",
        created_at: &now,
        closed_at: None,
    })
    .unwrap();

    let ms = db.get_milestone(50).unwrap().unwrap();
    assert_eq!(ms.name, "Release 2.0");
    assert_eq!(ms.status, IssueStatus::Open);
}

#[test]
fn test_insert_hydrated_milestone_issue() {
    let (db, _dir) = setup_test_db();
    let now = Utc::now().to_rfc3339();

    db.insert_hydrated_issue(&HydratedIssue {
        id: 5,
        uuid: "i-5",
        title: "Issue 5",
        description: None,
        status: "open",
        priority: "medium",
        parent_id: None,
        created_by: None,
        created_at: &now,
        updated_at: &now,
        closed_at: None,
        scheduled_at: None,
        due_at: None,
    })
    .unwrap();

    db.insert_hydrated_milestone(&HydratedMilestone {
        id: 3,
        uuid: "ms-3",
        name: "Sprint",
        description: None,
        status: "open",
        created_at: &now,
        closed_at: None,
    })
    .unwrap();

    db.insert_hydrated_milestone_issue(3, 5).unwrap();

    let uuid = db.get_milestone_uuid_for_issue(5).unwrap();
    assert_eq!(uuid.as_deref(), Some("ms-3"));
}

#[test]
fn test_clear_shared_data() {
    let (db, _dir) = setup_test_db();

    // Populate various tables
    let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
    let id2 = db.create_issue("Issue 2", None, "high").unwrap();
    db.add_comment(id1, "hello", "note").unwrap();
    db.add_label(id1, "bug").unwrap();
    db.add_relation(id1, id2).unwrap();
    db.start_timer(id1).unwrap();
    db.stop_timer(id1).unwrap();
    let ms_id = db.create_milestone("v1", None).unwrap();
    db.add_issue_to_milestone(ms_id, id1).unwrap();

    // Also start a session (should NOT be cleared)
    let sid = db.start_session().unwrap();

    // Verify data exists
    assert!(db.get_issue(id1).unwrap().is_some());
    assert!(!db.get_comments_with_author(id1).unwrap().is_empty());

    db.clear_shared_data().unwrap();

    // Issues and related data should be gone
    assert!(db.get_issue(id1).unwrap().is_none());
    assert!(db.get_issue(id2).unwrap().is_none());
    assert!(db.get_comments_with_author(id1).unwrap().is_empty());
    assert!(db.get_time_entries_for_issue(id1).unwrap().is_empty());
    assert!(db.get_related_issue_ids(id1).unwrap().is_empty());

    // Session should still exist (sessions are machine-local)
    let session = db.get_current_session().unwrap();
    assert!(session.is_some());
    assert_eq!(session.unwrap().id, sid);
}

// ==================== Token Usage Tests ====================

#[test]
fn test_create_and_get_token_usage() {
    let (db, _dir) = setup_test_db();

    let id = db
        .create_token_usage(
            "agent-1",
            None,
            1000,
            500,
            Some(200),
            Some(100),
            "gpt-4",
            Some(0.05),
        )
        .unwrap();
    assert!(id > 0);

    let usage = db.get_token_usage(id).unwrap().unwrap();
    assert_eq!(usage.agent_id, "agent-1");
    assert_eq!(usage.input_tokens, 1000);
    assert_eq!(usage.output_tokens, 500);
    assert_eq!(usage.cache_read_tokens, Some(200));
    assert_eq!(usage.cache_creation_tokens, Some(100));
    assert_eq!(usage.model, "gpt-4");
    assert_eq!(usage.cost_estimate, Some(0.05));
    assert!(usage.session_id.is_none());
}

#[test]
fn test_create_token_usage_with_session() {
    let (db, _dir) = setup_test_db();

    let sid = db.start_session().unwrap();
    let id = db
        .create_token_usage("agent-2", Some(sid), 500, 250, None, None, "claude-3", None)
        .unwrap();

    let usage = db.get_token_usage(id).unwrap().unwrap();
    assert_eq!(usage.session_id, Some(sid));
    assert_eq!(usage.agent_id, "agent-2");
    assert!(usage.cache_read_tokens.is_none());
    assert!(usage.cost_estimate.is_none());
}

#[test]
fn test_get_token_usage_nonexistent() {
    let (db, _dir) = setup_test_db();
    let usage = db.get_token_usage(99999).unwrap();
    assert!(usage.is_none());
}

#[test]
fn test_list_token_usage_unfiltered() {
    let (db, _dir) = setup_test_db();

    db.create_token_usage("a1", None, 100, 50, None, None, "m1", None)
        .unwrap();
    db.create_token_usage("a2", None, 200, 100, None, None, "m2", None)
        .unwrap();

    let all = db
        .list_token_usage(None, None, None, None, None, None)
        .unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_list_token_usage_filtered_by_agent() {
    let (db, _dir) = setup_test_db();

    db.create_token_usage("alpha", None, 100, 50, None, None, "m1", None)
        .unwrap();
    db.create_token_usage("beta", None, 200, 100, None, None, "m1", None)
        .unwrap();
    db.create_token_usage("alpha", None, 300, 150, None, None, "m2", None)
        .unwrap();

    let filtered = db
        .list_token_usage(Some("alpha"), None, None, None, None, None)
        .unwrap();
    assert_eq!(filtered.len(), 2);
    assert!(filtered.iter().all(|u| u.agent_id == "alpha"));
}

#[test]
fn test_list_token_usage_filtered_by_model() {
    let (db, _dir) = setup_test_db();

    db.create_token_usage("a", None, 100, 50, None, None, "gpt-4", None)
        .unwrap();
    db.create_token_usage("a", None, 200, 100, None, None, "claude", None)
        .unwrap();

    let filtered = db
        .list_token_usage(None, None, Some("claude"), None, None, None)
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].model, "claude");
}

#[test]
fn test_list_token_usage_with_limit() {
    let (db, _dir) = setup_test_db();

    for i in 0..5 {
        db.create_token_usage("a", None, i * 100, 50, None, None, "m", None)
            .unwrap();
    }

    let limited = db
        .list_token_usage(None, None, None, None, None, Some(3))
        .unwrap();
    assert_eq!(limited.len(), 3);
}

#[test]
fn test_get_usage_summary() {
    let (db, _dir) = setup_test_db();

    db.create_token_usage("a1", None, 100, 50, Some(10), Some(5), "gpt-4", Some(0.01))
        .unwrap();
    db.create_token_usage(
        "a1",
        None,
        200,
        100,
        Some(20),
        Some(10),
        "gpt-4",
        Some(0.02),
    )
    .unwrap();
    db.create_token_usage("a2", None, 300, 150, None, None, "claude", Some(0.03))
        .unwrap();

    // Unfiltered: should get 2 groups (a1/gpt-4 and a2/claude)
    let summary = db.get_usage_summary(None, None, None).unwrap();
    assert_eq!(summary.len(), 2);

    // Find the a1/gpt-4 group
    let a1_summary = summary.iter().find(|s| s.agent_id == "a1").unwrap();
    assert_eq!(a1_summary.model, "gpt-4");
    assert_eq!(a1_summary.request_count, 2);
    assert_eq!(a1_summary.total_input_tokens, 300);
    assert_eq!(a1_summary.total_output_tokens, 150);
    assert_eq!(a1_summary.total_cache_read_tokens, 30);
    assert_eq!(a1_summary.total_cache_creation_tokens, 15);
    assert!((a1_summary.total_cost - 0.03).abs() < 1e-9);
}

#[test]
fn test_get_usage_summary_filtered_by_agent() {
    let (db, _dir) = setup_test_db();

    db.create_token_usage("a1", None, 100, 50, None, None, "m", Some(0.01))
        .unwrap();
    db.create_token_usage("a2", None, 200, 100, None, None, "m", Some(0.02))
        .unwrap();

    let summary = db.get_usage_summary(Some("a1"), None, None).unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].agent_id, "a1");
    assert_eq!(summary[0].total_input_tokens, 100);
}

// ==================== Archive Tests ====================

#[test]
fn test_archive_older_than() {
    let (db, _dir) = setup_test_db();

    // Create and close two issues
    let id1 = db.create_issue("Old issue", None, "low").unwrap();
    let id2 = db.create_issue("Recent issue", None, "low").unwrap();
    let id3 = db.create_issue("Open issue", None, "low").unwrap();

    db.close_issue(id1).unwrap();
    db.close_issue(id2).unwrap();
    // id3 stays open

    // Backdate id1's closed_at to 100 days ago
    let old_date = (Utc::now() - chrono::Duration::days(100)).to_rfc3339();
    db.conn
        .execute(
            "UPDATE issues SET closed_at = ?1 WHERE id = ?2",
            params![old_date, id1],
        )
        .unwrap();

    // Archive issues closed more than 30 days ago
    let archived = db.archive_older_than(30).unwrap();
    assert_eq!(archived, 1);

    let issue1 = db.get_issue(id1).unwrap().unwrap();
    assert_eq!(issue1.status, IssueStatus::Archived);

    // id2 was just closed, should still be "closed"
    let issue2 = db.get_issue(id2).unwrap().unwrap();
    assert_eq!(issue2.status, IssueStatus::Closed);

    // id3 is still open
    let issue3 = db.get_issue(id3).unwrap().unwrap();
    assert_eq!(issue3.status, IssueStatus::Open);
}

#[test]
fn test_archive_older_than_none_eligible() {
    let (db, _dir) = setup_test_db();

    let id = db.create_issue("Fresh", None, "medium").unwrap();
    db.close_issue(id).unwrap();

    // Nothing older than 30 days
    let archived = db.archive_older_than(30).unwrap();
    assert_eq!(archived, 0);
}

// ==================== Schema / Count Tests ====================

#[test]
fn test_get_schema_version() {
    let (db, _dir) = setup_test_db();
    let version = db.get_schema_version().unwrap();
    // Should be the latest migration version (at least > 0)
    assert!(version > 0, "Schema version should be > 0, got {version}");
}

#[test]
fn test_get_issue_count() {
    let (db, _dir) = setup_test_db();

    assert_eq!(db.get_issue_count().unwrap(), 0);

    db.create_issue("One", None, "low").unwrap();
    db.create_issue("Two", None, "low").unwrap();

    assert_eq!(db.get_issue_count().unwrap(), 2);
}

#[test]
fn test_get_milestone_count() {
    let (db, _dir) = setup_test_db();

    assert_eq!(db.get_milestone_count().unwrap(), 0);

    db.create_milestone("v1", None).unwrap();
    db.create_milestone("v2", Some("second")).unwrap();

    assert_eq!(db.get_milestone_count().unwrap(), 2);
}

#[test]
fn test_get_max_display_id() {
    let (db, _dir) = setup_test_db();

    assert_eq!(db.get_max_display_id().unwrap(), 0);

    let id1 = db.create_issue("A", None, "low").unwrap();
    let id2 = db.create_issue("B", None, "low").unwrap();

    assert_eq!(db.get_max_display_id().unwrap(), id2);
    assert!(id2 > id1);
}

#[test]
fn test_get_max_comment_id() {
    let (db, _dir) = setup_test_db();

    assert_eq!(db.get_max_comment_id().unwrap(), 0);

    let issue_id = db.create_issue("X", None, "low").unwrap();
    db.add_comment(issue_id, "c1", "note").unwrap();
    let c2 = db.add_comment(issue_id, "c2", "plan").unwrap();

    assert_eq!(db.get_max_comment_id().unwrap(), c2);
}

// ==================== Insert Dependency/Relation Raw Tests ====================

#[test]
fn test_insert_dependency_raw() {
    let (db, _dir) = setup_test_db();
    let id1 = db.create_issue("Blocker", None, "high").unwrap();
    let id2 = db.create_issue("Blocked", None, "medium").unwrap();

    db.insert_dependency_raw(id1, id2).unwrap();

    let blocking = db.get_blocking(id1).unwrap();
    assert_eq!(blocking, vec![id2]);

    let blockers = db.get_blockers(id2).unwrap();
    assert_eq!(blockers, vec![id1]);
}

#[test]
fn test_insert_dependency_raw_idempotent() {
    let (db, _dir) = setup_test_db();
    let id1 = db.create_issue("A", None, "high").unwrap();
    let id2 = db.create_issue("B", None, "medium").unwrap();

    db.insert_dependency_raw(id1, id2).unwrap();
    db.insert_dependency_raw(id1, id2).unwrap(); // INSERT OR IGNORE

    let blocking = db.get_blocking(id1).unwrap();
    assert_eq!(blocking.len(), 1);
}

#[test]
fn test_insert_relation_raw() {
    let (db, _dir) = setup_test_db();
    let id1 = db.create_issue("First", None, "medium").unwrap();
    let id2 = db.create_issue("Second", None, "medium").unwrap();

    db.insert_relation_raw(id1, id2).unwrap();

    let related = db.get_related_issue_ids(id1).unwrap();
    assert_eq!(related, vec![id2]);

    // Verify bidirectional
    let related2 = db.get_related_issue_ids(id2).unwrap();
    assert_eq!(related2, vec![id1]);
}

#[test]
fn test_insert_relation_raw_normalizes_order() {
    let (db, _dir) = setup_test_db();
    let id1 = db.create_issue("A", None, "medium").unwrap();
    let id2 = db.create_issue("B", None, "medium").unwrap();

    // Insert with larger ID first -- should still work due to normalization
    db.insert_relation_raw(id2, id1).unwrap();

    let related = db.get_related_issue_ids(id1).unwrap();
    assert_eq!(related, vec![id2]);
}

// ==================== Property-Based Tests ====================

#[cfg(test)]
mod proptest_tests {
    use crate::db::*;
    use crate::models::IssueStatus;
    use anyhow::Result;
    use proptest::prelude::*;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("issues.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    // Generate valid priority strings
    fn valid_priority() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("low".to_string()),
            Just("medium".to_string()),
            Just("high".to_string()),
            Just("critical".to_string()),
        ]
    }

    // Generate arbitrary (but safe) strings for titles
    fn safe_string() -> impl Strategy<Value = String> {
        // Avoid null bytes; limit to MAX_TITLE_LEN so strings are valid as titles
        "[a-zA-Z0-9 _\\-\\.!?]{0,512}".prop_map(|s| s)
    }

    proptest! {
        /// Any valid title should be storable and retrievable unchanged
        #[test]
        fn prop_title_roundtrip(title in safe_string()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue(&title, None, "medium").unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.title, title);
        }

        /// Any valid description should be storable and retrievable unchanged
        #[test]
        fn prop_description_roundtrip(desc in safe_string()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue("Test", Some(&desc), "medium").unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.description, Some(desc));
        }

        /// All valid priorities should work
        #[test]
        fn prop_priority_valid(priority in valid_priority()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue("Test", None, &priority).unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.priority.to_string(), priority);
        }

        /// Labels should be storable and retrievable
        #[test]
        fn prop_label_roundtrip(label in "[a-zA-Z0-9_\\-]{1,50}") {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue("Test", None, "medium").unwrap();
            db.add_label(id, &label).unwrap();
            let labels = db.get_labels(id).unwrap();
            prop_assert!(labels.contains(&label));
        }

        /// Comments should be storable and retrievable
        #[test]
        fn prop_comment_roundtrip(content in safe_string()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue("Test", None, "medium").unwrap();
            db.add_comment(id, &content, "note").unwrap();
            let comments = db.get_comments(id).unwrap();
            prop_assert_eq!(comments.len(), 1);
            prop_assert_eq!(&comments[0].content, &content);
        }

        /// Creating multiple issues should always increase count
        #[test]
        fn prop_create_increases_count(count in 1usize..20) {
            let (db, _dir) = setup_test_db();
            for i in 0..count {
                db.create_issue(&format!("Issue {i}"), None, "medium").unwrap();
            }
            let issues = db.list_issues(None, None, None).unwrap();
            prop_assert_eq!(issues.len(), count);
        }

        /// Close then reopen should leave issue open
        #[test]
        fn prop_close_reopen_idempotent(title in safe_string()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue(&title, None, "medium").unwrap();

            db.close_issue(id).unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.status, IssueStatus::Closed);

            db.reopen_issue(id).unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.status, IssueStatus::Open);
        }

        /// Blocking should be reflected in blocked list
        #[test]
        fn prop_blocking_relationship(a in 1i64..100, b in 1i64..100) {
            if a == b {
                return Ok(()); // Skip self-blocking
            }
            let (db, _dir) = setup_test_db();

            // Create both issues
            for i in 1..=std::cmp::max(a, b) {
                db.create_issue(&format!("Issue {i}"), None, "medium").unwrap();
            }

            db.add_dependency(a, b).unwrap();
            let blockers = db.get_blockers(a).unwrap();
            prop_assert!(blockers.contains(&b));
        }

        /// Search should find issues with matching titles
        #[test]
        fn prop_search_finds_title(
            prefix in "[a-zA-Z]{3,10}",
            suffix in "[a-zA-Z]{3,10}"
        ) {
            let (db, _dir) = setup_test_db();
            let title = format!("{prefix} unique marker {suffix}");
            db.create_issue(&title, None, "medium").unwrap();

            // Search for the unique marker
            let results = db.search_issues("unique marker").unwrap();
            prop_assert!(!results.is_empty());
            prop_assert!(results.iter().any(|i| i.title.contains("unique marker")));
        }

        /// Circular dependencies should be prevented
        #[test]
        fn prop_no_circular_deps(chain_len in 2usize..6) {
            let (db, _dir) = setup_test_db();

            // Create a chain of issues
            let mut ids = Vec::new();
            for i in 0..chain_len {
                let id = db.create_issue(&format!("Issue {i}"), None, "medium").unwrap();
                ids.push(id);
            }

            // Create a linear dependency chain: 0 <- 1 <- 2 <- ... <- n-1
            for i in 0..chain_len - 1 {
                db.add_dependency(ids[i], ids[i + 1]).unwrap();
            }

            // Trying to close the cycle (n-1 <- 0) should fail
            let result = db.add_dependency(ids[chain_len - 1], ids[0]);
            prop_assert!(result.is_err(), "Circular dependency should be rejected");
        }

        /// Deleting a parent should cascade to all children
        #[test]
        fn prop_cascade_deletes_children(child_count in 1usize..5) {
            let (db, _dir) = setup_test_db();

            // Create parent
            let parent_id = db.create_issue("Parent", None, "medium").unwrap();

            // Create children
            let mut child_ids = Vec::new();
            for i in 0..child_count {
                let id = db.create_subissue(parent_id, &format!("Child {i}"), None, "low").unwrap();
                child_ids.push(id);
            }

            // Verify children exist
            let issues_before = db.list_issues(None, None, None).unwrap();
            prop_assert_eq!(issues_before.len(), child_count + 1);

            // Delete parent
            db.delete_issue(parent_id).unwrap();

            // All children should be gone too
            let issues_after = db.list_issues(None, None, None).unwrap();
            prop_assert_eq!(issues_after.len(), 0);

            // Verify each child is gone
            for child_id in child_ids {
                let child = db.get_issue(child_id).unwrap();
                prop_assert!(child.is_none(), "Child should be deleted");
            }
        }

        /// Ready list should never contain issues with open blockers
        #[test]
        fn prop_ready_list_correctness(issue_count in 2usize..8) {
            let (db, _dir) = setup_test_db();

            // Create issues
            let mut ids = Vec::new();
            for i in 0..issue_count {
                let id = db.create_issue(&format!("Issue {i}"), None, "medium").unwrap();
                ids.push(id);
            }

            // Create some dependencies (each issue blocked by next, except last)
            for i in 0..issue_count - 1 {
                let _ = db.add_dependency(ids[i], ids[i + 1]);
            }

            // Get ready issues
            let ready = db.list_ready_issues().unwrap();

            // Verify: no ready issue should have open blockers
            for issue in &ready {
                let blockers = db.get_blockers(issue.id).unwrap();
                for blocker_id in blockers {
                    if let Some(blocker) = db.get_issue(blocker_id).unwrap() {
                        prop_assert_ne!(
                            blocker.status, IssueStatus::Open,
                            "Ready issue {} has open blocker {}",
                            issue.id, blocker_id
                        );
                    }
                }
            }
        }

        /// Session active_issue_id should be set to NULL when issue is deleted
        #[test]
        fn prop_session_issue_delete_cascade(title in safe_string()) {
            let (db, _dir) = setup_test_db();

            // Create issue and session
            let issue_id = db.create_issue(&title, None, "medium").unwrap();
            let session_id = db.start_session().unwrap();
            db.set_session_issue(session_id, issue_id).unwrap();

            // Verify session has issue
            let session = db.get_current_session().unwrap().unwrap();
            prop_assert_eq!(session.active_issue_id, Some(issue_id));

            // Delete the issue
            db.delete_issue(issue_id).unwrap();

            // Session should still exist but with NULL active_issue_id
            let session_after = db.get_current_session().unwrap().unwrap();
            prop_assert_eq!(session_after.id, session_id);
            prop_assert_eq!(session_after.active_issue_id, None, "Session active_issue_id should be NULL after issue deletion");
        }

        /// Search wildcards should be escaped properly
        #[test]
        fn prop_search_wildcards_escaped(
            prefix in "[a-zA-Z]{3,5}",
            suffix in "[a-zA-Z]{3,5}"
        ) {
            let (db, _dir) = setup_test_db();

            // Create an issue with % and _ in title
            let special_title = format!("{prefix}%test_marker{suffix}");
            db.create_issue(&special_title, None, "medium").unwrap();

            // Create another issue that would match if wildcards weren't escaped
            db.create_issue("other content here", None, "medium").unwrap();

            // Search for the special characters literally
            let results = db.search_issues("%test_").unwrap();

            // Should find only the issue with literal % and _
            prop_assert!(results.iter().all(|i| i.title.contains("%test_")));
        }
    }

    // -- Validation error paths --

    #[test]
    fn validate_status_rejects_invalid() {
        let err = validate_status("bogus").unwrap_err();
        assert!(err.to_string().contains("Invalid status"));
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn validate_status_accepts_valid() {
        for s in VALID_STATUSES {
            validate_status(s).unwrap();
        }
    }

    #[test]
    fn validate_priority_rejects_invalid() {
        let err = validate_priority("bogus").unwrap_err();
        assert!(err.to_string().contains("Invalid priority"));
        assert!(err.to_string().contains("bogus"));
    }

    #[test]
    fn validate_priority_accepts_valid() {
        for p in VALID_PRIORITIES {
            validate_priority(p).unwrap();
        }
    }

    // -- UUID lookups --

    #[test]
    fn get_issue_id_by_uuid_not_found() {
        let (db, _dir) = setup_test_db();
        let err = db.get_issue_id_by_uuid("nonexistent-uuid");
        assert!(err.is_err());
    }

    #[test]
    fn get_issue_uuid_by_id_not_found() {
        let (db, _dir) = setup_test_db();
        let err = db.get_issue_uuid_by_id(999);
        assert!(err.is_err());
    }

    #[test]
    fn require_issue_not_found() {
        let (db, _dir) = setup_test_db();
        let err = db.require_issue(999).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn require_issue_found() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("test", None, "medium").unwrap();
        let issue = db.require_issue(id).unwrap();
        assert_eq!(issue.title, "test");
    }

    #[test]
    fn update_issue_title_too_long() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("short", None, "medium").unwrap();
        let long_title = "x".repeat(MAX_TITLE_LEN + 1);
        let err = db
            .update_issue(id, Some(&long_title), None, None)
            .unwrap_err();
        assert!(err.to_string().contains("maximum length"));
    }

    #[test]
    fn update_issue_description_too_long() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("t", None, "medium").unwrap();
        let long_desc = "x".repeat(MAX_DESCRIPTION_LEN + 1);
        let err = db
            .update_issue(id, None, Some(&long_desc), None)
            .unwrap_err();
        assert!(err.to_string().contains("maximum length"));
    }

    #[test]
    fn add_label_too_long() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("t", None, "low").unwrap();
        let long_label = "x".repeat(MAX_LABEL_LEN + 1);
        let err = db.add_label(id, &long_label).unwrap_err();
        assert!(err.to_string().contains("maximum length"));
    }

    #[test]
    fn add_comment_too_long() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("t", None, "low").unwrap();
        let long_comment = "x".repeat(MAX_COMMENT_LEN + 1);
        let err = db
            .add_comment(id, &long_comment, "observation")
            .unwrap_err();
        assert!(err.to_string().contains("maximum length"));
    }

    #[test]
    fn add_dependency_self_blocking() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("t", None, "low").unwrap();
        let err = db.add_dependency(id, id).unwrap_err();
        assert!(err.to_string().contains("cannot block itself"));
    }

    #[test]
    fn remove_relation_reversed_order() {
        let (db, _dir) = setup_test_db();
        let a = db.create_issue("a", None, "low").unwrap();
        let b = db.create_issue("b", None, "low").unwrap();
        db.add_relation(a, b).unwrap();
        // Remove with reversed argument order (b, a instead of a, b)
        let removed = db.remove_relation(b, a).unwrap();
        assert!(removed);
    }

    #[test]
    fn milestone_lifecycle() {
        let (db, _dir) = setup_test_db();
        let mid = db.create_milestone("M1", Some("desc")).unwrap();
        let id = db.create_issue("t", None, "low").unwrap();

        // Add issue to milestone
        assert!(db.add_issue_to_milestone(mid, id).unwrap());
        // Remove issue from milestone
        assert!(db.remove_issue_from_milestone(mid, id).unwrap());
        // Close milestone
        assert!(db.close_milestone(mid).unwrap());
        // Delete milestone
        assert!(db.delete_milestone(mid).unwrap());
        // Delete again returns false
        assert!(!db.delete_milestone(mid).unwrap());
    }

    #[test]
    fn token_usage_with_filters() {
        let (db, _dir) = setup_test_db();
        let sid = db.start_session().unwrap();
        db.create_token_usage("agent-a", Some(sid), 100, 50, None, None, "opus", Some(0.5))
            .unwrap();
        db.create_token_usage(
            "agent-b",
            Some(sid),
            200,
            100,
            Some(10),
            Some(5),
            "sonnet",
            Some(0.3),
        )
        .unwrap();

        // Filter by agent_id
        let rows = db
            .list_token_usage(Some("agent-a"), None, None, None, None, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].agent_id, "agent-a");

        // Filter by session_id
        let rows = db
            .list_token_usage(None, Some(sid), None, None, None, None)
            .unwrap();
        assert_eq!(rows.len(), 2);

        // Filter by model
        let rows = db
            .list_token_usage(None, None, Some("sonnet"), None, None, None)
            .unwrap();
        assert_eq!(rows.len(), 1);

        // Filter by time range
        let past = "2020-01-01T00:00:00Z";
        let future = "2099-01-01T00:00:00Z";
        let rows = db
            .list_token_usage(None, None, None, Some(past), Some(future), None)
            .unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn usage_summary_with_filters() {
        let (db, _dir) = setup_test_db();
        let sid = db.start_session().unwrap();
        db.create_token_usage("agent-a", Some(sid), 100, 50, None, None, "opus", Some(0.5))
            .unwrap();
        db.create_token_usage(
            "agent-a",
            Some(sid),
            200,
            100,
            None,
            None,
            "opus",
            Some(0.3),
        )
        .unwrap();

        // Filter by agent
        let summary = db.get_usage_summary(Some("agent-a"), None, None).unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].request_count, 2);

        // Filter by time range
        let past = "2020-01-01T00:00:00Z";
        let future = "2099-01-01T00:00:00Z";
        let summary = db
            .get_usage_summary(None, Some(past), Some(future))
            .unwrap();
        assert_eq!(summary.len(), 1);
    }

    #[test]
    fn stop_timer_no_active_timer() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("t", None, "low").unwrap();
        // No timer started, stop returns false
        let stopped = db.stop_timer(id).unwrap();
        assert!(!stopped);
    }

    #[test]
    fn transaction_rolls_back_on_error() {
        let (db, _dir) = setup_test_db();
        // Run a transaction that fails -- the DB should remain unchanged
        let result: Result<()> = db.transaction(|| {
            db.create_issue("will-be-rolled-back", None, "low")?;
            anyhow::bail!("intentional error");
        });
        assert!(result.is_err());
        // No issue should have been persisted
        let issues = db.list_issues(None, None, None).unwrap();
        assert!(issues.is_empty());
    }

    #[test]
    fn get_issue_uuid_by_id_returns_uuid() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("uuid-test", None, "low").unwrap();
        let uuid = db.get_issue_uuid_by_id(id).unwrap();
        assert!(!uuid.is_empty());
        // Round-trip: look up by UUID should give back the same ID
        let found_id = db.get_issue_id_by_uuid(&uuid).unwrap();
        assert_eq!(found_id, id);
    }

    #[test]
    fn get_issue_id_by_uuid_missing_returns_error() {
        let (db, _dir) = setup_test_db();
        let result = db.get_issue_id_by_uuid("nonexistent-uuid-00000000");
        assert!(result.is_err());
    }

    #[test]
    fn get_issue_uuid_by_id_missing_returns_error() {
        let (db, _dir) = setup_test_db();
        let result = db.get_issue_uuid_by_id(99999);
        assert!(result.is_err());
    }

    #[test]
    fn parse_datetime_fallback_uses_current_time() {
        // parse_datetime is private; exercise it indirectly by inserting a row
        // with a corrupt datetime and reading it back.
        let (db, _dir) = setup_test_db();
        // Insert an issue with a bad timestamp directly via SQL
        db.conn
            .execute(
                "INSERT INTO issues (title, priority, status, created_at, updated_at, uuid) \
                 VALUES ('bad-dt', 'low', 'open', 'not-a-date', 'not-a-date', 'fake-uuid-bad-dt')",
                [],
            )
            .unwrap();
        // Reading the issue triggers parse_datetime on the bad value; it should
        // not panic and should return something reasonable (current time fallback).
        let issues = db.list_issues(None, None, None).unwrap();
        assert_eq!(issues.len(), 1);
        // The created_at will be near now (fallback), not a distant epoch
        assert!(issues[0].created_at.timestamp() > 0);
    }
}
