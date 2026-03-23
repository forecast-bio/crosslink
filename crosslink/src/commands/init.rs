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
        if cfg!(target_os = "windows") {
            return ".venv\\Scripts\\python.exe".to_string();
        }
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
    if python_prefix.starts_with(".venv/") || python_prefix.starts_with(".venv\\") {
        let pip = python_prefix
            .replace("python3", "pip")
            .replace("python.exe", "pip.exe")
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
        // INTENTIONAL: cleanup of previous failed attempt is best-effort — clone below will fail if stale dir remains
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
        // INTENTIONAL: temp dir cleanup on failure is best-effort — OS will reclaim it eventually
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
    } else if python_prefix.starts_with(".venv/") || python_prefix.starts_with(".venv\\") {
        let pip = python_prefix
            .replace("python3", "pip")
            .replace("python.exe", "pip.exe")
            .replace("python", "pip");
        run_install_command(&pip, &["install", &tmp_dir_str])
    } else if python_prefix.starts_with("pipenv ") {
        run_install_command("pipenv", &["run", "pip", "install", &tmp_dir_str])
    } else {
        run_install_command("python3", &["-m", "pip", "install", &tmp_dir_str])
    };

    // INTENTIONAL: temp dir cleanup is best-effort — OS will reclaim it eventually
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
pub(crate) const HEARTBEAT_PY: &str = include_str!("../../resources/claude/hooks/heartbeat.py");

// Embed MCP servers
const SAFE_FETCH_SERVER_PY: &str = include_str!("../../resources/claude/mcp/safe-fetch-server.py");
const KNOWLEDGE_SERVER_PY: &str = include_str!("../../resources/claude/mcp/knowledge-server.py");
const MCP_JSON: &str = include_str!("../../resources/mcp.json");

// Embed slash commands — auto-generated by build.rs from resources/claude/commands/
include!(concat!(env!("OUT_DIR"), "/commands_gen.rs"));

// Embed hook configuration
pub(crate) const HOOK_CONFIG_JSON: &str =
    include_str!("../../resources/crosslink/hook-config.json");

// Embed rule files — auto-generated by build.rs from resources/crosslink/rules/
// Generates RULE_* consts and RULE_FILES array for all .md and .txt files.
include!(concat!(env!("OUT_DIR"), "/rules_gen.rs"));

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
.crosslink/rules.local/

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

use crate::commands::config::{ConfigGroup, ConfigType, PRESET_SOLO, PRESET_TEAM, REGISTRY};
use std::collections::HashMap;

/// TUI walkthrough choices for `crosslink init` — registry-driven.
struct TuiChoices {
    values: HashMap<String, serde_json::Value>,
    install_alias: bool,
}

impl Default for TuiChoices {
    fn default() -> Self {
        let mut values = HashMap::new();
        // Default values from the embedded config
        let defaults: serde_json::Value =
            serde_json::from_str(HOOK_CONFIG_JSON).unwrap_or_default();
        for entry in REGISTRY {
            if matches!(
                entry.config_type,
                ConfigType::StringArray | ConfigType::Map | ConfigType::Integer
            ) {
                continue;
            }
            if let Some(v) = defaults.get(entry.key) {
                values.insert(entry.key.to_string(), v.clone());
            }
        }
        Self {
            values,
            install_alias: false,
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
            println!();
            println!("Crosslink tracks issues, comments, and sessions in .crosslink/issues.db.");
            println!("AI agents use it to coordinate work across sessions and worktrees.");
            println!();
            println!("Quick start:");
            println!("  crosslink create \"Task\"     # Create an issue");
            println!("  crosslink list              # See all issues");
            println!("  crosslink session start     # Start a work session");
            println!();
            println!("Multi-agent features (agents, signing, locks, containers) are optional");
            println!("and only needed when multiple AI agents collaborate on the same repo.");
        }
    }
}

// ── Ratatui TUI walkthrough (registry-driven, REQ-5) ────────────────────────

struct InitWalkthroughApp {
    /// 0 = preset, 1..N = group screens, N+1 = alias, N+2 = confirm
    screen: usize,
    preset_selected: usize, // 0=Team, 1=Solo, 2=Custom
    group_names: Vec<&'static str>,
    group_keys: Vec<Vec<usize>>,       // indices into REGISTRY
    group_selections: Vec<Vec<usize>>, // per-group, per-key selected option
    group_cursor: usize,
    /// Shell alias question
    alias_selected: usize, // 0=Yes, 1=No
    _shell_name: String,
    shell_config_file: String,
    finished: bool,
    cancelled: bool,
}

impl InitWalkthroughApp {
    fn new(existing: &serde_json::Value) -> Self {
        let groups = ConfigGroup::all();
        let mut group_names = Vec::new();
        let mut group_keys: Vec<Vec<usize>> = Vec::new();
        let mut group_selections: Vec<Vec<usize>> = Vec::new();

        for group in groups {
            let mut keys_in_group = Vec::new();
            let mut selections = Vec::new();

            for (idx, entry) in REGISTRY.iter().enumerate() {
                if entry.group != *group {
                    continue;
                }
                // Skip arrays, maps, integers — advanced settings
                if matches!(
                    entry.config_type,
                    ConfigType::StringArray | ConfigType::Map | ConfigType::Integer
                ) {
                    continue;
                }
                keys_in_group.push(idx);
                let current_val = existing.get(entry.key);
                let sel = match entry.config_type {
                    ConfigType::Bool => {
                        let val = current_val.and_then(|v| v.as_bool()).unwrap_or(false);
                        if val {
                            0
                        } else {
                            1
                        }
                    }
                    ConfigType::Enum(options) => {
                        let val = current_val.and_then(|v| v.as_str()).unwrap_or("");
                        options.iter().position(|o| *o == val).unwrap_or(0)
                    }
                    _ => 0,
                };
                selections.push(sel);
            }

            if !keys_in_group.is_empty() {
                group_names.push(group.label());
                group_keys.push(keys_in_group);
                group_selections.push(selections);
            }
        }

        // Detect shell for alias question
        let shell_env = std::env::var("SHELL").unwrap_or_default();
        let home = std::env::var("HOME").unwrap_or_default();
        let (shell_name, shell_config_file) = if shell_env.ends_with("fish") {
            (
                "fish".to_string(),
                format!("{home}/.config/fish/config.fish"),
            )
        } else if shell_env.ends_with("zsh") {
            ("zsh".to_string(), format!("{home}/.zshrc"))
        } else if shell_env.ends_with("bash") {
            ("bash".to_string(), format!("{home}/.bashrc"))
        } else {
            ("unknown".to_string(), String::new())
        };

        // Check if alias already installed
        let alias_already = if !shell_config_file.is_empty() {
            let alias_line = if shell_name == "fish" {
                "abbr -a xl crosslink"
            } else {
                "alias xl='crosslink'"
            };
            fs::read_to_string(&shell_config_file)
                .map(|c| c.lines().any(|l| l.trim() == alias_line))
                .unwrap_or(false)
        } else {
            false
        };

        Self {
            screen: 0,
            preset_selected: 2,
            group_names,
            group_keys,
            group_selections,
            group_cursor: 0,
            alias_selected: if alias_already { 1 } else { 0 },
            _shell_name: shell_name,
            shell_config_file,
            finished: false,
            cancelled: false,
        }
    }

    fn total_screens(&self) -> usize {
        // preset + groups + alias + confirm
        1 + self.group_names.len() + 1 + 1
    }

    fn is_preset_screen(&self) -> bool {
        self.screen == 0
    }

    fn is_alias_screen(&self) -> bool {
        self.screen == self.total_screens() - 2
    }

    fn is_confirm_screen(&self) -> bool {
        self.screen == self.total_screens() - 1
    }

    fn current_group_idx(&self) -> Option<usize> {
        if self.screen >= 1 && self.screen < 1 + self.group_names.len() {
            Some(self.screen - 1)
        } else {
            None
        }
    }

    fn options_for_key(registry_idx: usize) -> Vec<&'static str> {
        let entry = &REGISTRY[registry_idx];
        match entry.config_type {
            ConfigType::Bool => vec!["true", "false"],
            ConfigType::Enum(opts) => opts.to_vec(),
            _ => vec![],
        }
    }

    fn move_up(&mut self) {
        if self.is_preset_screen() {
            self.preset_selected = self.preset_selected.saturating_sub(1);
        } else if self.is_alias_screen() {
            self.alias_selected = self.alias_selected.saturating_sub(1);
        } else if let Some(gi) = self.current_group_idx() {
            self.group_cursor = self.group_cursor.saturating_sub(1);
            let _ = gi;
        }
    }

    fn move_down(&mut self) {
        if self.is_preset_screen() {
            if self.preset_selected < 2 {
                self.preset_selected += 1;
            }
        } else if self.is_alias_screen() {
            if self.alias_selected < 1 {
                self.alias_selected += 1;
            }
        } else if let Some(gi) = self.current_group_idx() {
            let max = self.group_keys[gi].len().saturating_sub(1);
            if self.group_cursor < max {
                self.group_cursor += 1;
            }
        }
    }

    fn cycle_value(&mut self) {
        if let Some(gi) = self.current_group_idx() {
            if self.group_cursor < self.group_keys[gi].len() {
                let reg_idx = self.group_keys[gi][self.group_cursor];
                let options = Self::options_for_key(reg_idx);
                if !options.is_empty() {
                    let current = self.group_selections[gi][self.group_cursor];
                    self.group_selections[gi][self.group_cursor] = (current + 1) % options.len();
                }
            }
        }
    }

    fn confirm_action(&mut self) {
        if self.is_confirm_screen() {
            self.finished = true;
        } else if self.is_preset_screen() {
            if self.preset_selected < 2 {
                self.apply_preset_selections();
                // Skip group screens, go to alias
                self.screen = 1 + self.group_names.len();
                self.group_cursor = 0;
            } else {
                self.screen = 1;
                self.group_cursor = 0;
            }
        } else {
            self.screen += 1;
            self.group_cursor = 0;
        }
    }

    fn go_back(&mut self) {
        if self.screen > 0 {
            if self.is_alias_screen() && self.preset_selected < 2 {
                // Came from preset, go back to preset
                self.screen = 0;
            } else {
                self.screen -= 1;
            }
            self.group_cursor = 0;
        }
    }

    fn apply_preset_selections(&mut self) {
        let preset = if self.preset_selected == 0 {
            PRESET_TEAM
        } else {
            PRESET_SOLO
        };
        for (key, value) in preset {
            for (gi, keys) in self.group_keys.iter().enumerate() {
                for (ki, &reg_idx) in keys.iter().enumerate() {
                    if REGISTRY[reg_idx].key == *key {
                        let options = Self::options_for_key(reg_idx);
                        if let Some(pos) = options.iter().position(|o| o == value) {
                            self.group_selections[gi][ki] = pos;
                        }
                    }
                }
            }
        }
    }

    fn build_choices(&self) -> TuiChoices {
        let mut values = HashMap::new();
        for (gi, keys) in self.group_keys.iter().enumerate() {
            for (ki, &reg_idx) in keys.iter().enumerate() {
                let entry = &REGISTRY[reg_idx];
                let options = Self::options_for_key(reg_idx);
                let selected = self.group_selections[gi][ki];
                if selected < options.len() {
                    let val_str = options[selected];
                    let val = match entry.config_type {
                        ConfigType::Bool => match val_str {
                            "true" => serde_json::Value::Bool(true),
                            _ => serde_json::Value::Bool(false),
                        },
                        _ => serde_json::Value::String(val_str.to_string()),
                    };
                    values.insert(entry.key.to_string(), val);
                }
            }
        }
        TuiChoices {
            values,
            install_alias: self.alias_selected == 0,
        }
    }
}

fn draw_init_walkthrough(frame: &mut Frame, app: &InitWalkthroughApp) {
    let area = frame.area();

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

    // Progress dots
    let total = app.total_screens();
    let progress_spans: Vec<Span> = (0..total)
        .map(|i| {
            if i < app.screen {
                Span::styled(" \u{25cf} ", Style::default().fg(Color::Green))
            } else if i == app.screen {
                Span::styled(
                    " \u{25cf} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(" \u{25cb} ", Style::default().fg(Color::DarkGray))
            }
        })
        .collect();

    if app.is_preset_screen() {
        draw_init_preset(frame, app, inner, progress_spans);
    } else if app.is_alias_screen() {
        draw_init_alias(frame, app, inner, progress_spans);
    } else if app.is_confirm_screen() {
        draw_init_confirm(frame, app, inner, progress_spans);
    } else if let Some(gi) = app.current_group_idx() {
        draw_init_group(frame, app, gi, inner, progress_spans);
    }
}

fn draw_init_preset(
    frame: &mut Frame,
    app: &InitWalkthroughApp,
    area: Rect,
    progress_spans: Vec<Span>,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Quick-start presets",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Choose a preset or configure each setting individually",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[3],
    );

    let presets = [
        ("Team", "strict tracking, CI verification, signing enforced"),
        ("Solo", "relaxed tracking, local verification, no signing"),
        ("Custom", "configure each setting individually"),
    ];
    let items: Vec<ListItem> = presets
        .iter()
        .enumerate()
        .map(|(i, (label, desc))| {
            let (marker, style) = if i == app.preset_selected {
                (
                    "\u{276f} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Gray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(*label, style),
                Span::raw("  "),
                Span::styled(*desc, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items);
    let mut state = ListState::default().with_selected(Some(app.preset_selected));
    frame.render_stateful_widget(list, chunks[5], &mut state);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2191}\u{2193} select  Enter confirm  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );
}

fn draw_init_group(
    frame: &mut Frame,
    app: &InitWalkthroughApp,
    group_idx: usize,
    area: Rect,
    progress_spans: Vec<Span>,
) {
    let keys = &app.group_keys[group_idx];
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(keys.len() as u16 + 1),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            app.group_names[group_idx],
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    let items: Vec<ListItem> = keys
        .iter()
        .enumerate()
        .map(|(ki, &reg_idx)| {
            let entry = &REGISTRY[reg_idx];
            let options = InitWalkthroughApp::options_for_key(reg_idx);
            let selected = app.group_selections[group_idx][ki];
            let val_str = if selected < options.len() {
                options[selected]
            } else {
                "?"
            };
            let (marker, style) = if ki == app.group_cursor {
                (
                    "\u{276f} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Gray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(format!("{:<30}", entry.key), style),
                Span::styled(
                    format!("[{}]", val_str),
                    if ki == app.group_cursor {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
            ]))
        })
        .collect();
    let list = List::new(items);
    let mut state = ListState::default().with_selected(Some(app.group_cursor));
    frame.render_stateful_widget(list, chunks[4], &mut state);

    // Description pane
    if app.group_cursor < keys.len() {
        let reg_idx = keys[app.group_cursor];
        let entry = &REGISTRY[reg_idx];
        let options = InitWalkthroughApp::options_for_key(reg_idx);
        let valid = if options.len() > 1 {
            format!("  Valid: {}", options.join(", "))
        } else {
            String::new()
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    format!("  {}", entry.description),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(valid, Style::default().fg(Color::DarkGray))),
            ]),
            chunks[5],
        );
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2191}\u{2193} navigate  \u{2192}/\u{2190} cycle  Enter next  Backspace back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );
}

fn draw_init_alias(
    frame: &mut Frame,
    app: &InitWalkthroughApp,
    area: Rect,
    progress_spans: Vec<Span>,
) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Shell Alias",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    let desc = if app.shell_config_file.is_empty() {
        "Could not detect shell config file".to_string()
    } else {
        format!(
            "Install `xl` alias for `crosslink` in {}?",
            app.shell_config_file
        )
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            desc,
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[3],
    );

    let options = [
        ("Yes", "Add alias to shell config"),
        ("No", "Skip alias setup"),
    ];
    let items: Vec<ListItem> = options
        .iter()
        .enumerate()
        .map(|(i, (label, desc))| {
            let (marker, style) = if i == app.alias_selected {
                (
                    "\u{276f} ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("  ", Style::default().fg(Color::Gray))
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::styled(*label, style),
                Span::raw("  "),
                Span::styled(*desc, Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();
    let list = List::new(items);
    let mut state = ListState::default().with_selected(Some(app.alias_selected));
    frame.render_stateful_widget(list, chunks[5], &mut state);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "\u{2191}\u{2193} select  Enter confirm  Backspace back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[6],
    );
}

fn draw_init_confirm(
    frame: &mut Frame,
    app: &InitWalkthroughApp,
    area: Rect,
    progress_spans: Vec<Span>,
) {
    let total_keys: usize = app.group_keys.iter().map(|k| k.len()).sum();
    let summary_height = total_keys as u16 + app.group_names.len() as u16 + 3;

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(summary_height),
        Constraint::Length(1),
    ])
    .split(area);

    frame.render_widget(Paragraph::new(Line::from(progress_spans)), chunks[0]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Review your choices",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))),
        chunks[2],
    );

    let mut lines: Vec<Line> = Vec::new();
    for (gi, keys) in app.group_keys.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            format!("  {}", app.group_names[gi]),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        for (ki, &reg_idx) in keys.iter().enumerate() {
            let entry = &REGISTRY[reg_idx];
            let options = InitWalkthroughApp::options_for_key(reg_idx);
            let selected = app.group_selections[gi][ki];
            let val_str = if selected < options.len() {
                options[selected]
            } else {
                "?"
            };
            lines.push(Line::from(vec![
                Span::styled("    \u{2713} ", Style::default().fg(Color::Green)),
                Span::styled(
                    format!("{}: ", entry.key),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    val_str,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
    let alias_text = if app.alias_selected == 0 {
        format!(
            "    \u{2713} xl alias: will install ({})",
            app.shell_config_file
        )
    } else {
        "    \u{2713} xl alias: skip".to_string()
    };
    lines.push(Line::from(Span::styled(
        alias_text,
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Paragraph::new(lines), chunks[4]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter apply  Backspace go back  Esc cancel",
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[5],
    );
}

/// Run the interactive TUI walkthrough using ratatui, returning user choices.
/// Falls back to defaults if stdin is not a TTY.
fn run_tui_walkthrough(existing: Option<&serde_json::Value>) -> Result<TuiChoices> {
    if !std::io::stdin().is_terminal() {
        println!("Non-interactive environment detected, using defaults.");
        return Ok(TuiChoices::default());
    }

    let base = existing
        .cloned()
        .unwrap_or_else(|| serde_json::from_str(HOOK_CONFIG_JSON).unwrap_or_default());

    let mut app = InitWalkthroughApp::new(&base);

    const WALKTHROUGH_HEIGHT: u16 = 24;
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

    let result = (|| -> Result<()> {
        loop {
            terminal.draw(|f| draw_init_walkthrough(f, &app))?;

            if let Event::Key(key) = event::read().context("Failed to read terminal event")? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') if !app.is_confirm_screen() => app.move_up(),
                    KeyCode::Down | KeyCode::Char('j') if !app.is_confirm_screen() => {
                        app.move_down()
                    }
                    KeyCode::Right
                        if !app.is_preset_screen()
                            && !app.is_confirm_screen()
                            && !app.is_alias_screen() =>
                    {
                        app.cycle_value()
                    }
                    KeyCode::Left
                        if !app.is_preset_screen()
                            && !app.is_confirm_screen()
                            && !app.is_alias_screen() =>
                    {
                        // Cycle backwards
                        if let Some(gi) = app.current_group_idx() {
                            if app.group_cursor < app.group_keys[gi].len() {
                                let reg_idx = app.group_keys[gi][app.group_cursor];
                                let options = InitWalkthroughApp::options_for_key(reg_idx);
                                if !options.is_empty() {
                                    let current = app.group_selections[gi][app.group_cursor];
                                    app.group_selections[gi][app.group_cursor] = if current == 0 {
                                        options.len() - 1
                                    } else {
                                        current - 1
                                    };
                                }
                            }
                        }
                    }
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if !app.is_preset_screen()
                            && !app.is_confirm_screen()
                            && !app.is_alias_screen()
                        {
                            // On group screens, Enter advances to next key or next screen
                            if let Some(gi) = app.current_group_idx() {
                                if app.group_cursor + 1 < app.group_keys[gi].len() {
                                    app.group_cursor += 1;
                                } else {
                                    app.screen += 1;
                                    app.group_cursor = 0;
                                }
                            }
                        } else {
                            app.confirm_action();
                        }
                        if app.finished {
                            break;
                        }
                    }
                    KeyCode::Tab if !app.is_confirm_screen() => {
                        if app.is_preset_screen() {
                            app.confirm_action();
                        } else {
                            app.screen += 1;
                            app.group_cursor = 0;
                        }
                    }
                    KeyCode::Backspace => app.go_back(),
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

    // Clear inline viewport
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

    disable_raw_mode().ok();
    terminal.show_cursor().ok();

    result?;

    if app.cancelled {
        anyhow::bail!("Setup cancelled");
    }

    Ok(app.build_choices())
}

/// Apply TUI choices onto a config JSON value, preserving fields not covered by the TUI.
fn apply_tui_choices(config: &mut serde_json::Value, choices: &TuiChoices) -> Result<()> {
    let obj = config
        .as_object_mut()
        .context("hook-config.json is not a JSON object")?;
    for (k, v) in &choices.values {
        obj.insert(k.clone(), v.clone());
    }
    Ok(())
}

/// Install the `xl` shell alias if requested by the user during init.
fn setup_shell_alias(ui: &InitUI, choices: &TuiChoices) {
    if !choices.install_alias {
        return;
    }

    let shell_env = std::env::var("SHELL").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();

    let (config_file, alias_line) = if shell_env.ends_with("fish") {
        (
            format!("{home}/.config/fish/config.fish"),
            "abbr -a xl crosslink",
        )
    } else if shell_env.ends_with("zsh") {
        (format!("{home}/.zshrc"), "alias xl='crosslink'")
    } else if shell_env.ends_with("bash") {
        (format!("{home}/.bashrc"), "alias xl='crosslink'")
    } else {
        ui.warn("Could not detect shell for alias installation");
        return;
    };

    let path = std::path::Path::new(&config_file);

    // Idempotent: check if already present
    if let Ok(content) = fs::read_to_string(path) {
        if content.lines().any(|line| line.trim() == alias_line) {
            ui.step_skip("xl alias already installed");
            return;
        }
    }

    // Append the alias
    ui.step_start("Installing xl alias");
    let line_to_append = format!("\n# crosslink shortcut\n{}\n", alias_line);
    match fs::OpenOptions::new().append(true).open(path) {
        Ok(mut file) => {
            use std::io::Write;
            if let Err(e) = file.write_all(line_to_append.as_bytes()) {
                ui.warn(&format!("Failed to write alias: {e}"));
            } else {
                ui.step_ok(Some(&config_file));
                ui.detail(&format!(
                    "Run `source {}` or open a new terminal to activate",
                    config_file
                ));
            }
        }
        Err(e) => {
            ui.warn(&format!("Could not open {}: {e}", config_file));
        }
    }
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

/// Ensure `repo_compact_id` exists in `.crosslink/repo-id`.
///
/// Generates a 4-char base62 identifier on first init and preserves it
/// on subsequent inits. Stored in a separate file (not hook-config.json)
/// to avoid triggering policy drift detection.
fn ensure_repo_compact_id(crosslink_dir: &Path) -> Result<()> {
    let id_path = crosslink_dir.join("repo-id");
    if id_path.exists() {
        return Ok(());
    }
    let id = crate::utils::generate_compact_id();
    fs::write(&id_path, &id).context("Failed to write repo-id")?;
    Ok(())
}

/// Read `repo_compact_id` from `.crosslink/repo-id`, or return a fallback.
pub fn read_repo_compact_id(crosslink_dir: &Path) -> String {
    let id_path = crosslink_dir.join("repo-id");
    if let Ok(id) = fs::read_to_string(&id_path) {
        let trimmed = id.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    // Fallback: generate from directory hash for consistent identity
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    crosslink_dir.hash(&mut hasher);
    crate::utils::base62_encode_4(hasher.finish())
}

/// Detect project lint/test commands and populate `agent_overrides` in
/// hook-config.json so kickoff agents can self-validate their work (#495).
///
/// Only populates empty arrays — preserves any manually configured commands.
fn populate_agent_tool_commands(config_path: &Path, project_root: &Path) -> Result<()> {
    if !config_path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(config_path)?;
    let mut config: serde_json::Value = serde_json::from_str(&raw)?;

    let overrides = match config.get_mut("agent_overrides") {
        Some(serde_json::Value::Object(m)) => m,
        _ => return Ok(()),
    };

    // Only populate if arrays are empty (don't overwrite manual config)
    let lint_empty = overrides
        .get("agent_lint_commands")
        .and_then(|v| v.as_array())
        .is_none_or(|a| a.is_empty());
    let test_empty = overrides
        .get("agent_test_commands")
        .and_then(|v| v.as_array())
        .is_none_or(|a| a.is_empty());

    if !lint_empty && !test_empty {
        return Ok(()); // Both already configured
    }

    let conv = super::kickoff::detect_conventions(project_root);

    let mut changed = false;

    if lint_empty && !conv.lint_commands.is_empty() {
        overrides.insert(
            "agent_lint_commands".to_string(),
            serde_json::json!(conv.lint_commands),
        );
        changed = true;
    }

    if test_empty {
        if let Some(ref test_cmd) = conv.test_command {
            overrides.insert(
                "agent_test_commands".to_string(),
                serde_json::json!([test_cmd]),
            );
            changed = true;
        }
    }

    if changed {
        let output = serde_json::to_string_pretty(&config)?;
        fs::write(config_path, format!("{output}\n"))?;
    }

    Ok(())
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

    // Pre-flight: verify git repo exists and has at least one commit (#401).
    // Crosslink uses git worktrees for hub/knowledge branches, which require
    // a functioning repo with commit history.
    let git_dir = path.join(".git");
    if !git_dir.exists() {
        anyhow::bail!(
            "No git repository found at {}.\n\
             Run `git init` and create an initial commit before running `crosslink init`.",
            path.display()
        );
    }
    let has_commits = std::process::Command::new("git")
        .current_dir(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_commits {
        anyhow::bail!(
            "Git repository has no commits.\n\
             Create an initial commit before running `crosslink init`:\n\
             \n  git add .\n  git commit -m \"Initial commit\""
        );
    }

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
    let tui_choices = match tui_result {
        Some((mut config, choices)) => {
            apply_tui_choices(&mut config, &choices)?;
            let output = serde_json::to_string_pretty(&config)
                .context("Failed to serialize hook-config.json")?;
            fs::write(&config_path, format!("{}\n", output))
                .context("Failed to write hook-config.json")?;
            ui.step_created("hook-config.json");
            Some(choices)
        }
        None if !config_exists || force => {
            fs::write(&config_path, HOOK_CONFIG_JSON)
                .context("Failed to write hook-config.json")?;
            ui.step_created("hook-config.json");
            None
        }
        None => None,
    };

    // Ensure repo_compact_id exists in hook-config.json (idempotent)
    ensure_repo_compact_id(&crosslink_dir)?;

    // Auto-detect lint/test commands for agent overrides (#495)
    populate_agent_tool_commands(&config_path, path)?;

    // Write .crosslink/.gitignore for multi-agent files
    let crosslink_gitignore = crosslink_dir.join(".gitignore");
    if !crosslink_gitignore.exists() || force {
        fs::write(
            &crosslink_gitignore,
            "# Multi-agent collaboration (machine-local)\n\
             agent.json\n\
             repo-id\n\
             .hub-cache/\n\
             .knowledge-cache/\n\
             keys/\n\
             integrations/\n\
             \n\
             # Machine-local overrides\n\
             hook-config.local.json\n\
             rules.local/\n",
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

    // Create rules.local directory for machine-local rule overrides
    let rules_local_dir = crosslink_dir.join("rules.local");
    if !rules_local_dir.exists() {
        fs::create_dir_all(&rules_local_dir)
            .context("Failed to create .crosslink/rules.local directory")?;
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
        fs::write(hooks_dir.join("heartbeat.py"), HEARTBEAT_PY)
            .context("Failed to write heartbeat.py")?;

        let mcp_dir = claude_dir.join("mcp");
        fs::create_dir_all(&mcp_dir).context("Failed to create .claude/mcp directory")?;
        fs::write(mcp_dir.join("safe-fetch-server.py"), SAFE_FETCH_SERVER_PY)
            .context("Failed to write safe-fetch-server.py")?;
        fs::write(mcp_dir.join("knowledge-server.py"), KNOWLEDGE_SERVER_PY)
            .context("Failed to write knowledge-server.py")?;

        let commands_dir = claude_dir.join("commands");
        fs::create_dir_all(&commands_dir).context("Failed to create .claude/commands directory")?;
        for (filename, content) in COMMAND_FILES {
            fs::write(commands_dir.join(filename), content)
                .with_context(|| format!("Failed to write {}", filename))?;
        }

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

    // Shell alias setup (REQ-10)
    if let Some(ref choices) = tui_choices {
        setup_shell_alias(&ui, choices);
    }

    ui.success();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Create a temp directory with a git repo and initial commit.
    /// Required because init now checks for .git and HEAD (#401).
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        assert!(dir.path().join(".claude/mcp/knowledge-server.py").exists());
        assert!(dir.path().join(".mcp.json").exists());
    }

    #[test]
    fn test_run_creates_rule_files() {
        let dir = test_dir();
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
        let dir = test_dir();

        // First init
        run(dir.path(), &test_opts(false)).unwrap();

        // Second init without force - should succeed but not recreate
        let result = run(dir.path(), &test_opts(false));
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_force_update() {
        let dir = test_dir();

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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        assert!(parsed["mcpServers"]["crosslink-knowledge"].is_object());
    }

    #[test]
    fn test_run_partial_init_crosslink_only() {
        let dir = test_dir();

        // Create only .crosslink directory
        fs::create_dir_all(dir.path().join(".crosslink")).unwrap();

        let result = run(dir.path(), &test_opts(false));
        assert!(result.is_ok());

        // .claude should now exist
        assert!(dir.path().join(".claude").exists());
    }

    #[test]
    fn test_run_partial_init_claude_only() {
        let dir = test_dir();

        // Create only .claude directory
        fs::create_dir_all(dir.path().join(".claude")).unwrap();

        let result = run(dir.path(), &test_opts(false));
        assert!(result.is_ok());

        // .crosslink should now exist
        assert!(dir.path().join(".crosslink").exists());
    }

    #[test]
    fn test_run_database_usable() {
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();

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
    #[allow(clippy::const_is_empty)]
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
        // All auto-discovered command files should be non-empty
        assert!(
            COMMAND_FILES.len() >= 11,
            "Expected at least 11 command files, found {}",
            COMMAND_FILES.len()
        );
        for (filename, content) in COMMAND_FILES {
            assert!(!content.is_empty(), "Command file {} is empty", filename);
        }
        assert!(!RULE_SANITIZE_PATTERNS.is_empty());
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
        let dir = test_dir();
        run(dir.path(), &test_opts(false)).unwrap();

        let content = fs::read_to_string(dir.path().join(".crosslink/.gitignore")).unwrap();
        assert!(content.contains("agent.json"));
        assert!(content.contains(".hub-cache/"));
        assert!(content.contains("hook-config.local.json"));
    }

    // --- Python toolchain detection tests ---

    #[test]
    fn test_detect_python_prefix_default() {
        let dir = test_dir();
        assert_eq!(detect_python_prefix(dir.path()), "python3");
    }

    #[test]
    fn test_detect_python_prefix_uv_lock() {
        let dir = test_dir();
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "uv run python3");
    }

    #[test]
    fn test_detect_python_prefix_uv_pyproject() {
        let dir = test_dir();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"foo\"\n\n[tool.uv]\ndev-dependencies = []\n",
        )
        .unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "uv run python3");
    }

    #[test]
    fn test_detect_python_prefix_poetry_lock() {
        let dir = test_dir();
        fs::write(dir.path().join("poetry.lock"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "poetry run python3");
    }

    #[test]
    fn test_detect_python_prefix_poetry_pyproject() {
        let dir = test_dir();
        fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"foo\"\n\n[tool.poetry]\nname = \"foo\"\n",
        )
        .unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "poetry run python3");
    }

    #[test]
    fn test_detect_python_prefix_venv() {
        let dir = test_dir();
        fs::create_dir(dir.path().join(".venv")).unwrap();
        assert_eq!(detect_python_prefix(dir.path()), ".venv/bin/python3");
    }

    #[test]
    fn test_detect_python_prefix_pipenv() {
        let dir = test_dir();
        fs::write(dir.path().join("Pipfile"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "pipenv run python3");
    }

    #[test]
    fn test_detect_python_prefix_pipenv_lock() {
        let dir = test_dir();
        fs::write(dir.path().join("Pipfile.lock"), "{}").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "pipenv run python3");
    }

    #[test]
    fn test_detect_python_prefix_uv_beats_poetry() {
        let dir = test_dir();
        // Both uv.lock and poetry.lock present — uv wins
        fs::write(dir.path().join("uv.lock"), "").unwrap();
        fs::write(dir.path().join("poetry.lock"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "uv run python3");
    }

    #[test]
    fn test_detect_python_prefix_poetry_beats_venv() {
        let dir = test_dir();
        fs::write(dir.path().join("poetry.lock"), "").unwrap();
        fs::create_dir(dir.path().join(".venv")).unwrap();
        assert_eq!(detect_python_prefix(dir.path()), "poetry run python3");
    }

    #[test]
    fn test_detect_python_prefix_venv_beats_pipenv() {
        let dir = test_dir();
        fs::create_dir(dir.path().join(".venv")).unwrap();
        fs::write(dir.path().join("Pipfile"), "").unwrap();
        assert_eq!(detect_python_prefix(dir.path()), ".venv/bin/python3");
    }

    #[test]
    fn test_detect_python_prefix_pyproject_without_tools_is_default() {
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();
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
        let dir = test_dir();

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
        let dir = test_dir();

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
        let dir = test_dir();
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
        let dir = test_dir();
        write_root_gitignore(dir.path()).unwrap();

        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.starts_with(GITIGNORE_SECTION_START));
        assert!(content.contains(GITIGNORE_SECTION_END));
        assert!(content.contains(".crosslink/issues.db"));
    }

    #[test]
    fn test_write_root_gitignore_replaces_section() {
        let dir = test_dir();

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
        let dir = test_dir();
        run(dir.path(), &test_opts(false)).unwrap();

        let content = fs::read_to_string(dir.path().join(".crosslink/.gitignore")).unwrap();
        assert!(content.contains("integrations/"));
    }

    // --- Tier 1/2 smoke tests (GH issue #242) ---

    #[test]
    fn test_init_deploys_skill_files() {
        let dir = test_dir();
        run(dir.path(), &test_opts(false)).unwrap();

        let commands_dir = dir.path().join(".claude/commands");
        assert!(
            commands_dir.join("maintain.md").exists(),
            "maintain.md skill not deployed"
        );
        assert!(
            commands_dir.join("design.md").exists(),
            "design.md skill not deployed"
        );

        // Verify non-empty content
        let maintain = fs::read_to_string(commands_dir.join("maintain.md")).unwrap();
        assert!(!maintain.is_empty(), "maintain.md is empty");
        let design = fs::read_to_string(commands_dir.join("design.md")).unwrap();
        assert!(!design.is_empty(), "design.md is empty");
    }

    #[test]
    fn test_init_deploys_mcp_knowledge_server() {
        let dir = test_dir();
        run(dir.path(), &test_opts(false)).unwrap();

        // knowledge-server.py must exist
        assert!(
            dir.path().join(".claude/mcp/knowledge-server.py").exists(),
            "knowledge-server.py not deployed"
        );

        // .mcp.json must reference both MCP servers
        let mcp_content = fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let mcp: serde_json::Value = serde_json::from_str(&mcp_content).unwrap();
        let servers = mcp["mcpServers"].as_object().unwrap();
        assert!(
            servers.contains_key("crosslink-safe-fetch"),
            ".mcp.json missing crosslink-safe-fetch"
        );
        assert!(
            servers.contains_key("crosslink-knowledge"),
            ".mcp.json missing crosslink-knowledge"
        );
    }

    #[test]
    fn test_force_init_deploys_skill_files() {
        let dir = test_dir();
        // First init
        run(dir.path(), &test_opts(false)).unwrap();

        // Delete skill files to simulate upgrade scenario
        let commands_dir = dir.path().join(".claude/commands");
        fs::remove_file(commands_dir.join("maintain.md")).unwrap();
        fs::remove_file(commands_dir.join("design.md")).unwrap();
        assert!(!commands_dir.join("maintain.md").exists());
        assert!(!commands_dir.join("design.md").exists());

        // Force init should re-deploy them
        run(dir.path(), &test_opts(true)).unwrap();
        assert!(commands_dir.join("maintain.md").exists());
        assert!(commands_dir.join("design.md").exists());
    }
}
