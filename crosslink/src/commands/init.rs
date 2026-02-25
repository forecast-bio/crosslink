use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::db::Database;

// Embed hook files at compile time from resources/ (packaged with the crate)
const SETTINGS_JSON: &str = include_str!("../../resources/claude/settings.json");
const PROMPT_GUARD_PY: &str = include_str!("../../resources/claude/hooks/prompt-guard.py");
const POST_EDIT_CHECK_PY: &str = include_str!("../../resources/claude/hooks/post-edit-check.py");
const SESSION_START_PY: &str = include_str!("../../resources/claude/hooks/session-start.py");
const PRE_WEB_CHECK_PY: &str = include_str!("../../resources/claude/hooks/pre-web-check.py");
const WORK_CHECK_PY: &str = include_str!("../../resources/claude/hooks/work-check.py");

// Embed MCP server for safe web fetching
const SAFE_FETCH_SERVER_PY: &str = include_str!("../../resources/claude/mcp/safe-fetch-server.py");
const MCP_JSON: &str = include_str!("../../resources/mcp.json");

// Embed sanitization patterns
const SANITIZE_PATTERNS: &str =
    include_str!("../../resources/crosslink/rules/sanitize-patterns.txt");

// Embed hook configuration
const HOOK_CONFIG_JSON: &str = include_str!("../../resources/crosslink/hook-config.json");

// Embed tracking mode rule files
const RULE_TRACKING_STRICT: &str =
    include_str!("../../resources/crosslink/rules/tracking-strict.md");
const RULE_TRACKING_NORMAL: &str =
    include_str!("../../resources/crosslink/rules/tracking-normal.md");
const RULE_TRACKING_RELAXED: &str =
    include_str!("../../resources/crosslink/rules/tracking-relaxed.md");

// Embed rule files at compile time from resources/crosslink/rules/
const RULE_GLOBAL: &str = include_str!("../../resources/crosslink/rules/global.md");
const RULE_PROJECT: &str = include_str!("../../resources/crosslink/rules/project.md");
const RULE_RUST: &str = include_str!("../../resources/crosslink/rules/rust.md");
const RULE_PYTHON: &str = include_str!("../../resources/crosslink/rules/python.md");
const RULE_JAVASCRIPT: &str = include_str!("../../resources/crosslink/rules/javascript.md");
const RULE_TYPESCRIPT: &str = include_str!("../../resources/crosslink/rules/typescript.md");
const RULE_TYPESCRIPT_REACT: &str =
    include_str!("../../resources/crosslink/rules/typescript-react.md");
const RULE_JAVASCRIPT_REACT: &str =
    include_str!("../../resources/crosslink/rules/javascript-react.md");
const RULE_GO: &str = include_str!("../../resources/crosslink/rules/go.md");
const RULE_JAVA: &str = include_str!("../../resources/crosslink/rules/java.md");
const RULE_C: &str = include_str!("../../resources/crosslink/rules/c.md");
const RULE_CPP: &str = include_str!("../../resources/crosslink/rules/cpp.md");
const RULE_CSHARP: &str = include_str!("../../resources/crosslink/rules/csharp.md");
const RULE_RUBY: &str = include_str!("../../resources/crosslink/rules/ruby.md");
const RULE_PHP: &str = include_str!("../../resources/crosslink/rules/php.md");
const RULE_SWIFT: &str = include_str!("../../resources/crosslink/rules/swift.md");
const RULE_KOTLIN: &str = include_str!("../../resources/crosslink/rules/kotlin.md");
const RULE_SCALA: &str = include_str!("../../resources/crosslink/rules/scala.md");
const RULE_ZIG: &str = include_str!("../../resources/crosslink/rules/zig.md");
const RULE_ODIN: &str = include_str!("../../resources/crosslink/rules/odin.md");
const RULE_ELIXIR: &str = include_str!("../../resources/crosslink/rules/elixir.md");
const RULE_ELIXIR_PHOENIX: &str = include_str!("../../resources/crosslink/rules/elixir-phoenix.md");
const RULE_WEB: &str = include_str!("../../resources/crosslink/rules/web.md");

/// All rule files to deploy
const RULE_FILES: &[(&str, &str)] = &[
    ("global.md", RULE_GLOBAL),
    ("project.md", RULE_PROJECT),
    ("rust.md", RULE_RUST),
    ("python.md", RULE_PYTHON),
    ("javascript.md", RULE_JAVASCRIPT),
    ("typescript.md", RULE_TYPESCRIPT),
    ("typescript-react.md", RULE_TYPESCRIPT_REACT),
    ("javascript-react.md", RULE_JAVASCRIPT_REACT),
    ("go.md", RULE_GO),
    ("java.md", RULE_JAVA),
    ("c.md", RULE_C),
    ("cpp.md", RULE_CPP),
    ("csharp.md", RULE_CSHARP),
    ("ruby.md", RULE_RUBY),
    ("php.md", RULE_PHP),
    ("swift.md", RULE_SWIFT),
    ("kotlin.md", RULE_KOTLIN),
    ("scala.md", RULE_SCALA),
    ("zig.md", RULE_ZIG),
    ("odin.md", RULE_ODIN),
    ("elixir.md", RULE_ELIXIR),
    ("elixir-phoenix.md", RULE_ELIXIR_PHOENIX),
    ("web.md", RULE_WEB),
    ("sanitize-patterns.txt", SANITIZE_PATTERNS),
    ("tracking-strict.md", RULE_TRACKING_STRICT),
    ("tracking-normal.md", RULE_TRACKING_NORMAL),
    ("tracking-relaxed.md", RULE_TRACKING_RELAXED),
];

/// Merge crosslink's MCP server entries into an existing `.mcp.json`, or create it fresh.
/// Returns a list of warnings (e.g. overwritten keys) for the caller to display.
fn write_mcp_json_merged(mcp_path: &Path) -> Result<Vec<String>> {
    let embedded: serde_json::Value = serde_json::from_str(MCP_JSON)
        .context("embedded MCP_JSON is not valid JSON — this is a build defect")?;
    let src_servers = embedded
        .get("mcpServers")
        .and_then(|v| v.as_object())
        .context("embedded MCP_JSON missing mcpServers object — this is a build defect")?;

    let mut obj = match fs::read_to_string(mcp_path) {
        Ok(raw) => {
            let parsed: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
                format!(
                    "Existing .mcp.json at {} contains invalid JSON — \
                     refusing to overwrite. Fix or remove it, then retry.",
                    mcp_path.display()
                )
            })?;
            match parsed {
                serde_json::Value::Object(map) => map,
                _ => anyhow::bail!(
                    "Existing .mcp.json at {} is not a JSON object — \
                     refusing to overwrite. Fix or remove it, then retry.",
                    mcp_path.display()
                ),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(e) => return Err(anyhow::Error::from(e).context("Failed to read existing .mcp.json")),
    };

    let mut dest_map = match obj.remove("mcpServers") {
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => anyhow::bail!(
            "Existing .mcp.json has a non-object mcpServers value — \
             refusing to overwrite. Fix or remove it, then retry."
        ),
        None => serde_json::Map::new(),
    };

    let mut warnings = Vec::new();
    for (key, value) in src_servers {
        if dest_map.contains_key(key) {
            warnings.push(format!(
                "Warning: overwriting existing mcpServers entry \"{}\" with crosslink default",
                key
            ));
        }
        dest_map.insert(key.clone(), value.clone());
    }

    obj.insert("mcpServers".into(), serde_json::Value::Object(dest_map));

    let mut output = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
        .context("Failed to serialize .mcp.json")?;
    output.push('\n');
    fs::write(mcp_path, output).context("Failed to write .mcp.json")?;
    Ok(warnings)
}

pub fn run(path: &Path, force: bool) -> Result<()> {
    let crosslink_dir = path.join(".crosslink");
    let claude_dir = path.join(".claude");
    let hooks_dir = claude_dir.join("hooks");

    // Check if already initialized
    let crosslink_exists = crosslink_dir.exists();
    let claude_exists = claude_dir.exists();

    if crosslink_exists && claude_exists && !force {
        println!("Already initialized at {}", path.display());
        println!("Use --force to update hooks to latest version.");
        return Ok(());
    }

    let rules_dir = crosslink_dir.join("rules");

    // Create .crosslink directory and database
    if !crosslink_exists {
        fs::create_dir_all(&crosslink_dir).context("Failed to create .crosslink directory")?;

        let db_path = crosslink_dir.join("issues.db");
        Database::open(&db_path)?;
        println!("Created {}", crosslink_dir.display());
    }

    // Write hook config (create or update)
    let config_path = crosslink_dir.join("hook-config.json");
    if !config_path.exists() || force {
        fs::write(&config_path, HOOK_CONFIG_JSON).context("Failed to write hook-config.json")?;
    }

    // Write .crosslink/.gitignore for multi-agent files
    let crosslink_gitignore = crosslink_dir.join(".gitignore");
    if !crosslink_gitignore.exists() || force {
        fs::write(
            &crosslink_gitignore,
            "# Multi-agent collaboration (machine-local)\nagent.json\n.locks-cache/\n\n# Machine-local hook overrides\nhook-config.local.json\n",
        )
        .context("Failed to write .crosslink/.gitignore")?;
    }

    // Create or update rules directory
    let rules_exist = rules_dir.exists();
    if !rules_exist || force {
        fs::create_dir_all(&rules_dir).context("Failed to create .crosslink/rules directory")?;

        for (filename, content) in RULE_FILES {
            fs::write(rules_dir.join(filename), content)
                .with_context(|| format!("Failed to write {}", filename))?;
        }

        if force && rules_exist {
            println!("Updated {} with latest rules", rules_dir.display());
        } else {
            println!("Created {} with default rules", rules_dir.display());
        }
    }

    // Create .claude directory and hooks (or update if force)
    if !claude_exists || force {
        fs::create_dir_all(&hooks_dir).context("Failed to create .claude/hooks directory")?;

        // Write settings.json
        fs::write(claude_dir.join("settings.json"), SETTINGS_JSON)
            .context("Failed to write settings.json")?;

        // Write hook scripts
        fs::write(hooks_dir.join("prompt-guard.py"), PROMPT_GUARD_PY)
            .context("Failed to write prompt-guard.py")?;

        fs::write(hooks_dir.join("post-edit-check.py"), POST_EDIT_CHECK_PY)
            .context("Failed to write post-edit-check.py")?;

        fs::write(hooks_dir.join("session-start.py"), SESSION_START_PY)
            .context("Failed to write session-start.py")?;

        fs::write(hooks_dir.join("pre-web-check.py"), PRE_WEB_CHECK_PY)
            .context("Failed to write pre-web-check.py")?;

        fs::write(hooks_dir.join("work-check.py"), WORK_CHECK_PY)
            .context("Failed to write work-check.py")?;

        // Create MCP server directory and write safe-fetch server
        let mcp_dir = claude_dir.join("mcp");
        fs::create_dir_all(&mcp_dir).context("Failed to create .claude/mcp directory")?;
        fs::write(mcp_dir.join("safe-fetch-server.py"), SAFE_FETCH_SERVER_PY)
            .context("Failed to write safe-fetch-server.py")?;

        // Merge crosslink's MCP server entry into .mcp.json (preserving existing MCPs)
        let warnings =
            write_mcp_json_merged(&path.join(".mcp.json")).context("Failed to write .mcp.json")?;
        for warning in warnings {
            println!("{}", warning);
        }

        if force && claude_exists {
            println!("Updated {} with latest hooks", claude_dir.display());
        } else {
            println!("Created {} with Claude Code hooks", claude_dir.display());
        }
    }

    println!("Crosslink initialized successfully!");
    println!("\nNext steps:");
    println!("  crosslink session start     # Start a session");
    println!("  crosslink create \"Task\"     # Create an issue");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_run_fresh_init() {
        let dir = tempdir().unwrap();
        let result = run(dir.path(), false);
        assert!(result.is_ok());

        // Verify directories created
        assert!(dir.path().join(".crosslink").exists());
        assert!(dir.path().join(".crosslink/rules").exists());
        assert!(dir.path().join(".crosslink/issues.db").exists());
        assert!(dir.path().join(".claude").exists());
        assert!(dir.path().join(".claude/hooks").exists());
        assert!(dir.path().join(".claude/mcp").exists());
        assert!(dir.path().join(".crosslink/hook-config.json").exists());
    }

    #[test]
    fn test_run_creates_hook_files() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Verify hook files
        assert!(dir.path().join(".claude/settings.json").exists());
        assert!(dir.path().join(".claude/hooks/prompt-guard.py").exists());
        assert!(dir.path().join(".claude/hooks/post-edit-check.py").exists());
        assert!(dir.path().join(".claude/hooks/session-start.py").exists());
        assert!(dir.path().join(".claude/hooks/pre-web-check.py").exists());
        assert!(dir.path().join(".claude/hooks/work-check.py").exists());
        assert!(dir.path().join(".claude/mcp/safe-fetch-server.py").exists());
        assert!(dir.path().join(".mcp.json").exists());
    }

    #[test]
    fn test_run_creates_rule_files() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        let rules_dir = dir.path().join(".crosslink/rules");
        assert!(rules_dir.join("global.md").exists());
        assert!(rules_dir.join("project.md").exists());
        assert!(rules_dir.join("rust.md").exists());
        assert!(rules_dir.join("python.md").exists());
        assert!(rules_dir.join("javascript.md").exists());
        assert!(rules_dir.join("typescript.md").exists());
        assert!(rules_dir.join("tracking-strict.md").exists());
        assert!(rules_dir.join("tracking-normal.md").exists());
        assert!(rules_dir.join("tracking-relaxed.md").exists());
    }

    #[test]
    fn test_run_already_initialized_no_force() {
        let dir = tempdir().unwrap();

        // First init
        run(dir.path(), false).unwrap();

        // Second init without force - should succeed but not recreate
        let result = run(dir.path(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_force_update() {
        let dir = tempdir().unwrap();

        // First init
        run(dir.path(), false).unwrap();

        // Modify a hook file
        let hook_path = dir.path().join(".claude/hooks/prompt-guard.py");
        fs::write(&hook_path, "# modified").unwrap();

        // Force update
        run(dir.path(), true).unwrap();

        // Verify file was restored
        let content = fs::read_to_string(&hook_path).unwrap();
        assert_ne!(content, "# modified");
        assert!(content.contains("python") || content.contains("def") || content.len() > 20);
    }

    /// Keys that the embedded MCP_JSON is expected to manage.
    fn embedded_mcp_keys() -> Vec<String> {
        let embedded: serde_json::Value = serde_json::from_str(MCP_JSON).unwrap();
        embedded["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    #[test]
    fn test_force_init_preserves_existing_mcp_servers() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Add a custom MCP server entry alongside the embedded ones
        let mcp_path = dir.path().join(".mcp.json");
        let mut content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&mcp_path).unwrap()).unwrap();
        content["mcpServers"]["my-custom-server"] = serde_json::json!({
            "command": "node",
            "args": ["my-server.js"]
        });
        fs::write(&mcp_path, serde_json::to_string_pretty(&content).unwrap()).unwrap();

        // Force update
        run(dir.path(), true).unwrap();

        // Verify all embedded keys and the custom key are present
        let result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&mcp_path).unwrap()).unwrap();
        let servers = result["mcpServers"].as_object().unwrap();

        for key in embedded_mcp_keys() {
            assert!(
                servers.contains_key(&key),
                "embedded key \"{}\" should exist",
                key
            );
        }
        assert!(
            servers.contains_key("my-custom-server"),
            "custom server should be preserved"
        );
        assert_eq!(
            servers["my-custom-server"]["command"].as_str().unwrap(),
            "node"
        );
    }

    #[test]
    fn test_force_init_returns_warnings_for_overwritten_keys() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // The first init created .mcp.json with the embedded keys.
        // A second force init should warn about overwriting each one.
        let mcp_path = dir.path().join(".mcp.json");
        let warnings = write_mcp_json_merged(&mcp_path).unwrap();

        let expected_keys = embedded_mcp_keys();
        assert_eq!(
            warnings.len(),
            expected_keys.len(),
            "should warn once per embedded key"
        );
        for key in &expected_keys {
            assert!(
                warnings.iter().any(|w| w.contains(key)),
                "should warn about overwriting \"{}\"",
                key
            );
        }
    }

    #[test]
    fn test_write_mcp_json_merged_creates_fresh_file() {
        let dir = tempdir().unwrap();
        let mcp_path = dir.path().join(".mcp.json");

        // No pre-existing file
        assert!(!mcp_path.exists());

        let warnings = write_mcp_json_merged(&mcp_path).unwrap();
        assert!(
            warnings.is_empty(),
            "fresh creation should produce no warnings"
        );

        let content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&mcp_path).unwrap()).unwrap();
        let servers = content["mcpServers"].as_object().unwrap();

        // Should contain exactly the embedded keys
        let expected_keys = embedded_mcp_keys();
        assert_eq!(servers.len(), expected_keys.len());
        for key in &expected_keys {
            assert!(
                servers.contains_key(key),
                "fresh file should contain \"{}\"",
                key
            );
        }
    }

    #[test]
    fn test_force_init_fails_on_malformed_mcp_json() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Write invalid JSON to .mcp.json
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "not json {{{").unwrap();

        // Force init should fail, not silently overwrite
        let result = run(dir.path(), true);
        assert!(result.is_err());
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("invalid JSON"),
            "Error should mention invalid JSON, got: {}",
            err
        );

        // Original (broken) content should be untouched
        let content = fs::read_to_string(&mcp_path).unwrap();
        assert_eq!(content, "not json {{{");
    }

    #[test]
    fn test_force_init_fails_on_non_object_mcp_json() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Write a JSON array to .mcp.json
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "[1, 2, 3]").unwrap();

        // Force init should fail, not silently overwrite
        let result = run(dir.path(), true);
        assert!(result.is_err());
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("not a JSON object"),
            "Error should mention not a JSON object, got: {}",
            err
        );

        // Original content should be untouched
        let content = fs::read_to_string(&mcp_path).unwrap();
        assert_eq!(content, "[1, 2, 3]");
    }

    #[test]
    fn test_force_init_handles_empty_mcp_json_file() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Write empty file
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "").unwrap();

        // Should fail — empty file is not valid JSON
        let result = run(dir.path(), true);
        assert!(result.is_err());
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("invalid JSON"),
            "Error should mention invalid JSON, got: {}",
            err
        );
    }

    #[test]
    fn test_force_init_fails_on_non_object_mcp_servers_value() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Write valid JSON where mcpServers is a string instead of object
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, r#"{"mcpServers": "banana"}"#).unwrap();

        // Should fail, not silently replace
        let result = run(dir.path(), true);
        assert!(result.is_err());
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("non-object mcpServers"),
            "Error should mention non-object mcpServers, got: {}",
            err
        );

        // Original content should be untouched
        let content = fs::read_to_string(&mcp_path).unwrap();
        assert_eq!(content, r#"{"mcpServers": "banana"}"#);
    }

    #[test]
    fn test_init_merges_into_mcp_json_without_mcp_servers_key() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Write a valid object with no mcpServers key
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, r#"{"someOtherKey": true}"#).unwrap();

        // Force init should add mcpServers, preserving the other key
        run(dir.path(), true).unwrap();

        let content = fs::read_to_string(&mcp_path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["someOtherKey"], true);
        assert!(parsed["mcpServers"]["crosslink-safe-fetch"].is_object());
    }

    #[test]
    fn test_run_partial_init_crosslink_only() {
        let dir = tempdir().unwrap();

        // Create only .crosslink directory
        fs::create_dir_all(dir.path().join(".crosslink")).unwrap();

        let result = run(dir.path(), false);
        assert!(result.is_ok());

        // .claude should now exist
        assert!(dir.path().join(".claude").exists());
    }

    #[test]
    fn test_run_partial_init_claude_only() {
        let dir = tempdir().unwrap();

        // Create only .claude directory
        fs::create_dir_all(dir.path().join(".claude")).unwrap();

        let result = run(dir.path(), false);
        assert!(result.is_ok());

        // .crosslink should now exist
        assert!(dir.path().join(".crosslink").exists());
    }

    #[test]
    fn test_run_database_usable() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Open the created database and verify it works
        let db_path = dir.path().join(".crosslink/issues.db");
        let db = Database::open(&db_path).unwrap();

        // Should be able to create an issue
        let id = db.create_issue("Test issue", None, "medium").unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_run_rule_files_not_empty() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        let rules_dir = dir.path().join(".crosslink/rules");

        // Verify rule files have content
        let global = fs::read_to_string(rules_dir.join("global.md")).unwrap();
        assert!(!global.is_empty());

        let rust = fs::read_to_string(rules_dir.join("rust.md")).unwrap();
        assert!(!rust.is_empty());
    }

    #[test]
    fn test_run_force_updates_rules() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        // Modify a rule file
        let rule_path = dir.path().join(".crosslink/rules/global.md");
        fs::write(&rule_path, "# modified rule").unwrap();

        // Force update
        run(dir.path(), true).unwrap();

        // Verify file was restored
        let content = fs::read_to_string(&rule_path).unwrap();
        assert_ne!(content, "# modified rule");
    }

    #[test]
    fn test_run_idempotent_with_force() {
        let dir = tempdir().unwrap();

        // Multiple force runs should all succeed
        for _ in 0..3 {
            let result = run(dir.path(), true);
            assert!(result.is_ok());
        }

        // All files should still exist
        assert!(dir.path().join(".crosslink/issues.db").exists());
        assert!(dir.path().join(".claude/settings.json").exists());
    }

    #[test]
    fn test_embedded_constants_not_empty() {
        // Verify all embedded constants have content
        assert!(!SETTINGS_JSON.is_empty());
        assert!(!PROMPT_GUARD_PY.is_empty());
        assert!(!POST_EDIT_CHECK_PY.is_empty());
        assert!(!SESSION_START_PY.is_empty());
        assert!(!PRE_WEB_CHECK_PY.is_empty());
        assert!(!WORK_CHECK_PY.is_empty());
        assert!(!SAFE_FETCH_SERVER_PY.is_empty());
        assert!(!MCP_JSON.is_empty());
        assert!(!SANITIZE_PATTERNS.is_empty());
        assert!(!HOOK_CONFIG_JSON.is_empty());
        assert!(!RULE_TRACKING_STRICT.is_empty());
        assert!(!RULE_TRACKING_NORMAL.is_empty());
        assert!(!RULE_TRACKING_RELAXED.is_empty());
        assert!(!RULE_GLOBAL.is_empty());
        assert!(!RULE_RUST.is_empty());
    }

    #[test]
    fn test_rule_files_count() {
        // Verify we have the expected number of rule files
        assert!(RULE_FILES.len() >= 20);

        // All should have content
        for (name, content) in RULE_FILES {
            assert!(!name.is_empty(), "Rule file name should not be empty");
            assert!(
                !content.is_empty(),
                "Rule file {} should not be empty",
                name
            );
        }
    }

    #[test]
    fn test_gitignore_includes_local_config() {
        let dir = tempdir().unwrap();
        run(dir.path(), false).unwrap();

        let content = fs::read_to_string(dir.path().join(".crosslink/.gitignore")).unwrap();
        assert!(content.contains("agent.json"));
        assert!(content.contains(".locks-cache/"));
        assert!(content.contains("hook-config.local.json"));
    }
}
