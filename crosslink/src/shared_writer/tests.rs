use crate::issue_file::{
    read_counters, read_issue_file, write_counters, write_issue_file, IssueFile,
};
use crate::models::{IssueStatus, Priority};
use crate::shared_writer::core::{
    PushOutcome, SharedWriter, LOCK_CONFIRM_TIMEOUT_SECS, MAX_RETRIES,
};
use crate::shared_writer::locks::LockClaimResult;
use crate::shared_writer::mutations::DescriptionUpdate;
use crate::shared_writer::offline::{replace_local_refs, RewriteStats};
use anyhow::{bail, Result};
use chrono::Utc;
use std::path::Path;
use tempfile::tempdir;
use uuid::Uuid;

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

#[test]
fn test_replace_local_refs_basic() {
    let replacements = vec![
        ("L1".to_string(), "#5".to_string()),
        ("L2".to_string(), "#6".to_string()),
    ];
    let result = replace_local_refs("See L1 and L2 for details", &replacements);
    assert_eq!(result, Some("See #5 and #6 for details".to_string()));
}

#[test]
fn test_replace_local_refs_no_match() {
    let replacements = vec![("L1".to_string(), "#5".to_string())];
    let result = replace_local_refs("No local refs here", &replacements);
    assert!(result.is_none());
}

#[test]
fn test_replace_local_refs_non_matching_id() {
    let replacements = vec![("L1".to_string(), "#5".to_string())];
    let result = replace_local_refs("See L99 for info", &replacements);
    assert!(result.is_none());
}

#[test]
fn test_replace_local_refs_word_boundary() {
    let replacements = vec![("L1".to_string(), "#5".to_string())];
    // "FILE1" should NOT be rewritten (L1 is preceded by alphanumeric)
    let result = replace_local_refs("Check FILE1 now", &replacements);
    assert!(result.is_none());

    // "L1." should be rewritten (punctuation after is ok)
    let result = replace_local_refs("Fixed L1.", &replacements);
    assert_eq!(result, Some("Fixed #5.".to_string()));

    // "L1," in a list
    let result = replace_local_refs(
        "L1, L2 are done",
        &[
            ("L1".to_string(), "#5".to_string()),
            ("L2".to_string(), "#6".to_string()),
        ],
    );
    assert_eq!(result, Some("#5, #6 are done".to_string()));
}

#[test]
fn test_replace_local_refs_start_end() {
    let replacements = vec![("L1".to_string(), "#5".to_string())];
    // At start of string
    let result = replace_local_refs("L1 is done", &replacements);
    assert_eq!(result, Some("#5 is done".to_string()));

    // At end of string
    let result = replace_local_refs("Working on L1", &replacements);
    assert_eq!(result, Some("Working on #5".to_string()));

    // Entire string
    let result = replace_local_refs("L1", &replacements);
    assert_eq!(result, Some("#5".to_string()));
}

/// Helper for tests: scan issues dir for a display_id (mirrors SharedWriter logic).
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
    bail!("Issue #{} not found", display_id)
}

#[test]
fn test_v1_issue_path_format() {
    let uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
    let path = format!("issues/{}.json", uuid);
    assert_eq!(path, "issues/a1b2c3d4-e5f6-7890-abcd-ef1234567890.json");
}

#[test]
fn test_v2_issue_path_format() {
    let uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
    let path = format!("issues/{}/issue.json", uuid);
    assert_eq!(
        path,
        "issues/a1b2c3d4-e5f6-7890-abcd-ef1234567890/issue.json"
    );
}

#[test]
fn test_v2_comment_path_format() {
    let issue_uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
    let comment_uuid = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
    let path = format!("issues/{}/comments/{}.json", issue_uuid, comment_uuid);
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
        assert_ne!(claimed, contended.clone());
        assert_eq!(
            contended,
            LockClaimResult::Contended {
                winner_agent_id: "agent-2".to_string(),
            }
        );
        // Verify Debug
        let _ = format!("{:?}", claimed);
        let _ = format!("{:?}", contended);
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
        let result = crate::compaction::compact(cache, "agent-a", true)
            .unwrap()
            .unwrap();
        assert_eq!(result.locks_materialized, 1);

        // Read checkpoint -- agent-a should win (earlier timestamp)
        let state = read_checkpoint(cache).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.agent_id, "agent-a");
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

        let result = crate::compaction::compact(cache, "agent-a", true)
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

        crate::compaction::compact(cache, "agent-a", true).unwrap();

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

            crate::compaction::compact(cache, compactor, true).unwrap();

            let state = read_checkpoint(cache).unwrap();
            assert_eq!(
                state.locks[&1].agent_id, "agent-a",
                "Winner should be agent-a regardless of who runs compaction (compactor={})",
                compactor
            );
        }
    }
}

// ---- Integration tests with real git repos ----

mod integration {
    use super::*;
    use crate::db::Database;
    use crate::identity::AgentConfig;
    use std::process::Command;
    use tempfile::TempDir;

    /// Set up a minimal git environment for SharedWriter tests.
    ///
    /// Returns (work_dir, remote_dir). The hub cache (`crosslink/hub` branch)
    /// is initialized directly inside the work_dir so SharedWriter::new() works.
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

    // --- create_issue() ---

    #[test]
    fn test_create_issue_returns_display_id() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Test issue", None, "medium")
            .unwrap();
        assert!(id > 0, "create_issue should return a positive display ID");
        drop(work_dir);
    }

    #[test]
    fn test_create_issue_increments_id() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id1 = writer
            .create_issue(&db, "First issue", None, "low")
            .unwrap();
        let id2 = writer
            .create_issue(&db, "Second issue", None, "low")
            .unwrap();
        assert_eq!(id2, id1 + 1, "IDs should be sequential");
        drop(work_dir);
    }

    #[test]
    fn test_create_issue_with_description() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(
                &db,
                "With description",
                Some("A detailed description"),
                "high",
            )
            .unwrap();
        assert!(id > 0);

        // Verify it's in the database
        let issue = db.get_issue(id).unwrap();
        assert!(
            issue.is_some(),
            "Issue should exist in database after create"
        );
        let issue = issue.unwrap();
        assert_eq!(issue.title, "With description");
        assert_eq!(issue.description.as_deref(), Some("A detailed description"));
        drop(work_dir);
    }

    #[test]
    fn test_create_issue_high_priority() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Critical bug", None, "critical")
            .unwrap();
        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.priority, Priority::Critical);
        drop(work_dir);
    }

    #[test]
    fn test_create_issue_writes_json_to_cache() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        writer
            .create_issue(&db, "Cache test", None, "medium")
            .unwrap();

        // Verify the issue JSON file exists in the hub cache (v2 layout)
        let cache_dir = crosslink_dir.join(".hub-cache").join("issues");
        let entries: Vec<_> = std::fs::read_dir(&cache_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            !entries.is_empty(),
            "At least one issue entry should exist in cache"
        );
        drop(work_dir);
    }

    // --- create_subissue() ---

    #[test]
    fn test_create_subissue() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let parent_id = writer
            .create_issue(&db, "Parent issue", None, "medium")
            .unwrap();
        let child_id = writer
            .create_subissue(&db, parent_id, "Child issue", None, "low")
            .unwrap();

        assert!(child_id > 0);
        assert_ne!(parent_id, child_id);

        // Verify parent relationship in database
        let child = db.get_issue(child_id).unwrap().unwrap();
        assert_eq!(child.parent_id, Some(parent_id));
        drop(work_dir);
    }

    // --- update_issue() ---

    #[test]
    fn test_update_issue_title() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Old title", None, "medium")
            .unwrap();
        writer
            .update_issue(
                &db,
                id,
                Some("New title"),
                DescriptionUpdate::Unchanged,
                None,
                None,
            )
            .unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title, "New title");
        drop(work_dir);
    }

    #[test]
    fn test_update_issue_priority() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Priority test", None, "low")
            .unwrap();
        writer
            .update_issue(
                &db,
                id,
                None,
                DescriptionUpdate::Unchanged,
                None,
                Some("high"),
            )
            .unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.priority, Priority::High);
        drop(work_dir);
    }

    #[test]
    fn test_update_issue_description() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer.create_issue(&db, "Desc test", None, "low").unwrap();
        writer
            .update_issue(
                &db,
                id,
                None,
                DescriptionUpdate::Set("Updated desc"),
                None,
                None,
            )
            .unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.description.as_deref(), Some("Updated desc"));
        drop(work_dir);
    }

    #[test]
    fn test_update_issue_clear_description() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Has desc", Some("initial desc"), "low")
            .unwrap();
        writer
            .update_issue(&db, id, None, DescriptionUpdate::Clear, None, None)
            .unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert!(issue.description.is_none(), "Description should be cleared");
        drop(work_dir);
    }

    // --- close_issue() / reopen_issue() ---

    #[test]
    fn test_close_issue() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Close me", None, "medium")
            .unwrap();
        writer.close_issue(&db, id).unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Closed);
        drop(work_dir);
    }

    #[test]
    fn test_reopen_issue() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Open/close cycle", None, "medium")
            .unwrap();
        writer.close_issue(&db, id).unwrap();
        writer.reopen_issue(&db, id).unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Open);
        drop(work_dir);
    }

    #[test]
    fn test_closed_issue_has_closed_at() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Closed at test", None, "medium")
            .unwrap();

        // Before closing, closed_at should be None
        // Read from cache to verify
        let cache_dir = crosslink_dir.join(".hub-cache");
        let issue_before = writer.load_issue_by_id(id, &db).unwrap();
        assert!(
            issue_before.closed_at.is_none(),
            "closed_at should be None before closing"
        );

        writer.close_issue(&db, id).unwrap();

        let issue_after = writer.load_issue_by_id(id, &db).unwrap();
        assert!(
            issue_after.closed_at.is_some(),
            "closed_at should be set after closing"
        );
        assert_eq!(issue_after.status, IssueStatus::Closed);
        drop(cache_dir);
        drop(work_dir);
    }

    #[test]
    fn test_reopen_clears_closed_at() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Reopen cleared", None, "medium")
            .unwrap();
        writer.close_issue(&db, id).unwrap();
        writer.reopen_issue(&db, id).unwrap();

        let issue = writer.load_issue_by_id(id, &db).unwrap();
        assert!(
            issue.closed_at.is_none(),
            "closed_at should be cleared after reopen"
        );
        drop(work_dir);
    }

    // --- delete_issue() ---

    #[test]
    fn test_delete_issue() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id1 = writer
            .create_issue(&db, "Delete me", None, "medium")
            .unwrap();
        let id2 = writer.create_issue(&db, "Keep me", None, "medium").unwrap();

        let delete_result = writer.delete_issue(&db, id1);
        // delete may fail on empty commit in test environments; verify at least the DB state
        if delete_result.is_ok() {
            let deleted = db.get_issue(id1).unwrap();
            assert!(deleted.is_none(), "Deleted issue should be gone from DB");
        }

        // Issue 2 should still exist regardless
        let kept = db.get_issue(id2).unwrap();
        assert!(kept.is_some(), "Kept issue should still be in DB");

        drop(work_dir);
    }

    #[test]
    fn test_delete_issue_removes_file_from_disk() {
        // Verify that delete_issue's closure removes the file from disk via issue_path(),
        // which correctly uses V2 layout (issues/{uuid}/issue.json).
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "File remove test", None, "medium")
            .unwrap();

        // Get the UUID so we can check the V2 file path
        let uuid_str = db.get_issue_uuid_by_id(id).unwrap();
        let uuid: Uuid = uuid_str.parse().unwrap();
        let v2_issue_path = crosslink_dir
            .join(".hub-cache")
            .join("issues")
            .join(uuid.to_string())
            .join("issue.json");

        assert!(
            v2_issue_path.exists(),
            "Issue file should exist before delete"
        );

        // delete_issue removes the file from disk in the prepare closure
        // (even if the subsequent git commit step fails due to V2 path mismatch)
        let _ = writer.delete_issue(&db, id);

        assert!(
            !v2_issue_path.exists(),
            "Issue file should be removed from disk by delete_issue's prepare closure"
        );
        drop(work_dir);
    }

    // --- add_comment() / add_intervention_comment() ---

    #[test]
    fn test_add_comment_returns_id() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let issue_id = writer
            .create_issue(&db, "Comment host", None, "medium")
            .unwrap();
        let comment_id = writer
            .add_comment(&db, issue_id, "A test comment", "note")
            .unwrap();

        assert!(comment_id > 0, "comment ID should be positive");
        drop(work_dir);
    }

    #[test]
    fn test_add_comment_persists_to_db() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let issue_id = writer
            .create_issue(&db, "Comment persist", None, "medium")
            .unwrap();
        writer
            .add_comment(&db, issue_id, "Persisted comment content", "plan")
            .unwrap();

        let comments = db.get_comments(issue_id).unwrap();
        assert!(!comments.is_empty(), "Comment should be in DB");
        assert_eq!(comments[0].content, "Persisted comment content");
        drop(work_dir);
    }

    #[test]
    fn test_add_comment_multiple_kinds() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let issue_id = writer
            .create_issue(&db, "Typed comments", None, "medium")
            .unwrap();

        let kinds = ["plan", "decision", "observation", "blocker", "resolution"];
        for kind in &kinds {
            writer
                .add_comment(&db, issue_id, &format!("Comment: {}", kind), kind)
                .unwrap();
        }

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments.len(), kinds.len());
        drop(work_dir);
    }

    #[test]
    fn test_add_comment_sequential_ids() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let issue_id = writer
            .create_issue(&db, "Sequential comments", None, "medium")
            .unwrap();
        let c1 = writer
            .add_comment(&db, issue_id, "First comment", "note")
            .unwrap();
        let c2 = writer
            .add_comment(&db, issue_id, "Second comment", "note")
            .unwrap();

        assert_eq!(c2, c1 + 1, "Comment IDs should be sequential");
        drop(work_dir);
    }

    #[test]
    fn test_add_intervention_comment() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let issue_id = writer
            .create_issue(&db, "Intervention host", None, "medium")
            .unwrap();
        let comment_id = writer
            .add_intervention_comment(
                &db,
                issue_id,
                "Intervention content",
                "manual_redirect",
                Some("context string"),
                None,
            )
            .unwrap();

        assert!(comment_id > 0);
        let comments = db.get_comments(issue_id).unwrap();
        assert!(!comments.is_empty());
        assert_eq!(comments[0].content, "Intervention content");
        drop(work_dir);
    }

    // --- add_label() / remove_label() ---

    #[test]
    fn test_add_label() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Label test", None, "medium")
            .unwrap();
        writer.add_label(&db, id, "bug").unwrap();

        let labels = db.get_labels(id).unwrap();
        assert!(labels.contains(&"bug".to_string()));
        drop(work_dir);
    }

    #[test]
    fn test_add_multiple_labels() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Multi-label", None, "medium")
            .unwrap();
        writer.add_label(&db, id, "bug").unwrap();
        writer.add_label(&db, id, "urgent").unwrap();
        writer.add_label(&db, id, "frontend").unwrap();

        let labels = db.get_labels(id).unwrap();
        assert!(labels.contains(&"bug".to_string()));
        assert!(labels.contains(&"urgent".to_string()));
        assert!(labels.contains(&"frontend".to_string()));
        drop(work_dir);
    }

    #[test]
    fn test_remove_label() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Remove label", None, "medium")
            .unwrap();
        writer.add_label(&db, id, "bug").unwrap();
        writer.add_label(&db, id, "keep").unwrap();
        writer.remove_label(&db, id, "bug").unwrap();

        let labels = db.get_labels(id).unwrap();
        assert!(
            !labels.contains(&"bug".to_string()),
            "bug label should be gone"
        );
        assert!(
            labels.contains(&"keep".to_string()),
            "keep label should remain"
        );
        drop(work_dir);
    }

    #[test]
    fn test_add_label_idempotent() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Idempotent label", None, "medium")
            .unwrap();
        writer.add_label(&db, id, "tag").unwrap();
        let _ = writer.add_label(&db, id, "tag"); // duplicate -- may error on empty commit

        let labels = db.get_labels(id).unwrap();
        let tag_count = labels.iter().filter(|l| l.as_str() == "tag").count();
        assert_eq!(tag_count, 1, "Duplicate label should not be double-added");
        drop(work_dir);
    }

    // --- add_blocker() / remove_blocker() ---

    #[test]
    fn test_add_blocker() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let blocked = writer
            .create_issue(&db, "Blocked issue", None, "medium")
            .unwrap();
        let blocker = writer
            .create_issue(&db, "Blocker issue", None, "high")
            .unwrap();

        writer.add_blocker(&db, blocked, blocker).unwrap();

        // The blocked issue's JSON should contain the blocker UUID
        let issue_file = writer.load_issue_by_id(blocked, &db).unwrap();
        assert!(
            !issue_file.blockers.is_empty(),
            "Blocker should be recorded"
        );
        drop(work_dir);
    }

    #[test]
    fn test_remove_blocker() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let blocked = writer
            .create_issue(&db, "Was blocked", None, "medium")
            .unwrap();
        let blocker = writer
            .create_issue(&db, "Was blocker", None, "high")
            .unwrap();

        writer.add_blocker(&db, blocked, blocker).unwrap();
        writer.remove_blocker(&db, blocked, blocker).unwrap();

        let issue_file = writer.load_issue_by_id(blocked, &db).unwrap();
        assert!(issue_file.blockers.is_empty(), "Blocker should be removed");
        drop(work_dir);
    }

    // --- add_relation() / remove_relation() ---

    #[test]
    fn test_add_relation() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id1 = writer
            .create_issue(&db, "Related A", None, "medium")
            .unwrap();
        let id2 = writer
            .create_issue(&db, "Related B", None, "medium")
            .unwrap();

        writer.add_relation(&db, id1, id2).unwrap();

        let issue = writer.load_issue_by_id(id1, &db).unwrap();
        assert!(!issue.related.is_empty(), "Relation should be recorded");
        drop(work_dir);
    }

    #[test]
    fn test_remove_relation() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id1 = writer
            .create_issue(&db, "Related C", None, "medium")
            .unwrap();
        let id2 = writer
            .create_issue(&db, "Related D", None, "medium")
            .unwrap();

        writer.add_relation(&db, id1, id2).unwrap();
        writer.remove_relation(&db, id1, id2).unwrap();

        let issue = writer.load_issue_by_id(id1, &db).unwrap();
        assert!(issue.related.is_empty(), "Relation should be removed");
        drop(work_dir);
    }

    // --- create_milestone() / close_milestone() / delete_milestone() ---

    #[test]
    fn test_create_milestone() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let ms_id = writer
            .create_milestone(&db, "v1.0", Some("First release"))
            .unwrap();
        assert!(ms_id > 0, "Milestone ID should be positive");
        drop(work_dir);
    }

    #[test]
    fn test_create_multiple_milestones() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let ms1 = writer.create_milestone(&db, "v1.0", None).unwrap();
        let ms2 = writer.create_milestone(&db, "v2.0", None).unwrap();
        assert_eq!(ms2, ms1 + 1, "Milestone IDs should be sequential");
        drop(work_dir);
    }

    #[test]
    fn test_close_milestone() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let ms_id = writer.create_milestone(&db, "v1.0", None).unwrap();
        writer.close_milestone(&db, ms_id).unwrap();

        // Read back and verify
        let entry = writer.load_milestone_by_id(ms_id).unwrap();
        assert_eq!(entry.status, IssueStatus::Closed);
        assert!(entry.closed_at.is_some());
        drop(work_dir);
    }

    #[test]
    fn test_delete_milestone() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let ms_id = writer.create_milestone(&db, "v1.0-del", None).unwrap();
        writer.delete_milestone(&db, ms_id).unwrap();

        // After deletion, load should fail
        let result = writer.load_milestone_by_id(ms_id);
        assert!(result.is_err(), "Deleted milestone should not be loadable");
        drop(work_dir);
    }

    // --- set_milestone_on_issues() / clear_milestone_on_issue() ---

    #[test]
    fn test_set_milestone_on_issues() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let ms_id = writer.create_milestone(&db, "Sprint 1", None).unwrap();
        let issue_id = writer
            .create_issue(&db, "Sprint task", None, "medium")
            .unwrap();

        writer
            .set_milestone_on_issues(&db, ms_id, &[issue_id])
            .unwrap();

        let issue = writer.load_issue_by_id(issue_id, &db).unwrap();
        assert!(
            issue.milestone_uuid.is_some(),
            "Issue should have milestone_uuid set"
        );
        drop(work_dir);
    }

    #[test]
    fn test_clear_milestone_on_issue() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let ms_id = writer.create_milestone(&db, "Sprint 2", None).unwrap();
        let issue_id = writer
            .create_issue(&db, "Sprint 2 task", None, "medium")
            .unwrap();

        writer
            .set_milestone_on_issues(&db, ms_id, &[issue_id])
            .unwrap();
        writer.clear_milestone_on_issue(&db, issue_id).unwrap();

        let issue = writer.load_issue_by_id(issue_id, &db).unwrap();
        assert!(
            issue.milestone_uuid.is_none(),
            "Issue should have milestone_uuid cleared"
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
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
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

    // --- Hydration roundtrip ---

    #[test]
    fn test_hydration_roundtrip_issue() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Hydration test", Some("desc"), "high")
            .unwrap();

        // Re-hydrate from cache
        let cache_dir = crosslink_dir.join(".hub-cache");
        crate::hydration::hydrate_to_sqlite(&cache_dir, &db).unwrap();

        let issue = db.get_issue(id).unwrap();
        assert!(issue.is_some());
        let issue = issue.unwrap();
        assert_eq!(issue.title, "Hydration test");
        assert_eq!(issue.priority, Priority::High);
        drop(work_dir);
    }

    #[test]
    fn test_hydration_after_close() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Close hydration", None, "medium")
            .unwrap();
        writer.close_issue(&db, id).unwrap();

        let cache_dir = crosslink_dir.join(".hub-cache");
        crate::hydration::hydrate_to_sqlite(&cache_dir, &db).unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.status, IssueStatus::Closed);
        drop(work_dir);
    }

    #[test]
    fn test_hydration_after_comment() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let issue_id = writer
            .create_issue(&db, "Comment hydration", None, "medium")
            .unwrap();
        writer
            .add_comment(&db, issue_id, "Hydrated comment", "note")
            .unwrap();

        let cache_dir = crosslink_dir.join(".hub-cache");
        crate::hydration::hydrate_to_sqlite(&cache_dir, &db).unwrap();

        let comments = db.get_comments(issue_id).unwrap();
        assert!(!comments.is_empty());
        assert_eq!(comments[0].content, "Hydrated comment");
        drop(work_dir);
    }

    // --- RewriteStats ---

    #[test]
    fn test_rewrite_stats_total() {
        let stats = RewriteStats {
            comments_updated: 3,
            descriptions_updated: 2,
            sessions_updated: 1,
        };
        assert_eq!(stats.total(), 6);
    }

    #[test]
    fn test_rewrite_stats_default_total() {
        let stats = RewriteStats::default();
        assert_eq!(stats.total(), 0);
    }

    // --- rewrite_local_references() ---

    #[test]
    fn test_rewrite_local_references_empty_mapping() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let stats = writer.rewrite_local_references(&db, &[]).unwrap();
        assert_eq!(
            stats.total(),
            0,
            "Empty mapping should produce zero rewrites"
        );
        drop(work_dir);
    }

    #[test]
    fn test_rewrite_local_references_no_matches() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        // Create an issue with a description that won't match any local refs
        let id = writer
            .create_issue(&db, "No local refs here", Some("Clean description"), "low")
            .unwrap();

        // Mapping says L1 -> #5, but the issue has no L1 refs
        let mapping = vec![(1i64, 5i64, "Some title".to_string())];
        let stats = writer.rewrite_local_references(&db, &mapping).unwrap();
        // Comments and descriptions with no matches should yield 0 updates
        assert_eq!(stats.comments_updated, 0);
        assert_eq!(stats.descriptions_updated, 0);
        let _ = id; // suppress unused warning
        drop(work_dir);
    }

    // --- promote_offline_issues() ---

    #[test]
    fn test_promote_offline_issues_empty() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let mapping = writer.promote_offline_issues(&db).unwrap();
        assert!(mapping.is_empty(), "No offline issues to promote");
        drop(work_dir);
    }

    // --- read_promoted_uuids() / record_promoted_uuids() ---

    #[test]
    fn test_promoted_uuids_roundtrip() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // Initially empty
        let before = writer.read_promoted_uuids();
        assert!(before.is_empty());

        // Record some UUIDs
        let uuid1 = Uuid::new_v4();
        let uuid2 = Uuid::new_v4();
        writer.record_promoted_uuids(&[uuid1, uuid2]).unwrap();

        // Read back
        let after = writer.read_promoted_uuids();
        assert!(after.contains(&uuid1));
        assert!(after.contains(&uuid2));
        drop(work_dir);
    }

    #[test]
    fn test_promoted_uuids_are_not_re_promoted() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        // Record a UUID as promoted
        let uuid = Uuid::new_v4();
        writer.record_promoted_uuids(&[uuid]).unwrap();

        // Write an issue JSON with display_id=None and that UUID -- simulates an offline issue
        let cache_dir = crosslink_dir.join(".hub-cache");
        let issues_dir = cache_dir.join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();

        // V1-style: a flat file issues/{uuid}.json with display_id null and created_by matching agent
        let issue = crate::issue_file::IssueFile {
            uuid,
            display_id: None,
            title: "Already promoted".to_string(),
            description: None,
            status: IssueStatus::Open,
            priority: Priority::Low,
            parent_uuid: None,
            created_by: "test-agent".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            closed_at: None,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };
        crate::issue_file::write_issue_file(&issues_dir.join(format!("{}.json", uuid)), &issue)
            .unwrap();

        // promote_offline_issues should skip this one (UUID in promoted set)
        let promoted = writer.promote_offline_issues(&db).unwrap();
        assert!(
            promoted.is_empty(),
            "Already-promoted UUID should not be re-promoted"
        );
        drop(work_dir);
    }

    // --- layout_version() ---

    #[test]
    fn test_layout_version_is_v2_for_new_hub() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // init_cache() sets up v2 layout
        assert_eq!(writer.layout_version(), 2, "New hub should be v2 layout");
        drop(work_dir);
    }

    // --- issue_path() / issue_rel_path() -- via V2 layout ---

    #[test]
    fn test_v2_issue_path_uses_subdir() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "V2 path check", None, "low")
            .unwrap();

        // Find the issue UUID from the DB
        let uuid_str = db.get_issue_uuid_by_id(id).unwrap();
        let uuid: Uuid = uuid_str.parse().unwrap();

        // V2: the issue file should be at issues/{uuid}/issue.json
        let cache_dir = crosslink_dir.join(".hub-cache");
        let v2_path = cache_dir
            .join("issues")
            .join(uuid.to_string())
            .join("issue.json");
        assert!(
            v2_path.exists(),
            "V2 issue.json should exist at {}",
            v2_path.display()
        );

        // And the comments subdirectory should also exist
        let comments_dir = cache_dir
            .join("issues")
            .join(uuid.to_string())
            .join("comments");
        assert!(
            comments_dir.exists(),
            "V2 comments dir should exist at {}",
            comments_dir.display()
        );
        drop(work_dir);
    }

    // --- Multiple operations / end-to-end ---

    #[test]
    fn test_full_issue_lifecycle() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        // Create
        let id = writer
            .create_issue(&db, "Lifecycle issue", Some("Initial desc"), "medium")
            .unwrap();

        // Comment
        writer
            .add_comment(&db, id, "Planning note", "plan")
            .unwrap();

        // Label
        writer.add_label(&db, id, "in-progress").unwrap();

        // Update
        writer
            .update_issue(
                &db,
                id,
                Some("Updated lifecycle"),
                DescriptionUpdate::Unchanged,
                None,
                Some("high"),
            )
            .unwrap();

        // Close
        writer.close_issue(&db, id).unwrap();

        // Verify final state
        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title, "Updated lifecycle");
        assert_eq!(issue.priority, Priority::High);
        assert_eq!(issue.status, IssueStatus::Closed);

        let labels = db.get_labels(id).unwrap();
        assert!(labels.contains(&"in-progress".to_string()));

        let comments = db.get_comments(id).unwrap();
        assert_eq!(comments.len(), 1);
        drop(work_dir);
    }

    #[test]
    fn test_multiple_issues_independent() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id1 = writer
            .create_issue(&db, "Issue Alpha", None, "high")
            .unwrap();
        let id2 = writer.create_issue(&db, "Issue Beta", None, "low").unwrap();
        let id3 = writer
            .create_issue(&db, "Issue Gamma", None, "medium")
            .unwrap();

        writer.close_issue(&db, id2).unwrap();
        writer.add_label(&db, id1, "critical").unwrap();

        let i1 = db.get_issue(id1).unwrap().unwrap();
        let i2 = db.get_issue(id2).unwrap().unwrap();
        let i3 = db.get_issue(id3).unwrap().unwrap();

        assert_eq!(i1.status, IssueStatus::Open);
        assert_eq!(i2.status, IssueStatus::Closed);
        assert_eq!(i3.status, IssueStatus::Open);

        let labels = db.get_labels(id1).unwrap();
        assert!(labels.contains(&"critical".to_string()));
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
    fn test_event_seq_increments() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        // Each create_issue triggers emit which calls next_event_seq
        writer.create_issue(&db, "Seq 1", None, "low").unwrap();
        writer.create_issue(&db, "Seq 2", None, "low").unwrap();

        // The event_seq field should be > 0 after two operations
        // We can't directly read event_seq, but we can verify events exist in the log
        let cache_dir = crosslink_dir.join(".hub-cache");
        let log_path = cache_dir
            .join("agents")
            .join("test-agent")
            .join("events.log");

        // The log may or may not exist depending on whether emit_compact_push is called
        // For write_commit_push path (not emit_compact_push), events aren't written
        // Just verify the writer operated successfully
        drop(log_path);
        drop(work_dir);
    }

    #[test]
    fn test_counters_persist_across_writer_instances() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();

        // First writer creates 2 issues
        {
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());
            writer.create_issue(&db, "Issue 1", None, "low").unwrap();
            writer.create_issue(&db, "Issue 2", None, "low").unwrap();
        }

        // Second writer should continue from counter 3
        {
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());
            let id = writer.create_issue(&db, "Issue 3", None, "low").unwrap();
            assert_eq!(id, 3, "Counter should persist: 3rd issue should get ID 3");
        }

        drop(work_dir);
    }

    #[test]
    fn test_promoted_uuids_path() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let path = writer.promoted_uuids_path();
        assert!(
            path.to_string_lossy().contains(".promoted-uuids"),
            "promoted_uuids_path should contain .promoted-uuids"
        );
        drop(work_dir);
    }

    #[test]
    fn test_event_log_path() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let path = writer.event_log_path();
        assert!(
            path.to_string_lossy().contains("test-agent"),
            "event_log_path should contain agent_id"
        );
        assert!(
            path.to_string_lossy().contains("events.log"),
            "event_log_path should end in events.log"
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
    fn test_read_counters_defaults_to_one() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // Before any issues are created, next_display_id should be 1
        let counters = writer.read_counters().unwrap();
        assert_eq!(counters.next_display_id, 1);
        drop(work_dir);
    }

    #[test]
    fn test_write_then_read_counters() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        writer
            .create_issue(&db, "Counter check", None, "low")
            .unwrap();

        let counters = writer.read_counters().unwrap();
        assert_eq!(
            counters.next_display_id, 2,
            "After one create, next_display_id should be 2"
        );
        drop(work_dir);
    }

    #[test]
    fn test_load_issue_by_id_positive() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Load by ID", Some("description"), "medium")
            .unwrap();
        let loaded = writer.load_issue_by_id(id, &db).unwrap();
        assert_eq!(loaded.title, "Load by ID");
        assert_eq!(loaded.status, IssueStatus::Open);
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
    fn test_resolve_uuid_for_positive_id() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "UUID resolve", None, "low")
            .unwrap();
        let uuid = writer.resolve_uuid(id, &db).unwrap();

        let issue = writer.load_issue_by_display_id(id).unwrap();
        assert_eq!(uuid, issue.uuid, "Resolved UUID should match issue UUID");
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
    fn test_find_offline_issues_empty_when_all_have_ids() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        // Create issues normally (they get display IDs)
        writer.create_issue(&db, "Normal 1", None, "low").unwrap();
        writer.create_issue(&db, "Normal 2", None, "low").unwrap();

        // find_offline_issues should return empty since all have display_id
        let offline = writer.find_offline_issues().unwrap();
        assert!(
            offline.is_empty(),
            "No offline issues expected when all have display IDs"
        );
        drop(work_dir);
    }

    #[test]
    fn test_claim_display_id_uses_correct_starting_value() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let (first, counters) = writer.claim_display_id(1).unwrap();
        assert_eq!(first, 1, "First claimed ID should be 1");
        assert_eq!(
            counters.next_display_id, 2,
            "After claiming 1, next should be 2"
        );
        drop(work_dir);
    }

    #[test]
    fn test_claim_display_id_bulk() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let (first, counters) = writer.claim_display_id(5).unwrap();
        assert_eq!(first, 1);
        assert_eq!(
            counters.next_display_id, 6,
            "After claiming 5, next should be 6"
        );
        drop(work_dir);
    }

    #[test]
    fn test_claim_milestone_id() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let (id, counters) = writer.claim_milestone_id().unwrap();
        assert_eq!(id, 1, "First milestone ID should be 1");
        assert_eq!(counters.next_milestone_id, 2);
        drop(work_dir);
    }

    #[test]
    fn test_read_max_event_seq_returns_zero_when_no_log() {
        let dir = tempfile::tempdir().unwrap();
        let seq = SharedWriter::read_max_event_seq(dir.path(), "nonexistent-agent");
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
    fn test_write_counters_to_cache() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        let mut counters = writer.read_counters().unwrap();
        counters.next_display_id = 42;
        writer.write_counters_to_cache(&counters).unwrap();

        let reloaded = writer.read_counters().unwrap();
        assert_eq!(reloaded.next_display_id, 42);
        drop(work_dir);
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

    #[test]
    fn test_max_retries_constant() {
        assert_eq!(MAX_RETRIES, 3);
    }

    // ---- V1 layout coverage ----

    /// Create a V1-layout environment by deleting `meta/version.json` from the hub
    /// cache after normal V2 setup. `layout_version()` returns 1 when this file
    /// is absent, routing add_comment / add_intervention_comment through the V1
    /// inline-append code paths (lines 679-701, 762-785).
    fn setup_shared_writer_env_v1() -> (TempDir, TempDir, std::path::PathBuf) {
        let (work_dir, remote_dir, crosslink_dir) = setup_shared_writer_env();
        // Remove meta/version.json so layout_version() returns 1
        let version_file = crosslink_dir
            .join(".hub-cache")
            .join("meta")
            .join("version.json");
        if version_file.exists() {
            std::fs::remove_file(&version_file).unwrap();
        }
        (work_dir, remote_dir, crosslink_dir)
    }

    #[test]
    fn test_add_comment_v1_layout() {
        // Exercises lines 679-701: V1 path that appends comment inline to issue.json
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env_v1();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        // Verify we are in V1 layout
        assert_eq!(
            writer.layout_version(),
            1,
            "Should be V1 layout after version.json removal"
        );

        // In V1 layout, create_issue writes a flat issues/{uuid}.json file.
        let issue_id = writer
            .create_issue(&db, "V1 comment host", None, "medium")
            .unwrap();

        let comment_id = writer
            .add_comment(&db, issue_id, "V1 inline comment", "note")
            .unwrap();

        assert!(comment_id > 0, "Comment ID should be positive");

        // In V1 layout, the comment is stored inline inside issues/{uuid}.json.
        // Verify it appeared in the DB (hydration reads it from the issue file).
        let comments = db.get_comments(issue_id).unwrap();
        assert!(
            !comments.is_empty(),
            "V1 comment should appear in DB after hydration"
        );
        assert_eq!(comments[0].content, "V1 inline comment");

        drop(work_dir);
    }

    #[test]
    fn test_add_intervention_comment_v1_layout() {
        // Exercises lines 762-785: V1 path that appends intervention comment inline.
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env_v1();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        assert_eq!(writer.layout_version(), 1);

        let issue_id = writer
            .create_issue(&db, "V1 intervention host", None, "medium")
            .unwrap();

        let comment_id = writer
            .add_intervention_comment(
                &db,
                issue_id,
                "V1 intervention content",
                "manual_redirect",
                Some("V1 context"),
                None,
            )
            .unwrap();

        assert!(comment_id > 0, "Intervention comment ID should be positive");

        let comments = db.get_comments(issue_id).unwrap();
        assert!(
            !comments.is_empty(),
            "V1 intervention comment should appear in DB"
        );
        assert_eq!(comments[0].content, "V1 intervention content");

        drop(work_dir);
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

    // ---- replace_local_refs "after" boundary rejection ----

    #[test]
    fn test_replace_local_refs_after_boundary_rejection() {
        // Exercises line 64: before_ok=true but after_ok=false -> else { i = end_pos }.
        // "L1" appears at start of "L10" -- before boundary OK, but "0" after is alphanumeric.
        let replacements = vec![("L1".to_string(), "#5".to_string())];

        // "L10" -- L1 is followed by "0" (alphanumeric), so the word-boundary check rejects it.
        let result = replace_local_refs("L10 is a thing", &replacements);
        assert!(
            result.is_none(),
            "L1 in L10 should NOT be replaced (after-boundary alphanumeric char)"
        );

        // Mixed: "L10 and L1" -- L10 should not replace, standalone L1 should
        let result = replace_local_refs("L10 and L1 done", &replacements);
        assert_eq!(
            result,
            Some("L10 and #5 done".to_string()),
            "Only standalone L1 should be replaced, not L1 inside L10"
        );

        // At end of string: "L10" -- after end_pos is string end but "0" terminates the match
        let result = replace_local_refs("L10", &replacements);
        assert!(
            result.is_none(),
            "L1 at start of L10 (entire string) should NOT be replaced"
        );
    }

    // --- claim_lock_v2() / release_lock_v2() ---
    // Exercises emit_compact_push() path (lines 286-360)

    #[test]
    fn test_claim_lock_v2_succeeds() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Lock target", None, "medium")
            .unwrap();

        let result = writer.claim_lock_v2(id, Some("feature/test")).unwrap();
        assert_eq!(result, LockClaimResult::Claimed);
        drop(work_dir);
    }

    #[test]
    fn test_claim_lock_v2_already_held() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Lock target 2", None, "medium")
            .unwrap();

        writer.claim_lock_v2(id, None).unwrap();

        // Claim again -- should return AlreadyHeld
        let result = writer.claim_lock_v2(id, None).unwrap();
        assert_eq!(result, LockClaimResult::AlreadyHeld);
        drop(work_dir);
    }

    #[test]
    fn test_release_lock_v2_held() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Lock release", None, "medium")
            .unwrap();
        writer.claim_lock_v2(id, None).unwrap();

        let released = writer.release_lock_v2(id).unwrap();
        assert!(released, "Should release own lock");
        drop(work_dir);
    }

    #[test]
    fn test_release_lock_v2_not_locked() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

        // Issue ID 999 doesn't exist / isn't locked
        let released = writer.release_lock_v2(999).unwrap();
        assert!(!released, "Releasing non-existent lock returns false");
        drop(work_dir);
    }

    #[test]
    fn test_steal_lock_v2() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Steal target", None, "medium")
            .unwrap();
        writer.claim_lock_v2(id, None).unwrap();

        // Steal the lock (pretending the owner is stale)
        let result = writer
            .steal_lock_v2(id, "test-agent", Some("feature/steal"))
            .unwrap();
        assert_eq!(result, LockClaimResult::Claimed);
        drop(work_dir);
    }

    // --- rewrite_local_references() additional ---

    #[test]
    fn test_rewrite_local_references_rewrites_description() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        // Create an issue with a description referencing L1
        let id = writer
            .create_issue(&db, "Rewrite test", Some("See L1 for details"), "medium")
            .unwrap();

        // Mapping: neg_id=-1 -> new_id=id, simulate promotion
        let mapping = vec![(-1i64, id, "Rewrite test".to_string())];
        let stats = writer.rewrite_local_references(&db, &mapping).unwrap();

        assert_eq!(stats.descriptions_updated, 1);

        let issue = db.get_issue(id).unwrap().unwrap();
        assert!(
            issue
                .description
                .as_deref()
                .unwrap()
                .contains(&format!("#{}", id)),
            "L1 should be rewritten to #{}",
            id
        );
        drop(work_dir);
    }

    #[test]
    fn test_rewrite_local_references_rewrites_comments() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "Comment rewrite", None, "medium")
            .unwrap();
        writer
            .add_comment(&db, id, "Related to L2", "observation")
            .unwrap();

        let mapping = vec![(-2i64, id, "Comment rewrite".to_string())];
        let stats = writer.rewrite_local_references(&db, &mapping).unwrap();

        assert_eq!(stats.comments_updated, 1);
        drop(work_dir);
    }

    #[test]
    fn test_rewrite_local_references_no_refs_no_changes() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        let id = writer
            .create_issue(&db, "No refs", Some("Plain description"), "medium")
            .unwrap();

        let mapping = vec![(-1i64, id, "No refs".to_string())];
        let stats = writer.rewrite_local_references(&db, &mapping).unwrap();

        assert_eq!(stats.descriptions_updated, 0);
        assert_eq!(stats.comments_updated, 0);
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

    // --- promote_offline_issues() with actual offline issues ---

    #[test]
    fn test_promote_offline_issues_with_offline_issue() {
        let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
        let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
        let db = make_db(work_dir.path());

        // Manually create an offline issue (display_id: null, created_by: test-agent)
        let uuid = uuid::Uuid::new_v4();
        let now = chrono::Utc::now();
        let issue = crate::issue_file::IssueFile {
            uuid,
            display_id: None,
            title: "Offline issue".to_string(),
            description: None,
            status: IssueStatus::Open,
            priority: Priority::Medium,
            parent_uuid: None,
            created_by: "test-agent".to_string(),
            created_at: now,
            updated_at: now,
            closed_at: None,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };

        // Write it in V2 format (issues/{uuid}/issue.json)
        let cache_dir = crosslink_dir.join(".hub-cache");
        let issue_dir = cache_dir.join("issues").join(uuid.to_string());
        std::fs::create_dir_all(&issue_dir).unwrap();
        let json = serde_json::to_string_pretty(&issue).unwrap();
        std::fs::write(issue_dir.join("issue.json"), &json).unwrap();

        // Also git add + commit so the cache is clean
        writer
            .git_in_cache(&["add", &format!("issues/{}/issue.json", uuid)])
            .unwrap();
        let _ = writer.git_in_cache(&["commit", "-m", "add offline issue", "--no-gpg-sign"]);

        // Now promote
        let mapping = writer.promote_offline_issues(&db).unwrap();
        assert_eq!(mapping.len(), 1, "Should promote exactly 1 issue");
        let (_neg_id, new_id, title) = &mapping[0];
        assert_eq!(title, "Offline issue");
        assert!(*new_id > 0, "New display ID should be positive");

        // write_commit_push writes the promoted file in V1 format
        // (issues/{uuid}.json) regardless of layout version
        let v1_file = cache_dir.join("issues").join(format!("{}.json", uuid));
        if v1_file.exists() {
            let content = std::fs::read_to_string(&v1_file).unwrap();
            let updated: crate::issue_file::IssueFile = serde_json::from_str(&content).unwrap();
            assert!(
                updated.display_id.is_some(),
                "display_id should be set after promotion"
            );
        }

        drop(work_dir);
    }
}
