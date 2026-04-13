use anyhow::{bail, Result};
use chrono::Utc;

use crate::db::Database;
use crate::utils::format_issue_id;

pub fn start(db: &Database, issue_id: i64) -> Result<()> {
    // Verify issue exists
    let Some(issue) = db.get_issue(issue_id)? else {
        bail!("Issue {} not found", format_issue_id(issue_id));
    };

    // Check if there's already an active timer
    if let Some((active_id, _)) = db.get_active_timer()? {
        if active_id == issue_id {
            bail!(
                "Timer already running for issue {}",
                format_issue_id(issue_id)
            );
        }
        bail!(
            "Timer already running for issue {}. Stop it first with 'crosslink stop'.",
            format_issue_id(active_id)
        );
    }

    db.start_timer(issue_id)?;
    println!(
        "Started timer for {}: {}",
        format_issue_id(issue_id),
        issue.title
    );
    println!("Run 'crosslink stop' when done.");

    Ok(())
}

pub fn stop(db: &Database) -> Result<()> {
    let Some((issue_id, started_at)) = db.get_active_timer()? else {
        bail!("No timer running. Start one with 'crosslink start <id>'.");
    };
    let duration = Utc::now().signed_duration_since(started_at);

    db.stop_timer(issue_id)?;

    let issue = db.get_issue(issue_id)?;
    let title = issue.map_or_else(|| "(deleted)".to_string(), |i| i.title);

    let hours = duration.num_hours();
    let minutes = duration.num_minutes() % 60;
    let seconds = duration.num_seconds() % 60;

    println!("Stopped timer for {}: {}", format_issue_id(issue_id), title);
    println!("Time spent: {hours}h {minutes}m {seconds}s");

    // Show total time for this issue
    let total = db.get_total_time(issue_id)?;
    let total_hours = total / 3600;
    let total_minutes = (total % 3600) / 60;
    println!("Total time on this issue: {total_hours}h {total_minutes}m");

    Ok(())
}

pub fn status(db: &Database) -> Result<()> {
    let active = db.get_active_timer()?;

    match active {
        Some((issue_id, started_at)) => {
            let duration = Utc::now().signed_duration_since(started_at);
            let hours = duration.num_hours();
            let minutes = duration.num_minutes() % 60;
            let seconds = duration.num_seconds() % 60;

            let issue = db.get_issue(issue_id)?;
            let title = issue.map_or_else(|| "(deleted)".to_string(), |i| i.title);

            println!("Timer running: {} {}", format_issue_id(issue_id), title);
            println!("Elapsed: {hours}h {minutes}m {seconds}s");
        }
        None => {
            println!("No timer running.");
        }
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
    fn test_start_timer() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();

        let result = start(&db, id);
        assert!(result.is_ok());

        let active = db.get_active_timer().unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().0, id);
    }

    #[test]
    fn test_start_nonexistent_issue() {
        let (db, _dir) = setup_test_db();

        let result = start(&db, 99999);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_start_timer_already_running() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();

        start(&db, id).unwrap();
        let result = start(&db, id);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already running"));
    }

    #[test]
    fn test_start_timer_different_issue_running() {
        let (db, _dir) = setup_test_db();
        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

        start(&db, id1).unwrap();
        let result = start(&db, id2);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Stop it first"));
    }

    #[test]
    fn test_stop_timer() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();

        start(&db, id).unwrap();
        let result = stop(&db);
        assert!(result.is_ok());

        let active = db.get_active_timer().unwrap();
        assert!(active.is_none());
    }

    #[test]
    fn test_stop_no_timer() {
        let (db, _dir) = setup_test_db();

        let result = stop(&db);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No timer running"));
    }

    #[test]
    fn test_status_no_timer() {
        let (db, _dir) = setup_test_db();

        let result = status(&db);
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_with_timer() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();

        start(&db, id).unwrap();
        let result = status(&db);
        assert!(result.is_ok());
    }

    #[test]
    fn test_timer_workflow() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("Test issue", None, "medium").unwrap();

        start(&db, id).unwrap();
        status(&db).unwrap();
        stop(&db).unwrap();

        let active = db.get_active_timer().unwrap();
        assert!(active.is_none());
    }

    proptest! {
        #[test]
        fn prop_start_stop_roundtrip(idx in 0usize..5) {
            let (db, _dir) = setup_test_db();
            let ids: Vec<i64> = (0..5).map(|i| db.create_issue(&format!("Issue {i}"), None, "medium").unwrap()).collect();
            let id = ids[idx];

            start(&db, id).unwrap();
            prop_assert!(db.get_active_timer().unwrap().is_some());

            stop(&db).unwrap();
            prop_assert!(db.get_active_timer().unwrap().is_none());
        }
    }
}
