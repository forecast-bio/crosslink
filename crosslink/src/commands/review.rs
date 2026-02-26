use anyhow::Result;
use std::fs;
use std::path::Path;

use super::init;

/// Hook files to compare: (deployed filename, embedded default)
const HOOK_FILES: &[(&str, &str)] = &[
    ("prompt-guard.py", init::PROMPT_GUARD_PY),
    ("post-edit-check.py", init::POST_EDIT_CHECK_PY),
    ("session-start.py", init::SESSION_START_PY),
    ("pre-web-check.py", init::PRE_WEB_CHECK_PY),
    ("work-check.py", init::WORK_CHECK_PY),
];

/// Compare a deployed file against its embedded default.
/// Returns a description string.
fn compare_file(deployed_path: &Path, default_content: &str) -> String {
    match fs::read_to_string(deployed_path) {
        Ok(content) => {
            if content == default_content {
                "matches default".to_string()
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
                format!("customized ({} lines differ)", total_diff)
            }
        }
        Err(_) => "missing (not deployed)".to_string(),
    }
}

/// `crosslink review diff` — compare deployed policy files against embedded defaults.
pub fn diff(crosslink_dir: &Path, claude_dir: &Path, section: Option<&str>) -> Result<()> {
    let show_all = section.is_none();

    // --- Tracking Mode ---
    if show_all || section == Some("tracking") {
        println!("=== Tracking Mode ===");
        let config_path = crosslink_dir.join("hook-config.json");
        let status = compare_file(&config_path, init::HOOK_CONFIG_JSON);
        // Also extract the current tracking mode if customized
        if status.starts_with("customized") {
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
                        status, mode, default
                    );
                } else {
                    println!("  hook-config.json: {}", status);
                }
            } else {
                println!("  hook-config.json: {}", status);
            }
        } else {
            println!("  hook-config.json: {}", status);
        }
        println!();
    }

    // --- Rules ---
    if show_all || section == Some("rules") || section == Some("languages") {
        println!("=== Rules ===");
        let rules_dir = crosslink_dir.join("rules");
        for (filename, default_content) in init::RULE_FILES {
            let path = rules_dir.join(filename);
            let status = compare_file(&path, default_content);
            println!("  rules/{}: {}", filename, status);
        }
        println!();
    }

    // --- Hooks ---
    if show_all || section == Some("hooks") {
        println!("=== Hooks ===");
        let hooks_dir = claude_dir.join("hooks");
        for (filename, default_content) in HOOK_FILES {
            let path = hooks_dir.join(filename);
            let status = compare_file(&path, default_content);
            println!("  .claude/hooks/{}: {}", filename, status);
        }
        println!();
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
        assert_eq!(compare_file(&path, "hello world"), "matches default");
    }

    #[test]
    fn test_compare_file_customized() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello modified\nextra line").unwrap();
        let result = compare_file(&path, "hello world");
        assert!(result.starts_with("customized"));
        assert!(result.contains("lines differ"));
    }

    #[test]
    fn test_compare_file_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.txt");
        assert_eq!(compare_file(&path, "content"), "missing (not deployed)");
    }

    #[test]
    fn test_diff_defaults_match() {
        // Init a fresh crosslink dir, then diff — everything should match
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false).unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // Should not error
        diff(&crosslink_dir, &claude_dir, None).unwrap();
    }

    #[test]
    fn test_diff_customized_file() {
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false).unwrap();

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
        diff(&crosslink_dir, &claude_dir, Some("rules")).unwrap();
    }

    #[test]
    fn test_diff_section_filter() {
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false).unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        let claude_dir = dir.path().join(".claude");

        // Each section should work independently
        diff(&crosslink_dir, &claude_dir, Some("tracking")).unwrap();
        diff(&crosslink_dir, &claude_dir, Some("hooks")).unwrap();
        diff(&crosslink_dir, &claude_dir, Some("languages")).unwrap();
    }

    #[test]
    fn test_init_creates_commands_dir() {
        let dir = tempdir().unwrap();
        crate::commands::init::run(dir.path(), false).unwrap();

        assert!(dir.path().join(".claude/commands/review.md").exists());
        let content = fs::read_to_string(dir.path().join(".claude/commands/review.md")).unwrap();
        assert!(content.contains("policy review"));
    }
}
