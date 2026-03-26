// E-ana tablet — kickoff launch: agent launch infrastructure
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::identity::AgentConfig;

use super::helpers::*;
use super::types::*;

/// Resolve the correct `timeout` command for the current platform.
///
/// On macOS, `timeout` is not available by default. The GNU coreutils
/// package (via Homebrew) installs it as `gtimeout`.
/// Returns the command name to use, or an error with install instructions.
fn resolve_timeout_command(platform: &Platform) -> Result<&'static str> {
    if command_available("timeout") {
        return Ok("timeout");
    }
    if command_available("gtimeout") {
        return Ok("gtimeout");
    }
    bail!(
        "Neither `timeout` nor `gtimeout` found.\n{}",
        install_hint("timeout", platform)
    );
}

/// Read the `sandbox.command` setting from hook-config.json, if configured.
pub(super) fn read_sandbox_command(crosslink_dir: &Path) -> Option<String> {
    let config_path = crosslink_dir.join("hook-config.json");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
    parsed
        .get("sandbox")
        .and_then(|s| s.get("command"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub(super) fn read_watchdog_config(crosslink_dir: &Path) -> WatchdogConfig {
    let config_path = crosslink_dir.join("hook-config.json");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return WatchdogConfig::default(),
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return WatchdogConfig::default(),
    };

    let wd = match parsed.get("watchdog") {
        Some(v) => v,
        None => return WatchdogConfig::default(),
    };

    let mut cfg = WatchdogConfig::default();
    if let Some(v) = wd.get("enabled").and_then(|v| v.as_bool()) {
        cfg.enabled = v;
    }
    if let Some(v) = wd.get("staleness_secs").and_then(|v| v.as_u64()) {
        cfg.staleness_secs = v;
    }
    if let Some(v) = wd.get("max_nudges").and_then(|v| v.as_u64()) {
        cfg.max_nudges = v as u32;
    }
    if let Some(v) = wd.get("check_interval_secs").and_then(|v| v.as_u64()) {
        cfg.check_interval_secs = v;
    }
    if let Some(v) = wd.get("grace_period_secs").and_then(|v| v.as_u64()) {
        cfg.grace_period_secs = v;
    }
    cfg
}

/// Build the watchdog shell script that monitors heartbeat staleness and
/// nudges idle agents by sending "continue" via tmux send-keys.
pub(super) fn build_watchdog_script(
    session_name: &str,
    worktree_dir: &Path,
    cfg: &WatchdogConfig,
) -> String {
    // Use portable stat command — try GNU stat first, fall back to BSD
    format!(
        r#"NUDGES=0
sleep {grace}
while true; do
    sleep {interval}
    if [ -f "{worktree}/.kickoff-status" ]; then exit 0; fi
    if ! tmux has-session -t "{session}" 2>/dev/null; then exit 0; fi
    HB="{worktree}/.crosslink/.cache/last-heartbeat"
    if [ -f "$HB" ]; then
        LAST=$(stat -c %Y "$HB" 2>/dev/null || stat -f %m "$HB" 2>/dev/null)
        NOW=$(date +%s)
        AGE=$((NOW - LAST))
        if [ "$AGE" -gt {staleness} ]; then
            if [ "$NUDGES" -ge {max_nudges} ]; then exit 1; fi
            NUDGES=$((NUDGES + 1))
            tmux send-keys -t "{session}" "continue working, the task is not yet complete" Enter
        fi
    fi
done
"#,
        grace = cfg.grace_period_secs,
        interval = cfg.check_interval_secs,
        worktree = worktree_dir.display(),
        session = session_name,
        staleness = cfg.staleness_secs,
        max_nudges = cfg.max_nudges,
    )
}

/// Spawn a background watchdog process that monitors the agent's heartbeat
/// and sends "continue" to the tmux session if the agent goes idle.
pub(super) fn spawn_watchdog(
    session_name: &str,
    worktree_dir: &Path,
    cfg: &WatchdogConfig,
) -> Result<()> {
    let script = build_watchdog_script(session_name, worktree_dir, cfg);

    Command::new("bash")
        .args(["-c", &script])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn watchdog process")?;

    Ok(())
}

/// Build the shell command string for launching a claude agent.
///
/// When `sandbox_command` is set, the claude invocation is wrapped:
/// ```text
/// timeout 3600s my-sandbox --project-dir /path -- env -u CLAUDECODE claude ...
/// ```
/// Without sandbox:
/// ```text
/// timeout 3600s env -u CLAUDECODE claude ...
/// ```
#[allow(clippy::too_many_arguments)]
pub(super) fn build_agent_command(
    timeout_cmd: &str,
    timeout_secs: u64,
    model: &str,
    allowed_tools: &str,
    kickoff_file: &str,
    sandbox_command: Option<&str>,
    worktree_dir: &Path,
    skip_permissions: bool,
) -> String {
    use crate::utils::shell_escape_arg;

    let skip_flag = if skip_permissions {
        " --dangerously-skip-permissions"
    } else {
        ""
    };
    let escaped_model = shell_escape_arg(model);
    let escaped_tools = shell_escape_arg(allowed_tools);
    let escaped_kickoff = shell_escape_arg(kickoff_file);
    let claude_cmd = format!(
        "env -u CLAUDECODE claude{} --model {} --allowedTools {} -- \"$(cat {})\"",
        skip_flag, escaped_model, escaped_tools, escaped_kickoff
    );
    match sandbox_command {
        Some(cmd) => {
            let escaped_worktree = shell_escape_arg(&worktree_dir.to_string_lossy());
            let expanded = cmd.replace("{{worktree}}", &escaped_worktree);
            format!(
                "{} {}s {} {}",
                timeout_cmd, timeout_secs, expanded, claude_cmd
            )
        }
        None => format!("{} {}s {}", timeout_cmd, timeout_secs, claude_cmd),
    }
}

/// Pre-flight check: verify all required external commands are present before
/// creating worktrees, branches, or sessions. Emits clear errors with install
/// instructions for any missing command.
pub(super) fn preflight_check(
    container: &ContainerMode,
    verify: &VerifyLevel,
    crosslink_dir: &Path,
) -> Result<PreflightResult> {
    let platform = detect_platform();
    let mut missing: Vec<String> = Vec::new();

    // timeout (or gtimeout on macOS) — always required for agent timeout
    let timeout_cmd = match resolve_timeout_command(&platform) {
        Ok(cmd) => cmd,
        Err(e) => {
            missing.push(format!("{}", e));
            "timeout" // placeholder, won't be used since we'll bail
        }
    };

    // tmux — required for local (non-container) mode
    // On Windows, tmux is not available at all — bail early with a clear message.
    if *container == ContainerMode::None {
        if cfg!(target_os = "windows") {
            bail!(
                "Local kickoff mode requires tmux, which is not available on Windows.\n\
                 Use `--container docker` for agent kickoff on Windows."
            );
        }
        if !command_available("tmux") {
            missing.push(install_hint("tmux", &platform));
        }
    }

    // claude CLI — required for local mode
    if *container == ContainerMode::None && !command_available("claude") {
        missing.push(install_hint("claude", &platform));
    }

    // gh — required for CI/thorough verification
    if (*verify == VerifyLevel::Ci || *verify == VerifyLevel::Thorough) && !command_available("gh")
    {
        missing.push(install_hint("gh", &platform));
    }

    // docker/podman — required when using container mode
    match container {
        ContainerMode::Docker if !command_available("docker") => {
            missing.push(install_hint("docker", &platform));
        }
        ContainerMode::Podman if !command_available("podman") => {
            missing.push(install_hint("podman", &platform));
        }
        _ => {}
    }

    // sandbox command — validate the binary exists when configured
    let sandbox_command = read_sandbox_command(crosslink_dir);
    if let Some(ref cmd) = sandbox_command {
        // Extract the binary name (first word before any flags/templates)
        let binary = cmd.split_whitespace().next().unwrap_or(cmd);
        if !command_available(binary) {
            missing.push(format!(
                "`{}` (configured in hook-config.json sandbox.command) not found on PATH",
                binary
            ));
        }
    }

    if !missing.is_empty() {
        let header = format!(
            "Pre-flight check failed — {} missing command{}:\n",
            missing.len(),
            if missing.len() == 1 { "" } else { "s" }
        );
        let body = missing
            .iter()
            .enumerate()
            .map(|(i, msg)| format!("{}. {}", i + 1, msg))
            .collect::<Vec<_>>()
            .join("\n\n");
        bail!("{}{}", header, body);
    }

    Ok(PreflightResult {
        timeout_cmd,
        sandbox_command,
    })
}

/// Get the main git repository root, resolving through worktrees.
///
/// Uses `git rev-parse --show-toplevel` to find the current repo, then
/// `resolve_main_repo_root()` to follow worktree links back to the main
/// repository. This ensures worktrees are always created relative to the
/// main repo, not inside internal directories like `.crosslink/` (#425).
pub(super) fn repo_root() -> Result<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git rev-parse")?;
    if !output.status.success() {
        bail!("Not inside a git repository");
    }
    let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let toplevel_path = std::path::PathBuf::from(&toplevel);

    // Resolve through worktrees to the main repo root (#425)
    Ok(crate::utils::resolve_main_repo_root(&toplevel_path).unwrap_or(toplevel_path))
}

/// Create a feature branch and worktree for the agent.
///
/// The worktree is created at `<repo_root>/.worktrees/<slug>`. A safety
/// guard prevents worktrees from landing inside internal directories
/// like `.crosslink/` or `.git/` (#425).
pub(super) fn create_worktree(
    repo_root: &Path,
    slug: &str,
    base_branch: Option<&str>,
) -> Result<(std::path::PathBuf, String)> {
    let branch_name = format!("feature/{}", slug);
    let worktree_dir = repo_root.join(".worktrees").join(slug);

    // Safety guard: reject worktree paths that land inside internal directories (#425)
    let canonical_root = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    for forbidden in [".crosslink", ".git"] {
        let forbidden_dir = canonical_root.join(forbidden);
        if let Ok(canonical_wt) = worktree_dir.canonicalize() {
            if canonical_wt.starts_with(&forbidden_dir) {
                bail!(
                    "Worktree path {} would land inside {}/. \
                     This usually means repo_root resolved to an internal directory. \
                     Please run this command from the main repository root.",
                    worktree_dir.display(),
                    forbidden
                );
            }
        }
    }

    if worktree_dir.exists() {
        bail!(
            "Worktree already exists at {}. Remove it first or use --branch to target an existing branch.",
            worktree_dir.display()
        );
    }

    // Determine base ref
    let base = base_branch.unwrap_or("HEAD");

    // Handle existing branch refs from prior phases (#481).
    // A branch may exist from a previous kickoff/swarm phase that was
    // already merged. Rather than failing, clean it up automatically.
    let branch_exists = Command::new("git")
        .current_dir(repo_root)
        .args(["rev-parse", "--verify", &branch_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if branch_exists {
        // Check if the branch has an active worktree
        let wt_output = Command::new("git")
            .current_dir(repo_root)
            .args(["worktree", "list", "--porcelain"])
            .output()
            .context("Failed to list worktrees")?;
        let wt_list = String::from_utf8_lossy(&wt_output.stdout);
        let has_active_worktree = wt_list
            .lines()
            .any(|line| line.starts_with("branch ") && line.ends_with(&branch_name));

        if has_active_worktree {
            bail!(
                "Branch '{}' already exists and has an active worktree. \
                 Clean up the worktree first with: git worktree remove <path>",
                branch_name
            );
        }

        // Check if the branch is fully merged into the base
        let is_merged = Command::new("git")
            .current_dir(repo_root)
            .args(["merge-base", "--is-ancestor", &branch_name, base])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);

        if is_merged {
            // Branch is fully merged — safe to delete and recreate
            tracing::info!(
                "branch '{}' exists from a prior phase and is fully merged, recreating",
                branch_name
            );
            let delete_output = Command::new("git")
                .current_dir(repo_root)
                .args(["branch", "-d", &branch_name])
                .output()
                .context("Failed to delete merged branch")?;
            if !delete_output.status.success() {
                let stderr = String::from_utf8_lossy(&delete_output.stderr);
                bail!(
                    "Branch '{}' is merged but could not be deleted: {}",
                    branch_name,
                    stderr.trim()
                );
            }
        } else {
            bail!(
                "Branch '{}' already exists and has unmerged changes. \
                 Either merge it first, delete it manually with \
                 `git branch -D {}`, or use a different slug.",
                branch_name,
                branch_name
            );
        }
    }

    // Create the worktree with a new branch
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["worktree", "add", "-b", &branch_name])
        .arg(&worktree_dir)
        .arg(base)
        .output()
        .context("Failed to create git worktree")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create worktree: {}", stderr.trim());
    }

    Ok((worktree_dir, branch_name))
}

/// Initialize crosslink and agent identity in the worktree.
pub(super) fn init_worktree_agent(
    worktree_dir: &Path,
    crosslink_dir: &Path,
    compact_name: &str,
) -> Result<String> {
    // Run crosslink init --force in the worktree
    let output = Command::new("crosslink")
        .current_dir(worktree_dir)
        .args(["init", "--force", "--skip-signing", "--defaults"])
        .output()
        .context("Failed to run crosslink init in worktree")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::warn!("crosslink init in worktree: {}", stderr.trim());
    }

    // Use the compact name as the agent ID directly
    let agent_id = compact_name.to_string();

    // Initialize agent identity with its own signing key (#505).
    // Previous approach inherited the parent's key with no_key=true, but
    // that failed when no parent agent config existed (e.g. driver-invoked
    // kickoff). Now each subagent gets a dedicated keypair, and is
    // auto-approved since the driver explicitly launched it.
    let wt_crosslink = worktree_dir.join(".crosslink");
    if wt_crosslink.exists() {
        // Only init if not already configured
        if AgentConfig::load(&wt_crosslink)?.is_none() {
            if let Err(e) = super::super::agent::init(
                &wt_crosslink,
                &agent_id,
                Some(&format!("Kickoff agent for: {}", compact_name)),
                false, // generate dedicated signing key
                false,
            ) {
                tracing::warn!("could not initialize agent identity in worktree: {e} — agent will work without its own identity");
            }

            // Auto-approve: the driver explicitly invoked kickoff, so trust
            // is implicit. This eliminates the manual sync → pending → approve
            // workflow that blocked autonomous agent operation.
            if let Err(e) = super::super::trust::approve(crosslink_dir, &agent_id) {
                tracing::warn!(
                    "could not auto-approve agent '{}': {e} — run `crosslink trust approve {}` manually",
                    agent_id, agent_id
                );
            }
        }
    }

    // Sync coordination state
    let output = Command::new("crosslink")
        .current_dir(worktree_dir)
        .args(["sync"])
        .output();

    if let Ok(o) = output {
        if !o.status.success() {
            tracing::warn!("crosslink sync in worktree returned non-zero");
        }
    }

    Ok(agent_id)
}

/// Exclude kickoff files from git tracking.
pub(super) fn exclude_kickoff_files(worktree_dir: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(worktree_dir)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .context("Failed to get git common dir")?;

    let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let exclude_path = std::path::PathBuf::from(&common_dir).join("info/exclude");

    // Ensure parent directory exists
    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    let additions = missing_exclude_patterns(&existing);

    if !additions.is_empty() {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude_path)
            .context("Failed to open git exclude file")?;
        for pattern in additions {
            writeln!(file, "{}", pattern)?;
        }
    }

    Ok(())
}

/// Launch the agent as a local tmux process.
#[allow(clippy::too_many_arguments)]
pub(super) fn launch_local(
    worktree_dir: &Path,
    session_name: &str,
    model: &str,
    allowed_tools: &str,
    timeout: Duration,
    timeout_cmd: &str,
    sandbox_command: Option<&str>,
    crosslink_dir: &Path,
    skip_permissions: bool,
) -> Result<()> {
    // Create the tmux session
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session_name,
            "-c",
            &worktree_dir.to_string_lossy(),
        ])
        .output()
        .context("Failed to create tmux session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create tmux session: {}", stderr.trim());
    }

    // Build the claude command (with optional sandbox wrapping)
    let cmd = build_agent_command(
        timeout_cmd,
        timeout.as_secs(),
        model,
        allowed_tools,
        "KICKOFF.md",
        sandbox_command,
        worktree_dir,
        skip_permissions,
    );

    // Write initial status sentinel BEFORE sending the command.
    // This ensures we never have a worktree in limbo with no status.
    std::fs::write(worktree_dir.join(".kickoff-status"), "LAUNCHING\n")
        .context("Failed to write initial .kickoff-status")?;

    // Send the command to the tmux session
    let output = Command::new("tmux")
        .args(["send-keys", "-t", session_name, &cmd, "Enter"])
        .output()
        .context("Failed to send command to tmux session")?;

    if !output.status.success() {
        // INTENTIONAL: status file write is best-effort — used for monitoring, not control flow
        let _ = std::fs::write(worktree_dir.join(".kickoff-status"), "FAILED\n");
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to send keys to tmux: {}", stderr.trim());
    }

    // INTENTIONAL: status file write is best-effort — used for monitoring, not control flow
    let _ = std::fs::write(worktree_dir.join(".kickoff-status"), "RUNNING\n");

    // Spawn watchdog sidecar to nudge idle agents
    let watchdog_cfg = read_watchdog_config(crosslink_dir);
    if watchdog_cfg.enabled {
        if let Err(e) = spawn_watchdog(session_name, worktree_dir, &watchdog_cfg) {
            tracing::warn!("failed to spawn watchdog: {}", e);
        }
    }

    Ok(())
}

/// Launch the agent in a Docker or Podman container.
pub(super) fn launch_container(
    runtime: &ContainerMode,
    worktree_dir: &Path,
    image: &str,
    agent_id: &str,
    model: &str,
    allowed_tools: &str,
    timeout: Duration,
) -> Result<String> {
    let runtime_cmd = match runtime {
        ContainerMode::Docker => "docker",
        ContainerMode::Podman => "podman",
        ContainerMode::None => unreachable!(),
    };

    // Check runtime is available
    if !command_available(runtime_cmd) {
        bail!(
            "{} is not installed. Install it or use --container none for local mode.",
            runtime_cmd
        );
    }

    let timeout_secs = timeout.as_secs();
    let container_name = format!("crosslink-agent-{}", agent_id);

    // Resolve host auth path for credential mounting
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let host_auth = format!("{}/.claude", home);

    // Get host UID/GID for remapping (skip on Windows — Docker Desktop handles user mapping)
    let uid_gid = if cfg!(target_os = "windows") {
        None
    } else {
        let uid = Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "1000".to_string());
        let gid = Command::new("id")
            .arg("-g")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "1000".to_string());
        Some((uid, gid))
    };

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container_name.clone(),
        // Hard-kill the container after the timeout (grace period = 10s on top)
        "--stop-timeout".to_string(),
        format!("{}", timeout_secs),
        // Mount the worktree as workspace
        "-v".to_string(),
        format!("{}:/workspaces/repo", worktree_dir.to_string_lossy()),
        // Mount credentials read-only
        "-v".to_string(),
        format!("{}:/host-auth:ro", host_auth),
        // Environment
        "-e".to_string(),
        format!("AGENT_ID={}", agent_id),
    ];

    // Pass UID/GID to container for user remapping (non-Windows only)
    if let Some((uid, gid)) = &uid_gid {
        args.extend([
            "-e".to_string(),
            format!("HOST_UID={}", uid),
            "-e".to_string(),
            format!("HOST_GID={}", gid),
        ]);
    }

    // Image and command
    args.push(image.to_string());
    args.push("bash".to_string());
    args.push("-c".to_string());
    args.push(format!(
        "cd /workspaces/repo && timeout {}s claude --model {} --allowedTools '{}' -- \"$(cat KICKOFF.md)\"",
        timeout_secs, model, allowed_tools
    ));

    let output = Command::new(runtime_cmd)
        .args(&args)
        .output()
        .with_context(|| format!("Failed to launch {} container", runtime_cmd))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} container launch failed: {}", runtime_cmd, stderr.trim());
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(container_id)
}
