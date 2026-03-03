//! Clock skew detection using git commit timestamps as an independent witness.
//!
//! During compaction, event timestamps from agent logs are compared against
//! the git commit timestamp that introduced them to the hub branch. If the
//! skew exceeds a threshold, a `SkewViolation` is recorded.
//!
//! This provides a stronger integrity guarantee than comparing against
//! `Utc::now()` — the git committer date acts as a trusted timestamp oracle
//! that is independent of the agent's local clock.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

use crate::events::{Event, EventEnvelope};

/// Clock skew threshold in seconds. Events whose timestamp differs from the
/// git commit timestamp by more than this are flagged.
const SKEW_THRESHOLD_SECS: i64 = 60;

/// A clock skew violation detected by comparing an event timestamp against
/// the git commit that introduced it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkewViolation {
    pub agent_id: String,
    pub event_description: String,
    pub event_timestamp: DateTime<Utc>,
    pub commit_timestamp: DateTime<Utc>,
    pub skew_seconds: i64,
}

/// A git commit with its hash and committer timestamp.
#[derive(Debug, Clone)]
struct GitCommit {
    hash: String,
    timestamp: DateTime<Utc>,
}

/// Detect clock skew violations by comparing event timestamps against the
/// git commit timestamps that introduced them to the hub branch.
///
/// For each agent's `events.log`, finds all git commits that modified the file,
/// extracts the event lines added in each commit, and flags any where
/// `|event_timestamp - commit_timestamp| > SKEW_THRESHOLD_SECS`.
pub fn detect_git_skew_violations(cache_dir: &Path) -> Result<Vec<SkewViolation>> {
    let mut violations = Vec::new();

    let agents_dir = cache_dir.join("agents");
    if !agents_dir.exists() {
        return Ok(violations);
    }

    for entry in std::fs::read_dir(&agents_dir)
        .with_context(|| format!("Failed to read agents dir: {}", agents_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let agent_id = entry.file_name().to_string_lossy().to_string();
        let relative_log = format!("agents/{}/events.log", agent_id);

        let commits = get_commits_for_file(cache_dir, &relative_log)?;
        for commit in &commits {
            let added_events = get_events_added_in_commit(cache_dir, &commit.hash, &relative_log)?;

            for envelope in &added_events {
                let diff = (envelope.timestamp - commit.timestamp).num_seconds().abs();
                if diff > SKEW_THRESHOLD_SECS {
                    violations.push(SkewViolation {
                        agent_id: agent_id.clone(),
                        event_description: describe_event(&envelope.event),
                        event_timestamp: envelope.timestamp,
                        commit_timestamp: commit.timestamp,
                        skew_seconds: diff,
                    });
                }
            }
        }
    }

    Ok(violations)
}

/// Write skew violations to `checkpoint/skew_warnings.json`.
pub fn write_skew_violations(cache_dir: &Path, violations: &[SkewViolation]) -> Result<()> {
    let dir = cache_dir.join("checkpoint");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create checkpoint dir: {}", dir.display()))?;
    let path = dir.join("skew_warnings.json");
    let content = serde_json::to_string_pretty(violations)?;
    crate::utils::atomic_write(&path, content.as_bytes())
}

/// Read skew violations from `checkpoint/skew_warnings.json`.
pub fn read_skew_violations(cache_dir: &Path) -> Result<Vec<SkewViolation>> {
    let path = cache_dir.join("checkpoint").join("skew_warnings.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read skew warnings: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse skew warnings: {}", path.display()))
}

/// Get all git commits that modified a file, newest first.
fn get_commits_for_file(cache_dir: &Path, file_path: &str) -> Result<Vec<GitCommit>> {
    let output = Command::new("git")
        .args(["log", "--format=%H %cI", "--", file_path])
        .current_dir(cache_dir)
        .output()
        .context("Failed to run git log")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut commits = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((hash, timestamp_str)) = line.split_once(' ') {
            if let Ok(ts) = DateTime::parse_from_rfc3339(timestamp_str) {
                commits.push(GitCommit {
                    hash: hash.to_string(),
                    timestamp: ts.with_timezone(&Utc),
                });
            }
        }
    }

    Ok(commits)
}

/// Extract event envelopes that were added (not removed) in a specific commit.
///
/// Parses the git diff output for the commit, looking for lines starting with
/// `+` (excluding the `+++` diff header). Each added line is parsed as an
/// NDJSON event envelope.
fn get_events_added_in_commit(
    cache_dir: &Path,
    commit_hash: &str,
    file_path: &str,
) -> Result<Vec<EventEnvelope>> {
    let output = Command::new("git")
        .args(["show", "--format=", "-p", commit_hash, "--", file_path])
        .current_dir(cache_dir)
        .output()
        .context("Failed to run git show")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut events = Vec::new();

    for line in stdout.lines() {
        if line.starts_with('+') && !line.starts_with("+++") {
            let json_line = &line[1..];
            if let Ok(envelope) = serde_json::from_str::<EventEnvelope>(json_line) {
                events.push(envelope);
            }
        }
    }

    Ok(events)
}

/// Create a brief human-readable description of an event.
fn describe_event(event: &Event) -> String {
    match event {
        Event::IssueCreated { uuid, title, .. } => {
            format!("IssueCreated({}, {})", uuid, title)
        }
        Event::IssueUpdated { uuid, .. } => format!("IssueUpdated({})", uuid),
        Event::StatusChanged {
            uuid, new_status, ..
        } => format!("StatusChanged({}, {})", uuid, new_status),
        Event::LockClaimed {
            issue_display_id, ..
        } => format!("LockClaimed(#{})", issue_display_id),
        Event::LockReleased { issue_display_id } => {
            format!("LockReleased(#{})", issue_display_id)
        }
        Event::DependencyAdded {
            blocked_uuid,
            blocker_uuid,
        } => format!(
            "DependencyAdded({} blocked by {})",
            blocked_uuid, blocker_uuid
        ),
        Event::DependencyRemoved {
            blocked_uuid,
            blocker_uuid,
        } => format!(
            "DependencyRemoved({} unblocked from {})",
            blocked_uuid, blocker_uuid
        ),
        Event::RelationAdded { uuid_a, uuid_b } => {
            format!("RelationAdded({}, {})", uuid_a, uuid_b)
        }
        Event::RelationRemoved { uuid_a, uuid_b } => {
            format!("RelationRemoved({}, {})", uuid_a, uuid_b)
        }
        Event::MilestoneAssigned { issue_uuid, .. } => {
            format!("MilestoneAssigned({})", issue_uuid)
        }
        Event::LabelAdded {
            issue_uuid, label, ..
        } => format!("LabelAdded({}, {})", issue_uuid, label),
        Event::LabelRemoved {
            issue_uuid, label, ..
        } => format!("LabelRemoved({}, {})", issue_uuid, label),
        Event::ParentChanged { issue_uuid, .. } => {
            format!("ParentChanged({})", issue_uuid)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventEnvelope;
    use chrono::Duration;
    use std::process::Command;
    use uuid::Uuid;

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

    /// Set up a git repo in a temp directory to simulate the hub cache.
    fn setup_git_repo(dir: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .expect("git init failed");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .expect("git config email failed");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .expect("git config name failed");
        // Create initial commit so we have a HEAD
        std::fs::write(dir.join(".gitkeep"), "").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir)
            .output()
            .expect("git commit failed");
    }

    /// Commit a file with a specific committer timestamp.
    fn commit_with_timestamp(dir: &Path, message: &str, timestamp: &DateTime<Utc>) {
        let ts_str = timestamp.to_rfc3339();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .expect("git add failed");
        Command::new("git")
            .args(["commit", "-m", message, "--allow-empty-message"])
            .current_dir(dir)
            .env("GIT_COMMITTER_DATE", &ts_str)
            .env("GIT_AUTHOR_DATE", &ts_str)
            .output()
            .expect("git commit failed");
    }

    #[test]
    fn test_no_violations_within_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_git_repo(cache_dir);

        let agent_dir = cache_dir.join("agents/agent-1");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let now = Utc::now();
        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Normal event".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        env.timestamp = now;

        crate::events::append_event(&agent_dir.join("events.log"), &env).unwrap();
        commit_with_timestamp(cache_dir, "add events", &(now + Duration::seconds(10)));

        let violations = detect_git_skew_violations(cache_dir).unwrap();
        assert!(
            violations.is_empty(),
            "Expected no violations for events within threshold, got: {:?}",
            violations
        );
    }

    #[test]
    fn test_violation_detected_when_skew_exceeds_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_git_repo(cache_dir);

        let agent_dir = cache_dir.join("agents/agent-1");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let commit_time = Utc::now();
        let event_time = commit_time + Duration::seconds(300);

        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Skewed event".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        env.timestamp = event_time;

        crate::events::append_event(&agent_dir.join("events.log"), &env).unwrap();
        commit_with_timestamp(cache_dir, "add skewed events", &commit_time);

        let violations = detect_git_skew_violations(cache_dir).unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].agent_id, "agent-1");
        // Git truncates timestamps to second precision, so allow ±1s tolerance
        assert!(
            (violations[0].skew_seconds - 300).abs() <= 1,
            "Expected skew ~300s, got {}",
            violations[0].skew_seconds
        );
        // Compare at second precision (git truncates sub-seconds)
        assert_eq!(
            violations[0].commit_timestamp.timestamp(),
            commit_time.timestamp()
        );
    }

    #[test]
    fn test_multiple_agents_independent_detection() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_git_repo(cache_dir);

        let commit_time = Utc::now();

        // Agent 1: event within threshold
        let agent1_dir = cache_dir.join("agents/agent-1");
        std::fs::create_dir_all(&agent1_dir).unwrap();
        let mut env1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Good event".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        env1.timestamp = commit_time + Duration::seconds(5);
        crate::events::append_event(&agent1_dir.join("events.log"), &env1).unwrap();

        // Agent 2: event with excessive skew
        let agent2_dir = cache_dir.join("agents/agent-2");
        std::fs::create_dir_all(&agent2_dir).unwrap();
        let mut env2 = make_envelope(
            "agent-2",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Bad event".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-2".to_string(),
            },
        );
        env2.timestamp = commit_time + Duration::seconds(120);
        crate::events::append_event(&agent2_dir.join("events.log"), &env2).unwrap();

        commit_with_timestamp(cache_dir, "add all events", &commit_time);

        let violations = detect_git_skew_violations(cache_dir).unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].agent_id, "agent-2");
    }

    #[test]
    fn test_no_agents_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let violations = detect_git_skew_violations(dir.path()).unwrap();
        assert!(violations.is_empty());
    }

    #[test]
    fn test_skew_violations_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        std::fs::create_dir_all(cache_dir.join("checkpoint")).unwrap();

        let violations = vec![
            SkewViolation {
                agent_id: "agent-1".to_string(),
                event_description: "IssueCreated(abc, Test)".to_string(),
                event_timestamp: Utc::now(),
                commit_timestamp: Utc::now() - Duration::seconds(120),
                skew_seconds: 120,
            },
            SkewViolation {
                agent_id: "agent-2".to_string(),
                event_description: "LabelAdded(def, bug)".to_string(),
                event_timestamp: Utc::now(),
                commit_timestamp: Utc::now() - Duration::seconds(200),
                skew_seconds: 200,
            },
        ];

        write_skew_violations(cache_dir, &violations).unwrap();
        let loaded = read_skew_violations(cache_dir).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].agent_id, "agent-1");
        assert_eq!(loaded[0].skew_seconds, 120);
        assert_eq!(loaded[1].agent_id, "agent-2");
    }

    #[test]
    fn test_read_skew_violations_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = read_skew_violations(dir.path()).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_describe_event_variants() {
        let uuid = Uuid::new_v4();
        let cases = vec![
            (
                Event::IssueCreated {
                    uuid,
                    title: "Test".to_string(),
                    description: None,
                    priority: "medium".to_string(),
                    labels: vec![],
                    parent_uuid: None,
                    created_by: "agent-1".to_string(),
                },
                format!("IssueCreated({}, Test)", uuid),
            ),
            (
                Event::LockClaimed {
                    issue_display_id: 42,
                    branch: None,
                },
                "LockClaimed(#42)".to_string(),
            ),
            (
                Event::LabelAdded {
                    issue_uuid: uuid,
                    label: "bug".to_string(),
                },
                format!("LabelAdded({}, bug)", uuid),
            ),
        ];

        for (event, expected) in cases {
            assert_eq!(describe_event(&event), expected);
        }
    }

    #[test]
    fn test_negative_skew_detected() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_git_repo(cache_dir);

        let agent_dir = cache_dir.join("agents/agent-1");
        std::fs::create_dir_all(&agent_dir).unwrap();

        let commit_time = Utc::now();
        let event_time = commit_time - Duration::seconds(120);

        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Behind event".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        env.timestamp = event_time;

        crate::events::append_event(&agent_dir.join("events.log"), &env).unwrap();
        commit_with_timestamp(cache_dir, "add behind events", &commit_time);

        let violations = detect_git_skew_violations(cache_dir).unwrap();
        assert_eq!(violations.len(), 1);
        // Git truncates timestamps to second precision, so allow ±1s tolerance
        assert!(
            (violations[0].skew_seconds - 120).abs() <= 1,
            "Expected skew ~120s, got {}",
            violations[0].skew_seconds
        );
    }
}
