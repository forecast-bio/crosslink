//! Compaction engine for the event-sourced CRDT system.
//!
//! Reads append-only event logs from all agents, applies deterministic
//! reduction rules, and materializes the result as checkpoint state plus
//! per-entity JSON files (issues, locks).

use anyhow::{Context, Result};
use chrono::{Duration, Utc};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use uuid::Uuid;

use crate::checkpoint::{
    read_checkpoint, read_watermark, write_checkpoint, write_watermark, CheckpointState,
    CompactIssue, CompactionLease, LockEntry, SkewWarning, UnsignedEventWarning,
};
use crate::events::{Event, EventEnvelope, OrderingKey};
use crate::issue_file::{IssueFile, LockFileV2};

/// Compaction lease duration in seconds.
const LEASE_DURATION_SECS: i64 = 30;

/// Clock skew threshold in seconds.
const SKEW_THRESHOLD_SECS: i64 = 60;

/// Result of a compaction run.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub events_processed: usize,
    pub issues_materialized: usize,
    pub locks_materialized: usize,
    pub skew_warnings: usize,
    pub unsigned_warnings: usize,
    pub git_skew_violations: usize,
}

/// Run compaction on the hub cache.
///
/// Reads all agent event logs, applies reduction rules in deterministic order,
/// writes checkpoint state and materializes issue/lock files.
///
/// If `force` is false, respects the compaction lease.
/// Returns `None` if lease is held by another agent and not expired.
pub fn compact(cache_dir: &Path, agent_id: &str, force: bool) -> Result<Option<CompactionResult>> {
    let mut state = read_checkpoint(cache_dir)?;

    if !force && !try_acquire_lease(&mut state, agent_id) {
        return Ok(None);
    }

    // Read watermark for incremental compaction
    let watermark = read_watermark(cache_dir)?;

    // Collect events from all agent logs
    let agents_dir = cache_dir.join("agents");
    let mut all_events = Vec::new();

    if agents_dir.exists() {
        for entry in std::fs::read_dir(&agents_dir)
            .with_context(|| format!("Failed to read agents dir: {}", agents_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let log_path = entry.path().join("events.log");
            let events = if let Some(ref wm) = watermark {
                crate::events::read_events_after(&log_path, wm)?
            } else {
                crate::events::read_events(&log_path)?
            };
            all_events.extend(events);
        }
    }

    if all_events.is_empty() && watermark.is_some() {
        // Still run git-based skew detection even with no new events
        let git_violations =
            crate::clock_skew::detect_git_skew_violations(cache_dir).unwrap_or_default();
        let git_skew_violations = git_violations.len();
        crate::clock_skew::write_skew_violations(cache_dir, &git_violations)?;

        release_lease(&mut state);
        write_checkpoint(cache_dir, &state)?;
        return Ok(Some(CompactionResult {
            events_processed: 0,
            issues_materialized: 0,
            locks_materialized: 0,
            skew_warnings: state.skew_warnings.len(),
            unsigned_warnings: state.unsigned_event_warnings.len(),
            git_skew_violations,
        }));
    }

    // If no watermark, we're doing a full compaction — reset state
    if watermark.is_none() {
        let lease = state.compaction_lease.clone();
        state = CheckpointState::default();
        state.compaction_lease = lease;
    }

    // Sort by ordering key for deterministic reduction
    all_events.sort_by(|a, b| OrderingKey::from_envelope(a).cmp(&OrderingKey::from_envelope(b)));

    let events_processed = all_events.len();
    let mut changed_issues: HashSet<Uuid> = HashSet::new();
    let mut changed_locks: HashSet<i64> = HashSet::new();

    // Clear warnings for fresh compaction
    state.skew_warnings.clear();
    state.unsigned_event_warnings.clear();

    let allowed_signers_path = cache_dir.join("trust").join("allowed_signers");

    // Apply each event
    for envelope in &all_events {
        detect_clock_skew(&mut state, envelope);
        check_unsigned(&mut state, envelope, &allowed_signers_path);
        apply(
            &mut state,
            envelope,
            &mut changed_issues,
            &mut changed_locks,
        );
    }

    // Update watermark to last processed event
    if let Some(last) = all_events.last() {
        let new_watermark = OrderingKey::from_envelope(last);
        write_watermark(cache_dir, &new_watermark)?;
    }

    // Materialize changed entities to disk
    materialize(cache_dir, &state, &changed_issues, &changed_locks)?;

    // Run git-based clock skew detection
    let git_violations =
        crate::clock_skew::detect_git_skew_violations(cache_dir).unwrap_or_default();
    let git_skew_violations = git_violations.len();
    crate::clock_skew::write_skew_violations(cache_dir, &git_violations)?;

    let issues_materialized = changed_issues.len();
    let locks_materialized = changed_locks.len();
    let skew_warnings = state.skew_warnings.len();
    let unsigned_warnings = state.unsigned_event_warnings.len();

    release_lease(&mut state);
    write_checkpoint(cache_dir, &state)?;

    Ok(Some(CompactionResult {
        events_processed,
        issues_materialized,
        locks_materialized,
        skew_warnings,
        unsigned_warnings,
        git_skew_violations,
    }))
}

/// Prune (flush) compacted events from an agent's log.
///
/// Removes events at or below the current watermark.
/// Returns the number of events pruned.
pub fn prune_events(cache_dir: &Path, agent_id: &str) -> Result<usize> {
    let watermark = match read_watermark(cache_dir)? {
        Some(wm) => wm,
        None => return Ok(0),
    };

    let log_path = cache_dir.join("agents").join(agent_id).join("events.log");
    if !log_path.exists() {
        return Ok(0);
    }

    let all_events = crate::events::read_events(&log_path)?;
    let before_count = all_events.len();
    let remaining: Vec<_> = all_events
        .into_iter()
        .filter(|e| OrderingKey::from_envelope(e) > watermark)
        .collect();

    let pruned = before_count - remaining.len();
    if pruned > 0 {
        let codec = crate::events::NdjsonCodec;
        let bytes = <crate::events::NdjsonCodec as crate::events::EventCodec>::encode_batch(
            &codec, &remaining,
        )?;
        std::fs::write(&log_path, bytes)
            .with_context(|| format!("Failed to write pruned log: {}", log_path.display()))?;
    }

    Ok(pruned)
}

// ── Internal functions ───────────────────────────────────────────────

/// Try to acquire the compaction lease. Returns true if acquired.
fn try_acquire_lease(state: &mut CheckpointState, agent_id: &str) -> bool {
    let now = Utc::now();
    if let Some(ref lease) = state.compaction_lease {
        if lease.agent_id == agent_id {
            // We already hold it — refresh
        } else if lease.expires_at > now {
            // Another agent holds an unexpired lease
            return false;
        }
        // Expired lease from another agent — take it
    }

    state.compaction_lease = Some(CompactionLease {
        agent_id: agent_id.to_string(),
        acquired_at: now,
        expires_at: now + Duration::seconds(LEASE_DURATION_SECS),
    });
    true
}

/// Release the compaction lease.
fn release_lease(state: &mut CheckpointState) {
    state.compaction_lease = None;
}

/// Deterministic reduction: apply a single event to checkpoint state.
fn apply(
    state: &mut CheckpointState,
    envelope: &EventEnvelope,
    changed_issues: &mut HashSet<Uuid>,
    changed_locks: &mut HashSet<i64>,
) {
    match &envelope.event {
        Event::IssueCreated {
            uuid,
            title,
            description,
            priority,
            labels,
            parent_uuid,
            created_by,
        } => {
            // Skip if UUID already exists (idempotent)
            if state.issues.contains_key(uuid) {
                return;
            }
            let display_id = state.next_display_id;
            state.next_display_id += 1;
            state.display_id_map.insert(*uuid, display_id);
            state.issues.insert(
                *uuid,
                CompactIssue {
                    uuid: *uuid,
                    display_id: Some(display_id),
                    title: title.clone(),
                    description: description.clone(),
                    status: "open".to_string(),
                    priority: priority.clone(),
                    parent_uuid: *parent_uuid,
                    created_by: created_by.clone(),
                    created_at: envelope.timestamp,
                    updated_at: envelope.timestamp,
                    closed_at: None,
                    labels: labels.iter().cloned().collect(),
                    blockers: BTreeSet::new(),
                    related: BTreeSet::new(),
                    milestone_uuid: None,
                },
            );
            changed_issues.insert(*uuid);
        }

        Event::LockClaimed {
            issue_display_id,
            branch,
        } => {
            // First-claim-wins: reject if different agent holds it
            if let Some(existing) = state.locks.get(issue_display_id) {
                if existing.agent_id != envelope.agent_id {
                    return;
                }
            }
            state.locks.insert(
                *issue_display_id,
                LockEntry {
                    agent_id: envelope.agent_id.clone(),
                    branch: branch.clone(),
                    claimed_at: envelope.timestamp,
                },
            );
            changed_locks.insert(*issue_display_id);
        }

        Event::LockReleased { issue_display_id } => {
            // Only release if held by this agent
            if let Some(existing) = state.locks.get(issue_display_id) {
                if existing.agent_id == envelope.agent_id {
                    state.locks.remove(issue_display_id);
                    changed_locks.insert(*issue_display_id);
                }
            }
        }

        Event::IssueUpdated {
            uuid,
            title,
            description,
            priority,
        } => {
            if let Some(issue) = state.issues.get_mut(uuid) {
                // Last-writer-wins per field
                if let Some(t) = title {
                    issue.title = t.clone();
                }
                if let Some(d) = description {
                    issue.description = Some(d.clone());
                }
                if let Some(p) = priority {
                    issue.priority = p.clone();
                }
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid);
            }
        }

        Event::StatusChanged {
            uuid,
            new_status,
            closed_at,
        } => {
            if let Some(issue) = state.issues.get_mut(uuid) {
                // Last-writer-wins (latest timestamp)
                issue.status = new_status.clone();
                issue.closed_at = *closed_at;
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid);
            }
        }

        Event::DependencyAdded {
            blocked_uuid,
            blocker_uuid,
        } => {
            if let Some(issue) = state.issues.get_mut(blocked_uuid) {
                issue.blockers.insert(*blocker_uuid);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*blocked_uuid);
            }
        }

        Event::DependencyRemoved {
            blocked_uuid,
            blocker_uuid,
        } => {
            if let Some(issue) = state.issues.get_mut(blocked_uuid) {
                issue.blockers.remove(blocker_uuid);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*blocked_uuid);
            }
        }

        Event::RelationAdded { uuid_a, uuid_b } => {
            if let Some(issue) = state.issues.get_mut(uuid_a) {
                issue.related.insert(*uuid_b);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid_a);
            }
            if let Some(issue) = state.issues.get_mut(uuid_b) {
                issue.related.insert(*uuid_a);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid_b);
            }
        }

        Event::RelationRemoved { uuid_a, uuid_b } => {
            if let Some(issue) = state.issues.get_mut(uuid_a) {
                issue.related.remove(uuid_b);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid_a);
            }
            if let Some(issue) = state.issues.get_mut(uuid_b) {
                issue.related.remove(uuid_a);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid_b);
            }
        }

        Event::MilestoneAssigned {
            issue_uuid,
            milestone_uuid,
        } => {
            if let Some(issue) = state.issues.get_mut(issue_uuid) {
                issue.milestone_uuid = *milestone_uuid;
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*issue_uuid);
            }
        }

        Event::LabelAdded { issue_uuid, label } => {
            if let Some(issue) = state.issues.get_mut(issue_uuid) {
                issue.labels.insert(label.clone());
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*issue_uuid);
            }
        }

        Event::LabelRemoved { issue_uuid, label } => {
            if let Some(issue) = state.issues.get_mut(issue_uuid) {
                issue.labels.remove(label);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*issue_uuid);
            }
        }

        Event::ParentChanged {
            issue_uuid,
            new_parent_uuid,
        } => {
            if let Some(issue) = state.issues.get_mut(issue_uuid) {
                issue.parent_uuid = *new_parent_uuid;
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*issue_uuid);
            }
        }
    }
}

/// Materialize checkpoint state to disk (issue.json + lock files).
fn materialize(
    cache_dir: &Path,
    state: &CheckpointState,
    changed_issues: &HashSet<Uuid>,
    changed_locks: &HashSet<i64>,
) -> Result<()> {
    let issues_dir = cache_dir.join("issues");
    let locks_dir = cache_dir.join("locks");

    // Materialize changed issues
    for uuid in changed_issues {
        if let Some(compact) = state.issues.get(uuid) {
            let issue_dir = issues_dir.join(uuid.to_string());
            std::fs::create_dir_all(&issue_dir)
                .with_context(|| format!("Failed to create issue dir: {}", issue_dir.display()))?;

            let issue_file = compact_to_issue_file(compact);
            let path = issue_dir.join("issue.json");
            let content = serde_json::to_string_pretty(&issue_file)?;
            crate::utils::atomic_write(&path, content.as_bytes())?;
        }
    }

    // Materialize changed locks
    std::fs::create_dir_all(&locks_dir)?;
    for display_id in changed_locks {
        let lock_path = locks_dir.join(format!("{}.json", display_id));
        if let Some(lock_entry) = state.locks.get(display_id) {
            let lock_file = LockFileV2 {
                issue_id: *display_id,
                agent_id: lock_entry.agent_id.clone(),
                branch: lock_entry.branch.clone(),
                claimed_at: lock_entry.claimed_at,
                signed_by: None,
            };
            let content = serde_json::to_string_pretty(&lock_file)?;
            crate::utils::atomic_write(&lock_path, content.as_bytes())?;
        } else {
            // Lock was released — remove the file
            if lock_path.exists() {
                std::fs::remove_file(&lock_path).with_context(|| {
                    format!("Failed to remove lock file: {}", lock_path.display())
                })?;
            }
        }
    }

    Ok(())
}

/// Convert a CompactIssue to an IssueFile for materialization.
fn compact_to_issue_file(compact: &CompactIssue) -> IssueFile {
    IssueFile {
        uuid: compact.uuid,
        display_id: compact.display_id,
        title: compact.title.clone(),
        description: compact.description.clone(),
        status: compact.status.clone(),
        priority: compact.priority.clone(),
        parent_uuid: compact.parent_uuid,
        created_by: compact.created_by.clone(),
        created_at: compact.created_at,
        updated_at: compact.updated_at,
        closed_at: compact.closed_at,
        labels: compact.labels.iter().cloned().collect(),
        comments: vec![],
        blockers: compact.blockers.iter().cloned().collect(),
        related: compact.related.iter().cloned().collect(),
        milestone_uuid: compact.milestone_uuid,
        time_entries: vec![],
    }
}

/// Detect clock skew: flag events where |event_timestamp - now()| > threshold.
fn detect_clock_skew(state: &mut CheckpointState, envelope: &EventEnvelope) {
    let now = Utc::now();
    let diff = (envelope.timestamp - now).num_seconds().abs();
    if diff > SKEW_THRESHOLD_SECS {
        state.skew_warnings.push(SkewWarning {
            agent_id: envelope.agent_id.clone(),
            skew_seconds: diff,
            event_timestamp: envelope.timestamp,
        });
    }
}

/// Check for unsigned events and verify signatures when possible.
fn check_unsigned(
    state: &mut CheckpointState,
    envelope: &EventEnvelope,
    allowed_signers_path: &Path,
) {
    if envelope.signed_by.is_none() || envelope.signature.is_none() {
        state.unsigned_event_warnings.push(UnsignedEventWarning {
            agent_id: envelope.agent_id.clone(),
            agent_seq: envelope.agent_seq,
            timestamp: envelope.timestamp,
        });
    } else if allowed_signers_path.exists() {
        // Verify the signature against the trust store
        if let Ok(false) = crate::events::verify_event_signature(envelope, allowed_signers_path) {
            state.unsigned_event_warnings.push(UnsignedEventWarning {
                agent_id: envelope.agent_id.clone(),
                agent_seq: envelope.agent_seq,
                timestamp: envelope.timestamp,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{append_event, Event, EventEnvelope};
    use chrono::Duration;

    fn make_envelope(agent_id: &str, seq: u64, event: Event) -> EventEnvelope {
        EventEnvelope {
            agent_id: agent_id.to_string(),
            agent_seq: seq,
            timestamp: Utc::now(),
            event,
            signed_by: None,
            signature: None,
        }
    }

    fn setup_cache(dir: &Path) {
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::create_dir_all(dir.join("issues")).unwrap();
        std::fs::create_dir_all(dir.join("locks")).unwrap();
        std::fs::create_dir_all(dir.join("checkpoint")).unwrap();
    }

    #[test]
    fn test_compact_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let result = compact(cache_dir, "agent-1", false).unwrap().unwrap();
        assert_eq!(result.events_processed, 0);
        assert_eq!(result.issues_materialized, 0);
    }

    #[test]
    fn test_compact_issue_created() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log_path = cache_dir.join("agents/agent-1/events.log");
        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Test issue".to_string(),
                description: Some("A description".to_string()),
                priority: "high".to_string(),
                labels: vec!["bug".to_string()],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        append_event(&log_path, &env).unwrap();

        let result = compact(cache_dir, "agent-1", false).unwrap().unwrap();
        assert_eq!(result.events_processed, 1);
        assert_eq!(result.issues_materialized, 1);

        // Verify materialized file
        let issue_path = cache_dir
            .join("issues")
            .join(uuid.to_string())
            .join("issue.json");
        assert!(issue_path.exists());
        let issue: IssueFile =
            serde_json::from_str(&std::fs::read_to_string(&issue_path).unwrap()).unwrap();
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.display_id, Some(1));
        assert_eq!(issue.priority, "high");
        assert_eq!(issue.labels, vec!["bug".to_string()]);
    }

    #[test]
    fn test_display_id_stability() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid1 = Uuid::new_v4();
        let uuid2 = Uuid::new_v4();
        let log_path = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: uuid1,
                title: "First".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::IssueCreated {
                uuid: uuid2,
                title: "Second".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );

        append_event(&log_path, &e1).unwrap();
        append_event(&log_path, &e2).unwrap();

        // First compaction
        compact(cache_dir, "agent-1", true).unwrap();

        // Delete watermark to force full re-compaction
        let wm_path = cache_dir.join("checkpoint/watermark.json");
        if wm_path.exists() {
            std::fs::remove_file(&wm_path).unwrap();
        }

        // Second compaction — IDs should be the same
        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.display_id_map[&uuid1], 1);
        assert_eq!(state.display_id_map[&uuid2], 2);
    }

    #[test]
    fn test_idempotent_issue_created() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log_path = cache_dir.join("agents/agent-1/events.log");

        // Write the same IssueCreated twice (duplicate)
        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Issue".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(1);
        let e2 = make_envelope(
            "agent-1",
            2,
            Event::IssueCreated {
                uuid,
                title: "Issue duplicate".to_string(),
                description: None,
                priority: "high".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );

        append_event(&log_path, &e1).unwrap();
        append_event(&log_path, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.issues.len(), 1);
        assert_eq!(state.issues[&uuid].title, "Issue"); // First one wins
        assert_eq!(state.next_display_id, 2); // Only one ID consumed
    }

    #[test]
    fn test_lock_contention_first_wins() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log1 = cache_dir.join("agents/agent-1/events.log");
        let log2 = cache_dir.join("agents/agent-2/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/a".to_string()),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let mut e2 = make_envelope(
            "agent-2",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/b".to_string()),
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(1);

        append_event(&log1, &e1).unwrap();
        append_event(&log2, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.locks[&1].agent_id, "agent-1"); // First in order wins
    }

    #[test]
    fn test_lock_release() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.locks.is_empty());
        // Lock file should not exist
        assert!(!cache_dir.join("locks/1.json").exists());
    }

    #[test]
    fn test_issue_update_last_writer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log1 = cache_dir.join("agents/agent-1/events.log");
        let log2 = cache_dir.join("agents/agent-2/events.log");

        let mut e_create = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Original".to_string(),
                description: None,
                priority: "low".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e_create.timestamp = Utc::now() - Duration::seconds(10);

        let mut e_update1 = make_envelope(
            "agent-1",
            2,
            Event::IssueUpdated {
                uuid,
                title: Some("Agent 1 title".to_string()),
                description: None,
                priority: None,
            },
        );
        e_update1.timestamp = Utc::now() - Duration::seconds(5);

        let mut e_update2 = make_envelope(
            "agent-2",
            1,
            Event::IssueUpdated {
                uuid,
                title: Some("Agent 2 title".to_string()),
                description: Some("Agent 2 desc".to_string()),
                priority: None,
            },
        );
        e_update2.timestamp = Utc::now() - Duration::seconds(3);

        append_event(&log1, &e_create).unwrap();
        append_event(&log1, &e_update1).unwrap();
        append_event(&log2, &e_update2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let issue = &state.issues[&uuid];
        assert_eq!(issue.title, "Agent 2 title"); // Later timestamp wins
        assert_eq!(issue.description.as_deref(), Some("Agent 2 desc"));
    }

    #[test]
    fn test_label_add_remove_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e_create = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Test".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e_create.timestamp = Utc::now() - Duration::seconds(10);

        let mut e_add1 = make_envelope(
            "agent-1",
            2,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "bug".to_string(),
            },
        );
        e_add1.timestamp = Utc::now() - Duration::seconds(8);

        let mut e_add2 = make_envelope(
            "agent-1",
            3,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "bug".to_string(),
            },
        );
        e_add2.timestamp = Utc::now() - Duration::seconds(6);

        let e_remove = make_envelope(
            "agent-1",
            4,
            Event::LabelRemoved {
                issue_uuid: uuid,
                label: "bug".to_string(),
            },
        );

        append_event(&log, &e_create).unwrap();
        append_event(&log, &e_add1).unwrap();
        append_event(&log, &e_add2).unwrap();
        append_event(&log, &e_remove).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.issues[&uuid].labels.is_empty());
    }

    #[test]
    fn test_dependency_add_remove() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let blocked = Uuid::new_v4();
        let blocker = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: blocked,
                title: "Blocked".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::DependencyAdded {
                blocked_uuid: blocked,
                blocker_uuid: blocker,
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(5);

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.issues[&blocked].blockers.contains(&blocker));
    }

    #[test]
    fn test_relation_bidirectional() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid_a = Uuid::new_v4();
        let uuid_b = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: uuid_a,
                title: "A".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::IssueCreated {
                uuid: uuid_b,
                title: "B".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(9);

        let e3 = make_envelope("agent-1", 3, Event::RelationAdded { uuid_a, uuid_b });

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();
        append_event(&log, &e3).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.issues[&uuid_a].related.contains(&uuid_b));
        assert!(state.issues[&uuid_b].related.contains(&uuid_a));
    }

    #[test]
    fn test_lease_acquire_release() {
        let mut state = CheckpointState::default();

        assert!(try_acquire_lease(&mut state, "agent-1"));
        assert!(state.compaction_lease.is_some());
        assert_eq!(state.compaction_lease.as_ref().unwrap().agent_id, "agent-1");

        // Same agent can re-acquire
        assert!(try_acquire_lease(&mut state, "agent-1"));

        // Different agent cannot while lease is active
        assert!(!try_acquire_lease(&mut state, "agent-2"));

        release_lease(&mut state);
        assert!(state.compaction_lease.is_none());

        // Now agent-2 can acquire
        assert!(try_acquire_lease(&mut state, "agent-2"));
    }

    #[test]
    fn test_lease_expiry() {
        let mut state = CheckpointState {
            compaction_lease: Some(CompactionLease {
                agent_id: "agent-1".to_string(),
                acquired_at: Utc::now() - Duration::seconds(60),
                expires_at: Utc::now() - Duration::seconds(30),
            }),
            ..Default::default()
        };

        // Different agent can take expired lease
        assert!(try_acquire_lease(&mut state, "agent-2"));
        assert_eq!(state.compaction_lease.as_ref().unwrap().agent_id, "agent-2");
    }

    #[test]
    fn test_clock_skew_detection() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Skewed".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        // Set timestamp far in the future to trigger skew warning
        env.timestamp = Utc::now() + Duration::seconds(120);

        append_event(&log, &env).unwrap();

        let result = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert!(result.skew_warnings > 0);
    }

    #[test]
    fn test_unsigned_event_warning() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Unsigned".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        // env has signed_by = None, signature = None

        append_event(&log, &env).unwrap();

        let result = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert!(result.unsigned_warnings > 0);
    }

    #[test]
    fn test_prune_events() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Prunable".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "bug".to_string(),
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        // Compact to create watermark
        compact(cache_dir, "agent-1", true).unwrap();

        // Prune should remove events at/below watermark
        let pruned = prune_events(cache_dir, "agent-1").unwrap();
        assert_eq!(pruned, 2);

        // Log should be empty now
        let remaining = crate::events::read_events(&log).unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_deterministic_reduction_order() {
        // Two agents emit events; regardless of file read order, the state
        // should be the same because we sort by OrderingKey.
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log1 = cache_dir.join("agents/agent-1/events.log");
        let log2 = cache_dir.join("agents/agent-2/events.log");

        let mut e_create = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Test".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e_create.timestamp = Utc::now() - Duration::seconds(20);

        let mut e_label1 = make_envelope(
            "agent-2",
            1,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "feature".to_string(),
            },
        );
        e_label1.timestamp = Utc::now() - Duration::seconds(10);

        let mut e_label2 = make_envelope(
            "agent-1",
            2,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "urgent".to_string(),
            },
        );
        e_label2.timestamp = Utc::now() - Duration::seconds(5);

        append_event(&log1, &e_create).unwrap();
        append_event(&log2, &e_label1).unwrap();
        append_event(&log1, &e_label2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let issue = &state.issues[&uuid];
        assert!(issue.labels.contains("feature"));
        assert!(issue.labels.contains("urgent"));
        assert_eq!(issue.labels.len(), 2);
    }

    #[test]
    fn test_status_changed() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "To close".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let closed_at = Utc::now();
        let e2 = make_envelope(
            "agent-1",
            2,
            Event::StatusChanged {
                uuid,
                new_status: "closed".to_string(),
                closed_at: Some(closed_at),
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.issues[&uuid].status, "closed");
        assert!(state.issues[&uuid].closed_at.is_some());
    }

    #[test]
    fn test_compact_respects_lease() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        // Set an active lease by another agent
        let state = CheckpointState {
            compaction_lease: Some(CompactionLease {
                agent_id: "agent-2".to_string(),
                acquired_at: Utc::now(),
                expires_at: Utc::now() + Duration::seconds(30),
            }),
            ..Default::default()
        };
        write_checkpoint(cache_dir, &state).unwrap();

        // Try to compact as agent-1 without force
        let result = compact(cache_dir, "agent-1", false).unwrap();
        assert!(result.is_none());

        // Force should override
        let result = compact(cache_dir, "agent-1", true).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn test_materialized_issue_file_format() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Materialized".to_string(),
                description: Some("desc".to_string()),
                priority: "critical".to_string(),
                labels: vec!["bug".to_string(), "urgent".to_string()],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        append_event(&log, &env).unwrap();
        compact(cache_dir, "agent-1", true).unwrap();

        // Read back the materialized issue.json
        let path = cache_dir
            .join("issues")
            .join(uuid.to_string())
            .join("issue.json");
        let content = std::fs::read_to_string(&path).unwrap();
        let issue: IssueFile = serde_json::from_str(&content).unwrap();

        assert_eq!(issue.uuid, uuid);
        assert_eq!(issue.display_id, Some(1));
        assert_eq!(issue.title, "Materialized");
        assert_eq!(issue.description.as_deref(), Some("desc"));
        assert_eq!(issue.status, "open");
        assert_eq!(issue.priority, "critical");
        assert!(issue.comments.is_empty());
        assert!(issue.time_entries.is_empty());
    }

    #[test]
    fn test_parent_changed() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: child,
                title: "Child".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::ParentChanged {
                issue_uuid: child,
                new_parent_uuid: Some(parent),
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.issues[&child].parent_uuid, Some(parent));
    }

    #[test]
    fn test_milestone_assigned() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let issue_uuid = Uuid::new_v4();
        let milestone_uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: issue_uuid,
                title: "Milestone test".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::MilestoneAssigned {
                issue_uuid,
                milestone_uuid: Some(milestone_uuid),
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(
            state.issues[&issue_uuid].milestone_uuid,
            Some(milestone_uuid)
        );
    }

    // ── Lock claim / release / contention tests ─────────────────────

    #[test]
    fn test_lock_release_by_non_holder_ignored() {
        // Agent B cannot release a lock held by Agent A
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");

        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(3);

        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        // Lock should still be held by agent-a since agent-b cannot release it
        assert_eq!(state.locks[&1].agent_id, "agent-a");
        assert!(cache_dir.join("locks/1.json").exists());
    }

    #[test]
    fn test_lock_claim_release_reclaim_cycle() {
        // Agent claims, releases, then reclaims the same lock
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/first".to_string()),
            },
        );
        e1.timestamp = now - Duration::seconds(3);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e2.timestamp = now - Duration::seconds(2);

        let mut e3 = make_envelope(
            "agent-1",
            3,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/second".to_string()),
            },
        );
        e3.timestamp = now - Duration::seconds(1);

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();
        append_event(&log, &e3).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.agent_id, "agent-1");
        assert_eq!(lock.branch, Some("feature/second".to_string()));
    }

    #[test]
    fn test_three_way_lock_contention() {
        // Three agents all claim the same lock — earliest timestamp wins
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let now = Utc::now();

        for (agent, seq_offset) in &[("agent-a", 3), ("agent-b", 2), ("agent-c", 1)] {
            let log = cache_dir.join(format!("agents/{}/events.log", agent));
            let mut e = make_envelope(
                agent,
                1,
                Event::LockClaimed {
                    issue_display_id: 1,
                    branch: None,
                },
            );
            e.timestamp = now - Duration::seconds(*seq_offset);
            append_event(&log, &e).unwrap();
        }

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        // agent-c has the earliest timestamp (now - 1), but agent-a has (now - 3)
        assert_eq!(state.locks[&1].agent_id, "agent-a");
    }

    #[test]
    fn test_lock_contention_timestamp_tiebreaker_uses_agent_id() {
        // When timestamps are identical, agent_id string ordering breaks the tie
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let same_time = Utc::now() - Duration::seconds(5);

        let log_a = cache_dir.join("agents/agent-alpha/events.log");
        let log_b = cache_dir.join("agents/agent-beta/events.log");

        let mut e1 = make_envelope(
            "agent-alpha",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = same_time;

        let mut e2 = make_envelope(
            "agent-beta",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e2.timestamp = same_time;

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();

        compact(cache_dir, "agent-alpha", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        // "agent-alpha" < "agent-beta" lexicographically, so alpha wins
        assert_eq!(state.locks[&1].agent_id, "agent-alpha");
    }

    #[test]
    fn test_concurrent_claims_on_different_issues() {
        // Two agents claim different issues — both should succeed
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");

        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/a".to_string()),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 2,
                branch: Some("feature/b".to_string()),
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();

        let result = compact(cache_dir, "agent-a", true).unwrap().unwrap();
        assert_eq!(result.locks_materialized, 2);

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.locks.len(), 2);
        assert_eq!(state.locks[&1].agent_id, "agent-a");
        assert_eq!(state.locks[&2].agent_id, "agent-b");

        // Both lock files should exist
        assert!(cache_dir.join("locks/1.json").exists());
        assert!(cache_dir.join("locks/2.json").exists());
    }

    #[test]
    fn test_lock_branch_metadata_preserved_through_contention() {
        // Winner's branch metadata should be preserved, loser's discarded
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");

        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/winner-branch".to_string()),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/loser-branch".to_string()),
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.agent_id, "agent-a");
        assert_eq!(lock.branch, Some("feature/winner-branch".to_string()));

        // Verify materialized lock file also has correct branch
        let lock_content = std::fs::read_to_string(cache_dir.join("locks/1.json")).unwrap();
        let lock_file: LockFileV2 = serde_json::from_str(&lock_content).unwrap();
        assert_eq!(lock_file.branch, Some("feature/winner-branch".to_string()));
    }

    #[test]
    fn test_lock_release_removes_materialized_file() {
        // After claim + release, the lock file should be removed
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 5,
                branch: None,
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 5,
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        // Lock should be absent from checkpoint and disk
        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.locks.is_empty());
        assert!(!cache_dir.join("locks/5.json").exists());
    }

    #[test]
    fn test_lock_release_of_nonexistent_is_noop() {
        // Releasing a lock that was never claimed should be harmless
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");

        let e = make_envelope(
            "agent-1",
            1,
            Event::LockReleased {
                issue_display_id: 99,
            },
        );
        append_event(&log, &e).unwrap();

        let result = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert_eq!(result.events_processed, 1);
        assert_eq!(result.locks_materialized, 0);

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.locks.is_empty());
    }

    #[test]
    fn test_incremental_compaction_with_lock_changes() {
        // First compaction claims, second (incremental) releases
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        // First round: claim
        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/x".to_string()),
            },
        );
        e1.timestamp = now - Duration::seconds(3);
        append_event(&log, &e1).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        // Verify lock is held
        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.locks[&1].agent_id, "agent-1");
        assert!(cache_dir.join("locks/1.json").exists());

        // Second round: release (incremental — watermark set from first round)
        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e2.timestamp = now - Duration::seconds(1);
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        // Lock should be gone
        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.locks.is_empty());
        assert!(!cache_dir.join("locks/1.json").exists());
    }

    #[test]
    fn test_contention_loser_then_winner_releases() {
        // Agent A wins, Agent B loses. Then Agent A releases.
        // After compaction, the lock should be gone entirely.
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");
        let now = Utc::now();

        // Agent A claims first
        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = now - Duration::seconds(4);

        // Agent B claims second (will lose)
        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e2.timestamp = now - Duration::seconds(3);

        // Agent A releases
        let mut e3 = make_envelope(
            "agent-a",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e3.timestamp = now - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();
        append_event(&log_a, &e3).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        // Lock should be gone — agent-a won and then released
        assert!(state.locks.is_empty());
        assert!(!cache_dir.join("locks/1.json").exists());
    }

    #[test]
    fn test_same_agent_reclaim_after_contention_loss() {
        // Agent A claims at t=1, Agent B claims at t=2 (loses to A).
        // Agent A releases at t=3. Agent B reclaims at t=4 (now unopposed).
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = now - Duration::seconds(4);

        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e2.timestamp = now - Duration::seconds(3);

        let mut e3 = make_envelope(
            "agent-a",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e3.timestamp = now - Duration::seconds(2);

        let mut e4 = make_envelope(
            "agent-b",
            2,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/retry".to_string()),
            },
        );
        e4.timestamp = now - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();
        append_event(&log_a, &e3).unwrap();
        append_event(&log_b, &e4).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.agent_id, "agent-b");
        assert_eq!(lock.branch, Some("feature/retry".to_string()));
    }

    #[test]
    fn test_multiple_issues_independent_contention() {
        // Agent A and B each claim issue 1 and issue 2
        // Agent A wins issue 1 (earlier), Agent B wins issue 2 (earlier)
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");
        let now = Utc::now();

        // Issue 1: Agent A claims first
        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = now - Duration::seconds(4);

        // Issue 2: Agent B claims first
        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 2,
                branch: None,
            },
        );
        e2.timestamp = now - Duration::seconds(3);

        // Issue 1: Agent B claims second (loses)
        let mut e3 = make_envelope(
            "agent-b",
            2,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e3.timestamp = now - Duration::seconds(2);

        // Issue 2: Agent A claims second (loses)
        let mut e4 = make_envelope(
            "agent-a",
            2,
            Event::LockClaimed {
                issue_display_id: 2,
                branch: None,
            },
        );
        e4.timestamp = now - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();
        append_event(&log_b, &e3).unwrap();
        append_event(&log_a, &e4).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.locks.len(), 2);
        assert_eq!(state.locks[&1].agent_id, "agent-a");
        assert_eq!(state.locks[&2].agent_id, "agent-b");
    }

    #[test]
    fn test_prune_preserves_unpruned_lock_events() {
        // Claim at seq=1, release at seq=2. Watermark covers seq=1 only.
        // After prune, seq=2 (release) should remain.
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = now - Duration::seconds(3);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e2.timestamp = now - Duration::seconds(1);

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        // Set watermark to cover only the first event
        let watermark = OrderingKey {
            timestamp: now - Duration::seconds(2),
            agent_id: "agent-1".to_string(),
            agent_seq: 1,
        };
        crate::checkpoint::write_watermark(cache_dir, &watermark).unwrap();

        let pruned = prune_events(cache_dir, "agent-1").unwrap();
        assert_eq!(pruned, 1);

        // Remaining event should be the release
        let remaining = crate::events::read_events(&log).unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(matches!(
            remaining[0].event,
            Event::LockReleased {
                issue_display_id: 1
            }
        ));
    }

    #[test]
    fn test_lock_claimed_at_timestamp_matches_event() {
        // The claimed_at field in the lock entry should match the event timestamp
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let claim_time = Utc::now() - Duration::seconds(10);

        let mut e = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e.timestamp = claim_time;
        append_event(&log, &e).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.claimed_at, claim_time);

        // Check materialized file too
        let lock_content = std::fs::read_to_string(cache_dir.join("locks/1.json")).unwrap();
        let lock_file: LockFileV2 = serde_json::from_str(&lock_content).unwrap();
        assert_eq!(lock_file.claimed_at, claim_time);
    }
}
