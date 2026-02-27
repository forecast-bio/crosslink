use anyhow::Result;
use std::fs;
use std::path::Path;

use super::init;
use crate::db::Database;
use crate::utils::format_issue_id;

/// Hook files to compare: (deployed filename, embedded default)
const HOOK_FILES: &[(&str, &str)] = &[
    ("prompt-guard.py", init::PROMPT_GUARD_PY),
    ("post-edit-check.py", init::POST_EDIT_CHECK_PY),
    ("session-start.py", init::SESSION_START_PY),
    ("pre-web-check.py", init::PRE_WEB_CHECK_PY),
    ("work-check.py", init::WORK_CHECK_PY),
];

/// The marker comment that acknowledges intentional customization.
const CUSTOM_MARKER: &str = "# crosslink:custom";

/// Result of comparing a deployed file against its embedded default.
enum CompareResult {
    /// File matches the embedded default exactly.
    Matches,
    /// File differs from the default. Contains a human-readable description.
    Customized(String),
    /// File is missing (not deployed).
    Missing,
}

/// Compare a deployed file against its embedded default.
fn compare_file(deployed_path: &Path, default_content: &str) -> CompareResult {
    match fs::read_to_string(deployed_path) {
        Ok(content) => {
            if content == default_content {
                CompareResult::Matches
            } else {
                let diff_lines = content
                    .lines()
                    .zip(default_content.lines())
                    .filter(|(a, b)| a != b)
                    .count();
                let len_diff = content
                    .lines()
                    .count()
                    .abs_diff(default_content.lines().count());
                let total_diff = diff_lines + len_diff;
                CompareResult::Customized(format!("customized ({} lines differ)", total_diff))
            }
        }
        Err(_) => CompareResult::Missing,
    }
}

/// Format a CompareResult as a display string.
fn compare_display(result: &CompareResult) -> &str {
    match result {
        CompareResult::Matches => "matches default",
        CompareResult::Customized(desc) => desc,
        CompareResult::Missing => "missing (not deployed)",
    }
}

/// Check whether a deployed file contains the `# crosslink:custom` marker.
fn has_custom_marker(deployed_path: &Path) -> bool {
    fs::read_to_string(deployed_path)
        .map(|content| content.contains(CUSTOM_MARKER))
        .unwrap_or(false)
}

/// `crosslink review diff` — compare deployed policy files against embedded defaults.
///
/// When `check` is true, operates in CI mode: exits 0 if all drifted files are
/// marked with `# crosslink:custom`, exits 1 with a summary otherwise.
pub fn diff(
    crosslink_dir: &Path,
    claude_dir: &Path,
    section: Option<&str>,
    check: bool,
) -> Result<()> {
    let show_all = section.is_none();
    let mut drifted: Vec<String> = Vec::new();

    // --- Tracking Mode ---
    if show_all || section == Some("tracking") {
        let config_path = crosslink_dir.join("hook-config.json");
        let result = compare_file(&config_path, init::HOOK_CONFIG_JSON);

        if !check {
            println!("=== Tracking Mode ===");
            if let CompareResult::Customized(_) = &result {
                if let Ok(content) = fs::read_to_string(&config_path) {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                        let mode = parsed
                            .get("tracking_mode")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let default_mode: serde_json::Value =
                            serde_json::from_str(init::HOOK_CONFIG_JSON).unwrap_or_default();
                        let default = default_mode
                            .get("tracking_mode")
                            .and_then(|v| v.as_str())
                            .unwrap_or("strict");
                        println!(
                            "  hook-config.json: {} (tracking_mode: \"{}\", default: \"{}\")",
                            compare_display(&result),
                            mode,
                            default
                        );
                    } else {
                        println!("  hook-config.json: {}", compare_display(&result));
                    }
                } else {
                    println!("  hook-config.json: {}", compare_display(&result));
                }
            } else {
                println!("  hook-config.json: {}", compare_display(&result));
            }
            println!();
        }

        if let CompareResult::Customized(_) = result {
            if check && !has_custom_marker(&config_path) {
                drifted.push(".crosslink/hook-config.json".to_string());
            }
        }
    }

    // --- Rules ---
    if show_all || section == Some("rules") || section == Some("languages") {
        if !check {
            println!("=== Rules ===");
        }
        let rules_dir = crosslink_dir.join("rules");
        for (filename, default_content) in init::RULE_FILES {
            let path = rules_dir.join(filename);
            let result = compare_file(&path, default_content);
            if !check {
                println!("  rules/{}: {}", filename, compare_display(&result));
            }
            if let CompareResult::Customized(_) = result {
                if check && !has_custom_marker(&path) {
                    drifted.push(format!(".crosslink/rules/{}", filename));
                }
            }
        }
        if !check {
            println!();
        }
    }

    // --- Hooks ---
    if show_all || section == Some("hooks") {
        if !check {
            println!("=== Hooks ===");
        }
        let hooks_dir = claude_dir.join("hooks");
        for (filename, default_content) in HOOK_FILES {
            let path = hooks_dir.join(filename);
            let result = compare_file(&path, default_content);
            if !check {
                println!("  .claude/hooks/{}: {}", filename, compare_display(&result));
            }
            if let CompareResult::Customized(_) = result {
                if check && !has_custom_marker(&path) {
                    drifted.push(format!(".claude/hooks/{}", filename));
                }
            }
        }
        if !check {
            println!();
        }
    }

    if check {
        if drifted.is_empty() {
            println!("All policy files are up to date or explicitly customized.");
        } else {
            println!(
                "Policy drift detected ({} file{}):",
                drifted.len(),
                if drifted.len() == 1 { "" } else { "s" }
            );
            for path in &drifted {
                println!("  {}", path);
            }
            println!();
            println!(
                "These files differ from crosslink defaults and are not marked with '{}'.",
                CUSTOM_MARKER
            );
            println!(
                "Run 'crosslink review diff' for details, or add '{}' to acknowledge.",
                CUSTOM_MARKER
            );
            std::process::exit(1);
        }
    }

    Ok(())
}

/// `crosslink review trail <id>` — show chronological comment trail for an issue.
pub fn trail(db: &Database, id: i64, kind_filter: Option<&str>, json: bool) -> Result<()> {
    db.require_issue(id)?;

    let comments = db.get_comments(id)?;
    let filtered: Vec<_> = if let Some(kinds) = kind_filter {
        let kinds: Vec<&str> = kinds.split(',').map(|s| s.trim()).collect();
        comments
            .into_iter()
            .filter(|c| kinds.contains(&c.kind.as_str()))
            .collect()
    } else {
        comments
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&filtered)?);
    } else {
        println!("Comment trail for issue {}:", format_issue_id(id));
        println!();
        for comment in &filtered {
            let intervention_info = match (&comment.trigger_type, &comment.intervention_context) {
                (Some(trigger), Some(ctx)) => format!(" trigger={} ctx=\"{}\"", trigger, ctx),
                (Some(trigger), None) => format!(" trigger={}", trigger),
                _ => String::new(),
            };
            println!(
                "  [{}] [{}{}] {}",
                comment.created_at.format("%Y-%m-%d %H:%M"),
                comment.kind,
                intervention_info,
                comment.content
            );
        }
        if filtered.is_empty() {
            println!("  No comments found.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_compare_file_matches() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();
        assert!(matches!(
            compare_file(&path, "hello world"),
            CompareResult::Matches
        ));
    }

    #[test]
    fn test_compare_file_customized() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello modified\nextra line").unwrap();
        let result = compare_file(&path, "hello world");
        match result {
            CompareResult::Customized(desc) => assert!(desc.contains("lines differ")),
            _ => panic!("expected Customized"),
        }
    }

    #[test]
    fn test_compare_file_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.txt");
        assert!(matches!(
            compare_file(&path, "content"),
            CompareResult::Missing
        ));
    }

    #[test]
    fn test_diff_defaults_match() {
        // Init a fresh crosslink dir, then diff — everything should match
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false, None, true).unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // Should not error
        diff(&crosslink_dir, &claude_dir, None, false).unwrap();
    }

    #[test]
    fn test_diff_customized_file() {
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false, None, true).unwrap();

        // Modify a rule file
        let rule_path = dir.path().join(".crosslink/rules/global.md");
        fs::write(
            &rule_path,
            "# My custom global rules\nDifferent content here.",
        )
        .unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // Should not error — just prints customized status
        diff(&crosslink_dir, &claude_dir, Some("rules"), false).unwrap();
    }

    #[test]
    fn test_diff_section_filter() {
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false, None, true).unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // Each section should work independently
        diff(&crosslink_dir, &claude_dir, Some("tracking"), false).unwrap();
        diff(&crosslink_dir, &claude_dir, Some("hooks"), false).unwrap();
        diff(&crosslink_dir, &claude_dir, Some("languages"), false).unwrap();
    }

    #[test]
    fn test_init_creates_commands_dir() {
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false, None, true).unwrap();

        assert!(dir.path().join(".claude/commands/review.md").exists());
        let content = fs::read_to_string(dir.path().join(".claude/commands/review.md")).unwrap();
        assert!(content.contains("policy review"));
    }

    #[test]
    fn test_check_passes_when_defaults_match() {
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false, None, true).unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // All files match defaults, so --check should pass (exit 0)
        diff(&crosslink_dir, &claude_dir, None, true).unwrap();
    }

    #[test]
    fn test_check_passes_with_custom_marker() {
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false, None, true).unwrap();

        // Modify a rule file but add the custom marker
        let rule_path = dir.path().join(".crosslink/rules/global.md");
        fs::write(
            &rule_path,
            "# crosslink:custom\n# My custom global rules\nDifferent content here.",
        )
        .unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // Should pass because the file is marked as custom
        diff(&crosslink_dir, &claude_dir, Some("rules"), true).unwrap();
    }

    #[test]
    fn test_has_custom_marker_present() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "some content\n# crosslink:custom\nmore content").unwrap();
        assert!(has_custom_marker(&path));
    }

    #[test]
    fn test_has_custom_marker_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "some content\nno marker here").unwrap();
        assert!(!has_custom_marker(&path));
    }

    #[test]
    fn test_has_custom_marker_missing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.txt");
        assert!(!has_custom_marker(&path));
    }

    // ==================== Trail Tests ====================

    fn setup_trail_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_trail_no_comments() {
        let (db, _dir) = setup_trail_db();
        let id = db.create_issue("Test", None, "medium").unwrap();
        let result = trail(&db, id, None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_trail_with_comments() {
        let (db, _dir) = setup_trail_db();
        let id = db.create_issue("Test", None, "medium").unwrap();
        db.add_comment(id, "Plan: do the thing", "plan").unwrap();
        db.add_comment(id, "Decision: chose X", "decision").unwrap();
        db.add_comment(id, "Result: tests pass", "result").unwrap();

        let result = trail(&db, id, None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_trail_kind_filter() {
        let (db, _dir) = setup_trail_db();
        let id = db.create_issue("Test", None, "medium").unwrap();
        db.add_comment(id, "Plan: do the thing", "plan").unwrap();
        db.add_comment(id, "A regular note", "note").unwrap();
        db.add_comment(id, "Decision: chose X", "decision").unwrap();

        // Filter to only plan and decision
        let result = trail(&db, id, Some("plan,decision"), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_trail_json_output() {
        let (db, _dir) = setup_trail_db();
        let id = db.create_issue("Test", None, "medium").unwrap();
        db.add_comment(id, "Plan: approach", "plan").unwrap();

        let result = trail(&db, id, None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_trail_nonexistent_issue() {
        let (db, _dir) = setup_trail_db();
        let result = trail(&db, 99999, None, false);
        assert!(result.is_err());
    }
}
