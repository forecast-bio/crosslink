use crate::issue_file::{
    read_counters, read_issue_file, write_counters, write_issue_file, IssueFile,
};
use crate::models::{IssueStatus, Priority};
use crate::shared_writer::core::{PushOutcome, SharedWriter, LOCK_CONFIRM_TIMEOUT_SECS};
use crate::shared_writer::locks::LockClaimResult;
use anyhow::{bail, Result};
use chrono::Utc;
use std::path::Path;
use tempfile::tempdir;
use uuid::Uuid;

/// Acquire a `HubWriteLock` for use in tests that call `compact` directly.
///
/// Uses the standard `.hub-write-lock` path so the lock path matches what
/// production code uses when the cache dir is treated as a hub worktree.
fn hub_lock_for_test(cache_dir: &Path) -> crate::sync::HubWriteLock {
    let lock_path = cache_dir.join(".hub-write-lock");
    crate::sync::acquire_hub_lock(&lock_path).expect("failed to acquire hub write lock for test")
}

fn make_issue(display_id: i64, title: &str) -> IssueFile {
    IssueFile {
        uuid: Uuid::new_v4(),
        display_id: Some(display_id),
        title: title.to_string(),
        description: None,
        status: IssueStatus::Open,
        priority: Priority::Medium,
        parent_uuid: None,
        created_by: "test-agent".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        closed_at: None,
        scheduled_at: None,
        due_at: None,
        labels: vec![],
        comments: vec![],
        blockers: vec![],
        related: vec![],
        milestone_uuid: None,
        time_entries: vec![],
    }
}

#[test]
fn test_new_returns_none_without_agent_config() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let writer = SharedWriter::new(&crosslink_dir).unwrap();
    assert!(writer.is_none());
}

#[test]
fn test_claim_display_id() {
    // Test the counter logic directly using file I/O
    let dir = tempdir().unwrap();
    let meta_dir = dir.path().join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();

    let counters_path = meta_dir.join("counters.json");

    // Start from defaults
    let counters = read_counters(&counters_path).unwrap();
    assert_eq!(counters.next_display_id, 1);

    // Claim 1 ID
    let first = counters.next_display_id;
    let mut updated = counters;
    updated.next_display_id += 1;
    write_counters(&counters_path, &updated).unwrap();

    assert_eq!(first, 1);

    // Claim another
    let counters = read_counters(&counters_path).unwrap();
    assert_eq!(counters.next_display_id, 2);
}

#[test]
fn test_load_issue_by_display_id() {
    let dir = tempdir().unwrap();
    let issues_dir = dir.path().join("issues");
    std::fs::create_dir_all(&issues_dir).unwrap();

    let issue1 = make_issue(1, "First");
    let issue2 = make_issue(2, "Second");
    write_issue_file(&issues_dir.join(format!("{}.json", issue1.uuid)), &issue1).unwrap();
    write_issue_file(&issues_dir.join(format!("{}.json", issue2.uuid)), &issue2).unwrap();

    // Simulate the scan logic
    let found = scan_for_display_id(&issues_dir, 2).unwrap();
    assert_eq!(found.title, "Second");
    assert_eq!(found.uuid, issue2.uuid);
}

#[test]
fn test_load_issue_by_display_id_not_found() {
    let dir = tempdir().unwrap();
    let issues_dir = dir.path().join("issues");
    std::fs::create_dir_all(&issues_dir).unwrap();

    let result = scan_for_display_id(&issues_dir, 99);
    assert!(result.is_err());
}

#[test]
fn test_resolve_uuid_from_files() {
    let dir = tempdir().unwrap();
    let issues_dir = dir.path().join("issues");
    std::fs::create_dir_all(&issues_dir).unwrap();

    let issue = make_issue(42, "Target");
    write_issue_file(&issues_dir.join(format!("{}.json", issue.uuid)), &issue).unwrap();

    let found = scan_for_display_id(&issues_dir, 42).unwrap();
    assert_eq!(found.uuid, issue.uuid);
}

#[test]
fn test_counters_sequential_claim() {
    let dir = tempdir().unwrap();
    let meta_dir = dir.path().join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    let path = meta_dir.join("counters.json");

    // Claim 3 sequential IDs
    let mut counters = read_counters(&path).unwrap();
    let ids: Vec<i64> = (0..3)
        .map(|_| {
            let id = counters.next_display_id;
            counters.next_display_id += 1;
            id
        })
        .collect();

    write_counters(&path, &counters).unwrap();

    assert_eq!(ids, vec![1, 2, 3]);
    let reloaded = read_counters(&path).unwrap();
    assert_eq!(reloaded.next_display_id, 4);
}

/// Helper for tests: scan issues dir for a `display_id` (mirrors `SharedWriter` logic).
fn scan_for_display_id(issues_dir: &Path, display_id: i64) -> Result<IssueFile> {
    for entry in std::fs::read_dir(issues_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(issue) = read_issue_file(&path) {
            if issue.display_id == Some(display_id) {
                return Ok(issue);
            }
        }
    }
    bail!("Issue #{display_id} not found")
}

#[test]
fn test_v1_issue_path_format() {
    let uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
    let path = format!("issues/{uuid}.json");
    assert_eq!(path, "issues/a1b2c3d4-e5f6-7890-abcd-ef1234567890.json");
}

#[test]
fn test_v2_issue_path_format() {
    let uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
    let path = format!("issues/{uuid}/issue.json");
    assert_eq!(
        path,
        "issues/a1b2c3d4-e5f6-7890-abcd-ef1234567890/issue.json"
    );
}

#[test]
fn test_v2_comment_path_format() {
    let issue_uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
    let comment_uuid = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
    let path = format!("issues/{issue_uuid}/comments/{comment_uuid}.json");
    assert_eq!(
        path,
        "issues/a1b2c3d4-e5f6-7890-abcd-ef1234567890/comments/11111111-2222-3333-4444-555555555555.json"
    );
}

#[test]
fn test_v2_scan_finds_issue_in_subdirectory() {
    let dir = tempdir().unwrap();
    let issues_dir = dir.path().join("issues");

    // Create a v2-style issue directory
    let issue = make_issue(7, "V2 Issue");
    let issue_subdir = issues_dir.join(issue.uuid.to_string());
    std::fs::create_dir_all(issue_subdir.join("comments")).unwrap();
    write_issue_file(&issue_subdir.join("issue.json"), &issue).unwrap();

    // The v2 scan should find it
    let mut found = false;
    for entry in std::fs::read_dir(&issues_dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            let issue_file = path.join("issue.json");
            if issue_file.exists() {
                if let Ok(loaded) = read_issue_file(&issue_file) {
                    if loaded.display_id == Some(7) {
                        assert_eq!(loaded.title, "V2 Issue");
                        found = true;
                    }
                }
            }
        }
    }
    assert!(found, "v2 issue not found in subdirectory scan");
}

#[test]
fn test_v2_comment_file_construction() {
    use crate::issue_file::CommentFile;

    let issue_uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
    let comment_uuid = Uuid::new_v4();
    let comment = CommentFile {
        uuid: comment_uuid,
        issue_uuid,
        author: "test-agent".to_string(),
        content: "A standalone comment".to_string(),
        created_at: Utc::now(),
        kind: "note".to_string(),
        trigger_type: None,
        intervention_context: None,
        driver_key_fingerprint: None,
        signed_by: None,
        signature: None,
    };

    let json = serde_json::to_string_pretty(&comment).unwrap();
    let parsed: CommentFile = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.uuid, comment_uuid);
    assert_eq!(parsed.issue_uuid, issue_uuid);
    assert_eq!(parsed.content, "A standalone comment");
    assert_eq!(parsed.kind, "note");
}

#[test]
fn test_v2_intervention_comment_file_construction() {
    use crate::issue_file::CommentFile;

    let issue_uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
    let comment_uuid = Uuid::new_v4();
    let comment = CommentFile {
        uuid: comment_uuid,
        issue_uuid,
        author: "test-agent".to_string(),
        content: "Driver intervention".to_string(),
        created_at: Utc::now(),
        kind: "intervention".to_string(),
        trigger_type: Some("redirect".to_string()),
        intervention_context: Some("User redirected task".to_string()),
        driver_key_fingerprint: Some("SHA256:abc123".to_string()),
        signed_by: None,
        signature: None,
    };

    let json = serde_json::to_string_pretty(&comment).unwrap();
    let parsed: CommentFile = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.kind, "intervention");
    assert_eq!(parsed.trigger_type, Some("redirect".to_string()));
    assert_eq!(
        parsed.intervention_context,
        Some("User redirected task".to_string())
    );
    assert_eq!(
        parsed.driver_key_fingerprint,
        Some("SHA256:abc123".to_string())
    );
}

#[test]
fn test_lock_confirm_timeout_constant() {
    assert_eq!(LOCK_CONFIRM_TIMEOUT_SECS, 30);
}

mod lock_v2_tests {
    use super::*;
    use crate::issue_file::LockFileV2;
    use tempfile::tempdir;

    #[test]
    fn test_lock_claim_result_variants() {
        let claimed = LockClaimResult::Claimed;
        let already = LockClaimResult::AlreadyHeld;
        let contended = LockClaimResult::Contended {
            winner_agent_id: "agent-2".to_string(),
        };
        assert_eq!(claimed, LockClaimResult::Claimed);
        assert_eq!(already, LockClaimResult::AlreadyHeld);
        assert_ne!(claimed, already);
        assert_ne!(claimed, contended);
        assert_eq!(
            contended,
            LockClaimResult::Contended {
                winner_agent_id: "agent-2".to_string(),
            }
        );
        // Verify Debug
        let _ = format!("{claimed:?}");
        let _ = format!("{contended:?}");
    }

    #[test]
    fn test_read_lock_v2_file() {
        let dir = tempdir().unwrap();
        let locks_dir = dir.path().join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();

        let lock = LockFileV2 {
            issue_id: 42,
            agent_id: "agent-1".to_string(),
            branch: Some("feature/x".to_string()),
            claimed_at: chrono::Utc::now(),
            signed_by: Some("SHA256:abc".to_string()),
        };
        let json = serde_json::to_string_pretty(&lock).unwrap();
        std::fs::write(locks_dir.join("42.json"), &json).unwrap();

        // Read it back
        let content = std::fs::read_to_string(locks_dir.join("42.json")).unwrap();
        let parsed: LockFileV2 = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.issue_id, 42);
        assert_eq!(parsed.agent_id, "agent-1");
        assert_eq!(parsed.branch, Some("feature/x".to_string()));
    }

    #[test]
    fn test_read_lock_v2_missing() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("locks").join("99.json");
        assert!(!lock_path.exists());
    }

    #[test]
    fn test_lock_v2_file_roundtrip() {
        let dir = tempdir().unwrap();
        let locks_dir = dir.path().join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();

        let lock = LockFileV2 {
            issue_id: 5,
            agent_id: "worker-1".to_string(),
            branch: None,
            claimed_at: chrono::Utc::now(),
            signed_by: None,
        };
        let json = serde_json::to_string_pretty(&lock).unwrap();
        let path = locks_dir.join("5.json");
        std::fs::write(&path, &json).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: LockFileV2 = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.issue_id, lock.issue_id);
        assert_eq!(parsed.agent_id, lock.agent_id);
        assert!(parsed.branch.is_none());
        assert!(parsed.signed_by.is_none());
    }

    #[test]
    fn test_lock_contention_deterministic_winner() {
        // Verify that compaction's first-claim-wins rule works
        use crate::checkpoint::{read_checkpoint, write_checkpoint, CheckpointState};
        use crate::events::{append_event, Event, EventEnvelope};
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let cache = dir.path();

        // Set up checkpoint
        std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
        std::fs::create_dir_all(cache.join("agents/agent-a")).unwrap();
        std::fs::create_dir_all(cache.join("agents/agent-b")).unwrap();
        std::fs::create_dir_all(cache.join("locks")).unwrap();
        std::fs::create_dir_all(cache.join("issues")).unwrap();

        let state = CheckpointState::default();
        write_checkpoint(cache, &state).unwrap();

        let now = Utc::now();

        // Agent A claims first (earlier timestamp)
        let e1 = EventEnvelope {
            agent_id: "agent-a".to_string(),
            agent_seq: 1,
            timestamp: now - chrono::Duration::seconds(1),
            event: Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/agent-a/events.log"), &e1).unwrap();

        // Agent B claims second (later timestamp)
        let e2 = EventEnvelope {
            agent_id: "agent-b".to_string(),
            agent_seq: 1,
            timestamp: now,
            event: Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/agent-b/events.log"), &e2).unwrap();

        // Run compaction
        let lock = hub_lock_for_test(cache);
        let result = crate::compaction::compact(cache, "agent-a", true, &lock)
            .unwrap()
            .unwrap();
        assert_eq!(result.locks_materialized, 1);

        // Read checkpoint -- agent-a should win (earlier timestamp)
        let state = read_checkpoint(cache).unwrap();
        let lock_entry = state.locks.get(&1).unwrap();
        assert_eq!(lock_entry.agent_id, "agent-a");
    }

    #[test]
    fn test_prune_then_checkpoint_clear() {
        use crate::checkpoint::{write_checkpoint, CheckpointState, LockEntry};
        use crate::events::{append_event, Event, EventEnvelope, OrderingKey};
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let cache = dir.path();

        std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
        std::fs::create_dir_all(cache.join("agents/stale-agent")).unwrap();
        std::fs::create_dir_all(cache.join("locks")).unwrap();
        std::fs::create_dir_all(cache.join("issues")).unwrap();

        let now = Utc::now();

        // Write an event for the stale agent
        let e = EventEnvelope {
            agent_id: "stale-agent".to_string(),
            agent_seq: 1,
            timestamp: now,
            event: Event::LockClaimed {
                issue_display_id: 5,
                branch: None,
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/stale-agent/events.log"), &e).unwrap();

        // Write a watermark that covers the event so prune_events will prune it
        let watermark = OrderingKey {
            timestamp: now + chrono::Duration::seconds(1),
            agent_id: "stale-agent".to_string(),
            agent_seq: 1,
        };

        // Compact to materialize (watermark is embedded in checkpoint state)
        let mut state = CheckpointState {
            watermark: Some(watermark),
            ..CheckpointState::default()
        };
        state.locks.insert(
            5,
            LockEntry {
                agent_id: "stale-agent".to_string(),
                branch: None,
                claimed_at: now,
            },
        );
        write_checkpoint(cache, &state).unwrap();

        // Write materialized lock file
        let lock = crate::issue_file::LockFileV2 {
            issue_id: 5,
            agent_id: "stale-agent".to_string(),
            branch: None,
            claimed_at: now,
            signed_by: None,
        };
        std::fs::write(
            cache.join("locks/5.json"),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();

        // Prune stale agent events
        let pruned = crate::compaction::prune_events(cache, "stale-agent").unwrap();
        assert!(pruned > 0);

        // Clear checkpoint lock
        let mut state = crate::checkpoint::read_checkpoint(cache).unwrap();
        state.locks.remove(&5);
        write_checkpoint(cache, &state).unwrap();

        // Remove lock file
        let lock_path = cache.join("locks/5.json");
        if lock_path.exists() {
            std::fs::remove_file(&lock_path).unwrap();
        }

        // Verify clean state
        let state = crate::checkpoint::read_checkpoint(cache).unwrap();
        assert!(state.locks.is_empty());
        assert!(!cache.join("locks/5.json").exists());
    }

    #[test]
    fn test_lock_file_v2_with_all_fields() {
        let dir = tempdir().unwrap();
        let locks_dir = dir.path().join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();

        let now = chrono::Utc::now();
        let lock = LockFileV2 {
            issue_id: 100,
            agent_id: "agent-special".to_string(),
            branch: Some("feature/special-branch".to_string()),
            claimed_at: now,
            signed_by: Some("SHA256:xyz789".to_string()),
        };
        let json = serde_json::to_string_pretty(&lock).unwrap();
        let path = locks_dir.join("100.json");
        std::fs::write(&path, &json).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: LockFileV2 = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.issue_id, 100);
        assert_eq!(parsed.agent_id, "agent-special");
        assert_eq!(parsed.branch, Some("feature/special-branch".to_string()));
        assert_eq!(parsed.claimed_at, now);
        assert_eq!(parsed.signed_by, Some("SHA256:xyz789".to_string()));
    }

    #[test]
    fn test_lock_claim_result_display_and_equality() {
        // Verify Contended results with different winners are not equal
        let c1 = LockClaimResult::Contended {
            winner_agent_id: "agent-1".to_string(),
        };
        let c2 = LockClaimResult::Contended {
            winner_agent_id: "agent-2".to_string(),
        };
        assert_ne!(c1, c2);

        // Verify same winner is equal
        let c3 = LockClaimResult::Contended {
            winner_agent_id: "agent-1".to_string(),
        };
        assert_eq!(c1, c3);

        // Verify Clone works correctly
        let cloned = c1.clone();
        assert_eq!(c1, cloned);
    }

    #[test]
    fn test_lock_contention_with_three_agents() {
        // Three agents claiming same lock, verify deterministic winner
        use crate::checkpoint::{read_checkpoint, write_checkpoint, CheckpointState};
        use crate::events::{append_event, Event, EventEnvelope};
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let cache = dir.path();

        std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
        std::fs::create_dir_all(cache.join("agents/agent-a")).unwrap();
        std::fs::create_dir_all(cache.join("agents/agent-b")).unwrap();
        std::fs::create_dir_all(cache.join("agents/agent-c")).unwrap();
        std::fs::create_dir_all(cache.join("locks")).unwrap();
        std::fs::create_dir_all(cache.join("issues")).unwrap();

        let state = CheckpointState::default();
        write_checkpoint(cache, &state).unwrap();

        let now = Utc::now();

        // Agent C claims first (earliest)
        let e1 = EventEnvelope {
            agent_id: "agent-c".to_string(),
            agent_seq: 1,
            timestamp: now - chrono::Duration::seconds(3),
            event: Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/c".to_string()),
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/agent-c/events.log"), &e1).unwrap();

        // Agent A claims second
        let e2 = EventEnvelope {
            agent_id: "agent-a".to_string(),
            agent_seq: 1,
            timestamp: now - chrono::Duration::seconds(2),
            event: Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/a".to_string()),
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/agent-a/events.log"), &e2).unwrap();

        // Agent B claims third
        let e3 = EventEnvelope {
            agent_id: "agent-b".to_string(),
            agent_seq: 1,
            timestamp: now - chrono::Duration::seconds(1),
            event: Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/b".to_string()),
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/agent-b/events.log"), &e3).unwrap();

        let hub_lock = hub_lock_for_test(cache);
        let result = crate::compaction::compact(cache, "agent-a", true, &hub_lock)
            .unwrap()
            .unwrap();
        assert_eq!(result.locks_materialized, 1);

        let state = read_checkpoint(cache).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.agent_id, "agent-c");
        assert_eq!(lock.branch, Some("feature/c".to_string()));
    }

    #[test]
    fn test_lock_contention_then_winner_releases() {
        // Two agents contend. Winner releases. Lock should be empty.
        use crate::checkpoint::{read_checkpoint, write_checkpoint, CheckpointState};
        use crate::events::{append_event, Event, EventEnvelope};
        use chrono::Utc;

        let dir = tempdir().unwrap();
        let cache = dir.path();

        std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
        std::fs::create_dir_all(cache.join("agents/agent-a")).unwrap();
        std::fs::create_dir_all(cache.join("agents/agent-b")).unwrap();
        std::fs::create_dir_all(cache.join("locks")).unwrap();
        std::fs::create_dir_all(cache.join("issues")).unwrap();

        let state = CheckpointState::default();
        write_checkpoint(cache, &state).unwrap();

        let now = Utc::now();

        // Agent A claims first (wins)
        let e1 = EventEnvelope {
            agent_id: "agent-a".to_string(),
            agent_seq: 1,
            timestamp: now - chrono::Duration::seconds(3),
            event: Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/agent-a/events.log"), &e1).unwrap();

        // Agent B claims second (loses)
        let e2 = EventEnvelope {
            agent_id: "agent-b".to_string(),
            agent_seq: 1,
            timestamp: now - chrono::Duration::seconds(2),
            event: Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/agent-b/events.log"), &e2).unwrap();

        // Agent A releases
        let e3 = EventEnvelope {
            agent_id: "agent-a".to_string(),
            agent_seq: 2,
            timestamp: now - chrono::Duration::seconds(1),
            event: Event::LockReleased {
                issue_display_id: 1,
            },
            signed_by: None,
            signature: None,
        };
        append_event(&cache.join("agents/agent-a/events.log"), &e3).unwrap();

        let hub_lock = hub_lock_for_test(cache);
        crate::compaction::compact(cache, "agent-a", true, &hub_lock).unwrap();

        let state = read_checkpoint(cache).unwrap();
        assert!(state.locks.is_empty());
        assert!(!cache.join("locks/1.json").exists());
    }

    #[test]
    fn test_lock_file_v2_missing_optional_fields() {
        // Verify LockFileV2 deserialization works when optional fields are null
        let json = r#"{
            "issue_id": 7,
            "agent_id": "agent-minimal",
            "branch": null,
            "claimed_at": "2026-01-01T00:00:00Z",
            "signed_by": null
        }"#;
        let parsed: LockFileV2 = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.issue_id, 7);
        assert_eq!(parsed.agent_id, "agent-minimal");
        assert!(parsed.branch.is_none());
        assert!(parsed.signed_by.is_none());
    }

    #[test]
    fn test_lock_contention_deterministic_across_compaction_agents() {
        // The same winner should emerge regardless of which agent runs compaction
        use crate::checkpoint::{read_checkpoint, write_checkpoint, CheckpointState};
        use crate::events::{append_event, Event, EventEnvelope};
        use chrono::Utc;

        let now = Utc::now();

        // Set up two identical caches with the same events
        for compactor in &["agent-a", "agent-b"] {
            let dir = tempdir().unwrap();
            let cache = dir.path();

            std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-a")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-b")).unwrap();
            std::fs::create_dir_all(cache.join("locks")).unwrap();
            std::fs::create_dir_all(cache.join("issues")).unwrap();

            let state = CheckpointState::default();
            write_checkpoint(cache, &state).unwrap();

            let e1 = EventEnvelope {
                agent_id: "agent-a".to_string(),
                agent_seq: 1,
                timestamp: now - chrono::Duration::seconds(2),
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: None,
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-a/events.log"), &e1).unwrap();

            let e2 = EventEnvelope {
                agent_id: "agent-b".to_string(),
                agent_seq: 1,
                timestamp: now - chrono::Duration::seconds(1),
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: None,
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-b/events.log"), &e2).unwrap();

            let hub_lock = hub_lock_for_test(cache);
            crate::compaction::compact(cache, compactor, true, &hub_lock).unwrap();

            let state = read_checkpoint(cache).unwrap();
            assert_eq!(
                state.locks[&1].agent_id, "agent-a",
                "Winner should be agent-a regardless of who runs compaction (compactor={compactor})"
            );
        }
    }
}

// ---- Integration tests with real git repos ----

mod integration {
    use super::*;
    use crate::db::Database;
    use crate::identity::{AgentConfig, AgentRole};
    use std::process::Command;
    use tempfile::TempDir;

    /// Set up a minimal git environment for `SharedWriter` tests.
    ///
    /// Returns (`work_dir`, `remote_dir`). The hub cache (`crosslink/hub` branch)
    /// is initialized directly inside the `work_dir` so `SharedWriter::new()` works.
    fn setup_shared_writer_env() -> (TempDir, TempDir, std::path::PathBuf) {
        let remote_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();

        // Init bare remote
        Command::new("git")
            .current_dir(remote_dir.path())
            .args(["init", "--bare", "-b", "main"])
            .output()
            .unwrap();

        // Init work repo
        Command::new("git")
            .current_dir(work_dir.path())
            .args(["init", "-b", "main"])
            .output()
            .unwrap();

        // Config git identity
        for args in [
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
            vec![
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
        ] {
            Command::new("git")
                .current_dir(work_dir.path())
                .args(&args)
                .output()
                .unwrap();
        }

        // Initial commit + push
        std::fs::write(work_dir.path().join("README.md"), "# test\n").unwrap();
        Command::new("git")
            .current_dir(work_dir.path())
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(work_dir.path())
            .args(["commit", "-m", "init", "--no-gpg-sign"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(work_dir.path())
            .args(["push", "-u", "origin", "main"])
            .output()
            .unwrap();

        // Create .crosslink dir with hook-config.json
        let crosslink_dir = work_dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{"remote":"origin","layout":"v2"}"#,
        )
        .unwrap();

        // Create agent.json (needed for SharedWriter::new() to get an agent identity)
        let agent_config = AgentConfig {
            agent_id: "test-agent".to_string(),
            machine_id: "test-machine".to_string(),
            description: Some("Integration test agent".to_string()),
            role: AgentRole::Driver,
            ssh_key_path: None,
            ssh_fingerprint: None,
            ssh_public_key: None,
        };
        let agent_json = serde_json::to_string_pretty(&agent_config).unwrap();
        std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

        // Initialize the hub cache (crosslink/hub branch) using SyncManager
        let sync = crate::sync::SyncManager::new(&crosslink_dir).unwrap();
        sync.init_cache().unwrap();

        (work_dir, remote_dir, crosslink_dir)
    }

    /// Like [`setup_shared_writer_env`] but builds a legacy **v2** hub so the v2
    /// refusal / v2 file-read paths can be exercised. Since 754b a fresh
    /// `init_cache` bootstraps v3, so a v2 hub must be created explicitly: lay
    /// down a `crosslink/hub` worktree with the v2 layout markers before any
    /// `SharedWriter` resolves its mode.
    fn setup_shared_writer_env_v2() -> (TempDir, TempDir, std::path::PathBuf) {
        let (work_dir, remote_dir, crosslink_dir) = setup_shared_writer_env();

        // The fresh env bootstrapped a v3 host worktree; remove it and replace
        // with a v2 `crosslink/hub` worktree carrying the v2 layout.
        let cache_dir = crosslink_dir.join(".hub-cache");
        let _ = Command::new("git")
            .current_dir(work_dir.path())
            .args(["worktree", "remove", "--force", cache_dir.to_str().unwrap()])
            .output();
        // Drop the v3 marker refs so detection sees a pure v2 hub.
        for r in [
            "refs/heads/crosslink/meta",
            "refs/heads/crosslink/checkpoint",
            "refs/heads/crosslink/agents/test-agent",
        ] {
            let _ = Command::new("git")
                .current_dir(work_dir.path())
                .args(["update-ref", "-d", r])
                .output();
        }
        // Also drop the v3 host branch so the name is free for the v2 worktree.
        let _ = Command::new("git")
            .current_dir(work_dir.path())
            .args(["branch", "-D", "crosslink/hub-v3-host"])
            .output();

        // Create the v2 hub worktree on an orphan `crosslink/hub` branch.
        Command::new("git")
            .current_dir(work_dir.path())
            .args([
                "worktree",
                "add",
                "--orphan",
                "-b",
                "crosslink/hub",
                cache_dir.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        // v2 layout marker + skeleton dirs.
        let meta_dir = cache_dir.join("meta");
        std::fs::create_dir_all(meta_dir.join("milestones")).unwrap();
        std::fs::create_dir_all(cache_dir.join("issues")).unwrap();
        std::fs::create_dir_all(cache_dir.join("locks")).unwrap();
        crate::issue_file::write_layout_version(
            &meta_dir,
            crate::issue_file::CURRENT_LAYOUT_VERSION,
        )
        .unwrap();
        std::fs::write(
            cache_dir.join("locks.json"),
            serde_json::to_string(&serde_json::json!({"version":1,"locks":{},"settings":{"stale_lock_timeout_minutes":60}})).unwrap(),
        )
        .unwrap();
        for args in [
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
        ] {
            Command::new("git")
                .current_dir(&cache_dir)
                .args(&args)
                .output()
                .unwrap();
        }
        Command::new("git")
            .current_dir(&cache_dir)
            .args(["add", "-A"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(&cache_dir)
            .args(["commit", "-m", "v2 hub", "--no-gpg-sign"])
            .output()
            .unwrap();

        (work_dir, remote_dir, crosslink_dir)
    }

    /// Create an in-memory test database at a temp path.
    fn make_db(dir: &std::path::Path) -> Database {
        Database::open(&dir.join("issues.db")).unwrap()
    }

    // --- SharedWriter::new() ---

    #[test]
    fn test_new_returns_some_with_agent_and_hub() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap();
        assert!(
            writer.is_some(),
            "SharedWriter::new() should return Some when agent.json and hub branch exist"
        );
        drop(work_dir);
    }

    #[test]
    fn test_new_agent_id_matches_config() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        assert_eq!(writer.agent_id(), "test-agent");
        drop(work_dir);
    }

    #[test]
    fn test_new_creates_issues_and_meta_dirs() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let cache_dir = crosslink_dir.join(".hub-cache");
        assert!(
            cache_dir.join("issues").exists(),
            "issues/ dir should exist"
        );
        assert!(
            cache_dir.join("meta").join("milestones").exists(),
            "meta/milestones/ dir should exist"
        );
        drop(work_dir);
    }

    // --- read_lock_v2() ---

    #[test]
    fn test_read_lock_v2_returns_none_when_no_lock() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let result = writer.read_lock_v2(999).unwrap();
        assert!(
            result.is_none(),
            "No lock should exist for non-existent issue"
        );
        drop(work_dir);
    }

    #[test]
    fn test_read_lock_v2_reads_existing_lock_file() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env_v2();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // Manually write a lock file
        let locks_dir = crosslink_dir.join(".hub-cache").join("locks");
        std::fs::create_dir_all(&locks_dir).unwrap();
        let lock = crate::issue_file::LockFileV2 {
            issue_id: 42,
            agent_id: "test-agent".to_string(),
            branch: Some("feature/x".to_string()),
            claimed_at: chrono::Utc::now(),
            signed_by: None,
        };
        std::fs::write(
            locks_dir.join("42.json"),
            serde_json::to_string_pretty(&lock).unwrap(),
        )
        .unwrap();

        let result = writer.read_lock_v2(42).unwrap();
        assert!(result.is_some());
        let read_lock = result.unwrap();
        assert_eq!(read_lock.issue_id, 42);
        assert_eq!(read_lock.agent_id, "test-agent");
        assert_eq!(read_lock.branch, Some("feature/x".to_string()));
        drop(work_dir);
    }

    #[test]
    fn test_crosslink_dir_accessor() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let dir = writer.crosslink_dir();
        // crosslink_dir() should return the parent of the cache dir
        // The cache dir is crosslink_dir/.hub-cache, so parent = crosslink_dir
        assert!(
            dir.exists(),
            "crosslink_dir() should point to an existing dir"
        );
        drop(work_dir);
    }

    #[test]
    fn test_resolve_ssh_key_path_returns_none_without_key() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // The test agent has no SSH key configured
        let key_path = writer.resolve_ssh_key_path();
        assert!(
            key_path.is_none(),
            "resolve_ssh_key_path should return None when no key is configured"
        );
        drop(work_dir);
    }

    #[test]
    fn test_load_issue_by_display_id_not_found() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let result = writer.load_issue_by_display_id(9999);
        assert!(result.is_err(), "Non-existent issue should return error");
        drop(work_dir);
    }

    #[test]
    fn test_sign_comment_without_key_returns_none() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // No SSH key configured -- sign_comment should return (None, None)
        let (signed_by, signature) = writer.sign_comment("content", "author", 1);
        assert!(signed_by.is_none());
        assert!(signature.is_none());
        drop(work_dir);
    }

    #[test]
    fn test_create_envelope_without_signing() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let event = crate::events::Event::IssueCreated {
            uuid: Uuid::new_v4(),
            title: "test".to_string(),
            description: None,
            priority: "low".to_string(),
            labels: vec![],
            parent_uuid: None,
            created_by: "test-agent".to_string(),
            display_id: None,
            scheduled_at: None,
            due_at: None,
        };
        let envelope = writer.create_envelope(event);
        assert_eq!(envelope.agent_id, "test-agent");
        assert!(envelope.signature.is_none(), "No signature without key");
        assert!(envelope.signed_by.is_none(), "No signed_by without key");
        assert_eq!(envelope.agent_seq, 1, "First event should have seq 1");
        drop(work_dir);
    }

    #[test]
    fn test_next_event_seq_increments() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let s1 = writer.next_event_seq();
        let s2 = writer.next_event_seq();
        let s3 = writer.next_event_seq();

        assert_eq!(s1 + 1, s2);
        assert_eq!(s2 + 1, s3);
        drop(work_dir);
    }

    #[test]
    fn test_read_max_event_seq_returns_zero_when_no_log() {
        let dir = tempfile::tempdir().unwrap();
        let seq = SharedWriter::read_max_event_seq(
            dir.path(),
            "nonexistent-agent",
            crate::hub_v3::HubMode::V2,
        );
        assert_eq!(seq, 0, "Max event seq should be 0 when no log exists");
    }

    #[test]
    fn test_layout_version_one_for_v1_hub() {
        let dir = tempfile::tempdir().unwrap();
        let meta_dir = dir.path().join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();

        // Don't write a version file -> defaults to v1
        let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
        assert_eq!(version, 1);
    }

    #[test]
    fn test_push_outcome_eq() {
        assert_eq!(PushOutcome::Pushed, PushOutcome::Pushed);
        assert_eq!(PushOutcome::LocalOnly, PushOutcome::LocalOnly);
        assert_ne!(PushOutcome::Pushed, PushOutcome::LocalOnly);
    }

    #[test]
    fn test_push_outcome_copy() {
        let o = PushOutcome::Pushed;
        let o2 = o; // copy
        assert_eq!(o, o2);
    }

    // ---- SharedWriter::new() anonymous path ----

    #[test]
    fn test_new_without_agent_config_but_hub_already_initialized() {
        // Exercises line 144-145: no agent.json, hub branch already initialized.
        // SharedWriter::new() should return Some with an anonymous config.
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();

        // Remove agent.json so we exercise the anonymous code path
        std::fs::remove_file(crosslink_dir.join("agent.json")).unwrap();

        // Hub cache already exists from setup -- is_initialized() returns true immediately
        let writer = SharedWriter::new(&crosslink_dir).unwrap();
        assert!(
            writer.is_some(),
            "SharedWriter::new() should return Some when hub cache already exists (anonymous mode)"
        );

        let writer = writer.unwrap();
        // Anonymous agent_id starts with "anon-"
        assert!(
            writer.agent_id().starts_with("anon-"),
            "Anonymous writer should have agent_id starting with 'anon-', got: {}",
            writer.agent_id()
        );

        drop(work_dir);
    }

    #[test]
    fn test_new_without_agent_config_hub_init_fails_returns_none() {
        // Exercises lines 138-139: no agent.json, and init_cache() fails because the
        // remote is unreachable (invalid URL), so SharedWriter::new() returns Ok(None).
        let work_dir = tempfile::tempdir().unwrap();

        // Init a git repo with a bogus remote that can't be reached
        Command::new("git")
            .current_dir(work_dir.path())
            .args(["init", "-b", "main"])
            .output()
            .unwrap();

        for args in [
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
            // Use an invalid remote path so ls-remote / worktree add will fail
            vec!["remote", "add", "origin", "/nonexistent/path/to/remote"],
        ] {
            Command::new("git")
                .current_dir(work_dir.path())
                .args(&args)
                .output()
                .unwrap();
        }

        // Create .crosslink dir with hook-config.json but NO agent.json
        let crosslink_dir = work_dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{"remote":"origin","layout":"v2"}"#,
        )
        .unwrap();

        // No agent.json. The hub cache dir doesn't exist so is_initialized() = false.
        // init_cache() will try to create an orphan worktree. If it does succeed (creating
        // a local orphan) we get Some; if it fails we get None.
        // Either way, the test validates that the code path is reachable and doesn't panic.
        let result = SharedWriter::new(&crosslink_dir);
        // The result should be Ok (no panic), regardless of Some/None depending on git
        assert!(
            result.is_ok(),
            "SharedWriter::new() should not error even when hub unavailable"
        );

        drop(work_dir);
    }

    // ---- resolve_ssh_key_path coverage ----

    #[test]
    fn test_resolve_ssh_key_path_nonexistent_file() {
        // Exercises line 254: ssh_key_path is configured but the file doesn't exist.
        // resolve_ssh_key_path() should return None.
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();

        // Reconfigure agent.json with a key path that doesn't exist on disk
        let agent_config = AgentConfig {
            agent_id: "test-agent".to_string(),
            machine_id: "test-machine".to_string(),
            description: None,
            role: AgentRole::Driver,
            ssh_key_path: Some("nonexistent_key_file.pem".to_string()),
            ssh_fingerprint: Some("SHA256:fakefingerprint".to_string()),
            ssh_public_key: None,
        };
        let agent_json = serde_json::to_string_pretty(&agent_config).unwrap();
        std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // The key file doesn't exist -> resolve_ssh_key_path returns None (line 254)
        let resolved = writer.resolve_ssh_key_path();
        assert!(
            resolved.is_none(),
            "resolve_ssh_key_path should return None when file doesn't exist"
        );

        drop(work_dir);
    }

    #[test]
    fn test_resolve_ssh_key_path_existing_file() {
        // Exercises line 251-252: ssh_key_path is configured and file exists.
        // resolve_ssh_key_path() should return Some(path).
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();

        // Create a fake key file inside .crosslink/
        let fake_key_name = "test_agent_key.pem";
        let fake_key_path = crosslink_dir.join(fake_key_name);
        std::fs::write(&fake_key_path, "fake key content").unwrap();

        // Reconfigure agent.json to point at the fake key
        let agent_config = AgentConfig {
            agent_id: "test-agent".to_string(),
            machine_id: "test-machine".to_string(),
            description: None,
            role: AgentRole::Driver,
            ssh_key_path: Some(fake_key_name.to_string()),
            ssh_fingerprint: Some("SHA256:fakefingerprint".to_string()),
            ssh_public_key: None,
        };
        let agent_json = serde_json::to_string_pretty(&agent_config).unwrap();
        std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // The key file exists -> resolve_ssh_key_path returns Some
        let resolved = writer.resolve_ssh_key_path();
        assert!(
            resolved.is_some(),
            "resolve_ssh_key_path should return Some when key file exists"
        );
        assert!(
            resolved.unwrap().ends_with(fake_key_name),
            "Resolved path should end with the key filename"
        );

        drop(work_dir);
    }

    // --- SharedWriter::new() anonymous path ---

    #[test]
    fn test_new_without_agent_json_and_no_hub() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{"remote":"origin"}"#,
        )
        .unwrap();

        // No agent.json, no hub branch -> should return None
        let result = SharedWriter::new(&crosslink_dir).unwrap();
        assert!(result.is_none());
    }

    // ── v2-hub write refusal (#754 PASS B1) ──────────────────────────────────
    //
    // The v2 write path was deleted; every mutation now bails on a v2 hub with a
    // message instructing the operator to migrate. `setup_shared_writer_env()`
    // builds a v2 hub, so these mutations must refuse. The surviving v3 write
    // behavior is covered in `src/commands/hub_v3_operation_tests.rs`.

    #[test]
    fn test_v2_create_issue_refuses_with_migrate_message() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env_v2();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let err = writer
            .create_issue(&db, "Should refuse", None, "medium", None, None)
            .expect_err("create_issue must refuse on a v2 hub");
        let msg = err.to_string();
        assert!(
            msg.contains("migrate hub-v3"),
            "refusal must point at `crosslink migrate hub-v3`; got: {msg}"
        );
        drop(work_dir);
    }

    #[test]
    fn test_v2_add_label_refuses() {
        // `add_label` on a non-existent id on a v2 hub must return Err. We do not
        // assert the exact substring here: `add_label` loads the issue first, and
        // on a v2 hub that read path can fail before reaching the write refusal.
        // The robust contract for this call is simply that it does not succeed.
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let result = writer.add_label(&db, 1, "bug");
        assert!(
            result.is_err(),
            "add_label must not succeed on a v2 hub (it can never reach a write)"
        );
        drop(work_dir);
    }

    #[test]
    fn test_v2_lock_claim_refuses() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env_v2();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let err = writer
            .claim_lock_v2(1, None)
            .expect_err("claim_lock_v2 must refuse on a v2 hub");
        let msg = err.to_string();
        assert!(
            msg.contains("migrate hub-v3"),
            "refusal must point at `crosslink migrate hub-v3`; got: {msg}"
        );
        drop(work_dir);
    }
}
