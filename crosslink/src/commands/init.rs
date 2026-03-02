use anyhow::{Context, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    style::Stylize,
    terminal::{self, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph},
    Frame, TerminalOptions, Viewport,
};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::db::Database;

// Section markers for idempotent gitignore management
const GITIGNORE_SECTION_START: &str = "# === Crosslink managed (do not edit between markers) ===";
const GITIGNORE_SECTION_END: &str = "# === End crosslink managed ===";

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
/// Result of cpitd installation attempt.
enum CpitdResult {
    AlreadyInstalled,
    InstalledFromPypi,
    InstalledFromSource,
}

fn install_cpitd(python_prefix: &str) -> Result<CpitdResult> {
    if cpitd_is_installed() {
        return Ok(CpitdResult::AlreadyInstalled);
    }

    // First attempt: install from PyPI
    let pypi_result = install_cpitd_from_pypi(python_prefix);
    if let Ok(true) = pypi_result {
        return Ok(CpitdResult::InstalledFromPypi);
    }

    // Second attempt: clone repo and install from source
    match install_cpitd_from_source(python_prefix) {
        Ok(true) => Ok(CpitdResult::InstalledFromSource),
        Ok(false) => Ok(CpitdResult::AlreadyInstalled),
        Err(e) => Err(e),
    }
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
fn setup_driver_signing(project_root: &Path, signing_key: Option<&str>, ui: &InitUI) -> Result<()> {
    use crate::signing;

    let crosslink_dir = project_root.join(".crosslink");
    let driver_pub_path = crosslink_dir.join("driver-key.pub");

    // If driver key already configured and not forcing, skip
    if driver_pub_path.exists() {
        ui.step_start("Configuring signing");
        ui.step_ok(Some("already configured"));
        return Ok(());
    }

    // Find the key to use
    let pubkey_path = if let Some(key_path) = signing_key {
        let p = std::path::PathBuf::from(key_path);
        if !p.exists() {
            ui.warn(&format!("Signing key not found at {}", key_path));
            return Ok(());
        }
        Some(p)
    } else {
        signing::find_git_signing_key().or_else(signing::find_default_ssh_key)
    };

    let pubkey_path = match pubkey_path {
        Some(p) => p,
        None => {
            ui.step_skip("Signing: no SSH key found");
            ui.detail("Generate one with: ssh-keygen -t ed25519");
            ui.detail("Then re-run: crosslink init --force");
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

    ui.step_start("Configuring signing");
    match signing::read_public_key(&pubkey_path) {
        Ok(public_key) => {
            fs::write(&driver_pub_path, &public_key).context("Failed to write driver-key.pub")?;

            match signing::get_key_fingerprint(&pubkey_path) {
                Ok(fp) => ui.step_ok(Some(&fp)),
                Err(_) => ui.step_ok(Some(&pubkey_path.display().to_string())),
            }

            // NOTE: We intentionally do NOT call configure_git_ssh_signing()
            // on the project worktree here. Crosslink should not override the
            // user's git signing configuration. The hub cache worktree (used for
            // lock claims, issue entries, etc.) has its own signing config set
            // up separately in sync.rs.
        }
        Err(_) => {
            // Finish the step_start line, then show warning below
            println!();
            ui.warn(&format!(
                "{} does not appear to be an SSH public key",
                pubkey_path.display()
            ));
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
const PREFLIGHT_CMD_MD: &str = include_str!("../../resources/claude/commands/preflight.md");
const REVIEW_CMD_MD: &str = include_str!("../../resources/claude/commands/review.md");
const AUDIT_CMD_MD: &str = include_str!("../../resources/claude/commands/audit.md");

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
pub(crate) const RULE_KNOWLEDGE: &str =
    include_str!("../../resources/crosslink/rules/knowledge.md");

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
    ("knowledge.md", RULE_KNOWLEDGE),
    ("sanitize-patterns.txt", SANITIZE_PATTERNS),
    ("tracking-strict.md", RULE_TRACKING_STRICT),
    ("tracking-normal.md", RULE_TRACKING_NORMAL),
    ("tracking-relaxed.md", RULE_TRACKING_RELAXED),
];

/// The managed gitignore section content.
///
/// This block is placed between `GITIGNORE_SECTION_START` and `GITIGNORE_SECTION_END`
/// markers in the project root `.gitignore`. The markers make `crosslink init --force`
/// idempotent — re-running replaces the section in-place instead of appending duplicates.
const GITIGNORE_MANAGED_SECTION: &str = "\
# .crosslink/ — machine-local state (never commit)
.crosslink/issues.db
.crosslink/issues.db-wal
.crosslink/issues.db-shm
.crosslink/agent.json
.crosslink/session.json
.crosslink/daemon.pid
.crosslink/daemon.log
.crosslink/last_test_run
.crosslink/keys/
.crosslink/.hub-cache/
.crosslink/.knowledge-cache/
.crosslink/.cache/
.crosslink/hook-config.local.json
.crosslink/integrations/

# .crosslink/ — DO track these (project-level policy):
#   .crosslink/hook-config.json   — shared team configuration
#   .crosslink/rules/             — project coding standards
#   .crosslink/.gitignore         — inner gitignore for agent files

# .claude/ — auto-generated by crosslink init (not project source)
.claude/hooks/
.claude/commands/
.claude/mcp/

# .claude/ — DO track these (if manually configured):
#   .claude/settings.json         — Claude Code project settings
#   .claude/settings.local.json is per-developer, ignore separately if needed
";

/// Write or update a managed section in the project root `.gitignore`.
///
/// The section is delimited by `GITIGNORE_SECTION_START` / `GITIGNORE_SECTION_END` markers.
/// On first run the section is appended; on subsequent runs the existing section is replaced
/// in-place, preserving any user entries outside the markers.
fn write_root_gitignore(project_root: &Path) -> Result<()> {
    let gitignore_path = project_root.join(".gitignore");

    let managed_block = format!(
        "{}\n{}{}\n",
        GITIGNORE_SECTION_START, GITIGNORE_MANAGED_SECTION, GITIGNORE_SECTION_END
    );

    let existing = fs::read_to_string(&gitignore_path).unwrap_or_default();

    let new_content = if let (Some(start_pos), Some(end_pos)) = (
        existing.find(GITIGNORE_SECTION_START),
        existing.find(GITIGNORE_SECTION_END),
    ) {
        // Replace existing managed section in-place
        let before = &existing[..start_pos];
        let after = &existing[end_pos + GITIGNORE_SECTION_END.len()..];
        // Strip leading newline from `after` so we don't accumulate blank lines
        let after = after.strip_prefix('\n').unwrap_or(after);
        format!("{}{}{}", before, managed_block, after)
    } else {
        // Append new section (with a blank separator if file has content)
        if existing.is_empty() {
            managed_block
        } else {
            let separator = if existing.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            format!("{}{}{}", existing, separator, managed_block)
        }
    };

    fs::write(&gitignore_path, new_content).context("Failed to write .gitignore")?;
    Ok(())
}

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

/// Merge crosslink's default `allowedTools` into an existing `.claude/settings.json`,
/// or create it fresh.  Hooks are always overwritten (they are crosslink-managed),
/// but user-added `allowedTools` entries are preserved.
///
/// The `python_prefix` is substituted into hook commands via the `__PYTHON_PREFIX__`
/// placeholder in the embedded template.
fn write_settings_json_merged(settings_path: &Path, python_prefix: &str) -> Result<()> {
    let template_raw = SETTINGS_JSON.replace(PYTHON_PREFIX_PLACEHOLDER, python_prefix);
    let template: serde_json::Value = serde_json::from_str(&template_raw).context(
        "embedded SETTINGS_JSON is not valid JSON after substitution — this is a build defect",
    )?;

    let embedded_tools: Vec<String> = template
        .get("allowedTools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut obj = match fs::read_to_string(settings_path) {
        Ok(raw) => {
            let parsed: serde_json::Value = serde_json::from_str(&raw).with_context(|| {
                format!(
                    "Existing settings.json at {} contains invalid JSON — \
                         refusing to overwrite. Fix or remove it, then retry.",
                    settings_path.display()
                )
            })?;
            match parsed {
                serde_json::Value::Object(map) => map,
                _ => anyhow::bail!(
                    "Existing settings.json at {} is not a JSON object — \
                     refusing to overwrite. Fix or remove it, then retry.",
                    settings_path.display()
                ),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(e) => {
            return Err(anyhow::Error::from(e).context("Failed to read existing settings.json"))
        }
    };

    // Merge allowedTools: union of existing entries + embedded defaults (no duplicates)
    let mut tools: Vec<String> = obj
        .get("allowedTools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    for tool in &embedded_tools {
        if !tools.contains(tool) {
            tools.push(tool.clone());
        }
    }

    obj.insert(
        "allowedTools".into(),
        serde_json::Value::Array(tools.into_iter().map(serde_json::Value::String).collect()),
    );

    // Overwrite hooks (crosslink-managed) and enableAllProjectMcpServers
    if let Some(hooks) = template.get("hooks") {
        obj.insert("hooks".into(), hooks.clone());
    }
    if let Some(enable) = template.get("enableAllProjectMcpServers") {
        obj.insert("enableAllProjectMcpServers".into(), enable.clone());
    }

    let mut output = serde_json::to_string_pretty(&serde_json::Value::Object(obj))
        .context("Failed to serialize settings.json")?;
    output.push('\n');
    fs::write(settings_path, output).context("Failed to write settings.json")?;
    Ok(())
}

/// TUI walkthrough choices for `crosslink init`.
struct TuiChoices {
    tracking_mode: String,
    intervention_tracking: bool,
    comment_discipline: String,
    kickoff_verification: String,
}

impl Default for TuiChoices {
    fn default() -> Self {
        Self {
            tracking_mode: "strict".to_string(),
            intervention_tracking: true,
            comment_discipline: "encouraged".to_string(),
            kickoff_verification: "local".to_string(),
        }
    }
}

// ── Styled inline output helper ──────────────────────────────────────────────

struct InitUI {
    is_tty: bool,
}

impl InitUI {
    fn new() -> Self {
        Self {
            is_tty: io::stdout().is_terminal(),
        }
    }

    fn banner(&self) {
        if self.is_tty {
            println!();
            println!("  {} {}", "crosslink".bold().cyan(), "init".dark_grey());
            println!("  {}", "─".repeat(40).dark_grey());
            println!();
        }
    }

    fn step_start(&self, label: &str) {
        if self.is_tty {
            print!("  {} {}... ", "●".cyan(), label);
            io::stdout().flush().ok();
        } else {
            print!("{}... ", label);
        }
    }

    fn step_ok(&self, detail: Option<&str>) {
        if self.is_tty {
            match detail {
                Some(d) => println!("{} {}", "✓".green(), d.dark_grey()),
                None => println!("{}", "✓".green()),
            }
        } else {
            match detail {
                Some(d) => println!("done ({})", d),
                None => println!("done"),
            }
        }
    }

    fn step_created(&self, what: &str) {
        if self.is_tty {
            println!(
                "  {} {} {}",
                "✓".green(),
                "created".green(),
                what.dark_grey()
            );
        } else {
            println!("Created {}", what);
        }
    }

    fn step_skip(&self, msg: &str) {
        if self.is_tty {
            println!("  {} {}", "–".dark_grey(), msg.dark_grey());
        } else {
            println!("{}", msg);
        }
    }

    fn warn(&self, msg: &str) {
        if self.is_tty {
            println!("  {} {}", "⚠".yellow(), msg.yellow());
        } else {
            println!("Warning: {}", msg);
        }
    }

    fn detail(&self, msg: &str) {
        if self.is_tty {
            println!("    {}", msg.dark_grey());
        } else {
            println!("  {}", msg);
        }
    }

    fn success(&self) {
        if self.is_tty {
            println!();
            println!(
                "  {} {}",
                "✓".green().bold(),
                "Crosslink initialized successfully!".bold()
            );
            println!();
            println!(
                "  {} {} {}",
                "next".dark_grey(),
                "→".cyan(),
                "crosslink session start".white()
            );
            println!(
                "       {} {}",
                "→".cyan(),
                "crosslink create \"Task\"".white()
            );
            println!();
        } else {
            println!("Crosslink initialized successfully!");
            println!("\nNext steps:");
            println!("  crosslink session start     # Start a session");
            println!("  crosslink create \"Task\"     # Create an issue");
        }
    }
}

// ── Ratatui TUI walkthrough ─────────────────────────────────────────────────

struct WalkthroughQuestion {
    title: &'static str,
    description: &'static str,
    options: Vec<(&'static str, &'static str)>,
    selected: usize,
}

struct WalkthroughApp {
    questions: Vec<WalkthroughQuestion>,
    current: usize,
    confirming: bool,
    finished: bool,
    cancelled: bool,
}

impl WalkthroughApp {
    fn new(
        tracking_default: usize,
        intervention_default: usize,
        comment_default: usize,
        kickoff_default: usize,
    ) -> Self {
        Self {
            questions: vec![
                WalkthroughQuestion {
                    title: "Issue Tracking Enforcement",
                    description: "How strictly should agents be required to track work in issues?",
                    options: vec![
                        ("Strict", "Block tool calls without an active issue"),
                        ("Normal", "Remind agents but don't block"),
                        ("Relaxed", "No enforcement"),
                    ],
                    selected: tracking_default,
                },
                WalkthroughQuestion {
                    title: "Driver Intervention Tracking",
                    description: "Log when the human operator steps in to redirect an agent?",
                    options: vec![
                        ("Enabled", "Record interventions for process improvement"),
                        ("Disabled", "Don't track interventions"),
                    ],
                    selected: intervention_default,
                },
                WalkthroughQuestion {
                    title: "Comment Discipline",
                    description: "Should agents document their decisions on issues?",
                    options: vec![
                        ("Encouraged", "Remind agents to document decisions"),
                        ("Required", "Block after prolonged silence"),
                        ("Off", "No comment requirements"),
                    ],
                    selected: comment_default,
                },
                WalkthroughQuestion {
                    title: "Kickoff Verification Depth",
                    description: "How thoroughly should background agents verify their work?",
                    options: vec![
                        ("Local", "Tests + self-review only"),
                        ("CI", "Push and wait for CI"),
                        ("Thorough", "CI + adversarial review"),
                    ],
                    selected: kickoff_default,
                },
            ],
            current: 0,
            confirming: false,
            finished: false,
            cancelled: false,
        }
    }

    fn move_up(&mut self) {
        let q = &mut self.questions[self.current];
        if q.selected > 0 {
            q.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        let q = &mut self.questions[self.current];
        if q.selected < q.options.len() - 1 {
            q.selected += 1;
        }
    }

    fn confirm(&mut self) {
        if self.confirming {
            self.finished = true;
        } else if self.current + 1 < self.questions.len() {
            self.current += 1;
        } else {
            self.confirming = true;
        }
    }

    fn go_back(&mut self) {
        if self.confirming {
            self.confirming = false;
        } else if self.current > 0 {
            self.current -= 1;
        }
    }
}

fn draw_walkthrough(frame: &mut Frame, app: &WalkthroughApp) {
    let area = frame.area();

    if app.confirming {
        draw_confirmation(frame, app, area);
        return;
    }

    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "crosslink",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" setup ", Style::default().fg(Color::DarkGray)),
        ]))
        .padding(Padding::new(2, 2, 1, 1));

    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Build layout: progress, spacer, previous answers (if any), question, description, spacer, options, help
    let has_previous = app.current > 0;
    let previous_height = if has_previous {
        app.current as u16 + 1 // +1 for spacer after previous answers
    } else {
        0
    };

    let chunks = Layout::vertical([
        Constraint::Length(1),               // progress dots
        Constraint::Length(1),               // spacer
        Constraint::Length(previous_height), // previous answers + spacer
        Constraint::Length(1),               // question title
        Constraint::Length(1),               // description
        Constraint::Length(1),               // spacer
        Constraint::Min(3),                  // options list
        Constraint::Length(1),               // help bar
    ])
    .split(inner);

    // Progress dots
    let progress_spans: Vec<Span> = (0..app.questions.len())
        .map(|i| {
            if i < app.current {
                Span::styled(" ● ", Style::default().fg(Color::Green))
            } else if i == app.current {
                Span::styled(
                    " ● ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(" ○ ", Style::default().fg(Color::DarkGray))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);

    // Previous selections
    if has_previous {
        let summary_lines: Vec<Line> = app.questions[..app.current]
            .iter()
            .map(|pq| {
                let chosen = pq.options[pq.selected].0;
                Line::from(vec![
                    Span::styled("  ✓ ", Style::default().fg(Color::Green)),
                    Span::styled(pq.title, Style::default().fg(Color::DarkGray)),
                    Span::styled(": ", Style::default().fg(Color::DarkGray)),
                    Span::styled(chosen, Style::default().fg(Color::Green)),
                ])
            })
            .collect();
        frame.render_widget(Paragraph::new(summary_lines), chunks[2]);
    }

    let q = &app.questions[app.current];

    // Question title
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            q.title,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[3],
    );

    // Description
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            q.description,
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[4],
    );

    // Options list
    let items: Vec<ListItem> = q
        .options
        .iter()
        .enumerate()
        .map(|(i, (label, desc))| {
            let (marker, label_style) = if i == q.selected {
                (
                    "❯ ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Gray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, label_style),
                Span::styled(*label, label_style),
                Span::raw("  "),
                Span::styled(*desc, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let list = List::new(items);
    let mut state = ListState::default().with_selected(Some(q.selected));
    frame.render_stateful_widget(list, chunks[6], &mut state);

    // Help bar
    let help_text = if app.current > 0 {
        "↑↓ select  Enter confirm  Backspace back  Esc cancel"
    } else {
        "↑↓ select  Enter confirm  Esc cancel"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            help_text,
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[7],
    );
}

fn draw_confirmation(frame: &mut Frame, app: &WalkthroughApp, area: Rect) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "crosslink",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" setup ", Style::default().fg(Color::DarkGray)),
        ]))
        .padding(Padding::new(2, 2, 1, 1));

    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let num_questions = app.questions.len() as u16;
    let chunks = Layout::vertical([
        Constraint::Length(1),             // progress dots (all green)
        Constraint::Length(1),             // spacer
        Constraint::Length(1),             // "Review your choices" title
        Constraint::Length(1),             // spacer
        Constraint::Length(num_questions), // choices summary
        Constraint::Min(1),                // spacer / fill
        Constraint::Length(1),             // help bar
    ])
    .split(inner);

    // All progress dots green
    let progress_spans: Vec<Span> = (0..app.questions.len())
        .map(|_| Span::styled(" ● ", Style::default().fg(Color::Green)))
        .collect();
    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);

    // Title
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Review your choices",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    // All choices
    let summary_lines: Vec<Line> = app
        .questions
        .iter()
        .map(|q| {
            let chosen = q.options[q.selected].0;
            Line::from(vec![
                Span::styled("  ✓ ", Style::default().fg(Color::Green)),
                Span::styled(q.title, Style::default().fg(Color::Gray)),
                Span::styled(": ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    chosen,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(summary_lines), chunks[4]);

    // Help bar
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter apply  Backspace go back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );
}

/// Run the interactive TUI walkthrough using ratatui, returning user choices.
/// Falls back to defaults if stdin is not a TTY.
fn run_tui_walkthrough(existing: Option<&serde_json::Value>) -> Result<TuiChoices> {
    if !std::io::stdin().is_terminal() {
        println!("Non-interactive environment detected, using defaults.");
        return Ok(TuiChoices::default());
    }

    // Resolve current/default selections from existing config
    let current_tracking = existing
        .and_then(|v| v.get("tracking_mode"))
        .and_then(|v| v.as_str())
        .unwrap_or("strict");
    let current_intervention = existing
        .and_then(|v| v.get("intervention_tracking"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let current_comment = existing
        .and_then(|v| v.get("comment_discipline"))
        .and_then(|v| v.as_str())
        .unwrap_or("encouraged");
    let current_kickoff = existing
        .and_then(|v| v.get("kickoff_verification"))
        .and_then(|v| v.as_str())
        .unwrap_or("local");

    let tracking_default = match current_tracking {
        "normal" => 1,
        "relaxed" => 2,
        _ => 0,
    };
    let intervention_default = if current_intervention { 0 } else { 1 };
    let comment_default = match current_comment {
        "required" => 1,
        "off" => 2,
        _ => 0,
    };
    let kickoff_default = match current_kickoff {
        "ci" => 1,
        "thorough" => 2,
        _ => 0,
    };

    let mut app = WalkthroughApp::new(
        tracking_default,
        intervention_default,
        comment_default,
        kickoff_default,
    );

    // Inline viewport — renders below the current cursor, not full-screen
    const WALKTHROUGH_HEIGHT: u16 = 20;
    enable_raw_mode().context("Failed to enable raw mode")?;
    let stdout = io::stdout();
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(WALKTHROUGH_HEIGHT),
        },
    )
    .context("Failed to create terminal")?;

    // Run event loop — closure so we can always restore terminal on exit
    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| draw_walkthrough(f, &app))?;

            if let Event::Key(key) = event::read().context("Failed to read terminal event")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') if !app.confirming => app.move_up(),
                    KeyCode::Down | KeyCode::Char('j') if !app.confirming => app.move_down(),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        app.confirm();
                        if app.finished {
                            break;
                        }
                    }
                    KeyCode::Backspace | KeyCode::Left => app.go_back(),
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.cancelled = true;
                        break;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    })();

    // Clear the inline viewport area so progress output fills it cleanly
    {
        let area = terminal.get_frame().area();
        let backend = terminal.backend_mut();
        for row in area.y..area.y + area.height {
            execute!(
                backend,
                cursor::MoveTo(0, row),
                terminal::Clear(terminal::ClearType::CurrentLine)
            )
            .ok();
        }
        execute!(backend, cursor::MoveTo(0, area.y)).ok();
    }

    // Restore terminal state
    disable_raw_mode().ok();
    terminal.show_cursor().ok();

    result?;

    if app.cancelled {
        anyhow::bail!("Setup cancelled");
    }

    let tracking_mode = match app.questions[0].selected {
        1 => "normal",
        2 => "relaxed",
        _ => "strict",
    }
    .to_string();

    let intervention_tracking = app.questions[1].selected == 0;

    let comment_discipline = match app.questions[2].selected {
        1 => "required",
        2 => "off",
        _ => "encouraged",
    }
    .to_string();

    let kickoff_verification = match app.questions[3].selected {
        1 => "ci",
        2 => "thorough",
        _ => "local",
    }
    .to_string();

    Ok(TuiChoices {
        tracking_mode,
        intervention_tracking,
        comment_discipline,
        kickoff_verification,
    })
}

/// Apply TUI choices onto a config JSON value, preserving fields not covered by the TUI.
fn apply_tui_choices(config: &mut serde_json::Value, choices: &TuiChoices) -> Result<()> {
    let obj = config
        .as_object_mut()
        .context("hook-config.json is not a JSON object")?;
    obj.insert(
        "tracking_mode".into(),
        serde_json::Value::String(choices.tracking_mode.clone()),
    );
    obj.insert(
        "intervention_tracking".into(),
        serde_json::Value::Bool(choices.intervention_tracking),
    );
    obj.insert(
        "comment_discipline".into(),
        serde_json::Value::String(choices.comment_discipline.clone()),
    );
    obj.insert(
        "kickoff_verification".into(),
        serde_json::Value::String(choices.kickoff_verification.clone()),
    );
    Ok(())
}

/// Options for `crosslink init`.
pub struct InitOpts<'a> {
    pub force: bool,
    pub python_prefix: Option<&'a str>,
    pub skip_cpitd: bool,
    pub skip_signing: bool,
    pub signing_key: Option<&'a str>,
    pub reconfigure: bool,
    pub defaults: bool,
}

pub fn run(path: &Path, opts: &InitOpts<'_>) -> Result<()> {
    let force = opts.force;
    let python_prefix = opts.python_prefix;
    let skip_cpitd = opts.skip_cpitd;
    let skip_signing = opts.skip_signing;
    let signing_key = opts.signing_key;
    let reconfigure = opts.reconfigure;
    let defaults = opts.defaults;
    let crosslink_dir = path.join(".crosslink");
    let claude_dir = path.join(".claude");
    let hooks_dir = claude_dir.join("hooks");

    let ui = InitUI::new();

    // Check if already initialized
    let crosslink_exists = crosslink_dir.exists();
    let claude_exists = claude_dir.exists();

    if crosslink_exists && claude_exists && !force && !reconfigure {
        ui.step_skip("Already initialized");
        ui.detail("Use --force to update hooks to latest version.");
        ui.detail("Use --reconfigure to re-run the setup walkthrough.");
        return Ok(());
    }

    // ── TUI walkthrough FIRST — before any filesystem changes ────────────
    // If the user presses Esc, we bail before creating anything.
    let config_path = crosslink_dir.join("hook-config.json");
    let config_exists = config_path.exists();
    let should_run_tui = !defaults && (!config_exists || force || reconfigure);

    let tui_result = if should_run_tui {
        let base_config: serde_json::Value = if config_exists && reconfigure {
            let raw = fs::read_to_string(&config_path)
                .context("Failed to read existing hook-config.json")?;
            serde_json::from_str(&raw).context("hook-config.json contains invalid JSON")?
        } else {
            serde_json::from_str(HOOK_CONFIG_JSON)
                .context("Embedded hook-config.json is invalid")?
        };

        let existing_ref = if config_exists {
            Some(&base_config as &serde_json::Value)
        } else {
            None
        };
        let choices = run_tui_walkthrough(existing_ref)?;
        Some((base_config, choices))
    } else {
        None
    };

    // ── All filesystem changes happen below this point ───────────────────

    ui.banner();

    let rules_dir = crosslink_dir.join("rules");

    // Create .crosslink directory and database
    if !crosslink_exists {
        ui.step_start("Initializing database");
        fs::create_dir_all(&crosslink_dir).context("Failed to create .crosslink directory")?;
        let db_path = crosslink_dir.join("issues.db");
        Database::open(&db_path)?;
        ui.step_ok(None);
    }

    // Write hook config
    match tui_result {
        Some((mut config, choices)) => {
            apply_tui_choices(&mut config, &choices)?;
            let output = serde_json::to_string_pretty(&config)
                .context("Failed to serialize hook-config.json")?;
            fs::write(&config_path, format!("{}\n", output))
                .context("Failed to write hook-config.json")?;
            ui.step_created("hook-config.json");
        }
        None if !config_exists || force => {
            fs::write(&config_path, HOOK_CONFIG_JSON)
                .context("Failed to write hook-config.json")?;
            ui.step_created("hook-config.json");
        }
        None => {}
    }

    // Write .crosslink/.gitignore for multi-agent files
    let crosslink_gitignore = crosslink_dir.join(".gitignore");
    if !crosslink_gitignore.exists() || force {
        fs::write(
            &crosslink_gitignore,
            "# Multi-agent collaboration (machine-local)\n\
             agent.json\n\
             .hub-cache/\n\
             .knowledge-cache/\n\
             keys/\n\
             integrations/\n\
             \n\
             # Machine-local hook overrides\n\
             hook-config.local.json\n",
        )
        .context("Failed to write .crosslink/.gitignore")?;
    }

    // Write/update managed section in root .gitignore
    ui.step_start("Configuring .gitignore");
    write_root_gitignore(path).context("Failed to update root .gitignore")?;
    ui.step_ok(None);

    // Create or update rules directory
    let rules_exist = rules_dir.exists();
    if !rules_exist || force {
        ui.step_start("Deploying rules");
        fs::create_dir_all(&rules_dir).context("Failed to create .crosslink/rules directory")?;

        for (filename, content) in RULE_FILES {
            fs::write(rules_dir.join(filename), content)
                .with_context(|| format!("Failed to write {}", filename))?;
        }

        if force && rules_exist {
            ui.step_ok(Some("updated"));
        } else {
            ui.step_ok(Some(&format!("{} files", RULE_FILES.len())));
        }
    }

    // Detect or use provided Python prefix (needed for settings.json and cpitd install)
    let prefix = python_prefix
        .map(|s| s.to_string())
        .unwrap_or_else(|| detect_python_prefix(path));

    // Create .claude directory and hooks (or update if force)
    if !claude_exists || force {
        ui.step_start("Setting up Claude Code hooks");
        fs::create_dir_all(&hooks_dir).context("Failed to create .claude/hooks directory")?;

        write_settings_json_merged(&claude_dir.join("settings.json"), &prefix)
            .context("Failed to write settings.json")?;

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

        let mcp_dir = claude_dir.join("mcp");
        fs::create_dir_all(&mcp_dir).context("Failed to create .claude/mcp directory")?;
        fs::write(mcp_dir.join("safe-fetch-server.py"), SAFE_FETCH_SERVER_PY)
            .context("Failed to write safe-fetch-server.py")?;

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
        fs::write(commands_dir.join("preflight.md"), PREFLIGHT_CMD_MD)
            .context("Failed to write preflight.md")?;
        fs::write(commands_dir.join("review.md"), REVIEW_CMD_MD)
            .context("Failed to write review.md")?;
        fs::write(commands_dir.join("audit.md"), AUDIT_CMD_MD)
            .context("Failed to write audit.md")?;

        let warnings =
            write_mcp_json_merged(&path.join(".mcp.json")).context("Failed to write .mcp.json")?;
        for warning in &warnings {
            ui.warn(warning);
        }

        if force && claude_exists {
            ui.step_ok(Some("updated"));
        } else {
            ui.step_ok(None);
        }
    }

    // Auto-install cpitd unless skipped
    if !skip_cpitd {
        ui.step_start("Checking cpitd");
        match install_cpitd(&prefix) {
            Ok(CpitdResult::InstalledFromPypi) => ui.step_ok(Some("installed")),
            Ok(CpitdResult::InstalledFromSource) => ui.step_ok(Some("installed from source")),
            Ok(CpitdResult::AlreadyInstalled) => ui.step_ok(Some("already installed")),
            Err(e) => {
                // Finish the step_start line, then show warning below
                println!();
                ui.warn(&format!("Could not auto-install cpitd: {}", e));
                ui.detail("You can install it manually: pip install cpitd");
            }
        }
    }

    // Driver SSH key detection and setup
    if !skip_signing {
        setup_driver_signing(path, signing_key, &ui)?;
    }

    ui.success();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build default test opts (skips cpitd/signing, uses --defaults to skip TUI).
    fn test_opts(force: bool) -> InitOpts<'static> {
        InitOpts {
            force,
            python_prefix: None,
            skip_cpitd: true,
            skip_signing: true,
            signing_key: None,
            reconfigure: false,
            defaults: true,
        }
    }

    #[test]
    fn test_run_fresh_init() {
        let dir = tempdir().unwrap();
        let result = run(dir.path(), &test_opts(false));
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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

        // Second init without force - should succeed but not recreate
        let result = run(dir.path(), &test_opts(false));
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_force_update() {
        let dir = tempdir().unwrap();

        // First init
        run(dir.path(), &test_opts(false)).unwrap();

        // Modify a hook file
        let hook_path = dir.path().join(".claude/hooks/prompt-guard.py");
        fs::write(&hook_path, "# modified").unwrap();

        // Force update
        run(dir.path(), &test_opts(true)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(true)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

        // Write invalid JSON to .mcp.json
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "not json {{{").unwrap();

        // Force init should fail, not silently overwrite
        let result = run(dir.path(), &test_opts(true));
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
        run(dir.path(), &test_opts(false)).unwrap();

        // Write a JSON array to .mcp.json
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "[1, 2, 3]").unwrap();

        // Force init should fail, not silently overwrite
        let result = run(dir.path(), &test_opts(true));
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
        run(dir.path(), &test_opts(false)).unwrap();

        // Write empty file
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, "").unwrap();

        // Should fail — empty file is not valid JSON
        let result = run(dir.path(), &test_opts(true));
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
        run(dir.path(), &test_opts(false)).unwrap();

        // Write valid JSON where mcpServers is a string instead of object
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, r#"{"mcpServers": "banana"}"#).unwrap();

        // Should fail, not silently replace
        let result = run(dir.path(), &test_opts(true));
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
        run(dir.path(), &test_opts(false)).unwrap();

        // Write a valid object with no mcpServers key
        let mcp_path = dir.path().join(".mcp.json");
        fs::write(&mcp_path, r#"{"someOtherKey": true}"#).unwrap();

        // Force init should add mcpServers, preserving the other key
        run(dir.path(), &test_opts(true)).unwrap();

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

        let result = run(dir.path(), &test_opts(false));
        assert!(result.is_ok());

        // .claude should now exist
        assert!(dir.path().join(".claude").exists());
    }

    #[test]
    fn test_run_partial_init_claude_only() {
        let dir = tempdir().unwrap();

        // Create only .claude directory
        fs::create_dir_all(dir.path().join(".claude")).unwrap();

        let result = run(dir.path(), &test_opts(false));
        assert!(result.is_ok());

        // .crosslink should now exist
        assert!(dir.path().join(".crosslink").exists());
    }

    #[test]
    fn test_run_database_usable() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

        // Modify a rule file
        let rule_path = dir.path().join(".crosslink/rules/global.md");
        fs::write(&rule_path, "# modified rule").unwrap();

        // Force update
        run(dir.path(), &test_opts(true)).unwrap();

        // Verify file was restored
        let content = fs::read_to_string(&rule_path).unwrap();
        assert_ne!(content, "# modified rule");
    }

    #[test]
    fn test_run_idempotent_with_force() {
        let dir = tempdir().unwrap();

        // Multiple force runs should all succeed
        for _ in 0..3 {
            let result = run(dir.path(), &test_opts(true));
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
        assert!(!PREFLIGHT_CMD_MD.is_empty());
        assert!(!REVIEW_CMD_MD.is_empty());
        assert!(!AUDIT_CMD_MD.is_empty());
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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(
            dir.path(),
            &InitOpts {
                python_prefix: Some("custom-python"),
                ..test_opts(false)
            },
        )
        .unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();

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
        run(dir.path(), &test_opts(false)).unwrap();
        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(content.contains("python3 \\\"$HOOK\\\""));

        // Add uv.lock, force re-init → should now use uv
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        run(dir.path(), &test_opts(true)).unwrap();
        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert!(
            content.contains("uv run python3"),
            "Force re-init should re-detect toolchain"
        );
    }

    // --- Settings.json allowedTools merge tests ---

    /// The default allowedTools entries that the embedded template provides.
    fn embedded_allowed_tools() -> Vec<String> {
        let template: serde_json::Value = serde_json::from_str(SETTINGS_JSON).unwrap();
        template
            .get("allowedTools")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn test_settings_json_includes_allowed_tools() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let tools = parsed["allowedTools"]
            .as_array()
            .expect("allowedTools should be an array");

        for expected in embedded_allowed_tools() {
            assert!(
                tools.iter().any(|v| v.as_str() == Some(&expected)),
                "allowedTools should contain \"{}\"",
                expected
            );
        }
    }

    #[test]
    fn test_settings_json_includes_tmux_and_worktree_permissions() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        let content = fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let tools: Vec<&str> = parsed["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();

        assert!(
            tools.contains(&"Bash(tmux *)"),
            "allowedTools should include tmux permission"
        );
        assert!(
            tools.contains(&"Bash(git worktree *)"),
            "allowedTools should include git worktree permission"
        );
    }

    #[test]
    fn test_force_init_preserves_user_allowed_tools() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        // Add a custom allowedTools entry
        let settings_path = dir.path().join(".claude/settings.json");
        let mut content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        content["allowedTools"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::Value::String("Bash(my-custom-tool *)".into()));
        fs::write(
            &settings_path,
            serde_json::to_string_pretty(&content).unwrap(),
        )
        .unwrap();

        // Force re-init
        run(dir.path(), &test_opts(true)).unwrap();

        let result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let tools: Vec<&str> = result["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();

        // Should have embedded tools AND the custom one
        for expected in embedded_allowed_tools() {
            assert!(
                tools.contains(&expected.as_str()),
                "embedded tool \"{}\" should be preserved after force re-init",
                expected
            );
        }
        assert!(
            tools.contains(&"Bash(my-custom-tool *)"),
            "custom allowedTools entry should be preserved after force re-init"
        );
    }

    #[test]
    fn test_force_init_no_duplicate_allowed_tools() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        // Force re-init multiple times
        run(dir.path(), &test_opts(true)).unwrap();
        run(dir.path(), &test_opts(true)).unwrap();

        let settings_path = dir.path().join(".claude/settings.json");
        let content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let tools: Vec<&str> = content["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();

        // Each embedded tool should appear exactly once (no duplicates)
        for expected in embedded_allowed_tools() {
            let count = tools.iter().filter(|&&t| t == expected.as_str()).count();
            assert_eq!(
                count, 1,
                "\"{}\" should appear exactly once, found {}",
                expected, count
            );
        }
    }

    #[test]
    fn test_settings_json_merge_fails_on_malformed_json() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        // Write invalid JSON to settings.json
        let settings_path = dir.path().join(".claude/settings.json");
        fs::write(&settings_path, "not json {{{").unwrap();

        // Force init should fail, not silently overwrite
        let result = run(dir.path(), &test_opts(true));
        assert!(result.is_err());
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("invalid JSON"),
            "Error should mention invalid JSON, got: {}",
            err
        );

        // Original (broken) content should be untouched
        let content = fs::read_to_string(&settings_path).unwrap();
        assert_eq!(content, "not json {{{");
    }

    #[test]
    fn test_settings_json_merge_fails_on_non_object() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        // Write a JSON array to settings.json
        let settings_path = dir.path().join(".claude/settings.json");
        fs::write(&settings_path, "[1, 2, 3]").unwrap();

        // Force init should fail, not silently overwrite
        let result = run(dir.path(), &test_opts(true));
        assert!(result.is_err());
        let err = format!("{:#}", result.unwrap_err());
        assert!(
            err.contains("not a JSON object"),
            "Error should mention not a JSON object, got: {}",
            err
        );
    }

    #[test]
    fn test_settings_json_merge_creates_fresh_file() {
        let dir = tempdir().unwrap();
        let settings_path = dir.path().join(".claude/settings.json");
        fs::create_dir_all(dir.path().join(".claude")).unwrap();

        // No pre-existing file
        assert!(!settings_path.exists());

        write_settings_json_merged(&settings_path, "python3").unwrap();

        let content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        let tools: Vec<&str> = content["allowedTools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();

        for expected in embedded_allowed_tools() {
            assert!(
                tools.contains(&expected.as_str()),
                "fresh file should contain \"{}\"",
                expected
            );
        }
    }

    // --- Root .gitignore tests ---

    #[test]
    fn test_init_creates_root_gitignore() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains(GITIGNORE_SECTION_START));
        assert!(content.contains(GITIGNORE_SECTION_END));
        assert!(content.contains(".crosslink/issues.db"));
        assert!(content.contains(".crosslink/agent.json"));
        assert!(content.contains(".crosslink/session.json"));
        assert!(content.contains(".crosslink/daemon.pid"));
        assert!(content.contains(".crosslink/keys/"));
        assert!(content.contains(".crosslink/.hub-cache/"));
        assert!(content.contains(".crosslink/hook-config.local.json"));
        assert!(content.contains(".claude/hooks/"));
        assert!(content.contains(".claude/commands/"));
        assert!(content.contains(".claude/mcp/"));
    }

    #[test]
    fn test_root_gitignore_idempotent() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        let first = fs::read_to_string(dir.path().join(".gitignore")).unwrap();

        // Force re-init should produce identical content
        run(dir.path(), &test_opts(true)).unwrap();
        let second = fs::read_to_string(dir.path().join(".gitignore")).unwrap();

        assert_eq!(
            first, second,
            "Re-init should not duplicate gitignore entries"
        );
    }

    #[test]
    fn test_root_gitignore_preserves_user_entries() {
        let dir = tempdir().unwrap();

        // Write a pre-existing .gitignore with user content
        fs::write(dir.path().join(".gitignore"), "/target/\n*.log\n").unwrap();

        run(dir.path(), &test_opts(false)).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(
            content.contains("/target/"),
            "User entries before managed section should be preserved"
        );
        assert!(
            content.contains("*.log"),
            "User entries before managed section should be preserved"
        );
        assert!(content.contains(GITIGNORE_SECTION_START));
        assert!(content.contains(".crosslink/issues.db"));
    }

    #[test]
    fn test_root_gitignore_preserves_entries_around_managed_section() {
        let dir = tempdir().unwrap();

        // First init
        run(dir.path(), &test_opts(false)).unwrap();

        // Add user content before and after the managed section
        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        let new_content = format!(
            "# My custom rules\n/build/\n\n{}\n# Trailing rules\n*.tmp\n",
            content
        );
        fs::write(dir.path().join(".gitignore"), new_content).unwrap();

        // Force re-init
        run(dir.path(), &test_opts(true)).unwrap();

        let result = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(
            result.contains("/build/"),
            "Pre-section user entries preserved"
        );
        assert!(
            result.contains("*.tmp"),
            "Post-section user entries preserved"
        );
        assert!(
            result.contains(".crosslink/issues.db"),
            "Managed entries present"
        );

        // Should have exactly one managed section
        assert_eq!(
            result.matches(GITIGNORE_SECTION_START).count(),
            1,
            "Should have exactly one managed section start marker"
        );
        assert_eq!(
            result.matches(GITIGNORE_SECTION_END).count(),
            1,
            "Should have exactly one managed section end marker"
        );
    }

    #[test]
    fn test_root_gitignore_has_do_track_comments() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(
            content.contains("DO track"),
            "Should include DO track comments for documentation"
        );
        assert!(
            content.contains("hook-config.json"),
            "Should mention hook-config.json as tracked"
        );
    }

    #[test]
    fn test_write_root_gitignore_fresh() {
        let dir = tempdir().unwrap();
        write_root_gitignore(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.starts_with(GITIGNORE_SECTION_START));
        assert!(content.contains(GITIGNORE_SECTION_END));
        assert!(content.contains(".crosslink/issues.db"));
    }

    #[test]
    fn test_write_root_gitignore_replaces_section() {
        let dir = tempdir().unwrap();

        // Write twice
        write_root_gitignore(dir.path()).unwrap();
        write_root_gitignore(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert_eq!(
            content.matches(GITIGNORE_SECTION_START).count(),
            1,
            "Should have exactly one start marker after double write"
        );
    }

    #[test]
    fn test_crosslink_inner_gitignore_includes_integrations() {
        let dir = tempdir().unwrap();
        run(dir.path(), &test_opts(false)).unwrap();

        let content = fs::read_to_string(dir.path().join(".crosslink/.gitignore")).unwrap();
        assert!(content.contains("integrations/"));
    }
}
