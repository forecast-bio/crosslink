use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::identity::{AgentConfig, AgentRole};
use crate::signing;
use crate::sync;
use crate::utils::format_issue_id;
use crate::AgentCommands;

pub fn run(command: AgentCommands, crosslink_dir: &Path) -> Result<()> {
    match command {
        AgentCommands::Init {
            agent_id,
            description,
            no_key,
            force,
            role,
        } => {
            let parsed_role = match role.as_deref() {
                Some("driver") => AgentRole::Driver,
                Some("agent") | None => AgentRole::Agent,
                Some(other) => bail!("invalid role {other:?}; expected 'driver' or 'agent'"),
            };
            init(
                crosslink_dir,
                &agent_id,
                description.as_deref(),
                no_key,
                force,
                parsed_role,
            )
        }
        AgentCommands::Status => status(crosslink_dir),
        AgentCommands::Prompt {
            session,
            message,
            no_submit,
        } => prompt(&session, &message, !no_submit),
        AgentCommands::Bootstrap {
            repo,
            identity,
            branch,
            description,
            no_key,
            target,
        } => {
            let target_path = std::path::PathBuf::from(&target);
            bootstrap(
                &target_path,
                &repo,
                &identity,
                branch.as_deref(),
                description.as_deref(),
                no_key,
            )?;
            // The agent's v3 ref is created on its first hub write (heartbeat /
            // mutation), so there is no separate agent-directory bootstrap step
            // anymore (the v2 `agents/<id>/` write path was removed in 754b).
            Ok(())
        }
        AgentCommands::Request {
            target,
            kind,
            subject_issue,
            reason,
        } => request(
            crosslink_dir,
            &target,
            &kind,
            subject_issue,
            reason.as_deref(),
        ),
        AgentCommands::Requests { target, pending } => {
            list_requests(crosslink_dir, target.as_deref(), pending)
        }
        AgentCommands::PollRequests => poll_requests(crosslink_dir),
        AgentCommands::Flags { strict } => {
            show_flags(crosslink_dir, strict);
            Ok(())
        }
    }
}

/// `crosslink agent flags [--strict]`
///
/// Reports whether `paused` / `kill` / `reprioritise` flags are set
/// in `.crosslink/agent-flags/`. With `--strict`, exits 2 when either
/// `paused` or `kill` is active so `PreToolUse` hooks can block tool
/// invocation cleanly.
fn show_flags(crosslink_dir: &Path, strict: bool) {
    let paused = crate::agent_flags::is_paused(crosslink_dir);
    let kill = crate::agent_flags::should_exit(crosslink_dir);
    let hint = crate::agent_flags::read_reprioritise_hint(crosslink_dir)
        .ok()
        .flatten();

    let json = serde_json::json!({
        "paused": paused,
        "kill": kill,
        "reprioritise": hint.as_ref().map(|h| serde_json::json!({
            "issue_id": h.issue_id,
            "from_request_id": h.from_request_id,
        })),
    });
    println!("{json}");

    if strict && (paused || kill) {
        // Exit code 2 = "blocked by control flag". Distinguishes from
        // genuine errors (1) so hooks can pattern-match cleanly.
        std::process::exit(2);
    }
}

/// `crosslink agent poll-requests`
///
/// Scans the hub cache for pending requests targeted at this agent,
/// applies each one's local flag (pause / resume / kill / reprioritise),
/// and writes an ack to the hub so drivers see the outcome. Idempotent
/// — already-acked requests are skipped.
fn poll_requests(crosslink_dir: &Path) -> Result<()> {
    let writer = crate::shared_writer::SharedWriter::new(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "agent poll-requests requires shared-writer mode (run `crosslink agent init` first)"
        )
    })?;
    let agent = AgentConfig::load(crosslink_dir)?
        .ok_or_else(|| anyhow::anyhow!("no agent config; run `crosslink agent init`"))?;

    let result =
        crate::agent_requests::poll::process_pending(&writer, crosslink_dir, &agent.agent_id)?;

    if result.acted.is_empty() && result.skipped_existing_ack == 0 {
        println!("No pending requests for {}.", agent.agent_id);
        return Ok(());
    }
    for a in &result.acted {
        println!(
            "{}  {:?}  acted={}  {}  ({:?})",
            a.request_id, a.kind, a.acted, a.result, a.push_outcome
        );
    }
    if result.skipped_existing_ack > 0 {
        println!(
            "Skipped {} already-acked request(s).",
            result.skipped_existing_ack
        );
    }
    Ok(())
}

/// `crosslink agent request <target> <kind> [--subject-issue N] [--reason "..."]`
///
/// Writes a signed JSON file to `agents/<target>/requests/<ulid>.json`
/// on `crosslink/hub`. The target agent's polling loop picks it up and
/// executes the action (design doc §9).
fn request(
    crosslink_dir: &Path,
    target: &str,
    kind_str: &str,
    subject_issue: Option<i64>,
    reason: Option<&str>,
) -> Result<()> {
    let kind = crate::agent_requests::RequestKind::parse(kind_str)?;

    let writer = crate::shared_writer::SharedWriter::new(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "agent request requires hub access — run `crosslink sync` to initialize, or `crosslink agent init` for a full agent identity"
        )
    })?;

    // Drivers (operators) may not have run `crosslink agent init`;
    // they have a `user.signingkey` in git but no agent.json. Fall
    // back to that fingerprint so the dashboard can send requests
    // without forcing operators through agent setup.
    let requested_by = if let Some(driver) = AgentConfig::load(crosslink_dir)? {
        driver
            .ssh_fingerprint
            .clone()
            .unwrap_or_else(|| format!("agent:{}", driver.agent_id))
    } else {
        let workspace_root = crosslink_dir.parent().unwrap_or(crosslink_dir);
        resolve_driver_signing_key(workspace_root).unwrap_or_else(|| "driver:unknown".to_string())
    };

    let req = crate::agent_requests::AgentRequest {
        request_id: crate::agent_requests::new_request_id(),
        kind,
        subject: crate::agent_requests::RequestSubject {
            issue_id: subject_issue,
        },
        requested_by,
        requested_at: chrono::Utc::now().to_rfc3339(),
        reason: reason.map(str::to_string),
    };

    let outcome = writer.write_agent_request(target, &req)?;
    match outcome {
        crate::shared_writer::PushOutcome::Pushed => {
            println!(
                "Request {} ({}) sent to {}.",
                req.request_id, kind_str, target
            );
        }
        crate::shared_writer::PushOutcome::LocalOnly => {
            println!(
                "Request {} ({}) saved locally for {} (push deferred — offline or contested hub).",
                req.request_id, kind_str, target
            );
        }
    }
    Ok(())
}

/// Read `user.signingkey` from the workspace's git config and turn it
/// into a fingerprint. Used as a fallback identity when no agent.json
/// exists. Returns `None` if the config is unset or the key file
/// can't be fingerprinted.
fn resolve_driver_signing_key(workspace_root: &Path) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["config", "user.signingkey"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    // user.signingkey can be either a raw fingerprint or a path to a
    // public/private key file. If it's a path we fingerprint it; if
    // not we just record what's there as the principal.
    let path = std::path::Path::new(&raw);
    if path.exists() {
        let pub_path = if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pub"))
        {
            path.to_path_buf()
        } else {
            std::path::PathBuf::from(format!("{raw}.pub"))
        };
        if pub_path.exists() {
            if let Ok(fp) = signing::get_key_fingerprint(&pub_path) {
                return Some(fp);
            }
        }
    }
    Some(raw)
}

/// `crosslink agent requests [--target <id>] [--pending]`
fn list_requests(crosslink_dir: &Path, target: Option<&str>, pending_only: bool) -> Result<()> {
    let sync = sync::SyncManager::new(crosslink_dir)?;
    let cache = sync.cache_path();
    if !cache.exists() {
        println!("No hub cache present (nothing synced yet).");
        return Ok(());
    }

    let agents_dir = cache.join("agents");
    if !agents_dir.exists() {
        println!("No agents on hub yet.");
        return Ok(());
    }

    let agent_ids: Vec<String> = if let Some(t) = target {
        vec![t.to_string()]
    } else {
        std::fs::read_dir(&agents_dir)
            .with_context(|| format!("read_dir {}", agents_dir.display()))?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .collect()
    };

    let mut total = 0;
    for agent_id in agent_ids {
        let entries = crate::agent_requests::scan(cache, &agent_id)?;
        for row in entries {
            if pending_only && row.ack.is_some() {
                continue;
            }
            total += 1;
            let status = row
                .ack
                .as_ref()
                .map_or_else(|| "pending".to_string(), |a| format!("acked: {}", a.result));
            let subject = row
                .request
                .subject
                .issue_id
                .map_or_else(|| "-".to_string(), format_issue_id);
            println!(
                "{agent_id}  {}  {:?}  subject={subject}  [{status}]  by {}",
                row.request.request_id, row.request.kind, row.request.requested_by
            );
        }
    }

    if total == 0 {
        println!("No requests found.");
    }
    Ok(())
}

/// `crosslink agent init <agent-id> [-d "description"] [--no-key] [--force] [--role driver|agent]`
pub fn init(
    crosslink_dir: &Path,
    agent_id: &str,
    description: Option<&str>,
    no_key: bool,
    force: bool,
    role: AgentRole,
) -> Result<()> {
    match AgentConfig::load(crosslink_dir) {
        Ok(Some(_)) if force => {
            println!("Warning: Overwriting existing agent configuration (--force).");
        }
        Ok(Some(_)) => {
            bail!("Agent already configured. Use --force to overwrite, or delete .crosslink/agent.json to reconfigure.");
        }
        Ok(None) => {} // No existing config, proceed normally
        Err(e) => {
            println!(
                "Warning: Existing agent.json is malformed ({e}). Overwriting with new config."
            );
        }
    }
    // Role drives downstream signing: `Driver` makes hub commits
    // sign with the main repo's `user.signingkey` (the human's
    // GitHub-registered key); `Agent` makes them sign with this
    // agent's own key (subagent worktree attribution). See #718.
    let mut config = AgentConfig::init_with_role(crosslink_dir, agent_id, description, role)?;

    // Generate SSH key unless opted out
    if !no_key {
        // GH#610: store keys under the main repo's `.crosslink/keys/` so
        // they outlive `git worktree remove` of an agent worktree. The
        // worktree-scoped `user.signingkey` on the hub-cache (which is
        // also rooted in the main repo) then references a stable path.
        let host_crosslink = signing::host_crosslink_dir(crosslink_dir);
        let keys_dir = host_crosslink.join("keys");
        match signing::generate_agent_key(&keys_dir, agent_id, &config.machine_id) {
            Ok(keypair) => {
                // Store relative path from .crosslink/
                let rel_path = format!("keys/{agent_id}_ed25519");
                config.ssh_key_path = Some(rel_path);
                config.ssh_fingerprint = Some(keypair.fingerprint.clone());
                config.ssh_public_key = Some(keypair.public_key.clone());

                // Re-write agent.json with key info
                let path = crosslink_dir.join("agent.json");
                let json = serde_json::to_string_pretty(&config)?;
                std::fs::write(&path, json)?;

                println!("  SSH key: configured (commit signing enabled)");

                // Publish public key to hub for driver approval
                if let Err(e) =
                    super::trust::publish_agent_key(crosslink_dir, agent_id, &keypair.public_key)
                {
                    println!("  Note: Could not publish key to hub: {e}");
                    println!("  Key will be auto-published on next `crosslink sync`.");
                }

                // Configure signing on the hub cache worktree
                if let Ok(sync) = crate::sync::SyncManager::new(crosslink_dir) {
                    if let Err(e) = sync.configure_signing(crosslink_dir) {
                        tracing::warn!(
                            "could not configure commit signing: {e} — commits will be unsigned"
                        );
                    }
                }
            }
            Err(e) => {
                // On --force re-runs, key-gen errors with "already exists".
                // Reuse the on-disk key; otherwise agent.json loses
                // ssh_key_path and shared_writer silently disables signing.
                // GH#610: prefer the host-side keys dir (where new agents
                // store their keys) and fall back to the worktree-local
                // path for legacy agents whose keys predate the relocation.
                let rel_path = format!("keys/{agent_id}_ed25519");
                let host_private = host_crosslink.join(&rel_path);
                let host_public = host_crosslink.join(format!("keys/{agent_id}_ed25519.pub"));
                let (private_path, public_path) = if host_private.exists() && host_public.exists() {
                    (host_private, host_public)
                } else {
                    (
                        crosslink_dir.join(&rel_path),
                        crosslink_dir.join(format!("keys/{agent_id}_ed25519.pub")),
                    )
                };
                if private_path.exists() && public_path.exists() {
                    if let (Ok(fp), Ok(pub_key)) = (
                        signing::get_key_fingerprint(&public_path),
                        signing::read_public_key(&public_path),
                    ) {
                        config.ssh_key_path = Some(rel_path);
                        config.ssh_fingerprint = Some(fp);
                        config.ssh_public_key = Some(pub_key);
                        let path = crosslink_dir.join("agent.json");
                        if let Ok(json) = serde_json::to_string_pretty(&config) {
                            let _ = std::fs::write(&path, json);
                            println!("  SSH key: reused existing key (commit signing enabled)");
                        }
                    } else {
                        println!("  Warning: Could not reuse existing SSH key: {e}");
                        println!("  Signing will be unavailable. Use --no-key to suppress.");
                    }
                } else {
                    println!("  Warning: Could not generate SSH key: {e}");
                    println!(
                        "  Signing will be unavailable. Use --no-key to suppress this warning."
                    );
                }
            }
        }
    }

    // Re-run `configure_signing` unconditionally so that even in the
    // key-already-existed path, or the `--no-key` path, the hub-cache
    // worktree picks up the role-appropriate signing key (#718). The
    // helper is idempotent and role-aware — driver workspaces get
    // the human's GitHub-registered key, agent worktrees get the
    // agent's own key.
    if let Ok(sync) = crate::sync::SyncManager::new(crosslink_dir) {
        if let Err(e) = sync.configure_signing(crosslink_dir) {
            tracing::warn!("could not (re)configure commit signing after agent init: {e}");
        }
    }

    println!("Agent initialized:");
    println!("  ID:      {}", config.agent_id);
    println!("  Machine: {}", config.machine_id);
    if let Some(desc) = &config.description {
        println!("  Description: {desc}");
    }
    if config.ssh_fingerprint.is_some() {
        println!("  Signing: enabled");
    }
    println!();
    println!("Ask your driver to approve this agent with `crosslink trust approve {agent_id}`");
    Ok(())
}

/// Send a prompt to a running tmux-based agent session.
///
/// Uses `tmux load-buffer` + `paste-buffer` instead of raw `send-keys` to
/// avoid newline interpretation, length limits, and shell escaping issues (#503).
fn prompt(session: &str, message: &str, submit: bool) -> Result<()> {
    // Verify the tmux session exists
    let check = Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .context("tmux not found — is it installed?")?;

    if !check.status.success() {
        bail!("tmux session '{session}' not found. Check `tmux list-sessions`.");
    }

    // Write prompt to a temp file to avoid shell escaping issues
    let tmp = std::env::temp_dir().join(format!("crosslink-prompt-{}", std::process::id()));
    std::fs::write(&tmp, message).context("Failed to write prompt to temp file")?;

    // Load the file into a tmux buffer
    let load = Command::new("tmux")
        .args([
            "load-buffer",
            "-b",
            "crosslink-prompt",
            &tmp.to_string_lossy(),
        ])
        .output()
        .context("Failed to load tmux buffer")?;

    // Clean up temp file regardless of outcome
    let _ = std::fs::remove_file(&tmp);

    if !load.status.success() {
        let stderr = String::from_utf8_lossy(&load.stderr);
        bail!("tmux load-buffer failed: {}", stderr.trim());
    }

    // Paste the buffer into the target session
    let paste = Command::new("tmux")
        .args(["paste-buffer", "-b", "crosslink-prompt", "-t", session])
        .output()
        .context("Failed to paste tmux buffer")?;

    if !paste.status.success() {
        let stderr = String::from_utf8_lossy(&paste.stderr);
        bail!("tmux paste-buffer failed: {}", stderr.trim());
    }

    // Delete the named buffer
    let _ = Command::new("tmux")
        .args(["delete-buffer", "-b", "crosslink-prompt"])
        .output();

    // Optionally press Enter to submit
    if submit {
        let enter = Command::new("tmux")
            .args(["send-keys", "-t", session, "Enter"])
            .output()
            .context("Failed to send Enter key")?;

        if !enter.status.success() {
            let stderr = String::from_utf8_lossy(&enter.stderr);
            bail!("tmux send-keys Enter failed: {}", stderr.trim());
        }
    }

    println!("Prompt sent to session '{session}'");
    Ok(())
}

/// `crosslink agent bootstrap <target-dir> <repo-url> <agent-id> [--branch <branch>] [-d "description"] [--no-key]`
///
/// Enables a container agent to join an existing crosslink coordination hub by
/// cloning a repository, initializing an agent identity, and registering on
/// the hub branch.
pub fn bootstrap(
    target_dir: &Path,
    repo_url: &str,
    agent_id: &str,
    branch: Option<&str>,
    description: Option<&str>,
    no_key: bool,
) -> Result<()> {
    // Step 1: Clone or verify repo
    if !target_dir.exists()
        || target_dir
            .read_dir()
            .map_or(true, |mut d| d.next().is_none())
    {
        let output = Command::new("git")
            .args(["clone", "--depth", "1", repo_url])
            .arg(target_dir)
            .output()
            .context("Failed to run git clone")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git clone failed: {}", stderr.trim());
        }
    } else {
        // Verify the directory is a git repo with matching remote
        let output = Command::new("git")
            .current_dir(target_dir)
            .args(["remote", "get-url", "origin"])
            .output()
            .context("Failed to check git remote")?;
        if !output.status.success() {
            bail!(
                "Directory '{}' exists but is not a git repository with an origin remote",
                target_dir.display()
            );
        }
        let existing_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if existing_url != repo_url {
            bail!("Remote mismatch: existing origin is '{existing_url}', expected '{repo_url}'");
        }
    }

    // Step 2: Checkout branch if specified
    if let Some(br) = branch {
        let output = Command::new("git")
            .current_dir(target_dir)
            .args(["checkout", br])
            .output()
            .context("Failed to run git checkout")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git checkout '{}' failed: {}", br, stderr.trim());
        }
    }

    // Step 3: Find or create .crosslink
    let crosslink_dir = target_dir.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).context("Failed to create .crosslink directory")?;

    // Step 4: Initialize agent identity (skip if already exists).
    // Bootstrap provisions an autonomous agent — tag role accordingly (GH #566).
    // If a driver-typed identity already exists (e.g. from a prior `crosslink
    // init` in this repo), promote it to `agent` in place so hooks treat the
    // bootstrapped workspace correctly.
    match AgentConfig::load(&crosslink_dir)? {
        Some(existing) if existing.role == AgentRole::Agent => {
            println!("Agent already configured in this repo, skipping identity init.");
        }
        Some(mut existing) => {
            println!(
                "Promoting existing identity '{}' to agent role.",
                existing.agent_id
            );
            existing.role = AgentRole::Agent;
            let path = crosslink_dir.join("agent.json");
            let json = serde_json::to_string_pretty(&existing)?;
            std::fs::write(&path, json)?;
        }
        None => {
            AgentConfig::init_with_role(&crosslink_dir, agent_id, description, AgentRole::Agent)?;
        }
    }

    let mut config = AgentConfig::load(&crosslink_dir)?
        .ok_or_else(|| anyhow::anyhow!("Failed to load agent config after init"))?;

    // Step 5: Generate SSH key unless opted out
    if !no_key && config.ssh_key_path.is_none() {
        // GH#610: see `init` above — keys live under the main repo's
        // `.crosslink/keys/` so they survive agent-worktree cleanup.
        let keys_dir = signing::host_crosslink_dir(&crosslink_dir).join("keys");
        match signing::generate_agent_key(&keys_dir, agent_id, &config.machine_id) {
            Ok(keypair) => {
                let rel_path = format!("keys/{agent_id}_ed25519");
                config.ssh_key_path = Some(rel_path);
                config.ssh_fingerprint = Some(keypair.fingerprint);
                config.ssh_public_key = Some(keypair.public_key);

                // Re-write agent.json with key info
                let path = crosslink_dir.join("agent.json");
                let json = serde_json::to_string_pretty(&config)?;
                std::fs::write(&path, json)?;
            }
            Err(e) => {
                println!("  Warning: Could not generate SSH key: {e}");
                println!("  Signing will be unavailable.");
            }
        }
    }

    // Step 6: Initialize hub cache
    let sync = crate::sync::SyncManager::new(&crosslink_dir)?;
    sync.init_cache()?;
    // INTENTIONAL: fetch is best-effort — bootstrap proceeds with local state if offline
    let _ = sync.fetch();

    // Step 7: Create agent directory on hub
    let cache = sync.cache_path();
    let agent_dir = cache.join("agents").join(agent_id);
    std::fs::create_dir_all(&agent_dir).context("Failed to create agent directory on hub")?;

    let heartbeat = serde_json::json!({
        "agent_id": agent_id,
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "status": "active"
    });
    let heartbeat_path = agent_dir.join("heartbeat.json");
    std::fs::write(&heartbeat_path, serde_json::to_string_pretty(&heartbeat)?)
        .context("Failed to write heartbeat.json")?;

    let git_in_cache = |args: &[&str]| -> Result<()> {
        let output = Command::new("git").current_dir(cache).args(args).output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("nothing to commit") {
                bail!("git {:?} failed: {}", args, stderr.trim());
            }
        }
        Ok(())
    };

    git_in_cache(&["add", &format!("agents/{agent_id}/")])?;
    git_in_cache(&[
        "commit",
        "-m",
        &format!("bootstrap: register agent '{agent_id}'"),
    ])?;

    let remote = crate::sync::read_tracker_remote(&crosslink_dir);
    match Command::new("git")
        .current_dir(cache)
        .args(["push", &remote, crate::sync::HUB_BRANCH])
        .output()
    {
        Ok(o) if !o.status.success() => {
            eprintln!(
                "Warning: could not push agent registration to hub: {} — will be pushed on next sync",
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
        Err(e) => eprintln!(
            "Warning: could not push agent registration to hub: {e} — will be pushed on next sync"
        ),
        Ok(_) => {}
    }

    // Step 8: Publish key to hub (before configuring signing to avoid
    // the chicken-and-egg where signing is required for the publish commit)
    if let Some(pub_key) = &config.ssh_public_key {
        if let Err(e) = super::trust::publish_agent_key(&crosslink_dir, agent_id, pub_key) {
            println!("  Note: Could not publish key to hub: {e}");
            println!("  Key will be auto-published on next `crosslink sync`.");
        }
    }

    // Step 9: Configure signing (after key is published)
    if let Err(e) = sync.configure_signing(&crosslink_dir) {
        tracing::warn!("could not configure commit signing: {e} — commits will be unsigned");
    }

    // Step 10: Print summary
    println!("Bootstrap complete:");
    println!("  Agent ID:  {}", config.agent_id);
    println!("  Machine:   {}", config.machine_id);
    if config.ssh_fingerprint.is_some() {
        println!("  Signing: enabled");
    }
    println!("  Repo path: {}", target_dir.display());
    println!();
    println!("Ask your driver to approve this agent with `crosslink trust approve {agent_id}`");

    Ok(())
}

/// `crosslink agent status`
pub fn status(crosslink_dir: &Path) -> Result<()> {
    match AgentConfig::load(crosslink_dir)? {
        Some(config) => {
            println!("Agent: {}", config.agent_id);
            println!("Machine: {}", config.machine_id);
            if let Some(desc) = &config.description {
                println!("Description: {desc}");
            }
            if config.ssh_fingerprint.is_some() {
                println!("Signing: enabled");
            } else {
                println!("Signing: disabled");
            }

            // Show locked issues (best-effort)
            // Locks prevent other agents from working on the same issue simultaneously.
            if let Ok(sync) = crate::sync::SyncManager::new(crosslink_dir) {
                // INTENTIONAL: init and fetch are best-effort — status display works with stale data
                let _ = sync.init_cache();
                let _ = sync.fetch();
                if let Ok(locks) = sync.read_locks_auto() {
                    let my_locks = locks.agent_locks(&config.agent_id);
                    if my_locks.is_empty() {
                        println!("Locks: none");
                    } else {
                        println!(
                            "Locks: {} (exclusively assigned to this agent)",
                            my_locks
                                .iter()
                                .map(|id| format_issue_id(*id))
                                .collect::<Vec<_>>()
                                .join(", ")
                        );
                    }
                }
            }
        }
        None => {
            println!("No agent configured. Run 'crosslink agent init <id>' first.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as TestCommand;
    use tempfile::tempdir;

    /// Helper: create a bare git repo that can be used as a clone source.
    fn create_bare_repo(dir: &Path) {
        let output = TestCommand::new("git")
            .args(["init", "--bare", "-q"])
            .arg(dir)
            .output()
            .expect("git init --bare failed");
        assert!(output.status.success(), "Failed to create bare repo");
    }

    /// Helper: create a non-bare git repo with an initial commit so it can be cloned.
    fn create_repo_with_commit(dir: &Path) {
        TestCommand::new("git")
            .args(["init", "-q"])
            .arg(dir)
            .output()
            .expect("git init failed");
        TestCommand::new("git")
            .current_dir(dir)
            .args(["config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        TestCommand::new("git")
            .current_dir(dir)
            .args(["config", "user.name", "Test"])
            .output()
            .unwrap();
        // Disable gpg signing for test commits
        TestCommand::new("git")
            .current_dir(dir)
            .args(["config", "commit.gpgsign", "false"])
            .output()
            .unwrap();
        std::fs::write(dir.join("README"), "init").unwrap();
        TestCommand::new("git")
            .current_dir(dir)
            .args(["add", "."])
            .output()
            .unwrap();
        TestCommand::new("git")
            .current_dir(dir)
            .args(["commit", "-q", "-m", "initial"])
            .output()
            .unwrap();
    }

    #[test]
    fn test_bootstrap_creates_crosslink_dir() {
        let tmp = tempdir().unwrap();
        let bare = tmp.path().join("origin.git");
        create_repo_with_commit(&tmp.path().join("seed"));
        // Clone seed into a bare repo so we have something to clone from
        TestCommand::new("git")
            .args([
                "clone",
                "--bare",
                "-q",
                &tmp.path().join("seed").to_string_lossy(),
                &bare.to_string_lossy(),
            ])
            .output()
            .unwrap();

        let target = tmp.path().join("cloned");
        let result = bootstrap(
            &target,
            &bare.to_string_lossy(),
            "bot-001",
            None,
            Some("test bootstrap"),
            true, // skip SSH key generation in tests
        );
        assert!(result.is_ok(), "bootstrap failed: {:?}", result.err());

        // .crosslink dir should exist in the cloned repo
        assert!(target.join(".crosslink").exists());

        // agent.json should exist
        let config = AgentConfig::load(&target.join(".crosslink"))
            .unwrap()
            .unwrap();
        assert_eq!(config.agent_id, "bot-001");
        assert_eq!(config.description, Some("test bootstrap".to_string()));
    }

    #[test]
    fn test_bootstrap_rejects_invalid_agent_id() {
        let tmp = tempdir().unwrap();
        let bare = tmp.path().join("origin.git");
        create_bare_repo(&bare);

        let target = tmp.path().join("cloned");
        // "x" is too short (< 3 chars) — AgentConfig::init validates this
        let result = bootstrap(&target, &bare.to_string_lossy(), "x", None, None, true);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("at least 3 characters"),
            "Unexpected error: {err_msg}"
        );
    }

    #[test]
    fn test_bootstrap_existing_agent_skips() {
        let tmp = tempdir().unwrap();
        let bare = tmp.path().join("origin.git");
        create_repo_with_commit(&tmp.path().join("seed"));
        TestCommand::new("git")
            .args([
                "clone",
                "--bare",
                "-q",
                &tmp.path().join("seed").to_string_lossy(),
                &bare.to_string_lossy(),
            ])
            .output()
            .unwrap();

        let target = tmp.path().join("cloned");

        // First bootstrap
        bootstrap(
            &target,
            &bare.to_string_lossy(),
            "bot-002",
            None,
            Some("first"),
            true,
        )
        .unwrap();

        // Second bootstrap with same target — should succeed without error
        // (the agent identity step is skipped gracefully)
        let result = bootstrap(
            &target,
            &bare.to_string_lossy(),
            "bot-002",
            None,
            Some("second"),
            true,
        );
        assert!(
            result.is_ok(),
            "second bootstrap failed: {:?}",
            result.err()
        );

        // Config should still have the first description
        let config = AgentConfig::load(&target.join(".crosslink"))
            .unwrap()
            .unwrap();
        assert_eq!(config.description, Some("first".to_string()));
    }

    #[test]
    fn test_bootstrap_verifies_remote() {
        let tmp = tempdir().unwrap();

        // Create a repo with a real remote
        let bare = tmp.path().join("origin.git");
        create_repo_with_commit(&tmp.path().join("seed"));
        TestCommand::new("git")
            .args([
                "clone",
                "--bare",
                "-q",
                &tmp.path().join("seed").to_string_lossy(),
                &bare.to_string_lossy(),
            ])
            .output()
            .unwrap();

        let target = tmp.path().join("cloned");
        // Clone it first with the real URL
        TestCommand::new("git")
            .args([
                "clone",
                "-q",
                &bare.to_string_lossy(),
                &target.to_string_lossy(),
            ])
            .output()
            .unwrap();

        // Now try to bootstrap with a *different* URL — should fail
        let result = bootstrap(
            &target,
            "https://example.com/wrong-repo.git",
            "bot-003",
            None,
            None,
            true,
        );
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Remote mismatch"),
            "Unexpected error: {err_msg}"
        );
    }

    #[test]
    fn test_init_creates_config() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        init(
            &crosslink_dir,
            "worker-1",
            Some("Test agent"),
            true,
            false,
            AgentRole::Agent,
        )
        .unwrap();

        let config = AgentConfig::load(&crosslink_dir).unwrap().unwrap();
        assert_eq!(config.agent_id, "worker-1");
        assert_eq!(config.description, Some("Test agent".to_string()));
    }

    #[test]
    fn test_init_rejects_duplicate() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        init(
            &crosslink_dir,
            "worker-1",
            None,
            true,
            false,
            AgentRole::Agent,
        )
        .unwrap();
        let result = init(
            &crosslink_dir,
            "worker-2",
            None,
            true,
            false,
            AgentRole::Agent,
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("already configured"));
    }

    #[test]
    fn test_init_force_overwrites_valid_config() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        init(
            &crosslink_dir,
            "worker-1",
            None,
            true,
            false,
            AgentRole::Agent,
        )
        .unwrap();
        init(
            &crosslink_dir,
            "worker-2",
            Some("New agent"),
            true,
            true,
            AgentRole::Agent,
        )
        .unwrap();

        let config = AgentConfig::load(&crosslink_dir).unwrap().unwrap();
        assert_eq!(config.agent_id, "worker-2");
        assert_eq!(config.description, Some("New agent".to_string()));
    }

    #[test]
    fn test_init_overwrites_malformed_json() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Write invalid JSON
        std::fs::write(crosslink_dir.join("agent.json"), "not valid json").unwrap();

        init(
            &crosslink_dir,
            "worker-1",
            None,
            true,
            false,
            AgentRole::Agent,
        )
        .unwrap();

        let config = AgentConfig::load(&crosslink_dir).unwrap().unwrap();
        assert_eq!(config.agent_id, "worker-1");
    }

    #[test]
    fn test_init_overwrites_invalid_agent_id() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Write valid JSON but with agent_id that fails validation (too short)
        std::fs::write(
            crosslink_dir.join("agent.json"),
            r#"{"agent_id": "m1", "machine_id": "host"}"#,
        )
        .unwrap();

        init(
            &crosslink_dir,
            "worker-1",
            None,
            true,
            false,
            AgentRole::Agent,
        )
        .unwrap();

        let config = AgentConfig::load(&crosslink_dir).unwrap().unwrap();
        assert_eq!(config.agent_id, "worker-1");
    }

    #[test]
    fn test_status_no_config() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Should not error, just print message
        status(&crosslink_dir).unwrap();
    }

    #[test]
    fn test_status_with_config() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        init(
            &crosslink_dir,
            "my-agent",
            Some("My agent"),
            true,
            false,
            AgentRole::Agent,
        )
        .unwrap();
        status(&crosslink_dir).unwrap();
    }
}
