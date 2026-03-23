use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::identity::AgentConfig;
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
        } => init(
            crosslink_dir,
            &agent_id,
            description.as_deref(),
            no_key,
            force,
        ),
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
            // Ensure the agent directory exists on the hub branch
            let cl_dir = target_path.join(".crosslink");
            if let Ok(s) = sync::SyncManager::new(&cl_dir) {
                if let Err(e) = s.ensure_agent_dir(&identity) {
                    tracing::warn!(
                        "could not create agent dir on hub: {e} — will be created on next sync"
                    );
                }
            }
            Ok(())
        }
    }
}

/// `crosslink agent init <agent-id> [-d "description"] [--no-key] [--force]`
pub fn init(
    crosslink_dir: &Path,
    agent_id: &str,
    description: Option<&str>,
    no_key: bool,
    force: bool,
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
                "Warning: Existing agent.json is malformed ({}). Overwriting with new config.",
                e
            );
        }
    }
    let mut config = AgentConfig::init(crosslink_dir, agent_id, description)?;

    // Generate SSH key unless opted out
    if !no_key {
        let keys_dir = crosslink_dir.join("keys");
        match signing::generate_agent_key(&keys_dir, agent_id, &config.machine_id) {
            Ok(keypair) => {
                // Store relative path from .crosslink/
                let rel_path = format!("keys/{}_ed25519", agent_id);
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
                    println!("  Note: Could not publish key to hub: {}", e);
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
                println!("  Warning: Could not generate SSH key: {}", e);
                println!("  Signing will be unavailable. Use --no-key to suppress this warning.");
            }
        }
    }

    println!("Agent initialized:");
    println!("  ID:      {}", config.agent_id);
    println!("  Machine: {}", config.machine_id);
    if let Some(desc) = &config.description {
        println!("  Description: {}", desc);
    }
    if config.ssh_fingerprint.is_some() {
        println!("  Signing: enabled");
    }
    println!();
    println!(
        "Ask your driver to approve this agent with `crosslink trust approve {}`",
        agent_id
    );
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
        bail!(
            "tmux session '{}' not found. Check `tmux list-sessions`.",
            session
        );
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

    println!("Prompt sent to session '{}'", session);
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
            bail!(
                "Remote mismatch: existing origin is '{}', expected '{}'",
                existing_url,
                repo_url
            );
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

    // Step 4: Initialize agent identity (skip if already exists)
    if AgentConfig::load(&crosslink_dir)?.is_some() {
        println!("Agent already configured in this repo, skipping identity init.");
    } else {
        AgentConfig::init(&crosslink_dir, agent_id, description)?;
    }

    let mut config = AgentConfig::load(&crosslink_dir)?
        .ok_or_else(|| anyhow::anyhow!("Failed to load agent config after init"))?;

    // Step 5: Generate SSH key unless opted out
    if !no_key && config.ssh_key_path.is_none() {
        let keys_dir = crosslink_dir.join("keys");
        match signing::generate_agent_key(&keys_dir, agent_id, &config.machine_id) {
            Ok(keypair) => {
                let rel_path = format!("keys/{}_ed25519", agent_id);
                config.ssh_key_path = Some(rel_path);
                config.ssh_fingerprint = Some(keypair.fingerprint.clone());
                config.ssh_public_key = Some(keypair.public_key.clone());

                // Re-write agent.json with key info
                let path = crosslink_dir.join("agent.json");
                let json = serde_json::to_string_pretty(&config)?;
                std::fs::write(&path, json)?;
            }
            Err(e) => {
                println!("  Warning: Could not generate SSH key: {}", e);
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

    git_in_cache(&["add", &format!("agents/{}/", agent_id)])?;
    git_in_cache(&[
        "commit",
        "-m",
        &format!("bootstrap: register agent '{}'", agent_id),
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
            println!("  Note: Could not publish key to hub: {}", e);
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
    println!(
        "Ask your driver to approve this agent with `crosslink trust approve {}`",
        agent_id
    );

    Ok(())
}

/// `crosslink agent status`
pub fn status(crosslink_dir: &Path) -> Result<()> {
    match AgentConfig::load(crosslink_dir)? {
        Some(config) => {
            println!("Agent: {}", config.agent_id);
            println!("Machine: {}", config.machine_id);
            if let Some(desc) = &config.description {
                println!("Description: {}", desc);
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
            "Unexpected error: {}",
            err_msg
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
            "Unexpected error: {}",
            err_msg
        );
    }

    #[test]
    fn test_init_creates_config() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        init(&crosslink_dir, "worker-1", Some("Test agent"), true, false).unwrap();

        let config = AgentConfig::load(&crosslink_dir).unwrap().unwrap();
        assert_eq!(config.agent_id, "worker-1");
        assert_eq!(config.description, Some("Test agent".to_string()));
    }

    #[test]
    fn test_init_rejects_duplicate() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        init(&crosslink_dir, "worker-1", None, true, false).unwrap();
        let result = init(&crosslink_dir, "worker-2", None, true, false);
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

        init(&crosslink_dir, "worker-1", None, true, false).unwrap();
        init(&crosslink_dir, "worker-2", Some("New agent"), true, true).unwrap();

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

        init(&crosslink_dir, "worker-1", None, true, false).unwrap();

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

        init(&crosslink_dir, "worker-1", None, true, false).unwrap();

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

        init(&crosslink_dir, "my-agent", Some("My agent"), true, false).unwrap();
        status(&crosslink_dir).unwrap();
    }
}
