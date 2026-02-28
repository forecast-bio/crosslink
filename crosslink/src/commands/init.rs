use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::db::Database;

/// Detect the Python invocation prefix for hook commands based on project toolchain markers.
///
/// Checks (in priority order):
/// 1. `uv.lock` or `pyproject.toml` with `[tool.uv]` → `"uv run python3"`
/// 2. `poetry.lock` or `pyproject.toml` with `[tool.poetry]` → `"poetry run python3"`
/// 3. `.venv/` directory → `".venv/bin/python3"`
/// 4. `Pipfile` or `Pipfile.lock` → `"pipenv run python3"`
/// 5. Fallback → `"python3"`
pub fn detect_python_prefix(project_root: &Path) -> String {
    // 1. uv: check uv.lock or [tool.uv] in pyproject.toml
    if project_root.join("uv.lock").exists() {
        return "uv run python3".to_string();
    }
    if let Some(ref pyproject) = read_pyproject(project_root) {
        if pyproject.contains("[tool.uv]") {
            return "uv run python3".to_string();
        }
    }

    // 2. poetry: check poetry.lock or [tool.poetry] in pyproject.toml
    if project_root.join("poetry.lock").exists() {
        return "poetry run python3".to_string();
    }
    if let Some(ref pyproject) = read_pyproject(project_root) {
        if pyproject.contains("[tool.poetry]") {
            return "poetry run python3".to_string();
        }
    }

    // 3. local venv
    if project_root.join(".venv").is_dir() {
        return ".venv/bin/python3".to_string();
    }

    // 4. pipenv
    if project_root.join("Pipfile").exists() || project_root.join("Pipfile.lock").exists() {
        return "pipenv run python3".to_string();
    }

    // 5. system default
    "python3".to_string()
}

/// Read pyproject.toml contents, returning None if it doesn't exist or can't be read.
fn read_pyproject(project_root: &Path) -> Option<String> {
    fs::read_to_string(project_root.join("pyproject.toml")).ok()
}

/// Check if cpitd is already available on PATH.
fn cpitd_is_installed() -> bool {
    std::process::Command::new("cpitd")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const CPITD_REPO_URL: &str = "https://github.com/scythia-marrow/cpitd.git";

/// Install cpitd using the detected Python toolchain.
/// Returns Ok(true) if installed, Ok(false) if already present, Err on failure.
///
/// Tries `pip install cpitd` first (PyPI). If that fails, falls back to
/// cloning the git repo into a temp directory and installing from source.
fn install_cpitd(python_prefix: &str) -> Result<bool> {
    if cpitd_is_installed() {
        return Ok(false);
    }

    // First attempt: install from PyPI
    let pypi_result = install_cpitd_from_pypi(python_prefix);
    if let Ok(true) = pypi_result {
        return Ok(true);
    }

    // Second attempt: clone repo and install from source
    println!("  PyPI install failed, trying install from source...");
    install_cpitd_from_source(python_prefix)
}

/// Try installing cpitd from PyPI via pip/uv/poetry.
fn install_cpitd_from_pypi(python_prefix: &str) -> Result<bool> {
    if python_prefix.starts_with("uv ") {
        return run_install_command("uv", &["pip", "install", "cpitd"]);
    }
    if python_prefix.starts_with("poetry ") {
        return run_install_command("poetry", &["add", "--group", "dev", "cpitd"]);
    }
    if python_prefix.starts_with(".venv/") {
        let pip = python_prefix
            .replace("python3", "pip")
            .replace("python", "pip");
        return run_install_command(&pip, &["install", "cpitd"]);
    }
    if python_prefix.starts_with("pipenv ") {
        return run_install_command("pipenv", &["install", "--dev", "cpitd"]);
    }

    // Fallback: system python
    run_install_command("python3", &["-m", "pip", "install", "cpitd"])
}

/// Clone the cpitd repo to a temp directory and install from source.
fn install_cpitd_from_source(python_prefix: &str) -> Result<bool> {
    let tmp_dir = std::env::temp_dir().join("crosslink-cpitd-install");

    // Clean up any previous failed attempt
    if tmp_dir.exists() {
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    // Clone the repo
    let clone_output = std::process::Command::new("git")
        .args(["clone", "--depth", "1", CPITD_REPO_URL])
        .arg(&tmp_dir)
        .output()
        .context("Failed to run git clone for cpitd")?;

    if !clone_output.status.success() {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);
        // Clean up on failure
        let _ = fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("git clone failed: {}", stderr.trim());
    }

    let tmp_dir_str = tmp_dir.to_string_lossy();

    // Install from the cloned directory
    let result = if python_prefix.starts_with("uv ") {
        run_install_command("uv", &["pip", "install", &tmp_dir_str])
    } else if python_prefix.starts_with("poetry ") {
        // Poetry can't install from arbitrary paths into dev deps easily,
        // fall back to pip inside the poetry env
        run_install_command("poetry", &["run", "pip", "install", &tmp_dir_str])
    } else if python_prefix.starts_with(".venv/") {
        let pip = python_prefix
            .replace("python3", "pip")
            .replace("python", "pip");
        run_install_command(&pip, &["install", &tmp_dir_str])
    } else if python_prefix.starts_with("pipenv ") {
        run_install_command("pipenv", &["run", "pip", "install", &tmp_dir_str])
    } else {
        run_install_command("python3", &["-m", "pip", "install", &tmp_dir_str])
    };

    // Clean up cloned repo
    let _ = fs::remove_dir_all(&tmp_dir);

    result
}

fn run_install_command(program: &str, args: &[&str]) -> Result<bool> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run {} {}", program, args.join(" ")))?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("cpitd install failed: {}", stderr.trim());
    }
}

/// Detect or configure the driver's SSH signing key.
///
/// If `signing_key` is provided, uses that path. Otherwise checks for an
/// existing git signing key, then falls back to common SSH key locations.
/// Stores the driver's public key at `.crosslink/driver-key.pub`.
fn setup_driver_signing(project_root: &Path, signing_key: Option<&str>) -> Result<()> {
    use crate::signing;

    let crosslink_dir = project_root.join(".crosslink");
    let driver_pub_path = crosslink_dir.join("driver-key.pub");

    // If driver key already configured and not forcing, skip
    if driver_pub_path.exists() {
        return Ok(());
    }

    // Find the key to use
    let pubkey_path = if let Some(key_path) = signing_key {
        // Explicit --signing-key flag
        let p = std::path::PathBuf::from(key_path);
        if !p.exists() {
            println!("Warning: Signing key not found at {}", key_path);
            return Ok(());
        }
        Some(p)
    } else {
        // Try git's configured signing key first, then default SSH keys
        signing::find_git_signing_key().or_else(signing::find_default_ssh_key)
    };

    let pubkey_path = match pubkey_path {
        Some(p) => p,
        None => {
            println!("No SSH key found. Signing setup skipped.");
            println!("  Generate one with: ssh-keygen -t ed25519");
            println!("  Then re-run: crosslink init --force");
            return Ok(());
        }
    };

    // Ensure it's a public key (not private)
    let pubkey_path = if !pubkey_path.to_string_lossy().ends_with(".pub") {
        let pub_variant = std::path::PathBuf::from(format!("{}.pub", pubkey_path.display()));
        if pub_variant.exists() {
            pub_variant
        } else {
            pubkey_path
        }
    } else {
        pubkey_path
    };

    match signing::read_public_key(&pubkey_path) {
        Ok(public_key) => {
            // Copy driver public key into .crosslink/
            fs::write(&driver_pub_path, &public_key).context("Failed to write driver-key.pub")?;

            // Get fingerprint for display
            match signing::get_key_fingerprint(&pubkey_path) {
                Ok(fp) => println!("Driver signing key: {} ({})", fp, pubkey_path.display()),
                Err(_) => println!("Driver signing key: {}", pubkey_path.display()),
            }

            // Configure git to use SSH signing in this repo
            let private_key_path = if pubkey_path.to_string_lossy().ends_with(".pub") {
                let s = pubkey_path.to_string_lossy();
                std::path::PathBuf::from(&s[..s.len() - 4])
            } else {
                pubkey_path.clone()
            };

            if private_key_path.exists() {
                if let Err(e) =
                    signing::configure_git_ssh_signing(project_root, &private_key_path, None)
                {
                    println!("Warning: Could not configure git signing: {}", e);
                }
            }
        }
        Err(_) => {
            println!(
                "Warning: {} does not appear to be an SSH public key",
                pubkey_path.display()
            );
            println!("  Signing setup skipped.");
        }
    }

    Ok(())
}

/// The placeholder used in the settings.json template for the Python invocation prefix.
const PYTHON_PREFIX_PLACEHOLDER: &str = "__PYTHON_PREFIX__";

// Embed hook files at compile time from resources/ (packaged with the crate)
const SETTINGS_JSON: &str = include_str!("../../resources/claude/settings.json");
pub(crate) const PROMPT_GUARD_PY: &str =
    include_str!("../../resources/claude/hooks/prompt-guard.py");
pub(crate) const POST_EDIT_CHECK_PY: &str =
    include_str!("../../resources/claude/hooks/post-edit-check.py");
pub(crate) const SESSION_START_PY: &str =
    include_str!("../../resources/claude/hooks/session-start.py");
pub(crate) const PRE_WEB_CHECK_PY: &str =
    include_str!("../../resources/claude/hooks/pre-web-check.py");
pub(crate) const WORK_CHECK_PY: &str = include_str!("../../resources/claude/hooks/work-check.py");
pub(crate) const CROSSLINK_CONFIG_PY: &str =
    include_str!("../../resources/claude/hooks/crosslink_config.py");

// Embed MCP server for safe web fetching
const SAFE_FETCH_SERVER_PY: &str = include_str!("../../resources/claude/mcp/safe-fetch-server.py");
const MCP_JSON: &str = include_str!("../../resources/mcp.json");

// Embed slash commands
const WORKFLOW_CMD_MD: &str = include_str!("../../resources/claude/commands/workflow.md");
const FEATURE_CMD_MD: &str = include_str!("../../resources/claude/commands/feature.md");
const FEATREE_CMD_MD: &str = include_str!("../../resources/claude/commands/featree.md");
const KICKOFF_CMD_MD: &str = include_str!("../../resources/claude/commands/kickoff.md");
const CHECK_CMD_MD: &str = include_str!("../../resources/claude/commands/check.md");
const COMMIT_CMD_MD: &str = include_str!("../../resources/claude/commands/commit.md");

// Embed sanitization patterns
const SANITIZE_PATTERNS: &str =
    include_str!("../../resources/crosslink/rules/sanitize-patterns.txt");

// Embed hook configuration
pub(crate) const HOOK_CONFIG_JSON: &str =
    include_str!("../../resources/crosslink/hook-config.json");

// Embed tracking mode rule files
pub(crate) const RULE_TRACKING_STRICT: &str =
    include_str!("../../resources/crosslink/rules/tracking-strict.md");
pub(crate) const RULE_TRACKING_NORMAL: &str =
    include_str!("../../resources/crosslink/rules/tracking-normal.md");
pub(crate) const RULE_TRACKING_RELAXED: &str =
    include_str!("../../resources/crosslink/rules/tracking-relaxed.md");

// Embed rule files at compile time from resources/crosslink/rules/
pub(crate) const RULE_GLOBAL: &str = include_str!("../../resources/crosslink/rules/global.md");
pub(crate) const RULE_PROJECT: &str = include_str!("../../resources/crosslink/rules/project.md");
pub(crate) const RULE_RUST: &str = include_str!("../../resources/crosslink/rules/rust.md");
pub(crate) const RULE_PYTHON: &str = include_str!("../../resources/crosslink/rules/python.md");
pub(crate) const RULE_JAVASCRIPT: &str =
    include_str!("../../resources/crosslink/rules/javascript.md");
pub(crate) const RULE_TYPESCRIPT: &str =
    include_str!("../../resources/crosslink/rules/typescript.md");
pub(crate) const RULE_TYPESCRIPT_REACT: &str =
    include_str!("../../resources/crosslink/rules/typescript-react.md");
pub(crate) const RULE_JAVASCRIPT_REACT: &str =
    include_str!("../../resources/crosslink/rules/javascript-react.md");
pub(crate) const RULE_GO: &str = include_str!("../../resources/crosslink/rules/go.md");
pub(crate) const RULE_JAVA: &str = include_str!("../../resources/crosslink/rules/java.md");
pub(crate) const RULE_C: &str = include_str!("../../resources/crosslink/rules/c.md");
pub(crate) const RULE_CPP: &str = include_str!("../../resources/crosslink/rules/cpp.md");
pub(crate) const RULE_CSHARP: &str = include_str!("../../resources/crosslink/rules/csharp.md");
pub(crate) const RULE_RUBY: &str = include_str!("../../resources/crosslink/rules/ruby.md");
pub(crate) const RULE_PHP: &str = include_str!("../../resources/crosslink/rules/php.md");
pub(crate) const RULE_SWIFT: &str = include_str!("../../resources/crosslink/rules/swift.md");
pub(crate) const RULE_KOTLIN: &str = include_str!("../../resources/crosslink/rules/kotlin.md");
pub(crate) const RULE_SCALA: &str = include_str!("../../resources/crosslink/rules/scala.md");
pub(crate) const RULE_ZIG: &str = include_str!("../../resources/crosslink/rules/zig.md");
pub(crate) const RULE_ODIN: &str = include_str!("../../resources/crosslink/rules/odin.md");
pub(crate) const RULE_ELIXIR: &str = include_str!("../../resources/crosslink/rules/elixir.md");
pub(crate) const RULE_ELIXIR_PHOENIX: &str =
    include_str!("../../resources/crosslink/rules/elixir-phoenix.md");
pub(crate) const RULE_WEB: &str = include_str!("../../resources/crosslink/rules/web.md");

/// All rule files to deploy
pub(crate) const RULE_FILES: &[(&str, &str)] = &[
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

pub fn run(
    path: &Path,
    force: bool,
    python_prefix: Option<&str>,
    skip_cpitd: bool,
    skip_signing: bool,
    signing_key: Option<&str>,
) -> Result<()> {
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
            "# Multi-agent collaboration (machine-local)\nagent.json\n.hub-cache/\nkeys/\n\n# Machine-local hook overrides\nhook-config.local.json\n",
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

    // Detect or use provided Python prefix (needed for settings.json and cpitd install)
    let prefix = python_prefix
        .map(|s| s.to_string())
        .unwrap_or_else(|| detect_python_prefix(path));

    // Create .claude directory and hooks (or update if force)
    if !claude_exists || force {
        fs::create_dir_all(&hooks_dir).context("Failed to create .claude/hooks directory")?;
        let settings_content = SETTINGS_JSON.replace(PYTHON_PREFIX_PLACEHOLDER, &prefix);
        fs::write(claude_dir.join("settings.json"), settings_content)
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

        fs::write(hooks_dir.join("crosslink_config.py"), CROSSLINK_CONFIG_PY)
            .context("Failed to write crosslink_config.py")?;

        // Create MCP server directory and write safe-fetch server
        let mcp_dir = claude_dir.join("mcp");
        fs::create_dir_all(&mcp_dir).context("Failed to create .claude/mcp directory")?;
        fs::write(mcp_dir.join("safe-fetch-server.py"), SAFE_FETCH_SERVER_PY)
            .context("Failed to write safe-fetch-server.py")?;

        // Write slash commands
        let commands_dir = claude_dir.join("commands");
        fs::create_dir_all(&commands_dir).context("Failed to create .claude/commands directory")?;
        fs::write(commands_dir.join("workflow.md"), WORKFLOW_CMD_MD)
            .context("Failed to write workflow.md")?;
        fs::write(commands_dir.join("feature.md"), FEATURE_CMD_MD)
            .context("Failed to write feature.md")?;
        fs::write(commands_dir.join("featree.md"), FEATREE_CMD_MD)
            .context("Failed to write featree.md")?;
        fs::write(commands_dir.join("kickoff.md"), KICKOFF_CMD_MD)
            .context("Failed to write kickoff.md")?;
        fs::write(commands_dir.join("check.md"), CHECK_CMD_MD)
            .context("Failed to write check.md")?;
        fs::write(commands_dir.join("commit.md"), COMMIT_CMD_MD)
            .context("Failed to write commit.md")?;

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

    // Auto-install cpitd unless skipped
    if !skip_cpitd {
        match install_cpitd(&prefix) {
            Ok(true) => println!("Installed cpitd (code clone detection)"),
            Ok(false) => {} // already installed, silent
            Err(e) => {
                println!("Warning: Could not auto-install cpitd: {}", e);
                println!("  You can install it manually: pip install cpitd");
            }
        }
    }

    // Driver SSH key detection and setup
    if !skip_signing {
        setup_driver_signing(path, signing_key)?;
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
        let result = run(dir.path(), false, None, true, true, None);
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
        run(dir.path(), false, None, true, true, None).unwrap();

        // Verify hook files
        assert!(dir.path().join(".claude/settings.json").exists());
        assert!(dir.path().join(".claude/hooks/prompt-guard.py").exists());
        assert!(dir.path().join(".claude/hooks/post-edit-check.py").exists());
        assert!(dir.path().join(".claude/hooks/session-start.py").exists());
        assert!(dir.path().join(".claude/hooks/pre-web-check.py").exists());
        assert!(dir.path().join(".claude/hooks/work-check.py").exists());
        assert!(dir
            .path()
            .join(".claude/hooks/crosslink_config.py")
            .exists());
        assert!(dir.path().join(".claude/mcp/safe-fetch-server.py").exists());
        assert!(dir.path().join(".mcp.json").exists());
    }

    #[test]
    fn test_run_creates_rule_files() {
        let dir = tempdir().unwrap();
        run(dir.path(), false, None, true, true, None).unwrap();

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
        run(dir.path(), false, None, true, true, None).unwrap();

        // Second init without force - should succeed but not recreate
        let result = run(dir.path(), false, None, true, true, None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_force_update() {
        let dir = tempdir().unwrap();

        // First init
        run(dir.path(), false, None, true, true, None).unwrap();

        // Modify a hook file
        let hook_path = dir.path().join(".claude/hooks/prompt-guard.py");
        fs::write(&hook_path, "# modified").unwrap();

        // Force update
        run(dir.path(), true, None, true, true, None).unwrap();

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
        run(dir.path(), false, None, true, true, None).unwrap();

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
        run(dir.path(), true, None, true, true, None).unwrap();

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
        run(dir.path(), false, None, true, true, None).unwrap();

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
        run(dir.path(), false, None, true, true, None).unwrap();

        // Write invalid JSON to .mcp.json
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "not json {{{").unwrap();

        // Force init should fail, not silently overwrite
        let result = run(dir.path(), true, None, true, true, None);
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
        run(dir.path(), false, None, true, true, None).unwrap();

        // Write a JSON array to .mcp.json
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "[1, 2, 3]").unwrap();

        // Force init should fail, not silently overwrite
        let result = run(dir.path(), true, None, true, true, None);
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
        run(dir.path(), false, None, true, true, None).unwrap();

        // Write empty file
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "").unwrap();

        // Should fail — empty file is not valid JSON
        let result = run(dir.path(), true, None, true, true, None);
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
        run(dir.path(), false, None, true, true, None).unwrap();

        // Write valid JSON where mcpServers is a string instead of object
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, r#"{"mcpServers": "banana"}"#).unwrap();

        // Should fail, not silently replace
        let result = run(dir.path(), true, None, true, true, None);
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
        run(dir.path(), false, None, true, true, None).unwrap();

        // Write a valid object with no mcpServers key
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, r#"{"someOtherKey": true}"#).unwrap();

        // Force init should add mcpServers, preserving the other key
        run(dir.path(), true, None, true, true, None).unwrap();

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

        let result = run(dir.path(), false, None, true, true, None);
        assert!(result.is_ok());

        // .claude should now exist
        assert!(dir.path().join(".claude").exists());
    }

    #[test]
    fn test_run_partial_init_claude_only() {
        let dir = tempdir().unwrap();

        // Create only .claude directory
        fs::create_dir_all(dir.path().join(".claude")).unwrap();

        let result = run(dir.path(), false, None, true, true, None);
        assert!(result.is_ok());

        // .crosslink should now exist
        assert!(dir.path().join(".crosslink").exists());
    }

    #[test]
    fn test_run_database_usable() {
        let dir = tempdir().unwrap();
        run(dir.path(), false, None, true, true, None).unwrap();

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
        run(dir.path(), false, None, true, true, None).unwrap();

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
        run(dir.path(), false, None, true, true, None).unwrap();

        // Modify a rule file
        let rule_path = dir.path().join(".crosslink/rules/global.md");
        fs::write(&rule_path, "# modified rule").unwrap();

        // Force update
        run(dir.path(), true, None, true, true, None).unwrap();

        // Verify file was restored
        let content = fs::read_to_string(&rule_path).unwrap();
        assert_ne!(content, "# modified rule");
    }

    #[test]
    fn test_run_idempotent_with_force() {
        let dir = tempdir().unwrap();

        // Multiple force runs should all succeed
        for _ in 0..3 {
            let result = run(dir.path(), true, None, true, true, None);
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
        assert!(!CROSSLINK_CONFIG_PY.is_empty());
        assert!(!SAFE_FETCH_SERVER_PY.is_empty());
        assert!(!MCP_JSON.is_empty());
        assert!(!WORKFLOW_CMD_MD.is_empty());
        assert!(!FEATURE_CMD_MD.is_empty());
        assert!(!FEATREE_CMD_MD.is_empty());
        assert!(!KICKOFF_CMD_MD.is_empty());
        assert!(!CHECK_CMD_MD.is_empty());
        assert!(!COMMIT_CMD_MD.is_empty());
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
        run(dir.path(), false, None, true, true, None).unwrap();

        let content = fs::read_to_string(dir.path().join(".crosslink/.gitignore")).unwrap();
        assert!(content.contains("agent.json"));
        assert!(content.contains(".hub-cache/"));
        assert!(content.contains("hook-config.local.json"));
    }

    // --- Python toolchain detection tests ---

    #[test]
    fn test_detect_python_prefix_default() {
        let dir = tempdir().unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "python3");
    }

    #[test]
    fn test_detect_python_prefix_uv_lock() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "uv run python3");
    }

    #[test]
    fn test_detect_python_prefix_uv_pyproject() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"foo\"\n\n[tool.uv]\ndev-dependencies = []\n",
        )
        .unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "uv run python3");
    }

    #[test]
    fn test_detect_python_prefix_poetry_lock() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("poetry.lock"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "poetry run python3");
    }

    #[test]
    fn test_detect_python_prefix_poetry_pyproject() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"foo\"\n\n[tool.poetry]\nname = \"foo\"\n",
        )
        .unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "poetry run python3");
    }

    #[test]
    fn test_detect_python_prefix_venv() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".venv")).unwrap();
        assert_eq!(detect_python_prefix(dir.path()), ".venv/bin/python3");
    }

    #[test]
    fn test_detect_python_prefix_pipenv() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Pipfile"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "pipenv run python3");
    }

    #[test]
    fn test_detect_python_prefix_pipenv_lock() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Pipfile.lock"), "{}").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "pipenv run python3");
    }

    #[test]
    fn test_detect_python_prefix_uv_beats_poetry() {
        let dir = tempdir().unwrap();
        // Both uv.lock and poetry.lock present — uv wins
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        fs::write(dir.path().join("poetry.lock"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "uv run python3");
    }

    #[test]
    fn test_detect_python_prefix_poetry_beats_venv() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("poetry.lock"), "").unwrap();
        fs::create_dir(dir.path().join(".venv")).unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "poetry run python3");
    }

    #[test]
    fn test_detect_python_prefix_venv_beats_pipenv() {
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(".venv")).unwrap();
        fs::write(dir.path().join("Pipfile"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), ".venv/bin/python3");
    }

    #[test]
    fn test_detect_python_prefix_pyproject_without_tools_is_default() {
        let dir = tempdir().unwrap();
        // Plain pyproject.toml with no tool sections
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"foo\"\nversion = \"1.0\"\n",
        )
        .unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "python3");
    }

    // --- Settings.json templating tests ---

    #[test]
    fn test_settings_json_default_uses_python3() {
        let dir = tempdir().unwrap();
        run(dir.path(), false, None, true, true, None).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(
            content.contains("python3"),
            "Default init should use python3 in settings.json"
        );
        assert!(
            !content.contains(PYTHON_PREFIX_PLACEHOLDER),
            "Placeholder should be replaced"
        );
    }

    #[test]
    fn test_settings_json_uv_project() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        run(dir.path(), false, None, true, true, None).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(
            content.contains("uv run python3"),
            "uv project should use 'uv run python3' in settings.json"
        );
    }

    #[test]
    fn test_settings_json_cli_override() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        // CLI override should beat auto-detection
        run(dir.path(), false, Some("custom-python"), true, true, None).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(
            content.contains("custom-python"),
            "CLI override should be used in settings.json"
        );
        assert!(
            !content.contains("uv run python3"),
            "Auto-detected prefix should not appear when overridden"
        );
    }

    #[test]
    fn test_settings_json_produces_valid_json() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        run(dir.path(), false, None, true, true, None).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&content);
        assert!(
            parsed.is_ok(),
            "Settings JSON should be valid after templating"
        );
    }

    #[test]
    fn test_force_re_detects_toolchain() {
        let dir = tempdir().unwrap();
        // First init: no markers → python3
        run(dir.path(), false, None, true, true, None).unwrap();
        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(content.contains("\"python3 \\\"$(git"));

        // Add uv.lock, force re-init → should now use uv
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        run(dir.path(), true, None, true, true, None).unwrap();
        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(
            content.contains("uv run python3"),
            "Force re-init should re-detect toolchain"
        );
    }
}
