use anyhow::Result;

use crate::db::Database;
use crate::issue_file::validate_comment_kind;
use crate::shared_writer::SharedWriter;
use crate::utils::format_issue_id;

pub fn run(
    db: &Database,
    writer: Option<&SharedWriter>,
    issue_id: i64,
    content: &str,
    kind: &str,
) -> Result<()> {
    if !validate_comment_kind(kind) {
        tracing::warn!("unknown comment kind '{}'. Known kinds: note, plan, decision, observation, blocker, resolution, result, handoff, human", kind);
    }
    db.require_issue(issue_id)?;
    if let Some(w) = writer {
        w.add_comment(db, issue_id, content, kind)?;
    } else {
        db.add_comment(issue_id, content, kind)?;
    }
    println!("Added comment to issue {}", format_issue_id(issue_id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    // ==================== Unit Tests ====================

    #[test]
    fn test_add_comment_to_existing_issue() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let result = run(&db, None, issue_id, "This is a comment", "note");
        assert!(result.is_ok());

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, "This is a comment");
    }

    #[test]
    fn test_add_comment_to_nonexistent_issue() {
        let (db, _dir) = setup_test_db();

        let result = run(&db, None, 99999, "Comment on nothing", "note");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_add_multiple_comments() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        run(&db, None, issue_id, "First comment", "note").unwrap();
        run(&db, None, issue_id, "Second comment", "note").unwrap();
        run(&db, None, issue_id, "Third comment", "note").unwrap();

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].content, "First comment");
        assert_eq!(comments[1].content, "Second comment");
        assert_eq!(comments[2].content, "Third comment");
    }

    #[test]
    fn test_add_empty_comment() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let result = run(&db, None, issue_id, "", "note");
        assert!(result.is_ok());

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, "");
    }

    #[test]
    fn test_add_unicode_comment() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let unicode_content = "こんにちは 🎉 مرحبا αβγδ ← → ↑ ↓";
        let result = run(&db, None, issue_id, unicode_content, "note");
        assert!(result.is_ok());

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments[0].content, unicode_content);
    }

    #[test]
    fn test_add_very_long_comment() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let long_content = "a".repeat(100_000);
        let result = run(&db, None, issue_id, &long_content, "note");
        assert!(result.is_ok());

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments[0].content.len(), 100_000);
    }

    #[test]
    fn test_add_comment_with_newlines() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let multiline = "Line 1\nLine 2\nLine 3\n\nLine 5";
        let result = run(&db, None, issue_id, multiline, "note");
        assert!(result.is_ok());

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments[0].content, multiline);
    }

    #[test]
    fn test_add_comment_with_special_chars() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let special = "Quotes: \"test\" 'test' `test` | Symbols: @#$%^&*() | SQL: '; DROP TABLE;--";
        let result = run(&db, None, issue_id, special, "note");
        assert!(result.is_ok());

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments[0].content, special);
    }

    #[test]
    fn test_add_comment_sql_injection() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let malicious = "'); DELETE FROM comments; --";
        run(&db, None, issue_id, malicious, "note").unwrap();

        // Verify comment was stored literally, not executed
        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, malicious);

        // Verify database integrity
        let issues = db.list_issues(None, None, None).unwrap();
        assert!(!issues.is_empty());
    }

    #[test]
    fn test_comment_on_closed_issue() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        db.close_issue(issue_id).unwrap();

        // Should still be able to comment on closed issues
        let result = run(&db, None, issue_id, "Comment on closed issue", "note");
        assert!(result.is_ok());

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments.len(), 1);
    }

    #[test]
    fn test_comment_with_null_bytes() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let with_null = "before\0after";
        let result = run(&db, None, issue_id, with_null, "note");
        assert!(result.is_ok());

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments[0].content, with_null);
    }

    // ==================== Property-Based Tests ====================

    proptest! {
        #[test]
        fn prop_comment_roundtrip(content in ".*") {
            let (db, _dir) = setup_test_db();
            let issue_id = db.create_issue("Test", None, "medium").unwrap();

            let result = run(&db, None, issue_id, &content, "note");
            prop_assert!(result.is_ok());

            let comments = db.get_comments(issue_id).unwrap();
            prop_assert_eq!(comments.len(), 1);
            prop_assert_eq!(&comments[0].content, &content);
        }

        #[test]
        fn prop_nonexistent_issue_fails(issue_id in 1000i64..10000) {
            let (db, _dir) = setup_test_db();
            // Don't create any issues
            let result = run(&db, None, issue_id, "Comment", "note");
            prop_assert!(result.is_err());
        }

        #[test]
        fn prop_multiple_comments_preserve_order(count in 1usize..10) {
            let (db, _dir) = setup_test_db();
            let issue_id = db.create_issue("Test", None, "medium").unwrap();

            for i in 0..count {
                run(&db, None, issue_id, &format!("Comment {i}"), "note").unwrap();
            }

            let comments = db.get_comments(issue_id).unwrap();
            prop_assert_eq!(comments.len(), count);

            for (i, comment) in comments.iter().enumerate() {
                prop_assert_eq!(&comment.content, &format!("Comment {i}"));
            }
        }

        #[test]
        fn prop_unicode_comments_roundtrip(
            prefix in "[a-zA-Z]{0,10}",
            emoji in "[\u{1F300}-\u{1F9FF}]{0,5}",
            suffix in "[a-zA-Z]{0,10}"
        ) {
            let (db, _dir) = setup_test_db();
            let issue_id = db.create_issue("Test", None, "medium").unwrap();

            let content = format!("{prefix}{emoji}{suffix}");
            run(&db, None, issue_id, &content, "note").unwrap();

            let comments = db.get_comments(issue_id).unwrap();
            prop_assert_eq!(&comments[0].content, &content);
        }
    }
}
