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
        Some(IntegrityCommands::Layout { repair }) => {
            let result = check_layout(crosslink_dir, *repair)?;
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
        check_layout(crosslink_dir, false)?,
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
    if sync.is_v2_layout() {
        if let Ok(Some(writer)) = crate::shared_writer::SharedWriter::new(crosslink_dir) {
            for (id, stale_agent_id) in &stale {
                match writer.force_release_lock_v2(*id, stale_agent_id) {
                    Ok(_) => released += 1,
                    Err(e) => tracing::warn!("Could not release stale lock #{}: {}", id, e),
                }
            }
        }
    } else {
        for (id, _) in &stale {
            if sync.release_lock(&agent, *id, true)? {
                released += 1;
            }
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
// Layout check: detect mixed V1/V2 issue files
// ---------------------------------------------------------------------------

fn check_layout(crosslink_dir: &Path, repair: bool) -> Result<CheckResult> {
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let issues_dir = cache_dir.join("issues");

    if !issues_dir.exists() {
        return Ok(CheckResult {
            name: "layout".to_string(),
            status: CheckStatus::Skipped("no issues directory".to_string()),
        });
    }

    // Scan for V1 flat files and V2 directories
    let mut v1_uuids: Vec<String> = Vec::new();
    let mut v2_uuids: Vec<String> = Vec::new();
    let mut both_uuids: Vec<String> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&issues_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if path.is_file() && name.ends_with(".json") {
                let uuid = name.trim_end_matches(".json").to_string();
                v1_uuids.push(uuid);
            } else if path.is_dir() && path.join("issue.json").exists() {
                v2_uuids.push(name);
            }
        }
    }

    // Find UUIDs that exist in both formats
    let v1_set: std::collections::HashSet<&str> = v1_uuids.iter().map(|s| s.as_str()).collect();
    let v2_set: std::collections::HashSet<&str> = v2_uuids.iter().map(|s| s.as_str()).collect();
    for uuid in &v1_set {
        if v2_set.contains(uuid) {
            both_uuids.push(uuid.to_string());
        }
    }

    // Check version marker consistency
    let meta_dir = cache_dir.join("meta");
    let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
    let v1_only: Vec<&str> = v1_uuids
        .iter()
        .filter(|u| !v2_set.contains(u.as_str()))
        .map(|s| s.as_str())
        .collect();

    let has_problems = !both_uuids.is_empty() || (version >= 2 && !v1_only.is_empty());

    if !has_problems {
        return Ok(CheckResult {
            name: "layout".to_string(),
            status: CheckStatus::Pass,
        });
    }

    let mut issues_desc = Vec::new();
    if !both_uuids.is_empty() {
        issues_desc.push(format!(
            "{} UUID(s) have both V1 and V2 files",
            both_uuids.len()
        ));
    }
    if version >= 2 && !v1_only.is_empty() {
        issues_desc.push(format!("{} V1 flat file(s) on a V2 hub", v1_only.len()));
    }

    if !repair {
        return Ok(CheckResult {
            name: "layout".to_string(),
            status: CheckStatus::Fail(issues_desc.join("; ")),
        });
    }

    // Repair: migrate V1 → V2 and remove stale V1 duplicates
    let mut migrated = 0;
    let mut cleaned = 0;

    // Remove V1 files that have V2 equivalents (stale duplicates)
    for uuid in &both_uuids {
        let v1_path = issues_dir.join(format!("{}.json", uuid));
        if v1_path.exists() {
            let _ = std::fs::remove_file(&v1_path);
            cleaned += 1;
        }
    }

    // Migrate V1-only files to V2 format (when hub is V2)
    if version >= 2 {
        for uuid in &v1_only {
            let v1_path = issues_dir.join(format!("{}.json", uuid));
            let v2_dir = issues_dir.join(uuid);
            let v2_path = v2_dir.join("issue.json");

            if v1_path.exists() && !v2_path.exists() {
                if let Ok(content) = std::fs::read(&v1_path) {
                    if std::fs::create_dir_all(&v2_dir).is_ok()
                        && std::fs::write(&v2_path, &content).is_ok()
                    {
                        let _ = std::fs::remove_file(&v1_path);
                        migrated += 1;
                    }
                }
            }
        }
    }

    // Ensure version marker exists
    if !meta_dir.join("version.json").exists() {
        let _ = crate::issue_file::write_layout_version(
            &meta_dir,
            crate::issue_file::CURRENT_LAYOUT_VERSION,
        );
    }

    let mut repair_desc = Vec::new();
    if cleaned > 0 {
        repair_desc.push(format!("{} stale V1 duplicate(s) removed", cleaned));
    }
    if migrated > 0 {
        repair_desc.push(format!("{} V1 file(s) migrated to V2", migrated));
    }

    Ok(CheckResult {
        name: "layout".to_string(),
        status: CheckStatus::Repaired(repair_desc.join("; ")),
    })
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
