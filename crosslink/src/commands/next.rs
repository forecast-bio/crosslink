use anyhow::Result;
use std::path::Path;

use crate::db::Database;
use crate::locks::LocksFile;
use crate::models::Issue;
use crate::utils::format_issue_id;

/// Progress of an issue's subissues.
struct SubissueProgress {
    completed: i32,
    total: i32,
}

/// An issue annotated with its priority score and subissue progress.
struct ScoredIssue {
    issue: Issue,
    score: i32,
    progress: Option<SubissueProgress>,
}

/// Init cache, fetch remote, and load lock state for filtering.
/// Side effects: initializes the hub cache and fetches from remote (best-effort).
/// Returns (LocksFile, my_agent_id) or None if agent/sync not configured.
fn fetch_and_load_locks(crosslink_dir: &Path) -> Option<(LocksFile, String)> {
    let agent = crate::identity::AgentConfig::load(crosslink_dir).ok()??;
    let sync = crate::sync::SyncManager::new(crosslink_dir).ok()?;
    // INTENTIONAL: init and fetch are best-effort — lock filtering works with stale data
    let _ = sync.init_cache();
    let _ = sync.fetch();
    let locks = sync.read_locks_auto().ok()?;
    Some((locks, agent.agent_id))
}

/// Priority order for sorting (higher = more important).
fn priority_weight(priority: &crate::models::Priority) -> i32 {
    match priority {
        crate::models::Priority::Critical => 4,
        crate::models::Priority::High => 3,
        crate::models::Priority::Medium => 2,
        crate::models::Priority::Low => 1,
    }
}

/// Calculate progress for issues with subissues
fn calculate_progress(db: &Database, issue: &Issue) -> Result<Option<SubissueProgress>> {
    let subissues = db.get_subissues(issue.id)?;
    if subissues.is_empty() {
        return Ok(None);
    }

    let total = subissues.len() as i32;
    let completed = subissues
        .iter()
        .filter(|s| s.status == crate::models::IssueStatus::Closed)
        .count() as i32;
    Ok(Some(SubissueProgress { completed, total }))
}

pub fn run(db: &Database, crosslink_dir: &std::path::Path) -> Result<()> {
    let all_ready = db.list_ready_issues()?;

    if all_ready.is_empty() {
        println!("No issues ready to work on.");
        println!(
            "Use 'crosslink list' to see all issues or 'crosslink blocked' to see blocked issues."
        );
        return Ok(());
    }

    // Load lock state for filtering (best-effort, non-blocking)
    let locks_filter = fetch_and_load_locks(crosslink_dir);

    // Score and sort issues
    let mut scored: Vec<ScoredIssue> = Vec::new();

    for issue in &all_ready {
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
        let progress = calculate_progress(db, issue)?;

        // Boost score for issues that are partially complete (finish what you started)
        let progress_bonus = match &progress {
            Some(p) if p.completed > 0 && p.completed < p.total => 50,
            _ => 0,
        };

        let score = priority_score + progress_bonus;
        scored.push(ScoredIssue {
            issue: issue.clone(),
            score,
            progress,
        });
    }

    // Sort by score descending
    scored.sort_by(|a, b| b.score.cmp(&a.score));

    if scored.is_empty() {
        // All ready issues are subissues or locked, show first available instead
        if let Some(issue) = all_ready.first() {
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
    let top = &scored[0];
    println!(
        "Next: {} [{}] {}",
        format_issue_id(top.issue.id),
        top.issue.priority,
        top.issue.title
    );

    if let Some(ref p) = top.progress {
        println!(
            "       Progress: {}/{} subissues complete",
            p.completed, p.total
        );
    }

    if let Some(desc) = &top.issue.description {
        if !desc.is_empty() {
            let preview: String = desc.chars().take(80).collect();
            let suffix = if desc.chars().count() > 80 { "..." } else { "" };
            println!("       {}{}", preview, suffix);
        }
    }

    println!();
    println!("Run: crosslink session work {}", top.issue.id);

    // Show runners-up if any
    if scored.len() > 1 {
        println!();
        println!("Also ready:");
        for entry in scored.iter().skip(1).take(3) {
            let progress_str = match &entry.progress {
                Some(p) => format!(" ({}/{})", p.completed, p.total),
                None => String::new(),
            };
            println!(
                "  {} [{}] {}{}",
                format_issue_id(entry.issue.id),
                entry.issue.priority,
                entry.issue.title,
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

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_priority_weight_critical() {
        assert_eq!(priority_weight(&crate::models::Priority::Critical), 4);
    }

    #[test]
    fn test_priority_weight_high() {
        assert_eq!(priority_weight(&crate::models::Priority::High), 3);
    }

    #[test]
    fn test_priority_weight_medium() {
        assert_eq!(priority_weight(&crate::models::Priority::Medium), 2);
    }

    #[test]
    fn test_priority_weight_low() {
        assert_eq!(priority_weight(&crate::models::Priority::Low), 1);
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
        use crate::models::Priority;
        assert_eq!(priority_weight(&Priority::Critical), 4);
        assert!(priority_weight(&Priority::Critical) > priority_weight(&Priority::Low));
        assert!(priority_weight(&Priority::Critical) > priority_weight(&Priority::Medium));
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
        let p = progress.unwrap();
        assert_eq!(p.completed, 1);
        assert_eq!(p.total, 2);
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
            let p: crate::models::Priority = priority.parse().unwrap();
            let weight = priority_weight(&p);
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
