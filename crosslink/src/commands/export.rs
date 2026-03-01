use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use uuid::Uuid;

use crate::db::Database;
use crate::issue_file::{CommentEntry, IssueFile, TimeEntry};
use crate::models::Issue;
use crate::utils::format_issue_id;

// Legacy export types — kept for backward compatibility with `import` command.
// NOTE: The import command still reads the old ExportData envelope format.
// If you need round-trip import/export, the import command needs updating too.
#[derive(Serialize, Deserialize)]
pub struct ExportedIssue {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: String,
    pub parent_id: Option<i64>,
    pub labels: Vec<String>,
    pub comments: Vec<ExportedComment>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ExportedComment {
    pub content: String,
    pub created_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct ExportData {
    pub version: i32,
    pub exported_at: String,
    pub issues: Vec<ExportedIssue>,
}

/// Build a pre-computed map of issue display_id -> UUID for consistent cross-references.
/// Issues without a stored UUID get a freshly generated one.
fn build_uuid_map(db: &Database, issues: &[Issue]) -> Result<HashMap<i64, Uuid>> {
    let mut map = HashMap::new();
    for issue in issues {
        let (uuid_str, _) = db.get_issue_export_metadata(issue.id)?;
        let uuid = match uuid_str {
            Some(s) => Uuid::parse_str(&s).unwrap_or_else(|_| Uuid::new_v4()),
            None => Uuid::new_v4(),
        };
        map.insert(issue.id, uuid);
    }
    Ok(map)
}

/// Look up a UUID from the map, falling back to a DB query or a fresh UUID.
fn resolve_uuid(db: &Database, uuid_map: &HashMap<i64, Uuid>, id: i64) -> Uuid {
    if let Some(&uuid) = uuid_map.get(&id) {
        return uuid;
    }
    // Issue not in the exported set — try the DB
    db.get_issue_uuid_by_id(id)
        .ok()
        .and_then(|s| Uuid::parse_str(&s).ok())
        .unwrap_or_else(Uuid::new_v4)
}

fn build_issue_file(
    db: &Database,
    issue: &Issue,
    uuid_map: &HashMap<i64, Uuid>,
) -> Result<IssueFile> {
    let uuid = *uuid_map
        .get(&issue.id)
        .ok_or_else(|| anyhow::anyhow!("issue {} missing from uuid_map", issue.id))?;

    let (_, created_by) = db.get_issue_export_metadata(issue.id)?;

    let parent_uuid = issue.parent_id.map(|pid| resolve_uuid(db, uuid_map, pid));

    let labels = db.get_labels(issue.id)?;

    let comments_raw = db.get_comments_with_author(issue.id)?;
    let comments: Vec<CommentEntry> = comments_raw
        .into_iter()
        .map(
            |(
                id,
                author,
                content,
                created_at,
                kind,
                trigger_type,
                intervention_context,
                driver_key_fingerprint,
            )| {
                CommentEntry {
                    id,
                    author: author.unwrap_or_else(|| "unknown".to_string()),
                    content,
                    created_at,
                    kind,
                    trigger_type,
                    intervention_context,
                    driver_key_fingerprint,
                    signed_by: None,
                    signature: None,
                }
            },
        )
        .collect();

    let blocker_ids = db.get_blockers(issue.id)?;
    let blockers: Vec<Uuid> = blocker_ids
        .iter()
        .map(|&bid| resolve_uuid(db, uuid_map, bid))
        .collect();

    let related_ids = db.get_related_issue_ids(issue.id)?;
    let related: Vec<Uuid> = related_ids
        .iter()
        .map(|&rid| resolve_uuid(db, uuid_map, rid))
        .collect();

    let milestone_uuid = db
        .get_milestone_uuid_for_issue(issue.id)?
        .and_then(|s| Uuid::parse_str(&s).ok());

    let time_entries_raw = db.get_time_entries_for_issue(issue.id)?;
    let time_entries: Vec<TimeEntry> = time_entries_raw
        .into_iter()
        .map(|(id, started_at, ended_at, duration_seconds)| TimeEntry {
            id,
            started_at,
            ended_at,
            duration_seconds,
        })
        .collect();

    Ok(IssueFile {
        uuid,
        display_id: Some(issue.id),
        title: issue.title.clone(),
        description: issue.description.clone(),
        status: issue.status.clone(),
        priority: issue.priority.clone(),
        parent_uuid,
        created_by: created_by.unwrap_or_else(|| "unknown".to_string()),
        created_at: issue.created_at,
        updated_at: issue.updated_at,
        closed_at: issue.closed_at,
        labels,
        comments,
        blockers,
        related,
        milestone_uuid,
        time_entries,
    })
}

pub fn run_json(db: &Database, output_path: Option<&str>) -> Result<()> {
    let issues = db.list_issues(Some("all"), None, None)?;
    let uuid_map = build_uuid_map(db, &issues)?;

    let issue_files: Vec<IssueFile> = issues
        .iter()
        .map(|i| build_issue_file(db, i, &uuid_map))
        .collect::<Result<Vec<_>>>()?;

    let json = serde_json::to_string_pretty(&issue_files)?;

    match output_path {
        Some(path) => {
            fs::write(path, json).context("Failed to write export file")?;
            eprintln!("Exported {} issues to {}", issue_files.len(), path);
        }
        None => {
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "{}", json)?;
        }
    }
    Ok(())
}

pub fn run_markdown(db: &Database, output_path: Option<&str>) -> Result<()> {
    let issues = db.list_issues(Some("all"), None, None)?;
    let mut md = String::new();

    md.push_str("# Crosslink Issues Export\n\n");
    md.push_str(&format!(
        "Exported: {}\n\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
    ));

    // Group by status
    let open: Vec<_> = issues.iter().filter(|i| i.status == "open").collect();
    let closed: Vec<_> = issues.iter().filter(|i| i.status == "closed").collect();
    let archived: Vec<_> = issues.iter().filter(|i| i.status == "archived").collect();

    if !open.is_empty() {
        md.push_str("## Open Issues\n\n");
        for issue in &open {
            write_issue_md(&mut md, db, issue)?;
        }
    }

    if !closed.is_empty() {
        md.push_str("## Closed Issues\n\n");
        for issue in &closed {
            write_issue_md(&mut md, db, issue)?;
        }
    }

    if !archived.is_empty() {
        md.push_str("## Archived Issues\n\n");
        for issue in &archived {
            write_issue_md(&mut md, db, issue)?;
        }
    }

    match output_path {
        Some(path) => {
            fs::write(path, md).context("Failed to write export file")?;
            eprintln!("Exported {} issues to {}", issues.len(), path);
        }
        None => {
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "{}", md)?;
        }
    }
    Ok(())
}

fn write_issue_md(md: &mut String, db: &Database, issue: &Issue) -> Result<()> {
    let checkbox = if issue.status == "closed" {
        "[x]"
    } else {
        "[ ]"
    };

    md.push_str(&format!(
        "### {} {}: {}\n\n",
        checkbox,
        format_issue_id(issue.id),
        issue.title
    ));
    md.push_str(&format!("- **Priority:** {}\n", issue.priority));
    md.push_str(&format!("- **Status:** {}\n", issue.status));

    if let Some(parent_id) = issue.parent_id {
        md.push_str(&format!("- **Parent:** {}\n", format_issue_id(parent_id)));
    }

    let labels = db.get_labels(issue.id)?;
    if !labels.is_empty() {
        md.push_str(&format!("- **Labels:** {}\n", labels.join(", ")));
    }

    md.push_str(&format!(
        "- **Created:** {}\n",
        issue.created_at.format("%Y-%m-%d")
    ));

    if let Some(ref desc) = issue.description {
        if !desc.is_empty() {
            md.push_str(&format!("\n{}\n", desc));
        }
    }

    let comments = db.get_comments(issue.id)?;
    if !comments.is_empty() {
        md.push_str("\n**Comments:**\n");
        for comment in comments {
            md.push_str(&format!(
                "- [{}] {}\n",
                comment.created_at.format("%Y-%m-%d %H:%M"),
                comment.content
            ));
        }
    }

    md.push_str("\n---\n\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_file::IssueFile;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_export_issue_basic() {
        let (db, dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();
        let output_path = dir.path().join("export.json");
        run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].title, "Test issue");
        assert_eq!(issues[0].priority, "medium");
        assert_eq!(issues[0].status, "open");
        assert_eq!(issues[0].display_id, Some(id));
    }

    #[test]
    fn test_export_issue_with_labels() {
        let (db, dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();
        db.add_label(id, "bug").unwrap();
        db.add_label(id, "urgent").unwrap();
        let output_path = dir.path().join("export.json");
        run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        assert_eq!(issues[0].labels.len(), 2);
    }

    #[test]
    fn test_export_issue_with_comments() {
        let (db, dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();
        db.add_comment(id, "First comment", "note").unwrap();
        db.add_comment(id, "Second comment", "note").unwrap();
        let output_path = dir.path().join("export.json");
        run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        assert_eq!(issues[0].comments.len(), 2);
        assert_eq!(issues[0].comments[0].content, "First comment");
    }

    #[test]
    fn test_export_closed_issue() {
        let (db, dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();
        db.close_issue(id).unwrap();
        let output_path = dir.path().join("export.json");
        run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        assert_eq!(issues[0].status, "closed");
        assert!(issues[0].closed_at.is_some());
    }

    #[test]
    fn test_run_json_to_file() {
        let (db, dir) = setup_test_db();
        db.create_issue("Issue 1", None, "high").unwrap();
        db.create_issue("Issue 2", Some("Description"), "low")
            .unwrap();
        let output_path = dir.path().join("export.json");
        let result = run_json(&db, Some(output_path.to_str().unwrap()));
        assert!(result.is_ok());
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        assert_eq!(issues.len(), 2);
    }

    #[test]
    fn test_run_json_empty_database() {
        let (db, dir) = setup_test_db();
        let output_path = dir.path().join("export.json");
        let result = run_json(&db, Some(output_path.to_str().unwrap()));
        assert!(result.is_ok());
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        assert_eq!(issues.len(), 0);
    }

    #[test]
    fn test_run_markdown_to_file() {
        let (db, dir) = setup_test_db();
        db.create_issue("Issue 1", None, "high").unwrap();
        let output_path = dir.path().join("export.md");
        let result = run_markdown(&db, Some(output_path.to_str().unwrap()));
        assert!(result.is_ok());
        let content = fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("# Crosslink Issues Export"));
    }

    #[test]
    fn test_markdown_groups_by_status() {
        let (db, dir) = setup_test_db();
        db.create_issue("Open issue", None, "medium").unwrap();
        let closed_id = db.create_issue("Closed issue", None, "medium").unwrap();
        db.close_issue(closed_id).unwrap();
        let output_path = dir.path().join("export.md");
        run_markdown(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("## Open Issues"));
        assert!(content.contains("## Closed Issues"));
    }

    #[test]
    fn test_export_unicode_content() {
        let (db, dir) = setup_test_db();
        let id = db
            .create_issue("Test 🐛", Some("Description αβγ"), "medium")
            .unwrap();
        db.add_label(id, "バグ").unwrap();
        let output_path = dir.path().join("export.json");
        run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        assert_eq!(issues[0].title, "Test 🐛");
    }

    #[test]
    fn test_export_issue_file_roundtrip() {
        let (db, dir) = setup_test_db();
        let id = db.create_issue("Test", Some("Desc"), "medium").unwrap();
        db.add_label(id, "bug").unwrap();
        db.add_comment(id, "Comment", "note").unwrap();
        let output_path = dir.path().join("export.json");
        run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].labels, vec!["bug".to_string()]);
        assert_eq!(issues[0].comments.len(), 1);
        // Verify it can be re-serialized
        let re_json = serde_json::to_string_pretty(&issues).unwrap();
        let re_parsed: Vec<IssueFile> = serde_json::from_str(&re_json).unwrap();
        assert_eq!(re_parsed[0].uuid, issues[0].uuid);
    }

    #[test]
    fn test_export_with_blockers() {
        let (db, dir) = setup_test_db();
        let id1 = db.create_issue("Blocker", None, "high").unwrap();
        let id2 = db.create_issue("Blocked", None, "medium").unwrap();
        db.add_dependency(id2, id1).unwrap();
        let output_path = dir.path().join("export.json");
        run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        let blocked = issues.iter().find(|i| i.title == "Blocked").unwrap();
        let blocker = issues.iter().find(|i| i.title == "Blocker").unwrap();
        assert_eq!(blocked.blockers.len(), 1);
        assert_eq!(blocked.blockers[0], blocker.uuid);
    }

    #[test]
    fn test_export_with_parent() {
        let (db, dir) = setup_test_db();
        let parent_id = db.create_issue("Parent", None, "high").unwrap();
        db.create_subissue(parent_id, "Child", None, "medium")
            .unwrap();
        let output_path = dir.path().join("export.json");
        run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
        let content = fs::read_to_string(&output_path).unwrap();
        let issues: Vec<IssueFile> = serde_json::from_str(&content).unwrap();
        let parent = issues.iter().find(|i| i.title == "Parent").unwrap();
        let child = issues.iter().find(|i| i.title == "Child").unwrap();
        assert!(child.parent_uuid.is_some());
        assert_eq!(child.parent_uuid.unwrap(), parent.uuid);
    }

    proptest! {
        #[test]
        fn prop_export_never_panics(title in "[a-zA-Z0-9 ]{1,50}") {
            let (db, dir) = setup_test_db();
            db.create_issue(&title, None, "medium").unwrap();
            let output_path = dir.path().join("export.json");
            let result = run_json(&db, Some(output_path.to_str().unwrap()));
            prop_assert!(result.is_ok());
        }

        #[test]
        fn prop_json_is_valid(title in "[a-zA-Z0-9 ]{1,30}") {
            let (db, dir) = setup_test_db();
            db.create_issue(&title, None, "medium").unwrap();
            let output_path = dir.path().join("export.json");
            run_json(&db, Some(output_path.to_str().unwrap())).unwrap();
            let content = fs::read_to_string(&output_path).unwrap();
            let result: Result<Vec<IssueFile>, _> = serde_json::from_str(&content);
            prop_assert!(result.is_ok());
        }
    }
}
