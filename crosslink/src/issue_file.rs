//! JSON schema for issue files on the coordination branch.
//!
//! Each issue is stored as `issues/{uuid}.json` on the `crosslink/hub` branch.
//! This module defines the serde types for reading and writing those files,
//! plus the shared `counters.json` and `milestones.json` schemas.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A single issue as stored in `issues/{uuid}.json` on the coordination branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IssueFile {
    pub uuid: Uuid,
    /// Stable display ID assigned from the shared counter on first push.
    /// `None` for locally-created issues that haven't been pushed yet.
    pub display_id: Option<i64>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<Uuid>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<CommentEntry>,
    /// UUIDs of issues that block this one (single-direction storage).
    /// The reverse direction ("blocking") is derived during SQLite hydration.
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommentEntry {
    pub id: i64,
    pub author: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

/// An inline time-tracking entry within an issue file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimeEntry {
    pub id: i64,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<i64>,
}

/// Shared counter file at `meta/counters.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Counters {
    pub next_display_id: i64,
    pub next_comment_id: i64,
    #[serde(default = "default_one")]
    pub next_milestone_id: i64,
}

fn default_one() -> i64 {
    1
}

impl Default for Counters {
    fn default() -> Self {
        Counters {
            next_display_id: 1,
            next_comment_id: 1,
            next_milestone_id: 1,
        }
    }
}

/// Milestone registry at `meta/milestones.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MilestonesFile {
    pub milestones: std::collections::HashMap<Uuid, MilestoneEntry>,
}

/// A single milestone entry in the milestones file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MilestoneEntry {
    pub uuid: Uuid,
    pub display_id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
}

/// Read an issue file from disk.
pub fn read_issue_file(path: &std::path::Path) -> anyhow::Result<IssueFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read issue file: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse issue file: {}", path.display()))
}

/// Write an issue file to disk (pretty-printed JSON).
pub fn write_issue_file(path: &std::path::Path, issue: &IssueFile) -> anyhow::Result<()> {
    let content = serde_json::to_string_pretty(issue)?;
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write issue file: {}", path.display()))
}

/// Read all issue files from a directory.
pub fn read_all_issue_files(issues_dir: &std::path::Path) -> anyhow::Result<Vec<IssueFile>> {
    let mut issues = Vec::new();
    if !issues_dir.exists() {
        return Ok(issues);
    }
    for entry in std::fs::read_dir(issues_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            match read_issue_file(&path) {
                Ok(issue) => issues.push(issue),
                Err(e) => {
                    eprintln!(
                        "Warning: skipping malformed issue file {}: {e}",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(issues)
}

/// Read counters from `meta/counters.json`, returning defaults if missing.
pub fn read_counters(path: &std::path::Path) -> anyhow::Result<Counters> {
    if !path.exists() {
        return Ok(Counters::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

/// Write counters to `meta/counters.json`.
pub fn write_counters(path: &std::path::Path, counters: &Counters) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(counters)?;
    std::fs::write(path, content)?;
    Ok(())
}

/// Read milestones from `meta/milestones.json`, returning defaults if missing.
pub fn read_milestones_file(path: &std::path::Path) -> anyhow::Result<MilestonesFile> {
    if !path.exists() {
        return Ok(MilestonesFile::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

/// Read a single milestone file from disk.
pub fn read_milestone_file(path: &std::path::Path) -> anyhow::Result<MilestoneEntry> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read milestone file: {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse milestone file: {}", path.display()))
}

/// Write a single milestone file to disk (pretty-printed JSON).
pub fn write_milestone_file(path: &std::path::Path, entry: &MilestoneEntry) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(entry)?;
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write milestone file: {}", path.display()))
}

/// Read all milestone files from a directory.
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
                    eprintln!(
                        "Warning: skipping malformed milestone file {}: {e}",
                        path.display()
                    );
                }
            }
        }
    }
    Ok(entries)
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
            status: "open".to_string(),
            priority: "critical".to_string(),
            parent_uuid: None,
            created_by: "worker-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            labels: vec!["bug".to_string(), "auth".to_string()],
            comments: vec![CommentEntry {
                id: 1,
                author: "worker-1".to_string(),
                content: "Reproduced on staging".to_string(),
                created_at: Utc::now(),
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
                status: "open".to_string(),
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
            status: "open".to_string(),
            priority: "medium".to_string(),
            parent_uuid: None,
            created_by: "test".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
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
                status: "open".to_string(),
                priority: "medium".to_string(),
                parent_uuid: None,
                created_by: "test".to_string(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                closed_at: None,
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
            status: "open".to_string(),
            priority: "medium".to_string(),
            parent_uuid: None,
            created_by: "test".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
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
            status: "open".to_string(),
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
                status: "open".to_string(),
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
}
