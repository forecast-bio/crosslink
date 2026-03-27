//! Checkpoint state types and I/O for the compaction engine.
//!
//! The checkpoint is the materialized state produced by reducing all events.
//! It lives at `checkpoint/state.json` in the hub cache and tracks display ID
//! allocation, lock state, issue state, and compaction metadata.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use uuid::Uuid;

use crate::events::OrderingKey;

/// Materialized state produced by compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointState {
    pub next_display_id: i64,
    pub next_comment_id: i64,
    pub display_id_map: BTreeMap<Uuid, i64>,
    pub locks: BTreeMap<i64, LockEntry>,
    pub issues: BTreeMap<Uuid, CompactIssue>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skew_warnings: Vec<SkewWarning>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction_lease: Option<CompactionLease>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsigned_event_warnings: Vec<UnsignedEventWarning>,
    /// Compaction watermark (last processed ordering key), written atomically
    /// with the rest of the checkpoint state to prevent inconsistent recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<OrderingKey>,
}

impl Default for CheckpointState {
    fn default() -> Self {
        Self {
            next_display_id: 1,
            next_comment_id: 1,
            display_id_map: BTreeMap::new(),
            locks: BTreeMap::new(),
            issues: BTreeMap::new(),
            skew_warnings: Vec::new(),
            compaction_lease: None,
            unsigned_event_warnings: Vec::new(),
            watermark: None,
        }
    }
}

/// A lock entry in the checkpoint state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEntry {
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub claimed_at: DateTime<Utc>,
}

/// Compact issue representation for reduction (tracks mutable fields only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactIssue {
    pub uuid: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_id: Option<i64>,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: crate::models::IssueStatus,
    pub priority: crate::models::Priority,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<Uuid>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub labels: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub blockers: BTreeSet<Uuid>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub related: BTreeSet<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone_uuid: Option<Uuid>,
}

/// Advisory compaction lease to prevent concurrent compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionLease {
    pub agent_id: String,
    pub acquired_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// PID of the process that acquired the lease, used for stale detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// Warning about clock skew detected during compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkewWarning {
    pub agent_id: String,
    pub skew_seconds: i64,
    pub event_timestamp: DateTime<Utc>,
}

/// Warning about an unsigned event encountered during compaction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsignedEventWarning {
    pub agent_id: String,
    pub agent_seq: u64,
    pub timestamp: DateTime<Utc>,
}

// ── Checkpoint I/O ───────────────────────────────────────────────────

const CHECKPOINT_FILE: &str = "state.json";
const WATERMARK_FILE: &str = "watermark.json";

fn checkpoint_dir(cache_dir: &Path) -> std::path::PathBuf {
    cache_dir.join("checkpoint")
}

/// Read checkpoint state from disk. Returns default if missing.
pub fn read_checkpoint(cache_dir: &Path) -> Result<CheckpointState> {
    let path = checkpoint_dir(cache_dir).join(CHECKPOINT_FILE);
    if !path.exists() {
        return Ok(CheckpointState::default());
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read checkpoint: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse checkpoint: {}", path.display()))
}

/// Write checkpoint state to disk (pretty-printed JSON).
pub fn write_checkpoint(cache_dir: &Path, state: &CheckpointState) -> Result<()> {
    let dir = checkpoint_dir(cache_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create checkpoint dir: {}", dir.display()))?;
    let path = dir.join(CHECKPOINT_FILE);
    let content = serde_json::to_string_pretty(state)?;
    crate::utils::atomic_write(&path, content.as_bytes())
}

/// Read the compaction watermark (last processed ordering key).
///
/// Reads from the checkpoint state's embedded `watermark` field (atomic).
/// Falls back to the legacy `watermark.json` file for migration.
pub fn read_watermark(cache_dir: &Path) -> Result<Option<OrderingKey>> {
    // Prefer the watermark embedded in checkpoint state (atomic with state).
    let state = read_checkpoint(cache_dir)?;
    if state.watermark.is_some() {
        return Ok(state.watermark);
    }

    // Legacy fallback: separate watermark.json file.
    let path = checkpoint_dir(cache_dir).join(WATERMARK_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read watermark: {}", path.display()))?;
    let key: OrderingKey = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse watermark: {}", path.display()))?;
    Ok(Some(key))
}

/// Write the compaction watermark atomically with the checkpoint state.
///
/// Reads the current checkpoint, sets the watermark, and writes both
/// in a single atomic file operation. This prevents inconsistent state
/// if a crash occurs between writes.
#[cfg(test)]
pub(crate) fn write_watermark(cache_dir: &Path, key: &OrderingKey) -> Result<()> {
    let mut state = read_checkpoint(cache_dir)?;
    state.watermark = Some(key.clone());
    write_checkpoint(cache_dir, &state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_checkpoint_state() {
        let state = CheckpointState::default();
        assert_eq!(state.next_display_id, 1);
        assert_eq!(state.next_comment_id, 1);
        assert!(state.display_id_map.is_empty());
        assert!(state.locks.is_empty());
        assert!(state.issues.is_empty());
        assert!(state.skew_warnings.is_empty());
        assert!(state.compaction_lease.is_none());
        assert!(state.unsigned_event_warnings.is_empty());
    }

    #[test]
    fn test_checkpoint_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();

        let mut state = CheckpointState {
            next_display_id: 42,
            next_comment_id: 10,
            ..Default::default()
        };

        let uuid = Uuid::new_v4();
        state.display_id_map.insert(uuid, 1);
        state.issues.insert(
            uuid,
            CompactIssue {
                uuid,
                display_id: Some(1),
                title: "Test".to_string(),
                description: None,
                status: crate::models::IssueStatus::Open,
                priority: crate::models::Priority::Medium,
                parent_uuid: None,
                created_by: "agent-1".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                closed_at: None,
                labels: BTreeSet::from(["bug".to_string()]),
                blockers: BTreeSet::new(),
                related: BTreeSet::new(),
                milestone_uuid: None,
            },
        );
        state.locks.insert(
            1,
            LockEntry {
                agent_id: "agent-1".to_string(),
                branch: Some("feature/x".to_string()),
                claimed_at: Utc::now(),
            },
        );

        write_checkpoint(cache_dir, &state).unwrap();
        let loaded = read_checkpoint(cache_dir).unwrap();

        assert_eq!(loaded.next_display_id, 42);
        assert_eq!(loaded.next_comment_id, 10);
        assert_eq!(loaded.display_id_map.len(), 1);
        assert_eq!(loaded.issues.len(), 1);
        assert_eq!(loaded.locks.len(), 1);
        assert_eq!(loaded.issues[&uuid].title, "Test");
        assert!(loaded.issues[&uuid].labels.contains("bug"));
    }

    #[test]
    fn test_read_checkpoint_missing() {
        let dir = tempfile::tempdir().unwrap();
        let state = read_checkpoint(dir.path()).unwrap();
        assert_eq!(state.next_display_id, 1);
    }

    #[test]
    fn test_watermark_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();

        let key = OrderingKey {
            timestamp: Utc::now(),
            agent_id: "agent-1".to_string(),
            agent_seq: 5,
        };

        write_watermark(cache_dir, &key).unwrap();
        let loaded = read_watermark(cache_dir).unwrap().unwrap();

        assert_eq!(loaded.agent_id, "agent-1");
        assert_eq!(loaded.agent_seq, 5);
    }

    #[test]
    fn test_read_watermark_missing() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_watermark(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_compaction_lease_serialization() {
        let lease = CompactionLease {
            agent_id: "agent-1".to_string(),
            acquired_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::seconds(30),
            pid: Some(12345),
        };
        let json = serde_json::to_string(&lease).unwrap();
        let parsed: CompactionLease = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_id, "agent-1");
        assert_eq!(parsed.pid, Some(12345));
    }

    #[test]
    fn test_compaction_lease_backward_compat() {
        // Old leases without pid field should deserialize with pid = None
        let json = r#"{"agent_id":"agent-1","acquired_at":"2025-01-01T00:00:00Z","expires_at":"2025-01-01T00:00:30Z"}"#;
        let parsed: CompactionLease = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.agent_id, "agent-1");
        assert_eq!(parsed.pid, None);
    }

    #[test]
    fn test_compact_issue_with_sets() {
        let issue = CompactIssue {
            uuid: Uuid::new_v4(),
            display_id: Some(1),
            title: "Test".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::High,
            parent_uuid: None,
            created_by: "agent-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            labels: BTreeSet::from(["a".to_string(), "b".to_string()]),
            blockers: BTreeSet::from([Uuid::new_v4()]),
            related: BTreeSet::new(),
            milestone_uuid: None,
        };
        let json = serde_json::to_string(&issue).unwrap();
        let parsed: CompactIssue = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.labels.len(), 2);
        assert_eq!(parsed.blockers.len(), 1);
    }

    #[test]
    fn test_read_watermark_legacy_fallback() {
        // When checkpoint state has no embedded watermark but a legacy
        // watermark.json file exists, read_watermark should fall back
        // to reading the separate file (lines 163-167).
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();

        // Write a checkpoint state WITHOUT an embedded watermark.
        let state = CheckpointState::default();
        assert!(state.watermark.is_none());
        write_checkpoint(cache_dir, &state).unwrap();

        // Manually write a legacy watermark.json file in the checkpoint dir.
        let checkpoint_dir = cache_dir.join("checkpoint");
        let legacy_key = OrderingKey {
            timestamp: Utc::now(),
            agent_id: "legacy-agent".to_string(),
            agent_seq: 99,
        };
        let watermark_path = checkpoint_dir.join("watermark.json");
        let content = serde_json::to_string_pretty(&legacy_key).unwrap();
        std::fs::write(&watermark_path, content).unwrap();

        // read_watermark should fall back to the legacy watermark.json file.
        let loaded = read_watermark(cache_dir).unwrap().unwrap();
        assert_eq!(loaded.agent_id, "legacy-agent");
        assert_eq!(loaded.agent_seq, 99);
    }

    #[test]
    fn test_read_watermark_embedded_takes_precedence_over_legacy() {
        // When checkpoint state has an embedded watermark AND a legacy
        // watermark.json file exists, the embedded watermark should win.
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();

        // Write a checkpoint with an embedded watermark.
        let embedded_key = OrderingKey {
            timestamp: Utc::now(),
            agent_id: "embedded-agent".to_string(),
            agent_seq: 50,
        };
        write_watermark(cache_dir, &embedded_key).unwrap();

        // Also write a legacy watermark.json with different data.
        let checkpoint_dir = cache_dir.join("checkpoint");
        let legacy_key = OrderingKey {
            timestamp: Utc::now(),
            agent_id: "legacy-agent".to_string(),
            agent_seq: 99,
        };
        let watermark_path = checkpoint_dir.join("watermark.json");
        let content = serde_json::to_string_pretty(&legacy_key).unwrap();
        std::fs::write(&watermark_path, content).unwrap();

        // Should prefer the embedded watermark, not the legacy file.
        let loaded = read_watermark(cache_dir).unwrap().unwrap();
        assert_eq!(loaded.agent_id, "embedded-agent");
        assert_eq!(loaded.agent_seq, 50);
    }

    #[test]
    fn test_checkpoint_state_with_warnings() {
        let mut state = CheckpointState::default();
        state.skew_warnings.push(SkewWarning {
            agent_id: "agent-1".to_string(),
            skew_seconds: 120,
            event_timestamp: Utc::now(),
        });
        state.unsigned_event_warnings.push(UnsignedEventWarning {
            agent_id: "agent-2".to_string(),
            agent_seq: 3,
            timestamp: Utc::now(),
        });

        let json = serde_json::to_string_pretty(&state).unwrap();
        let parsed: CheckpointState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.skew_warnings.len(), 1);
        assert_eq!(parsed.unsigned_event_warnings.len(), 1);
    }
}
