use anyhow::{bail, Result};

use crate::commands::create::validate_priority;
use crate::db::Database;
use crate::shared_writer::{DescriptionUpdate, FieldUpdate, IssueUpdate, SharedWriter};
use crate::utils::format_issue_id;

pub fn run(
    db: &Database,
    writer: Option<&SharedWriter>,
    id: i64,
    update: IssueUpdate<'_>,
) -> Result<()> {
    let scheduling_touched = !matches!(update.scheduled_at, FieldUpdate::Unchanged)
        || !matches!(update.due_at, FieldUpdate::Unchanged);
    let metadata_touched = update.title.is_some()
        || !matches!(update.description, DescriptionUpdate::Unchanged)
        || update.status.is_some()
        || update.priority.is_some();
    if !metadata_touched && !scheduling_touched {
        bail!(
            "Nothing to update. Use --title, --description, --priority, \
             --scheduled/--no-scheduled, or --due/--no-due"
        );
    }

    if let Some(p) = update.priority {
        if !validate_priority(p) {
            bail!("Invalid priority '{p}'. Must be one of: low, medium, high, critical");
        }
    }

    if let Some(w) = writer {
        w.update_issue(db, id, update)?;
        println!("Updated issue {}", format_issue_id(id));
    } else {
        if scheduling_touched {
            bail!(
                "Updating scheduling dates requires the shared-writer path. \
                 Run `crosslink agent init <id>` first to enable it."
            );
        }
        // Direct-DB fallback: only the four metadata fields flow through
        // Database::update_issue. Description-clear (`--no-description`) is
        // not supported here because `Database::update_issue` takes
        // `Option<&str>` without the three-valued distinction; the CLI
        // never exercises that combination in the non-writer path.
        let desc_for_db = match update.description {
            DescriptionUpdate::Set(s) => Some(s),
            _ => None,
        };
        if db.update_issue(id, update.title, desc_for_db, update.priority)? {
            println!("Updated issue {}", format_issue_id(id));
        } else {
            bail!("Issue {} not found", format_issue_id(id));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    // ==================== Unit Tests ====================

    #[test]
    fn test_update_title() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Original title", None, "medium").unwrap();

        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                title: Some("New title"),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.title, "New title");
    }

    #[test]
    fn test_update_description() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();

        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                description: DescriptionUpdate::Set("New description"),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.description, Some("New description".to_string()));
    }

    #[test]
    fn test_update_priority() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();

        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                priority: Some("critical"),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.priority, "critical");
    }

    #[test]
    fn test_update_all_fields() {
        let (db, _dir) = setup_test_db();
        let issue_id = db
            .create_issue("Original", Some("Old desc"), "low")
            .unwrap();

        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                title: Some("New title"),
                description: DescriptionUpdate::Set("New description"),
                priority: Some("high"),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.title, "New title");
        assert_eq!(issue.description, Some("New description".to_string()));
        assert_eq!(issue.priority, "high");
    }

    #[test]
    fn test_update_nothing_fails() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();

        let result = run(&db, None, issue_id, IssueUpdate::default());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Nothing to update"));
    }

    #[test]
    fn test_update_nonexistent_issue() {
        let (db, _dir) = setup_test_db();

        let result = run(
            &db,
            None,
            99999,
            IssueUpdate {
                title: Some("New title"),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_update_invalid_priority() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();

        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                priority: Some("urgent"),
                ..Default::default()
            },
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid priority"));
    }

    #[test]
    fn test_update_preserves_unchanged_fields() {
        let (db, _dir) = setup_test_db();
        let issue_id = db
            .create_issue("Original title", Some("Original desc"), "high")
            .unwrap();

        // Only update title
        run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                title: Some("New title"),
                ..Default::default()
            },
        )
        .unwrap();

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.title, "New title");
        assert_eq!(issue.description, Some("Original desc".to_string()));
        assert_eq!(issue.priority, "high");
    }

    #[test]
    fn test_update_unicode_title() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Original", None, "medium").unwrap();

        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                title: Some("新しいタイトル 🎉"),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.title, "新しいタイトル 🎉");
    }

    #[test]
    fn test_update_empty_description() {
        let (db, _dir) = setup_test_db();
        let issue_id = db
            .create_issue("Test", Some("Has description"), "medium")
            .unwrap();

        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                description: DescriptionUpdate::Set(""),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.description, Some(String::new()));
    }

    #[test]
    fn test_update_sql_injection() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Original", None, "medium").unwrap();

        let malicious = "'; DROP TABLE issues; --";
        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                title: Some(malicious),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.title, malicious);

        // Verify database is intact
        let issues = db.list_issues(None, None, None).unwrap();
        assert!(!issues.is_empty());
    }

    #[test]
    fn test_update_closed_issue() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();
        db.close_issue(issue_id).unwrap();

        let result = run(
            &db,
            None,
            issue_id,
            IssueUpdate {
                title: Some("Updated closed issue"),
                ..Default::default()
            },
        );
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.title, "Updated closed issue");
        assert_eq!(issue.status, "closed"); // Status should remain closed
    }

    // ==================== Property-Based Tests ====================

    proptest! {
        #[test]
        fn prop_update_title_roundtrip(
            original in "[a-zA-Z0-9 ]{1,30}",
            new_title in "[a-zA-Z0-9 ]{1,30}"
        ) {
            let (db, _dir) = setup_test_db();
            let issue_id = db.create_issue(&original, None, "medium").unwrap();

            run(&db, None, issue_id, IssueUpdate { title: Some(&new_title), ..Default::default() }).unwrap();

            let issue = db.get_issue(issue_id).unwrap().unwrap();
            prop_assert_eq!(issue.title, new_title);
        }

        #[test]
        fn prop_update_priority_valid(priority in "low|medium|high|critical") {
            let (db, _dir) = setup_test_db();
            let issue_id = db.create_issue("Test", None, "medium").unwrap();

            let result = run(&db, None, issue_id, IssueUpdate { priority: Some(&priority), ..Default::default() });
            prop_assert!(result.is_ok());

            let issue = db.get_issue(issue_id).unwrap().unwrap();
            prop_assert_eq!(issue.priority, priority);
        }

        #[test]
        fn prop_update_priority_invalid(
            priority in "[a-zA-Z]{1,10}"
                .prop_filter("Exclude valid priorities", |s| {
                    !["low", "medium", "high", "critical"].contains(&s.as_str())
                })
        ) {
            let (db, _dir) = setup_test_db();
            let issue_id = db.create_issue("Test", None, "medium").unwrap();

            let result = run(&db, None, issue_id, IssueUpdate { priority: Some(&priority), ..Default::default() });
            prop_assert!(result.is_err());
        }

        #[test]
        fn prop_nonexistent_issue_fails(issue_id in 1000i64..10000) {
            let (db, _dir) = setup_test_db();

            let result = run(&db, None, issue_id, IssueUpdate { title: Some("New title"), ..Default::default() });
            prop_assert!(result.is_err());
        }

        #[test]
        fn prop_unicode_description_roundtrip(desc in "[\\p{L}\\p{N} ]{1,100}") {
            let (db, _dir) = setup_test_db();
            let issue_id = db.create_issue("Test", None, "medium").unwrap();

            run(&db, None, issue_id, IssueUpdate { description: DescriptionUpdate::Set(&desc), ..Default::default() }).unwrap();

            let issue = db.get_issue(issue_id).unwrap().unwrap();
            prop_assert_eq!(issue.description, Some(desc));
        }
    }
}
