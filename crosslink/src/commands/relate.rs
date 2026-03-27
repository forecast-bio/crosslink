use anyhow::Result;

use crate::db::Database;
use crate::shared_writer::SharedWriter;
use crate::utils::format_issue_id;

pub fn add(
    db: &Database,
    writer: Option<&SharedWriter>,
    issue_id: i64,
    related_id: i64,
) -> Result<()> {
    db.require_issue(issue_id)?;
    db.require_issue(related_id)?;

    if let Some(w) = writer {
        w.add_relation(db, issue_id, related_id)?;
        println!(
            "Linked {} ↔ {}",
            format_issue_id(issue_id),
            format_issue_id(related_id)
        );
    } else if db.add_relation(issue_id, related_id)? {
        println!(
            "Linked {} ↔ {}",
            format_issue_id(issue_id),
            format_issue_id(related_id)
        );
    } else {
        println!(
            "Issues {} and {} are already related",
            format_issue_id(issue_id),
            format_issue_id(related_id)
        );
    }

    Ok(())
}

pub fn remove(
    db: &Database,
    writer: Option<&SharedWriter>,
    issue_id: i64,
    related_id: i64,
) -> Result<()> {
    if let Some(w) = writer {
        w.remove_relation(db, issue_id, related_id)?;
        println!(
            "Unlinked {} ↔ {}",
            format_issue_id(issue_id),
            format_issue_id(related_id)
        );
    } else if db.remove_relation(issue_id, related_id)? {
        println!(
            "Unlinked {} ↔ {}",
            format_issue_id(issue_id),
            format_issue_id(related_id)
        );
    } else {
        println!(
            "No relation found between {} and {}",
            format_issue_id(issue_id),
            format_issue_id(related_id)
        );
    }

    Ok(())
}

pub fn list(db: &Database, issue_id: i64) -> Result<()> {
    db.require_issue(issue_id)?;

    let related = db.get_related_issues(issue_id)?;

    if related.is_empty() {
        println!("No related issues for {}", format_issue_id(issue_id));
        return Ok(());
    }

    println!("Related to {}:", format_issue_id(issue_id));
    for r in related {
        let status_marker = if r.status == crate::models::IssueStatus::Closed {
            "✓"
        } else {
            " "
        };
        println!(
            "  {:<5} [{}] {:8} {}",
            format_issue_id(r.id),
            status_marker,
            r.priority,
            r.title
        );
    }

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

    #[test]
    fn test_add_relation() {
        let (db, _dir) = setup_test_db();
        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

        let result = add(&db, None, id1, id2);
        assert!(result.is_ok());

        let related = db.get_related_issues(id1).unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].id, id2);
    }

    #[test]
    fn test_add_relation_bidirectional() {
        let (db, _dir) = setup_test_db();
        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

        add(&db, None, id1, id2).unwrap();

        let related1 = db.get_related_issues(id1).unwrap();
        let related2 = db.get_related_issues(id2).unwrap();
        assert_eq!(related1.len(), 1);
        assert_eq!(related2.len(), 1);
    }

    #[test]
    fn test_add_relation_nonexistent_issue() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("Issue 1", None, "medium").unwrap();

        let result = add(&db, None, id, 99999);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_add_duplicate_relation() {
        let (db, _dir) = setup_test_db();
        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

        add(&db, None, id1, id2).unwrap();
        let result = add(&db, None, id1, id2);
        assert!(result.is_ok());

        let related = db.get_related_issues(id1).unwrap();
        assert_eq!(related.len(), 1);
    }

    #[test]
    fn test_remove_relation() {
        let (db, _dir) = setup_test_db();
        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

        add(&db, None, id1, id2).unwrap();
        let result = remove(&db, None, id1, id2);
        assert!(result.is_ok());

        let related = db.get_related_issues(id1).unwrap();
        assert_eq!(related.len(), 0);
    }

    #[test]
    fn test_remove_nonexistent_relation() {
        let (db, _dir) = setup_test_db();
        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

        let result = remove(&db, None, id1, id2);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_relations() {
        let (db, _dir) = setup_test_db();
        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();
        let id3 = db.create_issue("Issue 3", None, "medium").unwrap();

        add(&db, None, id1, id2).unwrap();
        add(&db, None, id1, id3).unwrap();

        let result = list(&db, id1);
        assert!(result.is_ok());
    }

    #[test]
    fn test_list_relations_nonexistent() {
        let (db, _dir) = setup_test_db();

        let result = list(&db, 99999);
        assert!(result.is_err());
    }

    #[test]
    fn test_list_no_relations() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("Lonely issue", None, "medium").unwrap();

        let result = list(&db, id);
        assert!(result.is_ok());
    }

    proptest! {
        #[test]
        fn prop_add_remove_roundtrip(a in 0i64..3, b in 0i64..3) {
            if a != b {
                let (db, _dir) = setup_test_db();
                let ids: Vec<i64> = (0..5).map(|i| db.create_issue(&format!("Issue {}", i), None, "medium").unwrap()).collect();

                let id1 = ids[a as usize % ids.len()];
                let id2 = ids[b as usize % ids.len()];

                add(&db, None, id1, id2).unwrap();
                let related = db.get_related_issues(id1).unwrap();
                prop_assert!(!related.is_empty());

                remove(&db, None, id1, id2).unwrap();
                let related = db.get_related_issues(id1).unwrap();
                prop_assert!(related.is_empty());
            }
        }
    }
}
