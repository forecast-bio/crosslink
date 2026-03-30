use anyhow::{bail, Context, Result};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::db::Database;
use crate::shared_writer::SharedWriter;
use crate::utils::format_issue_id;

/// Controls how much output a close operation produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Print status messages to stdout.
    Normal,
    /// Suppress non-essential output (used by close-all and batch operations).
    Quiet,
}

pub fn close(
    db: &Database,
    writer: Option<&SharedWriter>,
    id: i64,
    update_changelog: bool,
    crosslink_dir: &Path,
) -> Result<()> {
    close_inner(
        db,
        writer,
        id,
        update_changelog,
        crosslink_dir,
        OutputMode::Normal,
    )
}

pub fn close_quiet(
    db: &Database,
    writer: Option<&SharedWriter>,
    id: i64,
    update_changelog: bool,
    crosslink_dir: &Path,
) -> Result<()> {
    close_inner(
        db,
        writer,
        id,
        update_changelog,
        crosslink_dir,
        OutputMode::Quiet,
    )
}

fn close_inner(
    db: &Database,
    writer: Option<&SharedWriter>,
    id: i64,
    update_changelog: bool,
    crosslink_dir: &Path,
    output: OutputMode,
) -> Result<()> {
    let quiet = output == OutputMode::Quiet;
    // Get issue details before closing
    let issue = db.get_issue(id)?;
    let Some(issue) = issue else {
        bail!("Issue {} not found", format_issue_id(id));
    };
    let labels = db.get_labels(id)?;

    if let Some(w) = writer {
        w.close_issue(db, id)?;
        if !quiet {
            println!("Closed issue {}", format_issue_id(id));
        }
    } else if db.close_issue(id)? {
        if !quiet {
            println!("Closed issue {}", format_issue_id(id));
        }
    } else {
        bail!("Issue {} not found", format_issue_id(id));
    }

    // Clear session active work item if this was the active issue
    // Prevents the cascade where closing the active issue leaves the session
    // without a work item, causing work-check hook to block all tool calls (#399)
    let agent_id = crate::identity::AgentConfig::load(crosslink_dir)
        .ok()
        .flatten()
        .map(|a| a.agent_id);
    if let Ok(Some(session)) = db.get_current_session_for_agent(agent_id.as_deref()) {
        if session.active_issue_id == Some(id) {
            // INTENTIONAL: clearing stale session issue is best-effort — prevents work-check hook from blocking
            let _ = db.clear_session_issue(session.id);
        }
    }

    // Auto-release lock in multi-agent mode
    match crate::lock_check::try_release_lock(crosslink_dir, id) {
        Ok(true) if !quiet => {
            println!("Released lock on issue {}", format_issue_id(id));
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("Could not release lock on {}: {}", format_issue_id(id), e),
    }

    // Update changelog if requested
    if update_changelog {
        update_changelog_for_issue(crosslink_dir, &issue.title, id, &labels, quiet);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// CHANGELOG manipulation helpers (extracted from close_inner for #443)
// ---------------------------------------------------------------------------

/// Update the project CHANGELOG.md with an entry for a closed issue.
/// Creates CHANGELOG.md if it doesn't exist. Best-effort: logs warnings on failure.
fn update_changelog_for_issue(
    crosslink_dir: &Path,
    title: &str,
    id: i64,
    labels: &[String],
    quiet: bool,
) {
    let project_root = crosslink_dir.parent().unwrap_or(crosslink_dir);
    let changelog_path = project_root.join("CHANGELOG.md");

    // Create CHANGELOG.md if it doesn't exist
    if !changelog_path.exists() {
        if let Err(e) = create_changelog(&changelog_path) {
            tracing::warn!("Could not create CHANGELOG.md: {}", e);
        } else if !quiet {
            println!("Created CHANGELOG.md");
        }
    }

    if changelog_path.exists() {
        let category = determine_changelog_category(labels);
        let entry = format!("- {} ({})\n", title, format_issue_id(id));

        if let Err(e) = append_to_changelog(&changelog_path, &category, &entry) {
            tracing::warn!("Could not update CHANGELOG.md: {}", e);
        } else if !quiet {
            println!("Added to CHANGELOG.md under {category}");
        }
    }
}

fn create_changelog(path: &Path) -> Result<()> {
    let template = r"# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added

### Fixed

### Changed
";
    fs::write(path, template).context("Failed to create CHANGELOG.md")?;
    Ok(())
}

fn determine_changelog_category(labels: &[String]) -> String {
    for label in labels {
        match label.to_lowercase().as_str() {
            "bug" | "fix" | "bugfix" => return "Fixed".to_string(),
            "feature" | "enhancement" => return "Added".to_string(),
            "breaking" | "breaking-change" => return "Changed".to_string(),
            "deprecated" => return "Deprecated".to_string(),
            "removed" => return "Removed".to_string(),
            "security" => return "Security".to_string(),
            _ => {}
        }
    }
    "Changed".to_string() // Default category
}

fn append_to_changelog(path: &Path, category: &str, entry: &str) -> Result<()> {
    let content = fs::read_to_string(path).context("Failed to read CHANGELOG.md")?;
    let heading = format!("### {category}");

    let mut result = String::new();
    if content.contains(&heading) {
        // Insert after the heading
        let mut found = false;
        for line in content.lines() {
            result.push_str(line);
            result.push('\n');
            if !found && line.trim() == heading {
                result.push_str(entry);
                found = true;
            }
        }
    } else {
        // Add new section after first ## heading (usually ## [Unreleased])
        let mut added = false;
        for line in content.lines() {
            result.push_str(line);
            result.push('\n');
            if !added && line.starts_with("## ") {
                result.push('\n');
                let _ = writeln!(result, "{heading}");
                result.push_str(entry);
                added = true;
            }
        }
        if !added {
            // No ## heading found, append at end
            result.push('\n');
            let _ = writeln!(result, "{heading}");
            result.push_str(entry);
        }
    }
    let new_content = result;

    fs::write(path, new_content).context("Failed to write CHANGELOG.md")?;
    Ok(())
}

pub fn close_all(
    db: &Database,
    writer: Option<&SharedWriter>,
    label_filter: Option<&str>,
    priority_filter: Option<&str>,
    update_changelog: bool,
    crosslink_dir: &Path,
) -> Result<()> {
    let issues = db.list_issues(Some("open"), label_filter, priority_filter)?;

    if issues.is_empty() {
        println!("No matching open issues found.");
        return Ok(());
    }

    let mut closed_count = 0;
    for issue in &issues {
        match close(db, writer, issue.id, update_changelog, crosslink_dir) {
            Ok(()) => closed_count += 1,
            Err(e) => tracing::warn!("Failed to close {}: {}", format_issue_id(issue.id), e),
        }
    }

    println!("Closed {closed_count} issue(s).");
    Ok(())
}

pub fn reopen(db: &Database, writer: Option<&SharedWriter>, id: i64) -> Result<()> {
    if let Some(w) = writer {
        w.reopen_issue(db, id)?;
        println!("Reopened issue {}", format_issue_id(id));
    } else if db.reopen_issue(id)? {
        println!("Reopened issue {}", format_issue_id(id));
    } else {
        bail!("Issue {} not found", format_issue_id(id));
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

    // ==================== Close Tests ====================

    #[test]
    fn test_close_existing_issue() {
        let (db, _dir) = setup_test_db();
        let crosslink_dir = _dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let result = close(&db, None, issue_id, false, &crosslink_dir);
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.status, "closed");
        assert!(issue.closed_at.is_some());
    }

    #[test]
    fn test_close_nonexistent_issue() {
        let (db, _dir) = setup_test_db();
        let crosslink_dir = _dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let result = close(&db, None, 99999, false, &crosslink_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_close_already_closed_issue() {
        let (db, _dir) = setup_test_db();
        let crosslink_dir = _dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        db.close_issue(issue_id).unwrap();

        // Closing again should be fine (idempotent at db level)
        let result = close(&db, None, issue_id, false, &crosslink_dir);
        assert!(result.is_ok());
    }

    // ==================== Reopen Tests ====================

    #[test]
    fn test_reopen_closed_issue() {
        let (db, _dir) = setup_test_db();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        db.close_issue(issue_id).unwrap();

        let result = reopen(&db, None, issue_id);
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.status, "open");
        assert!(issue.closed_at.is_none());
    }

    #[test]
    fn test_reopen_nonexistent_issue() {
        let (db, _dir) = setup_test_db();

        let result = reopen(&db, None, 99999);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_reopen_already_open_issue() {
        let (db, _dir) = setup_test_db();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        // Reopening an open issue - succeeds (idempotent operation)
        let result = reopen(&db, None, issue_id);
        assert!(result.is_ok());

        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.status, "open");
    }

    // ==================== Changelog Category Tests ====================

    #[test]
    fn test_determine_changelog_category_bug() {
        assert_eq!(determine_changelog_category(&["bug".to_string()]), "Fixed");
        assert_eq!(determine_changelog_category(&["fix".to_string()]), "Fixed");
        assert_eq!(
            determine_changelog_category(&["bugfix".to_string()]),
            "Fixed"
        );
    }

    #[test]
    fn test_determine_changelog_category_feature() {
        assert_eq!(
            determine_changelog_category(&["feature".to_string()]),
            "Added"
        );
        assert_eq!(
            determine_changelog_category(&["enhancement".to_string()]),
            "Added"
        );
    }

    #[test]
    fn test_determine_changelog_category_breaking() {
        assert_eq!(
            determine_changelog_category(&["breaking".to_string()]),
            "Changed"
        );
        assert_eq!(
            determine_changelog_category(&["breaking-change".to_string()]),
            "Changed"
        );
    }

    #[test]
    fn test_determine_changelog_category_other() {
        assert_eq!(
            determine_changelog_category(&["deprecated".to_string()]),
            "Deprecated"
        );
        assert_eq!(
            determine_changelog_category(&["removed".to_string()]),
            "Removed"
        );
        assert_eq!(
            determine_changelog_category(&["security".to_string()]),
            "Security"
        );
    }

    #[test]
    fn test_determine_changelog_category_default() {
        assert_eq!(
            determine_changelog_category(&["unknown".to_string()]),
            "Changed"
        );
        assert_eq!(determine_changelog_category(&[]), "Changed");
    }

    #[test]
    fn test_determine_changelog_category_first_match_wins() {
        // Bug comes before feature, so Fixed should win
        assert_eq!(
            determine_changelog_category(&["bug".to_string(), "feature".to_string()]),
            "Fixed"
        );
    }

    #[test]
    fn test_determine_changelog_category_case_insensitive() {
        assert_eq!(determine_changelog_category(&["BUG".to_string()]), "Fixed");
        assert_eq!(
            determine_changelog_category(&["Feature".to_string()]),
            "Added"
        );
    }

    // ==================== Close/Reopen Cycle Tests ====================

    #[test]
    fn test_close_reopen_cycle() {
        let (db, _dir) = setup_test_db();
        let crosslink_dir = _dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        // Close
        close(&db, None, issue_id, false, &crosslink_dir).unwrap();
        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.status, "closed");

        // Reopen
        reopen(&db, None, issue_id).unwrap();
        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.status, "open");

        // Close again
        close(&db, None, issue_id, false, &crosslink_dir).unwrap();
        let issue = db.get_issue(issue_id).unwrap().unwrap();
        assert_eq!(issue.status, "closed");
    }

    // ==================== Property-Based Tests ====================

    proptest! {
        #[test]
        fn prop_close_sets_status_to_closed(title in "[a-zA-Z0-9 ]{1,50}") {
            let (db, _dir) = setup_test_db();
            let crosslink_dir = _dir.path().join(".crosslink");
            std::fs::create_dir_all(&crosslink_dir).unwrap();

            let issue_id = db.create_issue(&title, None, "medium").unwrap();
            close(&db, None, issue_id, false, &crosslink_dir).unwrap();

            let issue = db.get_issue(issue_id).unwrap().unwrap();
            prop_assert_eq!(issue.status, "closed");
        }

        #[test]
        fn prop_reopen_sets_status_to_open(title in "[a-zA-Z0-9 ]{1,50}") {
            let (db, _dir) = setup_test_db();

            let issue_id = db.create_issue(&title, None, "medium").unwrap();
            db.close_issue(issue_id).unwrap();

            reopen(&db, None, issue_id).unwrap();

            let issue = db.get_issue(issue_id).unwrap().unwrap();
            prop_assert_eq!(issue.status, "open");
        }

        #[test]
        fn prop_nonexistent_issue_close_fails(issue_id in 1000i64..10000) {
            let (db, _dir) = setup_test_db();
            let crosslink_dir = _dir.path().join(".crosslink");
            std::fs::create_dir_all(&crosslink_dir).unwrap();

            let result = close(&db, None, issue_id, false, &crosslink_dir);
            prop_assert!(result.is_err());
        }

        #[test]
        fn prop_nonexistent_issue_reopen_fails(issue_id in 1000i64..10000) {
            let (db, _dir) = setup_test_db();

            let result = reopen(&db, None, issue_id);
            prop_assert!(result.is_err());
        }

        #[test]
        fn prop_changelog_category_returns_known_category(
            labels in proptest::collection::vec("[a-zA-Z]{1,20}", 0..5)
        ) {
            let valid_categories = ["Fixed", "Added", "Changed", "Deprecated", "Removed", "Security"];
            let category = determine_changelog_category(&labels);
            prop_assert!(
                valid_categories.contains(&category.as_str()),
                "Got unknown category: {}", category
            );
        }
    }
}
