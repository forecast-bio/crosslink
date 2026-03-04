use anyhow::{bail, Result};
use std::path::Path;

use crate::db::Database;
use crate::shared_writer::SharedWriter;
use crate::utils::format_issue_id;
use crate::MilestoneCommands;

pub fn run(command: MilestoneCommands, db: &Database, crosslink_dir: &Path) -> Result<()> {
    let shared = SharedWriter::new(crosslink_dir)?;
    let shared_ref = shared.as_ref();
    match command {
        MilestoneCommands::Create { name, description } => {
            create(db, shared_ref, &name, description.as_deref())
        }
        MilestoneCommands::List { status } => list(db, Some(&status)),
        MilestoneCommands::Show { id } => show(db, id),
        MilestoneCommands::Add { id, issues } => add(db, shared_ref, id, &issues),
        MilestoneCommands::Remove { id, issue } => remove(db, shared_ref, id, issue),
        MilestoneCommands::Close { id } => close(db, shared_ref, id),
        MilestoneCommands::Delete { id } => delete(db, shared_ref, id),
    }
}

pub fn create(
    db: &Database,
    shared: Option<&SharedWriter>,
    name: &str,
    description: Option<&str>,
) -> Result<()> {
    if let Some(sw) = shared {
        let id = sw.create_milestone(db, name, description)?;
        println!("Created milestone #{}: {}", id, name);
    } else {
        let id = db.create_milestone(name, description)?;
        println!("Created milestone #{}: {}", id, name);
    }
    Ok(())
}

pub fn list(db: &Database, status: Option<&str>) -> Result<()> {
    let milestones = db.list_milestones(status)?;

    if milestones.is_empty() {
        println!("No milestones found.");
        return Ok(());
    }

    for m in milestones {
        let issues = db.get_milestone_issues(m.id)?;
        let total = issues.len();
        let closed = issues.iter().filter(|i| i.status == "closed").count();
        let progress = if total > 0 {
            format!("{}/{}", closed, total)
        } else {
            "0/0".to_string()
        };

        let status_marker = if m.status == "closed" { "✓" } else { " " };
        println!("#{:<3} [{}] {} ({})", m.id, status_marker, m.name, progress);
    }

    Ok(())
}

pub fn show(db: &Database, id: i64) -> Result<()> {
    let m = match db.get_milestone(id)? {
        Some(m) => m,
        None => bail!("Milestone #{} not found", id),
    };
    println!("Milestone #{}: {}", m.id, m.name);
    println!("Status: {}", m.status);
    println!("Created: {}", m.created_at.format("%Y-%m-%d %H:%M:%S"));

    if let Some(closed) = m.closed_at {
        println!("Closed: {}", closed.format("%Y-%m-%d %H:%M:%S"));
    }

    if let Some(ref desc) = m.description {
        if !desc.is_empty() {
            println!("\nDescription:");
            for line in desc.lines() {
                println!("  {}", line);
            }
        }
    }

    let issues = db.get_milestone_issues(id)?;
    let total = issues.len();
    let closed = issues.iter().filter(|i| i.status == "closed").count();

    println!("\nProgress: {}/{} issues closed", closed, total);

    if !issues.is_empty() {
        println!("\nIssues:");
        for issue in issues {
            let status_marker = if issue.status == "closed" { "✓" } else { " " };
            println!(
                "  {:<5} [{}] {:8} {}",
                format_issue_id(issue.id),
                status_marker,
                issue.priority,
                issue.title
            );
        }
    }

    Ok(())
}

pub fn add(
    db: &Database,
    shared: Option<&SharedWriter>,
    milestone_id: i64,
    issue_ids: &[i64],
) -> Result<()> {
    let milestone = db.get_milestone(milestone_id)?;
    if milestone.is_none() {
        bail!("Milestone #{} not found", milestone_id);
    }

    // Validate issue IDs and collect the ones that exist
    let mut valid_ids = Vec::new();
    for &issue_id in issue_ids {
        if db.get_issue(issue_id)?.is_none() {
            println!(
                "Warning: Issue {} not found, skipping",
                format_issue_id(issue_id)
            );
            continue;
        }
        valid_ids.push(issue_id);
    }

    if let Some(sw) = shared {
        // Shared writer path: write JSON then hydrate back to SQLite
        sw.set_milestone_on_issues(db, milestone_id, &valid_ids)?;
        for &issue_id in &valid_ids {
            println!(
                "Added {} to milestone #{}",
                format_issue_id(issue_id),
                milestone_id
            );
        }
    } else {
        // SQLite-only fallback (no coordination branch)
        for &issue_id in &valid_ids {
            if db.add_issue_to_milestone(milestone_id, issue_id)? {
                println!(
                    "Added {} to milestone #{}",
                    format_issue_id(issue_id),
                    milestone_id
                );
            } else {
                println!(
                    "Issue {} already in milestone #{}",
                    format_issue_id(issue_id),
                    milestone_id
                );
            }
        }
    }

    Ok(())
}

pub fn remove(
    db: &Database,
    shared: Option<&SharedWriter>,
    milestone_id: i64,
    issue_id: i64,
) -> Result<()> {
    if let Some(sw) = shared {
        // Shared writer path: write JSON then hydrate back to SQLite
        sw.clear_milestone_on_issue(db, issue_id)?;
        println!(
            "Removed {} from milestone #{}",
            format_issue_id(issue_id),
            milestone_id
        );
    } else if db.remove_issue_from_milestone(milestone_id, issue_id)? {
        println!(
            "Removed {} from milestone #{}",
            format_issue_id(issue_id),
            milestone_id
        );
    } else {
        println!(
            "Issue {} not in milestone #{}",
            format_issue_id(issue_id),
            milestone_id
        );
    }

    Ok(())
}

pub fn close(db: &Database, shared: Option<&SharedWriter>, id: i64) -> Result<()> {
    if let Some(sw) = shared {
        sw.close_milestone(db, id)?;
        println!("Closed milestone #{}", id);
    } else if db.close_milestone(id)? {
        println!("Closed milestone #{}", id);
    } else {
        println!("Milestone #{} not found", id);
    }

    Ok(())
}

pub fn delete(db: &Database, shared: Option<&SharedWriter>, id: i64) -> Result<()> {
    if let Some(sw) = shared {
        sw.delete_milestone(db, id)?;
        println!("Deleted milestone #{}", id);
    } else if db.delete_milestone(id)? {
        println!("Deleted milestone #{}", id);
    } else {
        println!("Milestone #{} not found", id);
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
    fn test_create_milestone() {
        let (db, _dir) = setup_test_db();
        create(&db, None, "v1.0", None).unwrap();
        let milestones = db.list_milestones(None).unwrap();
        assert_eq!(milestones.len(), 1);
        assert_eq!(milestones[0].name, "v1.0");
    }

    #[test]
    fn test_create_milestone_with_description() {
        let (db, _dir) = setup_test_db();
        create(&db, None, "v1.0", Some("First release")).unwrap();
        let milestones = db.list_milestones(None).unwrap();
        assert_eq!(milestones[0].description, Some("First release".to_string()));
    }

    #[test]
    fn test_list_milestones_empty() {
        let (db, _dir) = setup_test_db();
        list(&db, None).unwrap();
        let milestones = db.list_milestones(None).unwrap();
        assert!(milestones.is_empty());
    }

    #[test]
    fn test_list_milestones() {
        let (db, _dir) = setup_test_db();
        db.create_milestone("v1.0", None).unwrap();
        db.create_milestone("v2.0", None).unwrap();
        list(&db, None).unwrap();
        let milestones = db.list_milestones(None).unwrap();
        assert_eq!(milestones.len(), 2);
    }

    #[test]
    fn test_show_milestone() {
        let (db, _dir) = setup_test_db();
        let id = db.create_milestone("v1.0", Some("Description")).unwrap();
        show(&db, id).unwrap();
        let m = db.get_milestone(id).unwrap().unwrap();
        assert_eq!(m.name, "v1.0");
        assert_eq!(m.description, Some("Description".to_string()));
    }

    #[test]
    fn test_show_nonexistent_milestone() {
        let (db, _dir) = setup_test_db();
        let result = show(&db, 99999);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_add_issue_to_milestone() {
        let (db, _dir) = setup_test_db();
        let milestone_id = db.create_milestone("v1.0", None).unwrap();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        add(&db, None, milestone_id, &[issue_id]).unwrap();
        let issues = db.get_milestone_issues(milestone_id).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, issue_id);
    }

    #[test]
    fn test_add_to_nonexistent_milestone() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        let result = add(&db, None, 99999, &[issue_id]);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_issue_from_milestone() {
        let (db, _dir) = setup_test_db();
        let milestone_id = db.create_milestone("v1.0", None).unwrap();
        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        db.add_issue_to_milestone(milestone_id, issue_id).unwrap();
        remove(&db, None, milestone_id, issue_id).unwrap();
        let issues = db.get_milestone_issues(milestone_id).unwrap();
        assert!(issues.is_empty(), "Issue should be removed from milestone");
    }

    #[test]
    fn test_close_milestone() {
        let (db, _dir) = setup_test_db();
        let id = db.create_milestone("v1.0", None).unwrap();
        close(&db, None, id).unwrap();
        let m = db.get_milestone(id).unwrap().unwrap();
        assert_eq!(m.status, "closed");
        assert!(m.closed_at.is_some());
    }

    #[test]
    fn test_delete_milestone() {
        let (db, _dir) = setup_test_db();
        let id = db.create_milestone("v1.0", None).unwrap();
        delete(&db, None, id).unwrap();
        let m = db.get_milestone(id).unwrap();
        assert!(m.is_none(), "Milestone should be deleted");
    }

    #[test]
    fn test_milestone_progress() {
        let (db, _dir) = setup_test_db();
        let milestone_id = db.create_milestone("v1.0", None).unwrap();
        let issue1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let issue2 = db.create_issue("Issue 2", None, "medium").unwrap();
        db.add_issue_to_milestone(milestone_id, issue1).unwrap();
        db.add_issue_to_milestone(milestone_id, issue2).unwrap();
        db.close_issue(issue1).unwrap();
        show(&db, milestone_id).unwrap();
        let issues = db.get_milestone_issues(milestone_id).unwrap();
        assert_eq!(issues.len(), 2);
        let closed_count = issues.iter().filter(|i| i.status == "closed").count();
        assert_eq!(closed_count, 1, "1 of 2 issues should be closed");
    }

    proptest! {
        #[test]
        fn prop_create_milestone_persists(name in "[a-zA-Z0-9 ]{1,30}") {
            let (db, _dir) = setup_test_db();
            create(&db, None, &name, None).unwrap();
            let milestones = db.list_milestones(None).unwrap();
            prop_assert_eq!(milestones.len(), 1);
            prop_assert_eq!(&milestones[0].name, &name);
        }

        #[test]
        fn prop_list_returns_correct_count(count in 0usize..5) {
            let (db, _dir) = setup_test_db();
            for i in 0..count {
                db.create_milestone(&format!("v{}.0", i), None).unwrap();
            }
            list(&db, None).unwrap();
            let milestones = db.list_milestones(None).unwrap();
            prop_assert_eq!(milestones.len(), count);
        }
    }
}
