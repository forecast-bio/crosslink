use anyhow::Result;
use std::path::Path;

use crate::db::Database;
use crate::locks::LocksFile;
use crate::models::Issue;
use crate::utils::format_issue_id;

/// Progress tuple: (completed subissues, total subissues)
type Progress = Option<(i32, i32)>;

/// Scored issue with priority score and progress
type ScoredIssue = (Issue, i32, Progress);

/// Best-effort load of lock state for filtering. Returns (LocksFile, my_agent_id) or None.
fn load_locks_filter(crosslink_dir: &Path) -> Option<(LocksFile, String)> {
    let agent = crate::identity::AgentConfig::load(crosslink_dir).ok()??;
    let sync = crate::sync::SyncManager::new(crosslink_dir).ok()?;
    // INTENTIONAL: init and fetch are best-effort — lock filtering works with stale data
    let _ = sync.init_cache();
    let _ = sync.fetch();
    let locks = sync.read_locks_auto().ok()?;
    Some((locks, agent.agent_id))
}

/// Priority order for sorting (higher = more important)
fn priority_weight(priority: &str) -> i32 {
    match priority {
        "critical" => 4,
        "high" => 3,
        "medium" => 2,
        "low" => 1,
        _ => 0,
    }
}

/// Calculate progress for issues with subissues
fn calculate_progress(db: &Database, issue: &Issue) -> Result<Progress> {
    let subissues = db.get_subissues(issue.id)?;
    if subissues.is_empty() {
        return Ok(None);
    }

    let total = subissues.len() as i32;
    let closed = subissues.iter().filter(|s| s.status == "closed").count() as i32;
    Ok(Some((closed, total)))
}

pub fn run(db: &Database, crosslink_dir: &std::path::Path) -> Result<()> {
    let ready = db.list_ready_issues()?;

    if ready.is_empty() {
        println!("No issues ready to work on.");
        println!(
            "Use 'crosslink list' to see all issues or 'crosslink blocked' to see blocked issues."
        );
        return Ok(());
    }

    // Load lock state for filtering (best-effort, non-blocking)
    let locks_filter = load_locks_filter(crosslink_dir);

    // Score and sort issues
    let mut scored: Vec<ScoredIssue> = Vec::new();

    for issue in ready {
        // Skip subissues - we want to recommend parent issues or standalone issues
        if issue.parent_id.is_some() {
            continue;
        }

        // Skip issues locked by other agents
        if let Some((ref locks, ref my_agent_id)) = locks_filter {
            if locks.is_locked(issue.id) && !locks.is_locked_by(issue.id, my_agent_id) {
                continue;
            }
        }

        let priority_score = priority_weight(&issue.priority) * 100;
        let progress = calculate_progress(db, &issue)?;

        // Boost score for issues that are partially complete (finish what you started)
        let progress_bonus = match &progress {
            Some((closed, total)) if *closed > 0 && *closed < *total => 50,
            _ => 0,
        };

        let score = priority_score + progress_bonus;
        scored.push((issue, score, progress));
    }

    // Sort by score descending
    scored.sort_by(|a, b| b.1.cmp(&a.1));

    if scored.is_empty() {
        // All ready issues are subissues, show them instead
        let ready = db.list_ready_issues()?;
        if let Some(issue) = ready.first() {
            println!(
                "Next: {} [{}] {}",
                format_issue_id(issue.id),
                issue.priority,
                issue.title
            );
            if let Some(parent_id) = issue.parent_id {
                println!("       (subissue of {})", format_issue_id(parent_id));
            }
        } else {
            println!("No issues ready to work on.");
        }
        return Ok(());
    }

    // Recommend the top issue
    let (top, _score, progress) = &scored[0];
    println!(
        "Next: {} [{}] {}",
        format_issue_id(top.id),
        top.priority,
        top.title
    );

    if let Some((closed, total)) = progress {
        println!("       Progress: {}/{} subissues complete", closed, total);
    }

    if let Some(desc) = &top.description {
        if !desc.is_empty() {
            let preview: String = desc.chars().take(80).collect();
            let suffix = if desc.chars().count() > 80 { "..." } else { "" };
            println!("       {}{}", preview, suffix);
        }
    }

    println!();
    println!("Run: crosslink session work {}", top.id);

    // Show runners-up if any
    if scored.len() > 1 {
        println!();
        println!("Also ready:");
        for (issue, _score, progress) in scored.iter().skip(1).take(3) {
            let progress_str = match progress {
                Some((c, t)) => format!(" ({}/{})", c, t),
                None => String::new(),
            };
            println!(
                "  {} [{}] {}{}",
                format_issue_id(issue.id),
                issue.priority,
                issue.title,
                progress_str
            );
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
    fn test_priority_weight_critical() {
        assert_eq!(priority_weight("critical"), 4);
    }

    #[test]
    fn test_priority_weight_high() {
        assert_eq!(priority_weight("high"), 3);
    }

    #[test]
    fn test_priority_weight_medium() {
        assert_eq!(priority_weight("medium"), 2);
    }

    #[test]
    fn test_priority_weight_low() {
        assert_eq!(priority_weight("low"), 1);
    }

    #[test]
    fn test_priority_weight_unknown() {
        assert_eq!(priority_weight("unknown"), 0);
    }

    #[test]
    fn test_run_no_issues() {
        let (db, dir) = setup_test_db();
        run(&db, dir.path()).unwrap();
        let ready = db.list_ready_issues().unwrap();
        assert!(ready.is_empty());
    }

    #[test]
    fn test_run_with_issues() {
        let (db, dir) = setup_test_db();
        let id = db.create_issue("Issue 1", None, "high").unwrap();

        run(&db, dir.path()).unwrap();
        let ready = db.list_ready_issues().unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, id);
    }

    #[test]
    fn test_run_prioritizes_higher() {
        let (db, dir) = setup_test_db();
        db.create_issue("Low priority", None, "low").unwrap();
        let critical_id = db
            .create_issue("Critical priority", None, "critical")
            .unwrap();
        db.create_issue("Medium priority", None, "medium").unwrap();

        run(&db, dir.path()).unwrap();
        // Verify the critical issue has the highest weight via the scoring function
        let ready = db.list_ready_issues().unwrap();
        assert_eq!(ready.len(), 3);
        let critical = ready.iter().find(|i| i.id == critical_id).unwrap();
        assert_eq!(critical.priority, "critical");
        // Critical should have highest weight
        assert_eq!(priority_weight("critical"), 4);
        assert!(priority_weight("critical") > priority_weight("low"));
        assert!(priority_weight("critical") > priority_weight("medium"));
    }

    #[test]
    fn test_calculate_progress_no_subissues() {
        let (db, _dir) = setup_test_db();
        let id = db.create_issue("Simple issue", None, "medium").unwrap();
        let issue = db.get_issue(id).unwrap().unwrap();

        let progress = calculate_progress(&db, &issue).unwrap();
        assert!(progress.is_none());
    }

    #[test]
    fn test_calculate_progress_with_subissues() {
        let (db, _dir) = setup_test_db();
        let parent_id = db.create_issue("Parent", None, "high").unwrap();
        let child1 = db
            .create_subissue(parent_id, "Child 1", None, "medium")
            .unwrap();
        db.create_subissue(parent_id, "Child 2", None, "medium")
            .unwrap();
        db.close_issue(child1).unwrap();

        let issue = db.get_issue(parent_id).unwrap().unwrap();
        let progress = calculate_progress(&db, &issue).unwrap();

        assert!(progress.is_some());
        let (closed, total) = progress.unwrap();
        assert_eq!(closed, 1);
        assert_eq!(total, 2);
    }

    #[test]
    fn test_run_skips_blocked() {
        let (db, dir) = setup_test_db();
        let blocker = db.create_issue("Blocker", None, "high").unwrap();
        let blocked = db.create_issue("Blocked", None, "critical").unwrap();
        db.add_dependency(blocked, blocker).unwrap();

        run(&db, dir.path()).unwrap();
        let ready = db.list_ready_issues().unwrap();
        assert!(
            !ready.iter().any(|i| i.id == blocked),
            "Blocked issue should not be in ready list"
        );
        assert!(
            ready.iter().any(|i| i.id == blocker),
            "Blocker should be in ready list"
        );
    }

    #[test]
    fn test_run_all_issues_closed() {
        let (db, dir) = setup_test_db();
        let id = db.create_issue("Done", None, "medium").unwrap();
        db.close_issue(id).unwrap();

        run(&db, dir.path()).unwrap();
        let ready = db.list_ready_issues().unwrap();
        assert!(
            ready.is_empty(),
            "Closed issues should not appear in ready list"
        );
    }

    proptest! {
        #[test]
        fn prop_priority_weight_valid(priority in "low|medium|high|critical") {
            let weight = priority_weight(&priority);
            prop_assert!((1..=4).contains(&weight));
        }

        #[test]
        fn prop_run_never_panics(count in 0usize..5) {
            let (db, dir) = setup_test_db();
            for i in 0..count {
                db.create_issue(&format!("Issue {}", i), None, "medium").unwrap();
            }
            let result = run(&db, dir.path());
            prop_assert!(result.is_ok());
        }
    }
}
