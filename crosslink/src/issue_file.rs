//! JSON schema for issue files on the coordination branch.
//!
//! Each issue is stored as `issues/{uuid}.json` on the `crosslink/hub` branch.
//! This module defines the serde types for reading and writing those files,
//! plus the shared `counters.json` and `milestones.json` schemas.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single issue as stored in `issues/{uuid}.json` on the coordination branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IssueFile {
    pub uuid: Uuid,
    /// Stable display ID assigned from the shared counter on first push.
    /// `None` for locally-created issues that haven't been pushed yet.
    pub display_id: Option<i64>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: crate::models::IssueStatus,
    pub priority: crate::models::Priority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<Uuid>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    /// When the issue becomes actionable (GH #361). `None` means always actionable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<DateTime<Utc>>,
    /// Hard deadline (GH #361). `None` means no deadline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<CommentEntry>,
    /// UUIDs of issues that block this one (single-direction storage).
    /// The reverse direction ("blocking") is derived during `SQLite` hydration.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<Uuid>,
    /// UUIDs of related issues (single-direction; hydration inserts both directions).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_uuid: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub time_entries: Vec<TimeEntry>,
}

/// An inline comment within an issue file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentEntry {
    pub id: i64,
    pub author: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
    #[serde(default = "default_comment_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intervention_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver_key_fingerprint: Option<String>,
    /// SSH fingerprint of the signer (e.g. "SHA256:..."), if signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
    /// Base64-encoded SSH signature over the canonical comment content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

fn default_comment_kind() -> String {
    "note".to_string()
}

const KNOWN_COMMENT_KINDS: &[&str] = &[
    "note",
    "plan",
    "decision",
    "observation",
    "blocker",
    "resolution",
    "result",
    "handoff",
    "human",
    "intervention",
    "system",
];

#[must_use]
pub fn validate_comment_kind(kind: &str) -> bool {
    KNOWN_COMMENT_KINDS.contains(&kind)
}

pub const KNOWN_TRIGGER_TYPES: &[&str] = &[
    "tool_rejected",
    "tool_blocked",
    "redirect",
    "context_provided",
    "manual_action",
    "question_answered",
];

#[must_use]
pub fn validate_trigger_type(trigger: &str) -> bool {
    KNOWN_TRIGGER_TYPES.contains(&trigger)
}

/// An inline time-tracking entry within an issue file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TimeEntry {
    pub id: i64,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
}

/// Shared counter file at `meta/counters.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Counters {
    pub next_display_id: i64,
    pub next_comment_id: i64,
    #[serde(default = "default_one")]
    pub next_milestone_id: i64,
}

const fn default_one() -> i64 {
    1
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            next_display_id: 1,
            next_comment_id: 1,
            next_milestone_id: 1,
        }
    }
}

/// Milestone registry at `meta/milestones.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MilestonesFile {
    pub milestones: std::collections::HashMap<Uuid, MilestoneEntry>,
}

/// A single milestone entry in the milestones file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MilestoneEntry {
    pub uuid: Uuid,
    pub display_id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: crate::models::IssueStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
}

impl From<&crate::checkpoint::CompactIssue> for IssueFile {
    fn from(compact: &crate::checkpoint::CompactIssue) -> Self {
        Self {
            uuid: compact.uuid,
            display_id: compact.display_id,
            title: compact.title.clone(),
            description: compact.description.clone(),
            status: compact.status,
            priority: compact.priority,
            parent_uuid: compact.parent_uuid,
            created_by: compact.created_by.clone(),
            created_at: compact.created_at,
            updated_at: compact.updated_at,
            closed_at: compact.closed_at,
            scheduled_at: compact.scheduled_at,
            due_at: compact.due_at,
            labels: compact.labels.iter().cloned().collect(),
            comments: vec![],
            blockers: compact.blockers.iter().copied().collect(),
            related: compact.related.iter().copied().collect(),
            milestone_uuid: compact.milestone_uuid,
            time_entries: vec![],
        }
    }
}

/// Read an issue file from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read or contains invalid JSON.
pub fn read_issue_file(path: &std::path::Path) -> anyhow::Result<IssueFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read issue file: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse issue file: {}", path.display()))
}

/// Write an issue file to disk (pretty-printed JSON).
/// Uses atomic write (temp file + rename) to prevent corruption from interrupted writes.
///
/// # Errors
///
/// Returns an error if serialization or the atomic write fails.
pub fn write_issue_file(path: &std::path::Path, issue: &IssueFile) -> anyhow::Result<()> {
    let content = serde_json::to_string_pretty(issue)?;
    crate::utils::atomic_write(path, content.as_bytes())
}

/// Read all issue files from a directory.
///
/// Handles both v1 layout (`issues/{uuid}.json`) and v2 layout
/// (`issues/{uuid}/issue.json`). When both exist for the same UUID,
/// the V2 version takes precedence (#428).
///
/// # Errors
///
/// Returns an error if the directory cannot be read.
pub fn read_all_issue_files(issues_dir: &std::path::Path) -> anyhow::Result<Vec<IssueFile>> {
    use std::collections::HashMap;

    if !issues_dir.exists() {
        return Ok(Vec::new());
    }

    // Two-pass collection: V1 first, then V2 overwrites any duplicates
    let mut by_uuid: HashMap<uuid::Uuid, IssueFile> = HashMap::new();
    for entry in std::fs::read_dir(issues_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
            // V1 layout: issues/{uuid}.json
            match read_issue_file(&path) {
                Ok(issue) => {
                    by_uuid.entry(issue.uuid).or_insert(issue);
                }
                Err(e) => {
                    tracing::warn!("skipping malformed issue file {}: {e}", path.display());
                }
            }
        } else if path.is_dir() {
            // V2 layout: issues/{uuid}/issue.json
            let issue_path = path.join("issue.json");
            if issue_path.exists() {
                match read_issue_file(&issue_path) {
                    Ok(issue) => {
                        // V2 always wins over V1 for the same UUID
                        by_uuid.insert(issue.uuid, issue);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "skipping malformed issue file {}: {e}",
                            issue_path.display()
                        );
                    }
                }
            }
        }
    }

    Ok(by_uuid.into_values().collect())
}

/// Read counters from `meta/counters.json`, returning defaults if missing.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read_counters(path: &std::path::Path) -> anyhow::Result<Counters> {
    if !path.exists() {
        return Ok(Counters::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

/// Write counters to `meta/counters.json`.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be written.
pub fn write_counters(path: &std::path::Path, counters: &Counters) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(counters)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Read milestones from `meta/milestones.json`, returning defaults if missing.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read_milestones_file(path: &std::path::Path) -> anyhow::Result<MilestonesFile> {
    if !path.exists() {
        return Ok(MilestonesFile::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

/// Read a single milestone file from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read or contains invalid JSON.
pub fn read_milestone_file(path: &std::path::Path) -> anyhow::Result<MilestoneEntry> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read milestone file: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse milestone file: {}", path.display()))
}

/// Write a single milestone file to disk (pretty-printed JSON).
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be written.
pub fn write_milestone_file(path: &std::path::Path, entry: &MilestoneEntry) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(entry)?;
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write milestone file: {}", path.display()))
}

/// Read all milestone files from a directory.
///
/// # Errors
///
/// Returns an error if the directory cannot be read.
pub fn read_all_milestone_files(
    milestones_dir: &std::path::Path,
) -> anyhow::Result<Vec<MilestoneEntry>> {
    let mut entries = Vec::new();
    if !milestones_dir.exists() {
        return Ok(entries);
    }
    for entry in std::fs::read_dir(milestones_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            match read_milestone_file(&path) {
                Ok(ms) => entries.push(ms),
                Err(e) => {
                    tracing::warn!("skipping malformed milestone file {}: {e}", path.display());
                }
            }
        }
    }
    Ok(entries)
}

/// A standalone comment file for the v2 hub layout.
///
/// Stored at `issues/{issue-uuid}/comments/{comment-uuid}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentFile {
    pub uuid: Uuid,
    pub issue_uuid: Uuid,
    pub author: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intervention_context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub driver_key_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// A per-issue lock file for the v2 hub layout.
///
/// Stored at `locks/{display-id}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFileV2 {
    pub issue_id: i64,
    pub agent_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub claimed_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
}

/// Layout version marker stored at `meta/version.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LayoutVersion {
    pub layout_version: u32,
}

/// The current hub directory layout version.
pub const CURRENT_LAYOUT_VERSION: u32 = 2;

/// Read a single comment file from disk.
///
/// # Errors
///
/// Returns an error if the file cannot be read or contains invalid JSON.
pub fn read_comment_file(path: &std::path::Path) -> anyhow::Result<CommentFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read comment file: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse comment file: {}", path.display()))
}

/// Write a comment file to disk (pretty-printed JSON).
/// Uses atomic write (temp file + rename) to prevent corruption from interrupted writes.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the atomic write fails.
pub fn write_comment_file(path: &std::path::Path, comment: &CommentFile) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(comment)?;
    crate::utils::atomic_write(path, content.as_bytes())
}

/// Read all comment files from a directory, sorted by `(created_at, author, uuid)`.
///
/// # Errors
///
/// Returns an error if the directory cannot be read.
pub fn read_comment_files(comments_dir: &std::path::Path) -> anyhow::Result<Vec<CommentFile>> {
    let mut comments = Vec::new();
    if !comments_dir.exists() {
        return Ok(comments);
    }
    for entry in std::fs::read_dir(comments_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            match read_comment_file(&path) {
                Ok(comment) => comments.push(comment),
                Err(e) => {
                    tracing::warn!("skipping malformed comment file {}: {e}", path.display());
                }
            }
        }
    }
    comments.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.author.cmp(&b.author))
            .then_with(|| a.uuid.cmp(&b.uuid))
    });
    Ok(comments)
}

/// Read the layout version from `meta/version.json`.
///
/// If the version file is missing, inspects the `issues/` directory for V2-style
/// subdirectories (containing `issue.json`). If any exist, returns `2` to avoid
/// silently reverting to V1 flat-file writes on a hub that lost its version marker
/// during a rebase or merge conflict (#428).
///
/// # Errors
///
/// Returns an error if the version file exists but cannot be read or parsed.
pub fn read_layout_version(meta_dir: &std::path::Path) -> anyhow::Result<u32> {
    let path = meta_dir.join("version.json");
    if path.exists() {
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read layout version: {}", path.display()))?;
        let version: LayoutVersion = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse layout version: {}", path.display()))?;
        return Ok(version.layout_version);
    }

    // Version file missing — detect layout from directory structure.
    // If any issues/{uuid}/issue.json directories exist, this is a V2 hub
    // that lost its version marker.
    let issues_dir = meta_dir
        .parent()
        .map(|p| p.join("issues"))
        .unwrap_or_default();
    if issues_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&issues_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() && entry.path().join("issue.json").exists() {
                    tracing::warn!(
                        "meta/version.json missing but V2-style issue directories found; \
                         treating as V2 layout (#428)"
                    );
                    return Ok(CURRENT_LAYOUT_VERSION);
                }
            }
        }
    }

    Ok(1)
}

/// Write the layout version to `meta/version.json`.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be written.
pub fn write_layout_version(meta_dir: &std::path::Path, version: u32) -> anyhow::Result<()> {
    std::fs::create_dir_all(meta_dir)?;
    let path = meta_dir.join("version.json");
    let layout = LayoutVersion {
        layout_version: version,
    };
    let content = serde_json::to_string_pretty(&layout)?;
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write layout version: {}", path.display()))
}

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_file_roundtrip() {
        let issue = IssueFile {
            uuid: Uuid::new_v4(),
            display_id: Some(42),
            title: "Fix auth timeout".to_string(),
            description: Some("Users see 504 errors".to_string()),
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Critical,
            parent_uuid: None,
            created_by: "worker-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            scheduled_at: None,
            due_at: None,
            labels: vec!["bug".to_string(), "auth".to_string()],
            comments: vec![CommentEntry {
                id: 1,
                author: "worker-1".to_string(),
                content: "Reproduced on staging".to_string(),
                created_at: Utc::now(),
                kind: "note".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None,
                signature: None,
            }],
            blockers: vec![Uuid::new_v4()],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };

        let json = serde_json::to_string_pretty(&issue).unwrap();
        let parsed: IssueFile = serde_json::from_str(&json).unwrap();
        assert_eq!(issue.uuid, parsed.uuid);
        assert_eq!(issue.display_id, parsed.display_id);
        assert_eq!(issue.title, parsed.title);
        assert_eq!(issue.labels, parsed.labels);
        assert_eq!(issue.blockers, parsed.blockers);
        assert_eq!(issue.comments.len(), parsed.comments.len());
    }

    #[test]
    fn test_issue_file_minimal() {
        // Minimal JSON with only required fields — optional arrays default to empty
        let json = r#"{
            "uuid": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "display_id": 1,
            "title": "Test",
            "status": "open",
            "priority": "medium",
            "created_by": "agent-1",
            "created_at": "2026-02-25T14:30:00Z",
            "updated_at": "2026-02-25T14:30:00Z"
        }"#;
        let parsed: IssueFile = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.title, "Test");
        assert!(parsed.labels.is_empty());
        assert!(parsed.blockers.is_empty());
        assert!(parsed.comments.is_empty());
        assert!(parsed.time_entries.is_empty());
        assert!(parsed.description.is_none());
    }

    #[test]
    fn test_issue_file_null_display_id() {
        // Offline-created issue: display_id is null
        let json = r#"{
            "uuid": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
            "display_id": null,
            "title": "Offline issue",
            "status": "open",
            "priority": "low",
            "created_by": "agent-1",
            "created_at": "2026-02-25T14:30:00Z",
            "updated_at": "2026-02-25T14:30:00Z"
        }"#;
        let parsed: IssueFile = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.display_id, None);
    }

    #[test]
    fn test_counters_default() {
        let c = Counters::default();
        assert_eq!(c.next_display_id, 1);
        assert_eq!(c.next_comment_id, 1);
    }

    #[test]
    fn test_counters_roundtrip() {
        let c = Counters {
            next_display_id: 42,
            next_comment_id: 157,
            next_milestone_id: 3,
        };
        let json = serde_json::to_string(&c).unwrap();
        let parsed: Counters = serde_json::from_str(&json).unwrap();
        assert_eq!(c, parsed);
    }

    #[test]
    fn test_milestones_file_roundtrip() {
        let mut milestones = std::collections::HashMap::new();
        let uuid = Uuid::new_v4();
        milestones.insert(
            uuid,
            MilestoneEntry {
                uuid,
                display_id: 1,
                name: "v1.0".to_string(),
                description: Some("First release".to_string()),
                status: crate::models::IssueStatus::Open,
                created_at: Utc::now(),
                closed_at: None,
            },
        );
        let mf = MilestonesFile { milestones };
        let json = serde_json::to_string_pretty(&mf).unwrap();
        let parsed: MilestonesFile = serde_json::from_str(&json).unwrap();
        assert_eq!(mf.milestones.len(), parsed.milestones.len());
    }

    #[test]
    fn test_read_write_issue_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-issue.json");

        let issue = IssueFile {
            uuid: Uuid::new_v4(),
            display_id: Some(1),
            title: "Test".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "test".to_string(),
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
        };

        write_issue_file(&path, &issue).unwrap();
        let loaded = read_issue_file(&path).unwrap();
        assert_eq!(issue.uuid, loaded.uuid);
        assert_eq!(issue.title, loaded.title);
    }

    #[test]
    fn test_read_all_issue_files() {
        let dir = tempfile::tempdir().unwrap();
        let issues_dir = dir.path().join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();

        for i in 0..3 {
            let issue = IssueFile {
                uuid: Uuid::new_v4(),
                display_id: Some(i + 1),
                title: format!("Issue {}", i + 1),
                description: None,
                status: crate::models::IssueStatus::Open,
                priority: crate::models::Priority::Medium,
                parent_uuid: None,
                created_by: "test".to_string(),
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
            };
            let path = issues_dir.join(format!("{}.json", issue.uuid));
            write_issue_file(&path, &issue).unwrap();
        }

        let loaded = read_all_issue_files(&issues_dir).unwrap();
        assert_eq!(loaded.len(), 3);
    }

    #[test]
    fn test_read_all_skips_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let issues_dir = dir.path().join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();

        // Write a valid file
        let issue = IssueFile {
            uuid: Uuid::new_v4(),
            display_id: Some(1),
            title: "Valid".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "test".to_string(),
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
        };
        write_issue_file(&issues_dir.join("valid.json"), &issue).unwrap();

        // Write a malformed file
        std::fs::write(issues_dir.join("bad.json"), "not valid json").unwrap();

        let loaded = read_all_issue_files(&issues_dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].title, "Valid");
    }

    #[test]
    fn test_read_counters_missing_file() {
        let path = std::path::Path::new("/nonexistent/counters.json");
        let c = read_counters(path).unwrap();
        assert_eq!(c, Counters::default());
    }

    #[test]
    fn test_write_read_counters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meta").join("counters.json");
        let c = Counters {
            next_display_id: 10,
            next_comment_id: 50,
            next_milestone_id: 1,
        };
        write_counters(&path, &c).unwrap();
        let loaded = read_counters(&path).unwrap();
        assert_eq!(c, loaded);
    }

    #[test]
    fn test_counters_backward_compat_missing_milestone_id() {
        // Old counters.json without next_milestone_id should default to 1
        let json = r#"{"next_display_id": 5, "next_comment_id": 3}"#;
        let parsed: Counters = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.next_milestone_id, 1);
    }

    #[test]
    fn test_milestone_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("milestone.json");

        let entry = MilestoneEntry {
            uuid: Uuid::new_v4(),
            display_id: 1,
            name: "v1.0".to_string(),
            description: Some("First release".to_string()),
            status: crate::models::IssueStatus::Open,
            created_at: Utc::now(),
            closed_at: None,
        };

        write_milestone_file(&path, &entry).unwrap();
        let loaded = read_milestone_file(&path).unwrap();
        assert_eq!(entry.uuid, loaded.uuid);
        assert_eq!(entry.name, loaded.name);
        assert_eq!(entry.description, loaded.description);
    }

    #[test]
    fn test_read_all_milestone_files() {
        let dir = tempfile::tempdir().unwrap();
        let ms_dir = dir.path().join("milestones");
        std::fs::create_dir_all(&ms_dir).unwrap();

        for i in 0..3 {
            let entry = MilestoneEntry {
                uuid: Uuid::new_v4(),
                display_id: i + 1,
                name: format!("v{}.0", i + 1),
                description: None,
                status: crate::models::IssueStatus::Open,
                created_at: Utc::now(),
                closed_at: None,
            };
            write_milestone_file(&ms_dir.join(format!("{}.json", entry.uuid)), &entry).unwrap();
        }

        let loaded = read_all_milestone_files(&ms_dir).unwrap();
        assert_eq!(loaded.len(), 3);
    }

    #[test]
    fn test_read_all_milestone_files_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ms_dir = dir.path().join("milestones");
        // Dir doesn't exist
        let loaded = read_all_milestone_files(&ms_dir).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_comment_file_roundtrip() {
        let comment = CommentFile {
            uuid: Uuid::new_v4(),
            issue_uuid: Uuid::new_v4(),
            author: "worker-1".to_string(),
            content: "This is a comment".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: Some("redirect".to_string()),
            intervention_context: None,
            driver_key_fingerprint: Some("SHA256:abc123".to_string()),
            signed_by: Some("SHA256:def456".to_string()),
            signature: Some("base64sig==".to_string()),
        };

        let json = serde_json::to_string_pretty(&comment).unwrap();
        let parsed: CommentFile = serde_json::from_str(&json).unwrap();
        assert_eq!(comment.uuid, parsed.uuid);
        assert_eq!(comment.issue_uuid, parsed.issue_uuid);
        assert_eq!(comment.author, parsed.author);
        assert_eq!(comment.content, parsed.content);
        assert_eq!(comment.kind, parsed.kind);
        assert_eq!(comment.trigger_type, parsed.trigger_type);
        assert_eq!(comment.intervention_context, parsed.intervention_context);
        assert_eq!(
            comment.driver_key_fingerprint,
            parsed.driver_key_fingerprint
        );
        assert_eq!(comment.signed_by, parsed.signed_by);
        assert_eq!(comment.signature, parsed.signature);
    }

    #[test]
    fn test_comment_file_optional_fields_omitted() {
        let comment = CommentFile {
            uuid: Uuid::new_v4(),
            issue_uuid: Uuid::new_v4(),
            author: "worker-1".to_string(),
            content: "Minimal comment".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };

        let json = serde_json::to_string(&comment).unwrap();
        // None fields should be omitted from the JSON
        assert!(!json.contains("trigger_type"));
        assert!(!json.contains("intervention_context"));
        assert!(!json.contains("driver_key_fingerprint"));
        assert!(!json.contains("signed_by"));
        assert!(!json.contains("signature"));
    }

    #[test]
    fn test_lock_file_v2_roundtrip() {
        let lock = LockFileV2 {
            issue_id: 42,
            agent_id: "worker-1".to_string(),
            branch: Some("feature/hub-layout".to_string()),
            claimed_at: Utc::now(),
            signed_by: Some("SHA256:abc123".to_string()),
        };

        let json = serde_json::to_string_pretty(&lock).unwrap();
        let parsed: LockFileV2 = serde_json::from_str(&json).unwrap();
        assert_eq!(lock.issue_id, parsed.issue_id);
        assert_eq!(lock.agent_id, parsed.agent_id);
        assert_eq!(lock.branch, parsed.branch);
        assert_eq!(lock.signed_by, parsed.signed_by);
    }

    #[test]
    fn test_lock_file_v2_optional_fields_omitted() {
        let lock = LockFileV2 {
            issue_id: 1,
            agent_id: "worker-2".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: None,
        };

        let json = serde_json::to_string(&lock).unwrap();
        assert!(!json.contains("branch"));
        assert!(!json.contains("signed_by"));
    }

    #[test]
    fn test_layout_version_roundtrip() {
        let version = LayoutVersion { layout_version: 2 };

        let json = serde_json::to_string_pretty(&version).unwrap();
        let parsed: LayoutVersion = serde_json::from_str(&json).unwrap();
        assert_eq!(version.layout_version, parsed.layout_version);
    }

    #[test]
    fn test_current_layout_version_constant() {
        assert_eq!(CURRENT_LAYOUT_VERSION, 2);
    }

    #[test]
    fn test_read_write_comment_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments").join("test-comment.json");

        let comment = CommentFile {
            uuid: Uuid::new_v4(),
            issue_uuid: Uuid::new_v4(),
            author: "worker-1".to_string(),
            content: "Test comment content".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };

        write_comment_file(&path, &comment).unwrap();
        let loaded = read_comment_file(&path).unwrap();
        assert_eq!(comment.uuid, loaded.uuid);
        assert_eq!(comment.issue_uuid, loaded.issue_uuid);
        assert_eq!(comment.content, loaded.content);
    }

    #[test]
    fn test_read_comment_files_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = dir.path().join("comments");
        std::fs::create_dir_all(&comments_dir).unwrap();

        let now = Utc::now();

        // Create comments with different timestamps, authors, and UUIDs
        // to verify the sort order: (created_at, author, uuid)
        let uuid_a = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let uuid_b = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let uuid_c = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();

        let issue_uuid = Uuid::new_v4();

        // Comment 3: newest timestamp
        let c3 = CommentFile {
            uuid: uuid_c,
            issue_uuid,
            author: "alice".to_string(),
            content: "Third".to_string(),
            created_at: now + chrono::Duration::seconds(2),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };

        // Comment 1: oldest timestamp
        let c1 = CommentFile {
            uuid: uuid_a,
            issue_uuid,
            author: "alice".to_string(),
            content: "First".to_string(),
            created_at: now,
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };

        // Comment 2: same timestamp as c1, different author (bob > alice)
        let c2 = CommentFile {
            uuid: uuid_b,
            issue_uuid,
            author: "bob".to_string(),
            content: "Second".to_string(),
            created_at: now,
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };

        // Write in non-sorted order
        write_comment_file(&comments_dir.join(format!("{}.json", c3.uuid)), &c3).unwrap();
        write_comment_file(&comments_dir.join(format!("{}.json", c1.uuid)), &c1).unwrap();
        write_comment_file(&comments_dir.join(format!("{}.json", c2.uuid)), &c2).unwrap();

        let loaded = read_comment_files(&comments_dir).unwrap();
        assert_eq!(loaded.len(), 3);
        // Sorted by (created_at, author, uuid): c1 (oldest, alice), c2 (oldest, bob), c3 (newest)
        assert_eq!(loaded[0].content, "First");
        assert_eq!(loaded[1].content, "Second");
        assert_eq!(loaded[2].content, "Third");
    }

    #[test]
    fn test_read_comment_files_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = dir.path().join("comments");
        // Dir doesn't exist
        let loaded = read_comment_files(&comments_dir).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_read_comment_files_skips_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = dir.path().join("comments");
        std::fs::create_dir_all(&comments_dir).unwrap();

        // Write a valid comment file
        let comment = CommentFile {
            uuid: Uuid::new_v4(),
            issue_uuid: Uuid::new_v4(),
            author: "worker-1".to_string(),
            content: "Valid".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };
        write_comment_file(&comments_dir.join("valid.json"), &comment).unwrap();

        // Write a malformed file
        std::fs::write(comments_dir.join("bad.json"), "not valid json").unwrap();

        let loaded = read_comment_files(&comments_dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].content, "Valid");
    }

    #[test]
    fn test_read_layout_version_missing() {
        let dir = tempfile::tempdir().unwrap();
        let meta_dir = dir.path().join("meta");
        // meta dir doesn't exist, should return 1
        let version = read_layout_version(&meta_dir).unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn test_write_read_layout_version() {
        let dir = tempfile::tempdir().unwrap();
        let meta_dir = dir.path().join("meta");

        write_layout_version(&meta_dir, CURRENT_LAYOUT_VERSION).unwrap();
        let version = read_layout_version(&meta_dir).unwrap();
        assert_eq!(version, CURRENT_LAYOUT_VERSION);
    }

    #[test]
    fn test_write_read_layout_version_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let meta_dir = dir.path().join("meta");

        // Write version 2
        write_layout_version(&meta_dir, 2).unwrap();
        assert_eq!(read_layout_version(&meta_dir).unwrap(), 2);

        // Overwrite with version 3
        write_layout_version(&meta_dir, 3).unwrap();
        assert_eq!(read_layout_version(&meta_dir).unwrap(), 3);
    }

    #[test]
    fn test_default_comment_kind() {
        assert_eq!(default_comment_kind(), "note");
    }

    #[test]
    fn test_validate_comment_kind_valid() {
        for kind in KNOWN_COMMENT_KINDS {
            assert!(
                validate_comment_kind(kind),
                "expected {kind:?} to be a valid comment kind"
            );
        }
    }

    #[test]
    fn test_validate_comment_kind_invalid() {
        assert!(!validate_comment_kind("bogus"));
        assert!(!validate_comment_kind(""));
        assert!(!validate_comment_kind("NOTE")); // case-sensitive
    }

    #[test]
    fn test_validate_trigger_type_valid() {
        for trigger in KNOWN_TRIGGER_TYPES {
            assert!(
                validate_trigger_type(trigger),
                "expected {trigger:?} to be a valid trigger type"
            );
        }
    }

    #[test]
    fn test_validate_trigger_type_invalid() {
        assert!(!validate_trigger_type("bogus"));
        assert!(!validate_trigger_type(""));
        assert!(!validate_trigger_type("REDIRECT")); // case-sensitive
    }

    #[test]
    fn test_read_all_issue_files_v2_layout() {
        let dir = tempfile::tempdir().unwrap();
        let issues_dir = dir.path().join("issues");

        // Create two issues using the V2 directory layout: issues/{uuid}/issue.json
        for i in 0..2 {
            let issue = IssueFile {
                uuid: Uuid::new_v4(),
                display_id: Some(i + 1),
                title: format!("V2 Issue {}", i + 1),
                description: None,
                status: crate::models::IssueStatus::Open,
                priority: crate::models::Priority::Medium,
                parent_uuid: None,
                created_by: "test".to_string(),
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
            };
            let subdir = issues_dir.join(issue.uuid.to_string());
            std::fs::create_dir_all(&subdir).unwrap();
            write_issue_file(&subdir.join("issue.json"), &issue).unwrap();
        }

        let loaded = read_all_issue_files(&issues_dir).unwrap();
        assert_eq!(loaded.len(), 2);
        // Both titles should start with "V2 Issue"
        for issue in &loaded {
            assert!(issue.title.starts_with("V2 Issue"));
        }
    }

    #[test]
    fn test_read_all_issue_files_v2_malformed_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let issues_dir = dir.path().join("issues");

        // Create one valid V2 issue
        let valid_uuid = Uuid::new_v4();
        let valid_issue = IssueFile {
            uuid: valid_uuid,
            display_id: Some(1),
            title: "Valid V2".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "test".to_string(),
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
        };
        let valid_subdir = issues_dir.join(valid_uuid.to_string());
        std::fs::create_dir_all(&valid_subdir).unwrap();
        write_issue_file(&valid_subdir.join("issue.json"), &valid_issue).unwrap();

        // Create a V2 directory with malformed issue.json
        let bad_subdir = issues_dir.join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&bad_subdir).unwrap();
        std::fs::write(bad_subdir.join("issue.json"), "not valid json").unwrap();

        let loaded = read_all_issue_files(&issues_dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].title, "Valid V2");
    }

    #[test]
    fn test_read_all_milestone_files_skips_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let ms_dir = dir.path().join("milestones");
        std::fs::create_dir_all(&ms_dir).unwrap();

        // Write a valid milestone file
        let entry = MilestoneEntry {
            uuid: Uuid::new_v4(),
            display_id: 1,
            name: "v1.0".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            created_at: Utc::now(),
            closed_at: None,
        };
        write_milestone_file(&ms_dir.join(format!("{}.json", entry.uuid)), &entry).unwrap();

        // Write a malformed milestone file
        std::fs::write(ms_dir.join("bad.json"), "not valid json").unwrap();

        let loaded = read_all_milestone_files(&ms_dir).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "v1.0");
    }

    // ── Scheduling backward-compat (GH #361 AC-11) ──

    #[test]
    fn test_issuefile_deserializes_without_scheduling_fields() {
        // An existing issue file written before scheduling existed must load
        // cleanly with both new fields defaulting to None. This is the same
        // shape produced by any crosslink version prior to #361.
        let legacy = serde_json::json!({
            "uuid": "00000000-0000-0000-0000-000000000001",
            "display_id": 42,
            "title": "legacy issue",
            "status": "open",
            "priority": "medium",
            "created_by": "legacy-agent",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z"
        });
        let file: IssueFile = serde_json::from_value(legacy).unwrap();
        assert_eq!(file.title, "legacy issue");
        assert!(file.scheduled_at.is_none());
        assert!(file.due_at.is_none());
    }

    #[test]
    fn test_issuefile_scheduling_fields_roundtrip() {
        let now = chrono::Utc::now();
        let file = IssueFile {
            uuid: uuid::Uuid::new_v4(),
            display_id: Some(1),
            title: "t".into(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "agent".into(),
            created_at: now,
            updated_at: now,
            closed_at: None,
            scheduled_at: Some(now),
            due_at: Some(now + chrono::Duration::days(7)),
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };
        let json = serde_json::to_string(&file).unwrap();
        let parsed: IssueFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.scheduled_at, file.scheduled_at);
        assert_eq!(parsed.due_at, file.due_at);
    }

    #[test]
    fn test_issuefile_scheduling_none_omitted_from_json() {
        // skip_serializing_if = Option::is_none keeps JSON clean for the
        // common case where no scheduling is set.
        let now = chrono::Utc::now();
        let file = IssueFile {
            uuid: uuid::Uuid::new_v4(),
            display_id: Some(1),
            title: "t".into(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "agent".into(),
            created_at: now,
            updated_at: now,
            closed_at: None,
            scheduled_at: None,
            due_at: None,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };
        let json = serde_json::to_string(&file).unwrap();
        assert!(
            !json.contains("scheduled_at"),
            "scheduled_at=None should not appear in JSON: {json}"
        );
        assert!(
            !json.contains("due_at"),
            "due_at=None should not appear in JSON: {json}"
        );
    }
}
