use anyhow::Result;
use std::path::Path;

use crate::db::{Database, SCHEMA_VERSION};
use crate::hydration::hydrate_to_sqlite;
use crate::identity::AgentConfig;
use crate::issue_file::{
    read_all_issue_files, read_all_milestone_files, read_counters, read_milestones_file,
    write_counters, Counters,
};
use crate::sync::SyncManager;
use crate::IntegrityCommands;

use crate::sync::HUB_CACHE_DIR;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum CheckStatus {
    Pass,
    Fail(String),
    Repaired(String),
    Skipped(String),
}

#[derive(Debug, Clone)]
struct CheckResult {
    name: String,
    status: CheckStatus,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(action: Option<&IntegrityCommands>, crosslink_dir: &Path, db: &Database) -> Result<()> {
    match action {
        None => run_all(crosslink_dir, db),
        Some(IntegrityCommands::Schema { repair }) => {
            let result = check_schema(db, *repair)?;
            print_result(&result);
            Ok(())
        }
        Some(IntegrityCommands::Counters { repair }) => {
            let result = check_counters(crosslink_dir, db, *repair)?;
            print_result(&result);
            Ok(())
        }
        Some(IntegrityCommands::Hydration { repair }) => {
            let result = check_hydration(crosslink_dir, db, *repair)?;
            print_result(&result);
            Ok(())
        }
        Some(IntegrityCommands::Locks { repair }) => {
            let result = check_locks(crosslink_dir, *repair)?;
            print_result(&result);
            Ok(())
        }
    }
}

fn run_all(crosslink_dir: &Path, db: &Database) -> Result<()> {
    println!("Running all integrity checks...\n");

    let results = vec![
        check_schema(db, false)?,
        check_counters(crosslink_dir, db, false)?,
        check_hydration(crosslink_dir, db, false)?,
        check_locks(crosslink_dir, false)?,
    ];

    for result in &results {
        print_result(result);
    }
    println!();
    print_summary(&results);
    Ok(())
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

fn check_schema(db: &Database, _repair: bool) -> Result<CheckResult> {
    let version = db.get_schema_version()?;
    let status = if version == SCHEMA_VERSION {
        CheckStatus::Pass
    } else {
        // Database::open() auto-migrates, so if we get here with a mismatch
        // something is genuinely wrong. Report it but there's nothing to repair
        // beyond reopening the DB (which already happened).
        CheckStatus::Fail(format!(
            "version {} does not match expected {}",
            version, SCHEMA_VERSION
        ))
    };
    Ok(CheckResult {
        name: "schema".to_string(),
        status,
    })
}

fn check_counters(crosslink_dir: &Path, db: &Database, repair: bool) -> Result<CheckResult> {
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    if !cache_dir.exists() {
        return Ok(CheckResult {
            name: "counters".to_string(),
            status: CheckStatus::Skipped("sync not configured".to_string()),
        });
    }

    let counters_path = cache_dir.join("meta").join("counters.json");
    let counters = read_counters(&counters_path)?;
    let max_display = db.get_max_display_id()?;
    let max_comment = db.get_max_comment_id()?;
    let expected_display = max_display + 1;
    let expected_comment = max_comment + 1;

    let display_ok = counters.next_display_id >= expected_display;
    let comment_ok = counters.next_comment_id >= expected_comment;

    if display_ok && comment_ok {
        return Ok(CheckResult {
            name: "counters".to_string(),
            status: CheckStatus::Pass,
        });
    }

    let mut issues = Vec::new();
    if !display_ok {
        issues.push(format!(
            "next_display_id is {}, expected >= {}",
            counters.next_display_id, expected_display
        ));
    }
    if !comment_ok {
        issues.push(format!(
            "next_comment_id is {}, expected >= {}",
            counters.next_comment_id, expected_comment
        ));
    }
    let details = issues.join("; ");

    if !repair {
        return Ok(CheckResult {
            name: "counters".to_string(),
            status: CheckStatus::Fail(details),
        });
    }

    let repaired = Counters {
        next_display_id: expected_display.max(counters.next_display_id),
        next_comment_id: expected_comment.max(counters.next_comment_id),
        next_milestone_id: counters.next_milestone_id,
    };
    write_counters(&counters_path, &repaired)?;

    Ok(CheckResult {
        name: "counters".to_string(),
        status: CheckStatus::Repaired(format!("fixed: {}", details)),
    })
}

fn check_hydration(crosslink_dir: &Path, db: &Database, repair: bool) -> Result<CheckResult> {
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    if !cache_dir.exists() {
        return Ok(CheckResult {
            name: "hydration".to_string(),
            status: CheckStatus::Skipped("sync not configured".to_string()),
        });
    }

    let issues_dir = cache_dir.join("issues");
    let json_issues = read_all_issue_files(&issues_dir)?;
    let json_issue_count = json_issues
        .iter()
        .filter(|i| i.display_id.is_some())
        .count() as i64;
    let db_issue_count = db.get_issue_count()?;

    // Count milestones: per-file first, fall back to legacy single-file
    let milestones_dir = cache_dir.join("meta").join("milestones");
    let json_milestone_entries = read_all_milestone_files(&milestones_dir)?;
    let json_milestone_count = if json_milestone_entries.is_empty() {
        let legacy_path = cache_dir.join("meta").join("milestones.json");
        let legacy = read_milestones_file(&legacy_path)?;
        legacy.milestones.len() as i64
    } else {
        json_milestone_entries.len() as i64
    };
    let db_milestone_count = db.get_milestone_count()?;

    let issues_ok = json_issue_count == db_issue_count;
    let milestones_ok = json_milestone_count == db_milestone_count;

    if issues_ok && milestones_ok {
        return Ok(CheckResult {
            name: "hydration".to_string(),
            status: CheckStatus::Pass,
        });
    }

    let mut issues = Vec::new();
    if !issues_ok {
        issues.push(format!(
            "{} issues in JSON, {} in SQLite",
            json_issue_count, db_issue_count
        ));
    }
    if !milestones_ok {
        issues.push(format!(
            "{} milestones in JSON, {} in SQLite",
            json_milestone_count, db_milestone_count
        ));
    }
    let details = issues.join("; ");

    if !repair {
        return Ok(CheckResult {
            name: "hydration".to_string(),
            status: CheckStatus::Fail(details),
        });
    }

    db.clear_shared_data()?;
    let stats = hydrate_to_sqlite(&cache_dir, db)?;

    Ok(CheckResult {
        name: "hydration".to_string(),
        status: CheckStatus::Repaired(format!(
            "re-hydrated {} issues, {} comments",
            stats.issues, stats.comments
        )),
    })
}

fn check_locks(crosslink_dir: &Path, repair: bool) -> Result<CheckResult> {
    let sync = match SyncManager::new(crosslink_dir) {
        Ok(s) => s,
        Err(_) => {
            return Ok(CheckResult {
                name: "locks".to_string(),
                status: CheckStatus::Skipped("sync not configured".to_string()),
            });
        }
    };

    if !sync.is_initialized() {
        return Ok(CheckResult {
            name: "locks".to_string(),
            status: CheckStatus::Skipped("sync cache not initialized".to_string()),
        });
    }

    let stale = sync.find_stale_locks()?;

    if stale.is_empty() {
        return Ok(CheckResult {
            name: "locks".to_string(),
            status: CheckStatus::Pass,
        });
    }

    let details = format!(
        "{} stale lock(s): {}",
        stale.len(),
        stale
            .iter()
            .map(|(id, agent)| format!("#{} ({})", id, agent))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if !repair {
        return Ok(CheckResult {
            name: "locks".to_string(),
            status: CheckStatus::Fail(details),
        });
    }

    let agent = match AgentConfig::load(crosslink_dir)? {
        Some(a) => a,
        None => {
            return Ok(CheckResult {
                name: "locks".to_string(),
                status: CheckStatus::Fail(format!(
                    "{}; cannot repair without agent identity",
                    details
                )),
            });
        }
    };

    let mut released = 0;
    for (id, _) in &stale {
        if sync.release_lock(&agent, *id, true)? {
            released += 1;
        }
    }

    Ok(CheckResult {
        name: "locks".to_string(),
        status: CheckStatus::Repaired(format!(
            "released {} of {} stale lock(s)",
            released,
            stale.len()
        )),
    })
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn print_result(result: &CheckResult) {
    let (tag, detail) = match &result.status {
        CheckStatus::Pass => ("PASS", String::new()),
        CheckStatus::Fail(d) => ("FAIL", d.clone()),
        CheckStatus::Repaired(d) => ("REPAIRED", d.clone()),
        CheckStatus::Skipped(d) => ("SKIPPED", d.clone()),
    };

    let tag_str = format!("[{}]", tag);
    if detail.is_empty() {
        println!("{:<12} {}", tag_str, result.name);
    } else {
        println!("{:<12} {:<12} {}", tag_str, result.name, detail);
    }
}

fn print_summary(results: &[CheckResult]) {
    let passed = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Pass))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Fail(_)))
        .count();
    let repaired = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Repaired(_)))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Skipped(_)))
        .count();

    let mut parts = Vec::new();
    if passed > 0 {
        parts.push(format!("{} passed", passed));
    }
    if failed > 0 {
        parts.push(format!("{} failed", failed));
    }
    if repaired > 0 {
        parts.push(format!("{} repaired", repaired));
    }
    if skipped > 0 {
        parts.push(format!("{} skipped", skipped));
    }

    println!("Integrity: {}", parts.join(", "));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_check_schema_pass() {
        let (db, _dir) = test_db();
        let result = check_schema(&db, false).unwrap();
        assert_eq!(result.name, "schema");
        assert!(matches!(result.status, CheckStatus::Pass));
    }

    #[test]
    fn test_check_counters_skipped_no_cache() {
        let (db, dir) = test_db();
        let crosslink_dir = dir.path();
        let result = check_counters(crosslink_dir, &db, false).unwrap();
        assert_eq!(result.name, "counters");
        assert!(matches!(result.status, CheckStatus::Skipped(_)));
    }

    #[test]
    fn test_check_counters_pass() {
        let (db, dir) = test_db();
        let crosslink_dir = dir.path();

        // Create cache dir and counters file
        let meta_dir = crosslink_dir.join(HUB_CACHE_DIR).join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        let counters = Counters {
            next_display_id: 1,
            next_comment_id: 1,
            next_milestone_id: 1,
        };
        write_counters(&meta_dir.join("counters.json"), &counters).unwrap();

        let result = check_counters(crosslink_dir, &db, false).unwrap();
        assert!(matches!(result.status, CheckStatus::Pass));
    }

    #[test]
    fn test_check_counters_fail_and_repair() {
        let (db, dir) = test_db();
        let crosslink_dir = dir.path();

        // Create an issue so max_display_id = 1
        db.create_issue("Test issue", None, "medium").unwrap();

        // Set counters too low
        let meta_dir = crosslink_dir.join(HUB_CACHE_DIR).join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        let counters = Counters {
            next_display_id: 1, // should be 2
            next_comment_id: 1,
            next_milestone_id: 1,
        };
        write_counters(&meta_dir.join("counters.json"), &counters).unwrap();

        // Check without repair — should fail
        let result = check_counters(crosslink_dir, &db, false).unwrap();
        assert!(matches!(result.status, CheckStatus::Fail(_)));

        // Check with repair — should fix
        let result = check_counters(crosslink_dir, &db, true).unwrap();
        assert!(matches!(result.status, CheckStatus::Repaired(_)));

        // Verify counter is now correct
        let fixed = read_counters(&meta_dir.join("counters.json")).unwrap();
        assert_eq!(fixed.next_display_id, 2);
    }

    #[test]
    fn test_check_hydration_skipped_no_cache() {
        let (db, dir) = test_db();
        let crosslink_dir = dir.path();
        let result = check_hydration(crosslink_dir, &db, false).unwrap();
        assert_eq!(result.name, "hydration");
        assert!(matches!(result.status, CheckStatus::Skipped(_)));
    }

    #[test]
    fn test_check_locks_skipped_no_sync() {
        let dir = tempdir().unwrap();
        let result = check_locks(dir.path(), false).unwrap();
        assert_eq!(result.name, "locks");
        assert!(matches!(result.status, CheckStatus::Skipped(_)));
    }

    #[test]
    fn test_print_summary_formatting() {
        let results = vec![
            CheckResult {
                name: "schema".to_string(),
                status: CheckStatus::Pass,
            },
            CheckResult {
                name: "counters".to_string(),
                status: CheckStatus::Fail("bad".to_string()),
            },
            CheckResult {
                name: "locks".to_string(),
                status: CheckStatus::Skipped("no sync".to_string()),
            },
        ];
        // Just verify it doesn't panic
        print_summary(&results);
    }
}
