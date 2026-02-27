use anyhow::{bail, Result};
use std::path::Path;

use crate::identity::AgentConfig;
use crate::signing;
use crate::utils::format_issue_id;

/// `crosslink agent init <agent-id> [-d "description"] [--no-key]`
pub fn init(
    crosslink_dir: &Path,
    agent_id: &str,
    description: Option<&str>,
    no_key: bool,
) -> Result<()> {
    if AgentConfig::load(crosslink_dir)?.is_some() {
        bail!("Agent already configured. Delete .crosslink/agent.json to reconfigure.");
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

                println!("  SSH key: {}", keypair.fingerprint);

                // Publish public key to hub for driver approval
                if let Err(e) =
                    super::trust::publish_agent_key(crosslink_dir, agent_id, &keypair.public_key)
                {
                    println!("  Note: Could not publish key to hub: {}", e);
                    println!("  The driver can manually copy your public key.");
                }

                // Configure signing on the hub cache worktree
                if let Ok(sync) = crate::sync::SyncManager::new(crosslink_dir) {
                    let _ = sync.configure_signing(crosslink_dir);
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
    if let Some(fp) = &config.ssh_fingerprint {
        println!("  Key:     {}", fp);
    }
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
            if let Some(fp) = &config.ssh_fingerprint {
                println!("SSH key: {}", fp);
            } else {
                println!("SSH key: none (signing disabled)");
            }

            // Show locked issues (best-effort)
            if let Ok(sync) = crate::sync::SyncManager::new(crosslink_dir) {
                let _ = sync.init_cache();
                let _ = sync.fetch();
                if let Ok(locks) = sync.read_locks() {
                    let my_locks = locks.agent_locks(&config.agent_id);
                    if my_locks.is_empty() {
                        println!("Locks: none");
                    } else {
                        println!(
                            "Locks: {}",
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
    use tempfile::tempdir;

    #[test]
    fn test_init_creates_config() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        init(&crosslink_dir, "worker-1", Some("Test agent"), true).unwrap();

        let config = AgentConfig::load(&crosslink_dir).unwrap().unwrap();
        assert_eq!(config.agent_id, "worker-1");
        assert_eq!(config.description, Some("Test agent".to_string()));
    }

    #[test]
    fn test_init_rejects_duplicate() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        init(&crosslink_dir, "worker-1", None, true).unwrap();
        let result = init(&crosslink_dir, "worker-2", None, true);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("already configured"));
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

        init(&crosslink_dir, "my-agent", Some("My agent"), true).unwrap();
        status(&crosslink_dir).unwrap();
    }
}
