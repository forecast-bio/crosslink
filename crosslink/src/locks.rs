use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// Custom serde for `HashMap`<i64, V> that serializes keys as strings for JSON
/// backward compatibility (locks.json uses string keys on disk).
mod string_key_map {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::HashMap;

    pub fn serialize<V: Serialize, S: Serializer>(
        map: &HashMap<i64, V>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let string_map: HashMap<String, &V> = map.iter().map(|(k, v)| (k.to_string(), v)).collect();
        string_map.serialize(serializer)
    }

    pub fn deserialize<'de, V: Deserialize<'de>, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<HashMap<i64, V>, D::Error> {
        let string_map: HashMap<String, V> = HashMap::deserialize(deserializer)?;
        string_map
            .into_iter()
            .map(|(k, v)| {
                k.parse::<i64>()
                    .map(|id| (id, v))
                    .map_err(|_| serde::de::Error::custom(format!("invalid lock key: {k}")))
            })
            .collect()
    }
}

/// A single issue lock entry in locks.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lock {
    pub agent_id: String,
    #[serde(default)]
    pub branch: Option<String>,
    pub claimed_at: DateTime<Utc>,
    pub signed_by: String,
}

/// Settings embedded in locks.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockSettings {
    #[serde(default = "default_stale_timeout")]
    pub stale_lock_timeout_minutes: u64,
}

const fn default_stale_timeout() -> u64 {
    60
}

impl Default for LockSettings {
    fn default() -> Self {
        Self {
            stale_lock_timeout_minutes: default_stale_timeout(),
        }
    }
}

/// The top-level locks.json structure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocksFile {
    pub version: u32,
    /// Map from issue ID to Lock.
    #[serde(with = "string_key_map")]
    pub locks: HashMap<i64, Lock>,
    #[serde(default)]
    pub settings: LockSettings,
}

impl LocksFile {
    /// Load and parse a locks.json file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed as valid JSON.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let locks: Self = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(locks)
    }

    /// Save to a file using atomic write (temp + rename) to prevent corruption.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be serialized or written atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        crate::utils::atomic_write(path, json.as_bytes())
    }

    /// Check if a specific issue is locked.
    #[must_use]
    pub fn is_locked(&self, issue_id: i64) -> bool {
        self.locks.contains_key(&issue_id)
    }

    /// Get the lock for a specific issue.
    #[must_use]
    pub fn get_lock(&self, issue_id: i64) -> Option<&Lock> {
        self.locks.get(&issue_id)
    }

    /// Check if an issue is locked by a specific agent.
    #[must_use]
    pub fn is_locked_by(&self, issue_id: i64, agent_id: &str) -> bool {
        self.locks
            .get(&issue_id)
            .is_some_and(|l| l.agent_id == agent_id)
    }

    /// List all issue IDs locked by a specific agent.
    #[must_use]
    pub fn agent_locks(&self, agent_id: &str) -> Vec<i64> {
        self.locks
            .iter()
            .filter(|(_, lock)| lock.agent_id == agent_id)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Create an empty locks file.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            version: 1,
            locks: HashMap::new(),
            settings: LockSettings::default(),
        }
    }
}

/// Heartbeat file for an agent (lives at `heartbeats/{agent_id}.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Heartbeat {
    pub agent_id: String,
    pub last_heartbeat: DateTime<Utc>,
    pub active_issue_id: Option<i64>,
    pub machine_id: String,
}

/// Trust keyring — list of trusted GPG key fingerprints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Keyring {
    pub trusted_fingerprints: Vec<String>,
}

impl Keyring {
    /// Load and parse a keyring.json file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed as valid JSON.
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Check if a fingerprint is trusted.
    #[must_use]
    pub fn is_trusted(&self, fingerprint: &str) -> bool {
        self.trusted_fingerprints.iter().any(|f| f == fingerprint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use tempfile::tempdir;

    fn sample_lock() -> Lock {
        Lock {
            agent_id: "worker-1".to_string(),
            branch: Some("feature/auth".to_string()),
            claimed_at: Utc::now(),
            signed_by: "ABCD1234".to_string(),
        }
    }

    fn sample_locks_file() -> LocksFile {
        let mut locks = HashMap::new();
        locks.insert(5, sample_lock());
        locks.insert(
            8,
            Lock {
                agent_id: "worker-2".to_string(),
                branch: Some("fix/api-timeout".to_string()),
                claimed_at: Utc::now(),
                signed_by: "EFGH5678".to_string(),
            },
        );
        LocksFile {
            version: 1,
            locks,
            settings: LockSettings::default(),
        }
    }

    // ==================== LocksFile Tests ====================

    #[test]
    fn test_empty_locks() {
        let locks = LocksFile::empty();
        assert_eq!(locks.version, 1);
        assert!(locks.locks.is_empty());
        assert_eq!(locks.settings.stale_lock_timeout_minutes, 60);
    }

    #[test]
    fn test_is_locked() {
        let locks = sample_locks_file();
        assert!(locks.is_locked(5));
        assert!(locks.is_locked(8));
        assert!(!locks.is_locked(1));
        assert!(!locks.is_locked(99));
    }

    #[test]
    fn test_get_lock() {
        let locks = sample_locks_file();
        let lock = locks.get_lock(5).unwrap();
        assert_eq!(lock.agent_id, "worker-1");
        assert_eq!(lock.branch, Some("feature/auth".to_string()));
        assert!(locks.get_lock(99).is_none());
    }

    #[test]
    fn test_is_locked_by() {
        let locks = sample_locks_file();
        assert!(locks.is_locked_by(5, "worker-1"));
        assert!(!locks.is_locked_by(5, "worker-2"));
        assert!(locks.is_locked_by(8, "worker-2"));
        assert!(!locks.is_locked_by(99, "worker-1"));
    }

    #[test]
    fn test_agent_locks() {
        let locks = sample_locks_file();
        let w1_locks = locks.agent_locks("worker-1");
        assert_eq!(w1_locks, vec![5]);
        let w2_locks = locks.agent_locks("worker-2");
        assert_eq!(w2_locks, vec![8]);
        let nobody_locks = locks.agent_locks("nobody");
        assert!(nobody_locks.is_empty());
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("locks.json");

        let original = sample_locks_file();
        original.save(&path).unwrap();

        let loaded = LocksFile::load(&path).unwrap();
        assert_eq!(loaded.version, original.version);
        assert_eq!(loaded.locks.len(), original.locks.len());
        assert_eq!(
            loaded.settings.stale_lock_timeout_minutes,
            original.settings.stale_lock_timeout_minutes
        );
    }

    #[test]
    fn test_load_missing_file() {
        let result = LocksFile::load(Path::new("/nonexistent/locks.json"));
        assert!(result.is_err());
    }

    #[test]
    fn test_json_roundtrip() {
        let locks = sample_locks_file();
        let json = serde_json::to_string_pretty(&locks).unwrap();
        let parsed: LocksFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.version, locks.version);
        assert_eq!(parsed.locks.len(), locks.locks.len());
    }

    #[test]
    fn test_missing_settings_defaults() {
        let json = r#"{"version": 1, "locks": {}}"#;
        let locks: LocksFile = serde_json::from_str(json).unwrap();
        assert_eq!(locks.settings.stale_lock_timeout_minutes, 60);
    }

    #[test]
    fn test_custom_stale_timeout() {
        let json =
            r#"{"version": 1, "locks": {}, "settings": {"stale_lock_timeout_minutes": 120}}"#;
        let locks: LocksFile = serde_json::from_str(json).unwrap();
        assert_eq!(locks.settings.stale_lock_timeout_minutes, 120);
    }

    // ==================== Lock Tests ====================

    #[test]
    fn test_lock_json_roundtrip() {
        let lock = sample_lock();
        let json = serde_json::to_string(&lock).unwrap();
        let parsed: Lock = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_id, lock.agent_id);
        assert_eq!(parsed.branch, lock.branch);
        assert_eq!(parsed.signed_by, lock.signed_by);
    }

    #[test]
    fn test_lock_no_branch() {
        let lock = Lock {
            agent_id: "worker-1".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: "ABC".to_string(),
        };
        let json = serde_json::to_string(&lock).unwrap();
        let parsed: Lock = serde_json::from_str(&json).unwrap();
        assert!(parsed.branch.is_none());
    }

    // ==================== Heartbeat Tests ====================

    #[test]
    fn test_heartbeat_json_roundtrip() {
        let hb = Heartbeat {
            agent_id: "worker-1".to_string(),
            last_heartbeat: Utc::now(),
            active_issue_id: Some(5),
            machine_id: "my-host".to_string(),
        };
        let json = serde_json::to_string(&hb).unwrap();
        let parsed: Heartbeat = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_id, hb.agent_id);
        assert_eq!(parsed.active_issue_id, Some(5));
        assert_eq!(parsed.machine_id, hb.machine_id);
    }

    #[test]
    fn test_heartbeat_no_active_issue() {
        let hb = Heartbeat {
            agent_id: "worker-1".to_string(),
            last_heartbeat: Utc::now(),
            active_issue_id: None,
            machine_id: "my-host".to_string(),
        };
        let json = serde_json::to_string(&hb).unwrap();
        let parsed: Heartbeat = serde_json::from_str(&json).unwrap();
        assert!(parsed.active_issue_id.is_none());
    }

    // ==================== Keyring Tests ====================

    #[test]
    fn test_keyring_is_trusted() {
        let keyring = Keyring {
            trusted_fingerprints: vec!["ABC123".to_string(), "DEF456".to_string()],
        };
        assert!(keyring.is_trusted("ABC123"));
        assert!(keyring.is_trusted("DEF456"));
        assert!(!keyring.is_trusted("XYZ999"));
        assert!(!keyring.is_trusted(""));
    }

    #[test]
    fn test_keyring_empty() {
        let keyring = Keyring {
            trusted_fingerprints: vec![],
        };
        assert!(!keyring.is_trusted("anything"));
    }

    #[test]
    fn test_keyring_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("keyring.json");

        let keyring = Keyring {
            trusted_fingerprints: vec!["ABC".to_string(), "DEF".to_string()],
        };
        let json = serde_json::to_string_pretty(&keyring).unwrap();
        std::fs::write(&path, json).unwrap();

        let loaded = Keyring::load(&path).unwrap();
        assert_eq!(loaded, keyring);
    }

    // ==================== Property-Based Tests ====================

    proptest! {
        #[test]
        fn prop_locks_file_roundtrip(
            id1 in 1i64..1000,
            id2 in 1001i64..2000,
            agent1 in "[a-z]{3,10}",
            agent2 in "[a-z]{3,10}",
        ) {
            let mut locks = HashMap::new();
            locks.insert(
                id1,
                Lock {
                    agent_id: agent1.clone(),
                    branch: None,
                    claimed_at: Utc::now(),
                    signed_by: "ABC".to_string(),
                },
            );
            locks.insert(
                id2,
                Lock {
                    agent_id: agent2.clone(),
                    branch: Some("branch".to_string()),
                    claimed_at: Utc::now(),
                    signed_by: "DEF".to_string(),
                },
            );

            let file = LocksFile {
                version: 1,
                locks,
                settings: LockSettings::default(),
            };

            let json = serde_json::to_string(&file).unwrap();
            let parsed: LocksFile = serde_json::from_str(&json).unwrap();

            prop_assert!(parsed.is_locked(id1));
            prop_assert!(parsed.is_locked(id2));
            prop_assert!(parsed.is_locked_by(id1, &agent1));
            prop_assert!(parsed.is_locked_by(id2, &agent2));
            prop_assert!(!parsed.is_locked_by(id1, &agent2));
        }

        #[test]
        fn prop_empty_locks_nothing_locked(id in 1i64..10000) {
            let locks = LocksFile::empty();
            prop_assert!(!locks.is_locked(id));
            prop_assert!(locks.get_lock(id).is_none());
        }
    }
}
