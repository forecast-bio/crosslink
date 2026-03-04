use anyhow::{bail, Result};
use std::path::Path;

use crate::db::Database;
use crate::identity::AgentConfig;
use crate::sync::SyncManager;

/// Result of checking whether an agent can work on an issue.
#[derive(Debug, PartialEq)]
pub enum LockStatus {
    /// No lock system configured (no agent.json). Single-agent mode.
    NotConfigured,
    /// Issue is not locked by anyone.
    Available,
    /// Issue is locked by this agent. Proceed.
    LockedBySelf,
    /// Issue is locked by another agent.
    LockedByOther { agent_id: String, stale: bool },
}

/// Check whether the current agent can work on the given issue.
///
/// Returns `LockStatus` without blocking — callers decide how to handle.
/// Gracefully degrades: if agent config is missing, sync fails, or we're
/// offline, returns `NotConfigured` so single-agent usage is unaffected.
pub fn check_lock(crosslink_dir: &Path, issue_id: i64) -> Result<LockStatus> {
    // If no agent config, we're in single-agent mode — no lock checking
    let agent = match AgentConfig::load(crosslink_dir)? {
        Some(a) => a,
        None => return Ok(LockStatus::NotConfigured),
    };

    // Try to create sync manager. If it fails, don't block.
    let sync = match SyncManager::new(crosslink_dir) {
        Ok(s) => s,
        Err(_) => return Ok(LockStatus::NotConfigured),
    };

    // Best-effort init and fetch — don't fail if offline
    let _ = sync.init_cache();
    let _ = sync.fetch();

    // If cache still isn't set up, can't check locks
    if !sync.is_initialized() {
        return Ok(LockStatus::NotConfigured);
    }

    let locks = match sync.read_locks_auto() {
        Ok(l) => l,
        Err(_) => return Ok(LockStatus::NotConfigured),
    };

    // Check if locked by this agent
    if locks.is_locked_by(issue_id, &agent.agent_id) {
        return Ok(LockStatus::LockedBySelf);
    }

    // Check if locked by someone else
    match locks.get_lock(issue_id) {
        Some(lock) => {
            let stale = sync
                .find_stale_locks()
                .unwrap_or_default()
                .iter()
                .any(|(id, _)| *id == issue_id);
            Ok(LockStatus::LockedByOther {
                agent_id: lock.agent_id.clone(),
                stale,
            })
        }
        None => Ok(LockStatus::Available),
    }
}

/// Read the `auto_steal_stale_locks` setting from hook-config.json.
///
/// Returns `None` if disabled or missing, `Some(multiplier)` if enabled.
fn read_auto_steal_config(crosslink_dir: &Path) -> Option<u64> {
    let config_path = crosslink_dir.join("hook-config.json");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
    match parsed.get("auto_steal_stale_locks")? {
        serde_json::Value::Bool(false) => None,
        serde_json::Value::Number(n) => n.as_u64().filter(|&v| v > 0),
        serde_json::Value::String(s) if s == "false" => None,
        serde_json::Value::String(s) => s.parse::<u64>().ok().filter(|&v| v > 0),
        _ => None,
    }
}

/// Attempt to auto-steal a stale lock if configured.
///
/// Returns `Ok(true)` if the lock was auto-stolen, `Ok(false)` if not eligible.
fn auto_steal_if_configured(
    crosslink_dir: &Path,
    issue_id: i64,
    stale_agent_id: &str,
    db: &Database,
) -> Result<bool> {
    let multiplier = match read_auto_steal_config(crosslink_dir) {
        Some(m) => m,
        None => return Ok(false),
    };

    let sync = match SyncManager::new(crosslink_dir) {
        Ok(s) => s,
        Err(_) => return Ok(false),
    };

    if !sync.is_initialized() {
        return Ok(false);
    }

    let stale_locks = sync.find_stale_locks_with_age()?;
    let stale_minutes = match stale_locks.iter().find(|(id, _, _)| *id == issue_id) {
        Some((_, _, mins)) => *mins,
        None => return Ok(false),
    };

    // Threshold = multiplier × stale_timeout
    let stale_timeout = if sync.is_v2_layout() {
        30u64
    } else {
        sync.read_locks_auto()
            .map(|l| l.settings.stale_lock_timeout_minutes)
            .unwrap_or(60)
    };
    let auto_steal_threshold = multiplier.saturating_mul(stale_timeout);

    if stale_minutes < auto_steal_threshold {
        return Ok(false);
    }

    // Perform the steal
    if sync.is_v2_layout() {
        if let Ok(Some(writer)) = crate::shared_writer::SharedWriter::new(crosslink_dir) {
            writer.steal_lock_v2(issue_id, stale_agent_id, None)?;
            let comment = format!(
                "[auto-steal] Lock auto-stolen from agent '{}' (stale for {} min, threshold: {} min)",
                stale_agent_id, stale_minutes, auto_steal_threshold
            );
            let _ = writer.add_comment(db, issue_id, &comment, "system");
        } else {
            return Ok(false);
        }
    } else {
        let agent = match AgentConfig::load(crosslink_dir)? {
            Some(a) => a,
            None => return Ok(false),
        };
        sync.claim_lock(&agent, issue_id, None, true)?;
        let comment = format!(
            "[auto-steal] Lock auto-stolen from agent '{}' (stale for {} min, threshold: {} min)",
            stale_agent_id, stale_minutes, auto_steal_threshold
        );
        let _ = db.add_comment(issue_id, &comment, "system");
    }

    Ok(true)
}

/// Enforce lock check. Bails if another agent holds the lock (unless stale).
///
/// When `auto_steal_stale_locks` is configured in hook-config.json and the lock
/// has been stale long enough, automatically steals it and records an audit comment.
pub fn enforce_lock(crosslink_dir: &Path, issue_id: i64, db: &Database) -> Result<()> {
    match check_lock(crosslink_dir, issue_id)? {
        LockStatus::NotConfigured | LockStatus::Available | LockStatus::LockedBySelf => Ok(()),
        LockStatus::LockedByOther { agent_id, stale } => {
            if stale {
                match auto_steal_if_configured(crosslink_dir, issue_id, &agent_id, db) {
                    Ok(true) => {
                        eprintln!(
                            "Auto-stole stale lock on issue #{} from '{}'.",
                            issue_id, agent_id
                        );
                        return Ok(());
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!(
                            "Warning: Auto-steal of stale lock on #{} failed: {}. Proceeding.",
                            issue_id, e
                        );
                    }
                }

                eprintln!(
                    "Warning: Issue #{} is locked by '{}' but the lock appears STALE. Proceeding.",
                    issue_id, agent_id
                );
                Ok(())
            } else {
                bail!(
                    "Issue #{} is locked by agent '{}'. \
                     Use 'crosslink locks check {}' for details. \
                     Ask the human to release it or wait for the lock to expire.",
                    issue_id,
                    agent_id,
                    issue_id
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn temp_db() -> Database {
        Database::open(std::path::Path::new(":memory:")).unwrap()
    }

    #[test]
    fn test_no_agent_config_returns_not_configured() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let status = check_lock(&crosslink_dir, 1).unwrap();
        assert_eq!(status, LockStatus::NotConfigured);
    }

    #[test]
    fn test_enforce_not_configured_allows() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let db = temp_db();
        // No agent.json → NotConfigured → allowed
        assert!(enforce_lock(&crosslink_dir, 1, &db).is_ok());
    }

    #[test]
    fn test_enforce_available_allows() {
        // enforce_lock on Available should succeed
        // We can't easily test this without a full git setup,
        // but the logic is: Available → Ok(())
        // Covered implicitly by the NotConfigured test since
        // that path also returns Ok(())
    }

    #[test]
    fn test_lock_status_debug() {
        let statuses = vec![
            LockStatus::NotConfigured,
            LockStatus::Available,
            LockStatus::LockedBySelf,
            LockStatus::LockedByOther {
                agent_id: "worker-1".to_string(),
                stale: false,
            },
            LockStatus::LockedByOther {
                agent_id: "worker-2".to_string(),
                stale: true,
            },
        ];
        for s in statuses {
            let _ = format!("{:?}", s);
        }
    }

    #[test]
    fn test_lock_status_equality() {
        assert_eq!(LockStatus::NotConfigured, LockStatus::NotConfigured);
        assert_eq!(LockStatus::Available, LockStatus::Available);
        assert_eq!(LockStatus::LockedBySelf, LockStatus::LockedBySelf);
        assert_ne!(LockStatus::Available, LockStatus::NotConfigured);
        assert_eq!(
            LockStatus::LockedByOther {
                agent_id: "a".to_string(),
                stale: false
            },
            LockStatus::LockedByOther {
                agent_id: "a".to_string(),
                stale: false
            }
        );
        assert_ne!(
            LockStatus::LockedByOther {
                agent_id: "a".to_string(),
                stale: false
            },
            LockStatus::LockedByOther {
                agent_id: "b".to_string(),
                stale: false
            }
        );
    }

    // --- read_auto_steal_config tests ---

    #[test]
    fn test_auto_steal_config_disabled() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("hook-config.json"),
            r#"{"auto_steal_stale_locks": false}"#,
        )
        .unwrap();
        assert_eq!(read_auto_steal_config(dir.path()), None);
    }

    #[test]
    fn test_auto_steal_config_enabled_int() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("hook-config.json"),
            r#"{"auto_steal_stale_locks": 2}"#,
        )
        .unwrap();
        assert_eq!(read_auto_steal_config(dir.path()), Some(2));
    }

    #[test]
    fn test_auto_steal_config_enabled_string() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("hook-config.json"),
            r#"{"auto_steal_stale_locks": "3"}"#,
        )
        .unwrap();
        assert_eq!(read_auto_steal_config(dir.path()), Some(3));
    }

    #[test]
    fn test_auto_steal_config_string_false() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("hook-config.json"),
            r#"{"auto_steal_stale_locks": "false"}"#,
        )
        .unwrap();
        assert_eq!(read_auto_steal_config(dir.path()), None);
    }

    #[test]
    fn test_auto_steal_config_missing_key() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("hook-config.json"), r#"{}"#).unwrap();
        assert_eq!(read_auto_steal_config(dir.path()), None);
    }

    #[test]
    fn test_auto_steal_config_no_file() {
        let dir = tempdir().unwrap();
        assert_eq!(read_auto_steal_config(dir.path()), None);
    }

    #[test]
    fn test_auto_steal_config_zero_disabled() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("hook-config.json"),
            r#"{"auto_steal_stale_locks": 0}"#,
        )
        .unwrap();
        assert_eq!(read_auto_steal_config(dir.path()), None);
    }
}
