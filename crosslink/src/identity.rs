use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::signing;

/// Machine-local agent identity. Lives at `.crosslink/agent.json`.
/// This file is gitignored — each machine has its own.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    pub agent_id: String,
    pub machine_id: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Path to SSH private key, relative to .crosslink/ (e.g. "keys/agent_ed25519").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_key_path: Option<String>,
    /// SSH public key fingerprint (e.g. "SHA256:...").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_fingerprint: Option<String>,
    /// Full SSH public key line (e.g. "ssh-ed25519 AAAA... comment").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_public_key: Option<String>,
}

impl AgentConfig {
    /// Load from the .crosslink directory. Returns None if agent.json doesn't exist.
    pub fn load(crosslink_dir: &Path) -> Result<Option<Self>> {
        let path = crosslink_dir.join("agent.json");
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let config: AgentConfig = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        config.validate()?;
        Ok(Some(config))
    }

    /// Create and write a new agent config.
    pub fn init(crosslink_dir: &Path, agent_id: &str, description: Option<&str>) -> Result<Self> {
        let machine_id = detect_hostname();
        let config = AgentConfig {
            agent_id: agent_id.to_string(),
            machine_id,
            description: description.map(|s| s.to_string()),
            ssh_key_path: None,
            ssh_fingerprint: None,
            ssh_public_key: None,
        };
        config.validate()?;
        let path = crosslink_dir.join("agent.json");
        let json = serde_json::to_string_pretty(&config)?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write {}", path.display()))?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.agent_id.is_empty(), "agent_id cannot be empty");
        anyhow::ensure!(
            self.agent_id
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
            "agent_id must be alphanumeric with hyphens/underscores only"
        );
        anyhow::ensure!(
            self.agent_id.len() <= 64,
            "agent_id must be <= 64 characters"
        );
        Ok(())
    }
}

/// Detect the hostname of the current machine.
fn detect_hostname() -> String {
    // Windows: COMPUTERNAME env var
    if let Ok(name) = std::env::var("COMPUTERNAME") {
        return name;
    }
    // Unix: HOSTNAME env var
    if let Ok(name) = std::env::var("HOSTNAME") {
        return name;
    }
    // Fallback: run hostname command
    if let Ok(output) = std::process::Command::new("hostname").output() {
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }
    "unknown".to_string()
}

/// Resolve the current driver's SSH key fingerprint from `.crosslink/driver-key.pub`.
///
/// Returns `None` if the driver key file doesn't exist or the fingerprint can't be computed.
pub fn resolve_driver_fingerprint(crosslink_dir: &Path) -> Option<String> {
    let driver_pub = crosslink_dir.join("driver-key.pub");
    if !driver_pub.exists() {
        return None;
    }
    signing::get_key_fingerprint(&driver_pub).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_missing_file() {
        let dir = tempdir().unwrap();
        let result = AgentConfig::load(dir.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_init_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let config = AgentConfig::init(dir.path(), "worker-1", Some("Test agent")).unwrap();
        assert_eq!(config.agent_id, "worker-1");
        assert_eq!(config.description, Some("Test agent".to_string()));
        assert!(!config.machine_id.is_empty());

        let loaded = AgentConfig::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.agent_id, config.agent_id);
        assert_eq!(loaded.machine_id, config.machine_id);
        assert_eq!(loaded.description, config.description);
    }

    #[test]
    fn test_init_no_description() {
        let dir = tempdir().unwrap();
        let config = AgentConfig::init(dir.path(), "worker-2", None).unwrap();
        assert_eq!(config.agent_id, "worker-2");
        assert!(config.description.is_none());
    }

    /// Helper to build a minimal AgentConfig for tests.
    fn test_config(agent_id: &str) -> AgentConfig {
        AgentConfig {
            agent_id: agent_id.to_string(),
            machine_id: "test".to_string(),
            description: None,
            ssh_key_path: None,
            ssh_fingerprint: None,
            ssh_public_key: None,
        }
    }

    #[test]
    fn test_validate_empty_id() {
        let config = test_config("");
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_chars() {
        assert!(test_config("worker 1").validate().is_err());
        assert!(test_config("worker@1").validate().is_err());
    }

    #[test]
    fn test_validate_too_long() {
        let config = AgentConfig {
            agent_id: "a".repeat(65),
            ..test_config("x")
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_valid_ids() {
        for id in &["worker-1", "agent_2", "MyAgent", "a", "test-agent-42"] {
            assert!(test_config(id).validate().is_ok(), "Failed for id: {}", id);
        }
    }

    #[test]
    fn test_json_roundtrip() {
        let config = AgentConfig {
            description: Some("Test agent".to_string()),
            machine_id: "my-host".to_string(),
            ..test_config("worker-1")
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_json_missing_description_defaults_none() {
        let json = r#"{"agent_id": "worker-1", "machine_id": "host"}"#;
        let config: AgentConfig = serde_json::from_str(json).unwrap();
        assert!(config.description.is_none());
        assert!(config.ssh_key_path.is_none());
        assert!(config.ssh_fingerprint.is_none());
        assert!(config.ssh_public_key.is_none());
    }

    #[test]
    fn test_json_backward_compat_no_ssh_fields() {
        // Old agent.json without SSH fields should deserialize fine
        let json = r#"{"agent_id": "worker-1", "machine_id": "host", "description": "old agent"}"#;
        let config: AgentConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.agent_id, "worker-1");
        assert!(config.ssh_key_path.is_none());
    }

    #[test]
    fn test_json_with_ssh_fields() {
        let config = AgentConfig {
            ssh_key_path: Some("keys/test_ed25519".to_string()),
            ssh_fingerprint: Some("SHA256:abc123".to_string()),
            ssh_public_key: Some("ssh-ed25519 AAAA test".to_string()),
            ..test_config("worker-1")
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ssh_key_path, Some("keys/test_ed25519".to_string()));
        assert_eq!(parsed.ssh_fingerprint, Some("SHA256:abc123".to_string()));
    }

    #[test]
    fn test_detect_hostname_returns_something() {
        let hostname = detect_hostname();
        assert!(!hostname.is_empty());
    }

    #[test]
    fn test_resolve_driver_fingerprint_missing_file() {
        let dir = tempdir().unwrap();
        assert!(resolve_driver_fingerprint(dir.path()).is_none());
    }

    #[test]
    fn test_resolve_driver_fingerprint_invalid_content() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("driver-key.pub"), "not a key").unwrap();
        // ssh-keygen will fail on invalid content
        assert!(resolve_driver_fingerprint(dir.path()).is_none());
    }

    proptest! {
        #[test]
        fn prop_valid_ids_roundtrip(id in "[a-zA-Z0-9_-]{1,64}") {
            let config = test_config(&id);
            prop_assert!(config.validate().is_ok());
            let json = serde_json::to_string(&config).unwrap();
            let parsed: AgentConfig = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(parsed.agent_id, id);
        }
    }
}
