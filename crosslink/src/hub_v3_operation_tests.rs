//! Unit tests for `hydrate_from_state` (754a PASS 2 — hub-version-routed
//! operation). These live in the lib test tree because they only need the
//! library surface (`Database`, `CheckpointState`, `hydration`). The full
//! end-to-end V3 lifecycle / two-writer / lock / fetch / heartbeat / request
//! tests live in the bin test tree (`commands::hub_v3_operation_tests`) because
//! they drive the `migrate hub-v3` command, which is bin-only.

#![cfg(test)]

use std::path::Path;

use crate::db::Database;

#[test]
fn hydrate_from_state_empty_is_data_loss_guard() {
    // An empty state must NOT clear a populated SQLite (data-loss guard).
    let db = Database::open(Path::new(":memory:")).unwrap();
    let id = db.create_issue("keep me", None, "medium").unwrap();
    let empty = crate::checkpoint::CheckpointState::default();
    let stats = crate::hydration::hydrate_from_state(&empty, &db).unwrap();
    assert_eq!(stats.issues, 0, "empty state hydrates nothing");
    assert!(
        db.get_issue(id).unwrap().is_some(),
        "empty state must not wipe existing SQLite issues"
    );
}

#[test]
fn hydrate_from_state_preserves_sqlite_only_issue() {
    // #443 analogue: a direct-SQLite issue (created_by NULL) absent from the
    // reduced state survives hydration.
    let db = Database::open(Path::new(":memory:")).unwrap();
    let kept = db.create_issue("sqlite only", None, "low").unwrap();

    let mut state = crate::checkpoint::CheckpointState::default();
    let uuid = uuid::Uuid::new_v4();
    state.display_id_map.insert(uuid, 1);
    state
        .issues
        .insert(uuid, sample_compact_issue(uuid, 1, "from state"));

    crate::hydration::hydrate_from_state(&state, &db).unwrap();
    assert!(
        db.get_issue(kept).unwrap().is_some(),
        "SQLite-only issue must be preserved across hydrate_from_state"
    );
    assert!(
        db.get_issue(1).unwrap().is_some(),
        "state issue must be hydrated"
    );
}

#[test]
fn hydrate_from_state_maps_issue_children() {
    // A state issue with a label, comment, blocker, and milestone link
    // hydrates each child table row.
    let db = Database::open(Path::new(":memory:")).unwrap();

    let mut state = crate::checkpoint::CheckpointState::default();

    // Milestone (id 7) referenced by the issue.
    let ms_uuid = uuid::Uuid::new_v4();
    state.milestones.insert(
        ms_uuid,
        crate::checkpoint::CompactMilestone {
            uuid: ms_uuid,
            display_id: Some(7),
            name: "m1".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            created_at: chrono::Utc::now(),
            closed_at: None,
        },
    );

    // Blocker issue (id 2).
    let blocker_uuid = uuid::Uuid::new_v4();
    state.display_id_map.insert(blocker_uuid, 2);
    state.issues.insert(
        blocker_uuid,
        sample_compact_issue(blocker_uuid, 2, "blocker"),
    );

    // Main issue (id 1) with a label, comment, blocker, and milestone link.
    let uuid = uuid::Uuid::new_v4();
    state.display_id_map.insert(uuid, 1);
    let mut issue = sample_compact_issue(uuid, 1, "main");
    issue.labels.insert("bug".to_string());
    issue.blockers.insert(blocker_uuid);
    issue.milestone_uuid = Some(ms_uuid);
    let comment_uuid = uuid::Uuid::new_v4();
    issue.comments.insert(
        comment_uuid,
        crate::checkpoint::CompactComment {
            display_id: Some(5),
            author: "alpha".to_string(),
            content: "hello".to_string(),
            created_at: chrono::Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        },
    );
    state.issues.insert(uuid, issue);

    let stats = crate::hydration::hydrate_from_state(&state, &db).unwrap();
    assert_eq!(stats.issues, 2);
    assert_eq!(stats.milestones, 1);
    assert_eq!(stats.comments, 1);
    assert_eq!(stats.dependencies, 1);

    assert!(db.get_labels(1).unwrap().iter().any(|l| l == "bug"));
    assert!(!db.get_comments(1).unwrap().is_empty());
}

fn sample_compact_issue(
    uuid: uuid::Uuid,
    display_id: i64,
    title: &str,
) -> crate::checkpoint::CompactIssue {
    crate::checkpoint::CompactIssue {
        uuid,
        display_id: Some(display_id),
        title: title.to_string(),
        description: None,
        status: crate::models::IssueStatus::Open,
        priority: crate::models::Priority::Medium,
        parent_uuid: None,
        created_by: "alpha".to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        closed_at: None,
        scheduled_at: None,
        due_at: None,
        labels: Default::default(),
        blockers: Default::default(),
        related: Default::default(),
        milestone_uuid: None,
        comments: Default::default(),
        time_entries: Default::default(),
    }
}
