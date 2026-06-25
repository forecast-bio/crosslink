use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::export::{ExportData, ExportedIssue};
use crate::db::Database;
use crate::issue_file::IssueFile;
use crate::utils::format_issue_id;

/// Maximum import file size (10 MB).
const MAX_IMPORT_SIZE: u64 = 10 * 1024 * 1024;

pub fn run_json(db: &Database, input_path: &Path) -> Result<()> {
    let metadata = fs::metadata(input_path).context("Failed to read import file metadata")?;
    if metadata.len() > MAX_IMPORT_SIZE {
        anyhow::bail!(
            "Import file is {} bytes, exceeding the {} byte limit",
            metadata.len(),
            MAX_IMPORT_SIZE
        );
    }
    let content = fs::read_to_string(input_path).context("Failed to read import file")?;

    // Try new IssueFile array format first, then fall back to legacy ExportData envelope.
    if let Ok(issue_files) = serde_json::from_str::<Vec<IssueFile>>(&content) {
        return import_issue_files(db, &issue_files, input_path);
    }

    let data: ExportData = serde_json::from_str(&content).context("Failed to parse JSON")?;
    import_legacy(db, &data, input_path)
}

fn import_issue_files(db: &Database, issues: &[IssueFile], input_path: &Path) -> Result<()> {
    println!(
        "Importing {} issues from {} (IssueFile format)",
        issues.len(),
        input_path.display()
    );

    let count = db.transaction(|| {
        // Map uuid -> new display_id for parent/blocker resolution
        let mut uuid_to_new_id: HashMap<uuid::Uuid, i64> = HashMap::new();

        // First pass: create all issues without parent relationships
        for issue in issues {
            let new_id = db.create_issue(
                &issue.title,
                issue.description.as_deref(),
                issue.priority.as_str(),
            )?;

            // Add labels
            for label in &issue.labels {
                db.add_label(new_id, label)?;
            }

            // Add comments
            for comment in &issue.comments {
                db.add_comment(new_id, &comment.content, "note")?;
            }

            // Close if needed
            if issue.status == crate::models::IssueStatus::Closed {
                db.close_issue(new_id)?;
            }

            uuid_to_new_id.insert(issue.uuid, new_id);

            println!(
                "  Imported: {} -> {} {}",
                issue
                    .display_id
                    .map_or_else(|| issue.uuid.to_string(), format_issue_id),
                format_issue_id(new_id),
                issue.title
            );
        }

        // Second pass: update parent relationships
        for issue in issues {
            if let Some(parent_uuid) = issue.parent_uuid {
                if let (Some(&new_id), Some(&new_parent_id)) = (
                    uuid_to_new_id.get(&issue.uuid),
                    uuid_to_new_id.get(&parent_uuid),
                ) {
                    db.update_parent(new_id, Some(new_parent_id))?;
                }
            }
        }

        // Third pass: restore blocker dependencies
        for issue in issues {
            if let Some(&new_blocked_id) = uuid_to_new_id.get(&issue.uuid) {
                for blocker_uuid in &issue.blockers {
                    if let Some(&new_blocker_id) = uuid_to_new_id.get(blocker_uuid) {
                        // INTENTIONAL: dependency failure is non-fatal — import proceeds without the graph edge
                        let _ = db.add_dependency(new_blocked_id, new_blocker_id);
                    }
                }
            }
        }

        Ok(issues.len())
    })?;

    println!("Successfully imported {count} issues");
    Ok(())
}

fn import_legacy(db: &Database, data: &ExportData, input_path: &Path) -> Result<()> {
    println!(
        "Importing {} issues from {} (legacy format)",
        data.issues.len(),
        input_path.display()
    );

    let count = db.transaction(|| {
        let mut id_map: HashMap<i64, i64> = HashMap::new();

        for issue in &data.issues {
            let new_id = import_issue(db, issue, None)?;
            id_map.insert(issue.id, new_id);
        }

        for issue in &data.issues {
            if let Some(old_parent_id) = issue.parent_id {
                if let Some(&new_parent_id) = id_map.get(&old_parent_id) {
                    if let Some(&new_id) = id_map.get(&issue.id) {
                        db.update_parent(new_id, Some(new_parent_id))?;
                    }
                }
            }
        }

        Ok(data.issues.len())
    })?;

    println!("Successfully imported {count} issues");
    Ok(())
}

fn import_issue(db: &Database, issue: &ExportedIssue, parent_id: Option<i64>) -> Result<i64> {
    let id = if let Some(pid) = parent_id {
        db.create_subissue(
            pid,
            &issue.title,
            issue.description.as_deref(),
            issue.priority.as_str(),
        )?
    } else {
        db.create_issue(
            &issue.title,
            issue.description.as_deref(),
            issue.priority.as_str(),
        )?
    };

    // Add labels
    for label in &issue.labels {
        db.add_label(id, label)?;
    }

    // Add comments
    for comment in &issue.comments {
        db.add_comment(id, &comment.content, "note")?;
    }

    // Close if needed
    if issue.status == crate::models::IssueStatus::Closed {
        db.close_issue(id)?;
    }

    println!(
        "  Imported: #{} -> {} {}",
        issue.id,
        format_issue_id(id),
        issue.title
    );
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::super::export::{ExportData, ExportedIssue};
    use super::*;
    use chrono::Utc;
    use proptest::prelude::*;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    fn create_test_export(issues: Vec<ExportedIssue>) -> String {
        let data = ExportData {
            version: 1,
            exported_at: "2024-01-01T00:00:00Z".to_string(),
            issues,
        };
        serde_json::to_string_pretty(&data).unwrap()
    }

    fn make_issue(id: i64, title: &str, parent_id: Option<i64>, status: &str) -> ExportedIssue {
        ExportedIssue {
            id,
            title: title.to_string(),
            description: None,
            status: status.to_string(),
            priority: "medium".to_string(),
            parent_id,
            labels: vec![],
            comments: vec![],
            created_at: "2024-01-01T00:00:00Z".to_string(),
            updated_at: "2024-01-01T00:00:00Z".to_string(),
            closed_at: None,
        }
    }

    #[test]
    fn test_import_single_issue() {
        let (db, dir) = setup_test_db();
        let json = create_test_export(vec![make_issue(1, "Test issue", None, "open")]);
        let import_path = dir.path().join("import.json");
        fs::write(&import_path, json).unwrap();
        let result = run_json(&db, &import_path);
        assert!(result.is_ok());
        let issues = db.list_issues(Some("all"), None, None).unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn test_import_multiple_issues() {
        let (db, dir) = setup_test_db();
        let json = create_test_export(vec![
            make_issue(1, "Issue 1", None, "open"),
            make_issue(2, "Issue 2", None, "open"),
        ]);
        let import_path = dir.path().join("import.json");
        fs::write(&import_path, json).unwrap();
        run_json(&db, &import_path).unwrap();
        let issues = db.list_issues(Some("all"), None, None).unwrap();
        assert_eq!(issues.len(), 2);
    }

    #[test]
    fn test_import_closed_issue() {
        let (db, dir) = setup_test_db();
        let json = create_test_export(vec![make_issue(1, "Closed", None, "closed")]);
        let import_path = dir.path().join("import.json");
        fs::write(&import_path, json).unwrap();
        run_json(&db, &import_path).unwrap();
        let issues = db.list_issues(Some("closed"), None, None).unwrap();
        assert_eq!(issues.len(), 1);
    }

    #[test]
    fn test_import_with_labels() {
        let (db, dir) = setup_test_db();
        let mut issue = make_issue(1, "Labeled", None, "open");
        issue.labels = vec!["bug".to_string()];
        let json = create_test_export(vec![issue]);
        let import_path = dir.path().join("import.json");
        fs::write(&import_path, json).unwrap();
        run_json(&db, &import_path).unwrap();
        let issues = db.list_issues(Some("all"), None, None).unwrap();
        let labels = db.get_labels(issues[0].id).unwrap();
        assert!(labels.contains(&"bug".to_string()));
    }

    #[test]
    fn test_import_invalid_json() {
        let (db, dir) = setup_test_db();
        let import_path = dir.path().join("invalid.json");
        fs::write(&import_path, "not valid json").unwrap();
        let result = run_json(&db, &import_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_import_missing_file() {
        let (db, dir) = setup_test_db();
        let import_path = dir.path().join("nonexistent.json");
        let result = run_json(&db, &import_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_import_empty_issues() {
        let (db, dir) = setup_test_db();
        let json = create_test_export(vec![]);
        let import_path = dir.path().join("import.json");
        fs::write(&import_path, json).unwrap();
        let result = run_json(&db, &import_path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_import_issue_file_format() {
        let (db, dir) = setup_test_db();
        let issue = IssueFile {
            uuid: uuid::Uuid::new_v4(),
            display_id: Some(1),
            title: "New format issue".to_string(),
            description: Some("Imported from IssueFile".to_string()),
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::High,
            parent_uuid: None,
            created_by: "test".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            scheduled_at: None,
            due_at: None,
            labels: vec!["feature".to_string()],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };
        let json = serde_json::to_string_pretty(&vec![issue]).unwrap();
        let import_path = dir.path().join("import.json");
        fs::write(&import_path, &json).unwrap();
        run_json(&db, &import_path).unwrap();
        let issues = db.list_issues(Some("all"), None, None).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].title, "New format issue");
        let labels = db.get_labels(issues[0].id).unwrap();
        assert!(labels.contains(&"feature".to_string()));
    }

    proptest! {
        #[test]
        fn prop_import_never_panics(title in "[a-zA-Z0-9 ]{1,50}") {
            let (db, dir) = setup_test_db();
            let json = create_test_export(vec![make_issue(1, &title, None, "open")]);
            let import_path = dir.path().join("import.json");
            fs::write(&import_path, json).unwrap();
            let result = run_json(&db, &import_path);
            prop_assert!(result.is_ok());
        }
    }
}
