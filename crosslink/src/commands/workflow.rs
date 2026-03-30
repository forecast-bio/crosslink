use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use super::init;
use crate::db::Database;
use crate::utils::format_issue_id;
use crate::WorkflowCommands;

pub fn run(
    command: WorkflowCommands,
    crosslink_dir: &Path,
    get_db: impl FnOnce() -> Result<Database>,
) -> Result<()> {
    let claude_dir = crosslink_dir
        .parent()
        .context("Cannot determine project root")?
        .join(".claude");
    match command {
        WorkflowCommands::Diff { section, check } => {
            diff(crosslink_dir, &claude_dir, section.as_deref(), check);
            Ok(())
        }
        WorkflowCommands::Trail { id, kind, json } => {
            let db = get_db()?;
            trail(&db, id, kind.as_deref(), json)
        }
    }
}

/// Hook files to compare: (deployed filename, embedded default)
const HOOK_FILES: &[(&str, &str)] = &[
    ("prompt-guard.py", init::PROMPT_GUARD_PY),
    ("post-edit-check.py", init::POST_EDIT_CHECK_PY),
    ("session-start.py", init::SESSION_START_PY),
    ("pre-web-check.py", init::PRE_WEB_CHECK_PY),
    ("work-check.py", init::WORK_CHECK_PY),
    ("heartbeat.py", init::HEARTBEAT_PY),
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
    fs::read_to_string(deployed_path).map_or(CompareResult::Missing, |content| {
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
            CompareResult::Customized(format!("customized ({total_diff} lines differ)"))
        }
    })
}

/// Format a `CompareResult` as a display string.
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

/// `crosslink workflow diff` — compare deployed policy files against embedded defaults.
///
/// When `check` is true, operates in CI mode: exits 0 if all drifted files are
/// marked with `# crosslink:custom`, exits 1 with a summary otherwise.
pub fn diff(crosslink_dir: &Path, claude_dir: &Path, section: Option<&str>, check: bool) {
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
        let rules_local_dir = crosslink_dir.join("rules.local");
        for (filename, default_content) in init::RULE_FILES {
            // Check if this rule is overridden by rules.local/
            let local_path = rules_local_dir.join(filename);
            if local_path.exists() {
                if !check {
                    println!("  rules/{filename}: overridden by rules.local/");
                }
                // Don't flag drift for files that have a local override
                continue;
            }
            let path = rules_dir.join(filename);
            let result = compare_file(&path, default_content);
            if !check {
                println!("  rules/{}: {}", filename, compare_display(&result));
            }
            if let CompareResult::Customized(_) = result {
                if check && !has_custom_marker(&path) {
                    drifted.push(format!(".crosslink/rules/{filename}"));
                }
            }
        }
        // Show additive local rules
        if rules_local_dir.is_dir() {
            let standard_files: std::collections::HashSet<&str> =
                init::RULE_FILES.iter().map(|(f, _)| *f).collect();
            if let Ok(entries) = std::fs::read_dir(&rules_local_dir) {
                let mut local_only: Vec<_> = entries
                    .filter_map(std::result::Result::ok)
                    .filter(|e| !standard_files.contains(e.file_name().to_str().unwrap_or("")))
                    .collect();
                local_only.sort_by_key(std::fs::DirEntry::file_name);
                for entry in &local_only {
                    let name = entry.file_name();
                    if !check {
                        println!("  rules.local/{}: additive", name.to_string_lossy());
                    }
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
                    drifted.push(format!(".claude/hooks/{filename}"));
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
                println!("  {path}");
            }
            println!();
            println!(
                "These files differ from crosslink defaults and are not marked with '{CUSTOM_MARKER}'."
            );
            println!(
                "Run 'crosslink workflow diff' for details, or add '{CUSTOM_MARKER}' to acknowledge."
            );
            std::process::exit(1);
        }
    }
}

/// `crosslink workflow trail <id>` — show chronological comment trail for an issue.
pub fn trail(db: &Database, id: i64, kind_filter: Option<&str>, json: bool) -> Result<()> {
    db.require_issue(id)?;

    let comments = db.get_comments(id)?;
    let filtered: Vec<_> = if let Some(kinds) = kind_filter {
        let kinds: Vec<&str> = kinds.split(',').map(str::trim).collect();
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
                (Some(trigger), Some(ctx)) => format!(" trigger={trigger} ctx=\"{ctx}\""),
                (Some(trigger), None) => format!(" trigger={trigger}"),
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

    /// Create a temp directory with a git repo and initial commit.
    fn test_dir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let init = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["init"])
            .output()
            .expect("git init failed");
        assert!(init.status.success(), "git init failed");
        // Use -c flags so identity works even when env vars or global config are absent
        let commit = std::process::Command::new("git")
            .current_dir(dir.path())
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@test",
                "commit",
                "--allow-empty",
                "-m",
                "init",
            ])
            .output()
            .expect("git commit failed");
        assert!(commit.status.success(), "git commit --allow-empty failed");
        dir
    }

    #[test]
    fn test_compare_file_matches() {
        let dir = tempdir().unwrap(); // This test doesn't call init — tempdir is fine
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
        let dir = test_dir();
        crate::commands::init::run(
            dir.path(),
            &crate::commands::init::InitOpts {
                force: false,
                update: false,
                dry_run: false,
                no_prompt: false,
                python_prefix: None,
                skip_cpitd: true,
                skip_signing: true,
                signing_key: None,
                reconfigure: false,
                defaults: true,
            },
        )
        .unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // Should not error
        diff(&crosslink_dir, &claude_dir, None, false);
    }

    #[test]
    fn test_diff_customized_file() {
        let dir = test_dir();
        crate::commands::init::run(
            dir.path(),
            &crate::commands::init::InitOpts {
                force: false,
                update: false,
                dry_run: false,
                no_prompt: false,
                python_prefix: None,
                skip_cpitd: true,
                skip_signing: true,
                signing_key: None,
                reconfigure: false,
                defaults: true,
            },
        )
        .unwrap();

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
        diff(&crosslink_dir, &claude_dir, Some("rules"), false);
    }

    #[test]
    fn test_diff_section_filter() {
        let dir = test_dir();
        crate::commands::init::run(
            dir.path(),
            &crate::commands::init::InitOpts {
                force: false,
                update: false,
                dry_run: false,
                no_prompt: false,
                python_prefix: None,
                skip_cpitd: true,
                skip_signing: true,
                signing_key: None,
                reconfigure: false,
                defaults: true,
            },
        )
        .unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // Each section should work independently
        diff(&crosslink_dir, &claude_dir, Some("tracking"), false);
        diff(&crosslink_dir, &claude_dir, Some("hooks"), false);
        diff(&crosslink_dir, &claude_dir, Some("languages"), false);
    }

    #[test]
    fn test_init_creates_commands_dir() {
        let dir = test_dir();
        crate::commands::init::run(
            dir.path(),
            &crate::commands::init::InitOpts {
                force: false,
                update: false,
                dry_run: false,
                no_prompt: false,
                python_prefix: None,
                skip_cpitd: true,
                skip_signing: true,
                signing_key: None,
                reconfigure: false,
                defaults: true,
            },
        )
        .unwrap();

        assert!(dir.path().join(".claude/commands/workflow.md").exists());
        let content = fs::read_to_string(dir.path().join(".claude/commands/workflow.md")).unwrap();
        assert!(content.contains("policy review"));
    }

    #[test]
    fn test_check_passes_when_defaults_match() {
        let dir = test_dir();
        crate::commands::init::run(
            dir.path(),
            &crate::commands::init::InitOpts {
                force: false,
                update: false,
                dry_run: false,
                no_prompt: false,
                python_prefix: None,
                skip_cpitd: true,
                skip_signing: true,
                signing_key: None,
                reconfigure: false,
                defaults: true,
            },
        )
        .unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // All files match defaults, so --check should pass (exit 0)
        diff(&crosslink_dir, &claude_dir, None, true);
    }

    #[test]
    fn test_check_passes_with_custom_marker() {
        let dir = test_dir();
        crate::commands::init::run(
            dir.path(),
            &crate::commands::init::InitOpts {
                force: false,
                update: false,
                dry_run: false,
                no_prompt: false,
                python_prefix: None,
                skip_cpitd: true,
                skip_signing: true,
                signing_key: None,
                reconfigure: false,
                defaults: true,
            },
        )
        .unwrap();

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
        diff(&crosslink_dir, &claude_dir, Some("rules"), true);
    }

    #[test]
    fn test_has_custom_marker_present() {
        let dir = test_dir();
        let path = dir.path().join("test.txt");
        fs::write(&path, "some content\n# crosslink:custom\nmore content").unwrap();
        assert!(has_custom_marker(&path));
    }

    #[test]
    fn test_has_custom_marker_absent() {
        let dir = test_dir();
        let path = dir.path().join("test.txt");
        fs::write(&path, "some content\nno marker here").unwrap();
        assert!(!has_custom_marker(&path));
    }

    #[test]
    fn test_has_custom_marker_missing_file() {
        let dir = test_dir();
        let path = dir.path().join("nonexistent.txt");
        assert!(!has_custom_marker(&path));
    }

    // ==================== Trail Tests ====================

    fn setup_trail_db() -> (Database, tempfile::TempDir) {
        let dir = test_dir();
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
