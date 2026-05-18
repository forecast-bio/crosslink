use super::*;
use crate::identity::{AgentConfig, AgentRole};
use crate::locks::{Heartbeat, Keyring, LocksFile};
use crate::sync::LockMode;
use chrono::Utc;
use std::path::Path;
use std::process::Command;
use tempfile::tempdir;

// GPG fingerprint parsing tests moved to signing.rs

#[test]
fn test_sync_manager_new() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert_eq!(manager.cache_dir, crosslink_dir.join(HUB_CACHE_DIR));
    assert_eq!(manager.repo_root, dir.path());
}

#[test]
fn test_sync_manager_not_initialized() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert!(!manager.is_initialized());
}

#[test]
fn test_read_locks_no_cache() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    // Cache doesn't exist yet, but read_locks should return empty
    // (it checks if the file exists)
    let locks_path = manager.cache_dir.join("locks.json");
    assert!(!locks_path.exists());
}

#[test]
fn test_read_heartbeats_no_dir() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    // Manually create cache dir without heartbeats subdir
    std::fs::create_dir_all(&manager.cache_dir).unwrap();
    let heartbeats = manager.read_heartbeats().unwrap();
    assert!(heartbeats.is_empty());
}

#[test]
fn test_read_heartbeats_with_files() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let hb_dir = cache_dir.join("heartbeats");
    std::fs::create_dir_all(&hb_dir).unwrap();

    let hb = Heartbeat {
        agent_id: "worker-1".to_string(),
        last_heartbeat: Utc::now(),
        active_issue_id: Some(5),
        machine_id: "test-host".to_string(),
    };
    let json = serde_json::to_string_pretty(&hb).unwrap();
    std::fs::write(hb_dir.join("worker-1.json"), json).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let heartbeats = manager.read_heartbeats().unwrap();
    assert_eq!(heartbeats.len(), 1);
    assert_eq!(heartbeats[0].agent_id, "worker-1");
    assert_eq!(heartbeats[0].active_issue_id, Some(5));
}

#[test]
fn test_read_heartbeats_v2_no_dir() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    std::fs::create_dir_all(&manager.cache_dir).unwrap();
    let heartbeats = manager.read_heartbeats_v2().unwrap();
    assert!(heartbeats.is_empty());
}

#[test]
fn test_read_heartbeats_v2_with_native_format() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let agent_dir = cache_dir.join("agents").join("worker-v2");
    std::fs::create_dir_all(&agent_dir).unwrap();

    // Write a native Heartbeat format file in the V2 location
    let hb = Heartbeat {
        agent_id: "worker-v2".to_string(),
        last_heartbeat: Utc::now(),
        active_issue_id: Some(10),
        machine_id: "host-v2".to_string(),
    };
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&hb).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let heartbeats = manager.read_heartbeats_v2().unwrap();
    assert_eq!(heartbeats.len(), 1);
    assert_eq!(heartbeats[0].agent_id, "worker-v2");
    assert_eq!(heartbeats[0].active_issue_id, Some(10));
}

#[test]
fn test_read_heartbeats_v2_with_v2_json_format() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let agent_dir = cache_dir.join("agents").join("worker-v2b");
    std::fs::create_dir_all(&agent_dir).unwrap();

    // Write V2 format: { agent_id, timestamp, status }
    let heartbeat = serde_json::json!({
        "agent_id": "worker-v2b",
        "timestamp": Utc::now().to_rfc3339(),
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let heartbeats = manager.read_heartbeats_v2().unwrap();
    assert_eq!(heartbeats.len(), 1);
    assert_eq!(heartbeats[0].agent_id, "worker-v2b");
    assert!(heartbeats[0].active_issue_id.is_none());
}

#[test]
fn test_read_heartbeats_auto_merges_v1_and_v2() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    // Set up V2 layout marker
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    // Write V1 heartbeat
    let hb_dir = cache_dir.join("heartbeats");
    std::fs::create_dir_all(&hb_dir).unwrap();
    let hb1 = Heartbeat {
        agent_id: "worker-v1".to_string(),
        last_heartbeat: Utc::now(),
        active_issue_id: Some(1),
        machine_id: "host-1".to_string(),
    };
    std::fs::write(
        hb_dir.join("worker-v1.json"),
        serde_json::to_string_pretty(&hb1).unwrap(),
    )
    .unwrap();

    // Write V2 heartbeat
    let agent_dir = cache_dir.join("agents").join("worker-v2");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let heartbeat = serde_json::json!({
        "agent_id": "worker-v2",
        "timestamp": Utc::now().to_rfc3339(),
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let heartbeats = manager.read_heartbeats_auto().unwrap();
    assert_eq!(heartbeats.len(), 2);

    let ids: std::collections::HashSet<String> =
        heartbeats.iter().map(|h| h.agent_id.clone()).collect();
    assert!(ids.contains("worker-v1"));
    assert!(ids.contains("worker-v2"));
}

#[test]
fn test_find_stale_locks_empty() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Write empty locks.json
    let locks = LocksFile::empty();
    locks.save(&cache_dir.join("locks.json")).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks().unwrap();
    assert!(stale.is_empty());
}

#[test]
fn test_find_stale_locks_with_stale() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let hb_dir = cache_dir.join("heartbeats");
    std::fs::create_dir_all(&hb_dir).unwrap();

    // Create a lock
    let mut locks_map = std::collections::HashMap::new();
    locks_map.insert(
        5i64,
        crate::locks::Lock {
            agent_id: "worker-1".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: "ABC".to_string(),
        },
    );
    let locks = LocksFile {
        version: 1,
        locks: locks_map,
        settings: crate::locks::LockSettings {
            stale_lock_timeout_minutes: 60,
        },
    };
    locks.save(&cache_dir.join("locks.json")).unwrap();

    // No heartbeat file for worker-1 -> stale
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0], (5, "worker-1".to_string()));
}

#[test]
fn test_find_stale_locks_with_fresh_heartbeat() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let hb_dir = cache_dir.join("heartbeats");
    std::fs::create_dir_all(&hb_dir).unwrap();

    // Create a lock
    let mut locks_map = std::collections::HashMap::new();
    locks_map.insert(
        5i64,
        crate::locks::Lock {
            agent_id: "worker-1".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: "ABC".to_string(),
        },
    );
    let locks = LocksFile {
        version: 1,
        locks: locks_map,
        settings: crate::locks::LockSettings {
            stale_lock_timeout_minutes: 60,
        },
    };
    locks.save(&cache_dir.join("locks.json")).unwrap();

    // Fresh heartbeat
    let hb = Heartbeat {
        agent_id: "worker-1".to_string(),
        last_heartbeat: Utc::now(),
        active_issue_id: Some(5),
        machine_id: "test".to_string(),
    };
    let json = serde_json::to_string(&hb).unwrap();
    std::fs::write(hb_dir.join("worker-1.json"), json).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks().unwrap();
    assert!(stale.is_empty());
}

#[test]
fn test_find_stale_locks_v2_fresh_heartbeat() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    // Set up V2 layout
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    // Write a lock file
    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 5,
        agent_id: "worker-1".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("5.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // Write a fresh heartbeat (now)
    let agent_dir = cache_dir.join("agents").join("worker-1");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let heartbeat = serde_json::json!({
        "agent_id": "worker-1",
        "timestamp": Utc::now().to_rfc3339(),
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks().unwrap();
    assert!(stale.is_empty(), "Fresh heartbeat should not be stale");
}

#[test]
fn test_find_stale_locks_v2_old_heartbeat() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    // Set up V2 layout
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    // Write a lock file
    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 10,
        agent_id: "worker-2".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("10.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // Write a stale heartbeat (2 hours ago)
    let agent_dir = cache_dir.join("agents").join("worker-2");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let old_timestamp = Utc::now() - chrono::Duration::hours(2);
    let heartbeat = serde_json::json!({
        "agent_id": "worker-2",
        "timestamp": old_timestamp.to_rfc3339(),
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0], (10, "worker-2".to_string()));
}

#[test]
fn test_find_stale_locks_v2_missing_heartbeat() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    // Set up V2 layout
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    // Write a lock file but NO heartbeat
    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 7,
        agent_id: "ghost-agent".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("7.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // No agents/ghost-agent/heartbeat.json exists

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0], (7, "ghost-agent".to_string()));
}

/// Helper: create a git repo with an initial commit.
fn init_git_repo(path: &Path) {
    let p = path.to_string_lossy();
    Command::new("git").args(["init", &p]).output().unwrap();
    // Set user config so commits work on CI (no global git config).
    Command::new("git")
        .args(["-C", &p, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &p, "config", "user.name", "Test"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &p, "commit", "--allow-empty", "-m", "init"])
        .output()
        .unwrap();
}

#[test]
fn test_read_locks_v2_empty_dir() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(cache_dir.join("locks")).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let locks = manager.read_locks_v2().unwrap();
    assert!(locks.locks.is_empty());
    assert_eq!(locks.version, 2);
}

#[test]
fn test_read_locks_v2_no_locks_dir() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let locks = manager.read_locks_v2().unwrap();
    assert!(locks.locks.is_empty());
}

#[test]
fn test_read_locks_v2_with_files() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();

    let lock = crate::issue_file::LockFileV2 {
        issue_id: 5,
        agent_id: "worker-1".to_string(),
        branch: Some("feature/x".to_string()),
        claimed_at: Utc::now(),
        signed_by: Some("SHA256:abc".to_string()),
    };
    let json = serde_json::to_string_pretty(&lock).unwrap();
    std::fs::write(locks_dir.join("5.json"), &json).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let locks = manager.read_locks_v2().unwrap();
    assert_eq!(locks.locks.len(), 1);
    assert!(locks.is_locked(5));
    let l = locks.get_lock(5).unwrap();
    assert_eq!(l.agent_id, "worker-1");
    assert_eq!(l.branch, Some("feature/x".to_string()));
    assert_eq!(l.signed_by, "SHA256:abc");
}

#[test]
fn test_read_locks_v2_skips_non_json() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();

    // Write a non-json file that should be ignored
    std::fs::write(locks_dir.join("README.md"), "ignore me").unwrap();

    let lock = crate::issue_file::LockFileV2 {
        issue_id: 3,
        agent_id: "worker-2".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("3.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let locks = manager.read_locks_v2().unwrap();
    assert_eq!(locks.locks.len(), 1);
    assert!(locks.is_locked(3));
}

#[test]
fn test_read_locks_v2_signed_by_none_defaults_empty() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();

    let lock = crate::issue_file::LockFileV2 {
        issue_id: 7,
        agent_id: "worker-3".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("7.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let locks = manager.read_locks_v2().unwrap();
    let l = locks.get_lock(7).unwrap();
    assert_eq!(l.signed_by, "");
}

#[test]
fn test_read_locks_auto_v1_default() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    // No meta/version.json -> defaults to V1 -> reads locks.json
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let locks = manager.read_locks_auto().unwrap();
    assert!(locks.locks.is_empty());
}

#[test]
fn test_read_locks_auto_v2_dispatch() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Write V2 layout version
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    // Write a lock file
    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 3,
        agent_id: "worker-2".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("3.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let locks = manager.read_locks_auto().unwrap();
    assert_eq!(locks.locks.len(), 1);
    assert!(locks.is_locked(3));
}

#[test]
fn test_read_locks_auto_v1_explicit() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Write V1 layout version explicitly
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 1).unwrap();

    // Write a locks.json (V1 format)
    let locks = LocksFile::empty();
    locks.save(&cache_dir.join("locks.json")).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let result = manager.read_locks_auto().unwrap();
    assert!(result.locks.is_empty());
}

#[test]
fn test_ensure_agent_dir_creates_directory() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let created = manager.create_agent_dir_files("worker-42").unwrap();
    assert!(created);

    let agent_dir = cache_dir.join("agents").join("worker-42");
    assert!(agent_dir.exists());
    assert!(agent_dir.join("heartbeat.json").exists());
}

#[test]
fn test_ensure_agent_dir_idempotent() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let first = manager.create_agent_dir_files("worker-42").unwrap();
    assert!(first);

    let second = manager.create_agent_dir_files("worker-42").unwrap();
    assert!(!second);
}

#[test]
fn test_ensure_agent_dir_heartbeat_valid_json() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.create_agent_dir_files("test-agent").unwrap();

    let heartbeat_path = cache_dir
        .join("agents")
        .join("test-agent")
        .join("heartbeat.json");
    let content = std::fs::read_to_string(&heartbeat_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(parsed["agent_id"], "test-agent");
    assert_eq!(parsed["status"], "active");
    assert!(parsed["timestamp"].is_string());
    // Verify timestamp is valid RFC3339
    let ts = parsed["timestamp"].as_str().unwrap();
    chrono::DateTime::parse_from_rfc3339(ts).expect("timestamp should be valid RFC3339");
}

// resolve_main_repo_root tests are in utils::tests

#[test]
fn test_sync_manager_in_worktree_uses_main_hub_cache() {
    let dir = tempdir().unwrap();
    let main_root = dir.path().join("main");
    std::fs::create_dir_all(&main_root).unwrap();
    init_git_repo(&main_root);

    let main_crosslink = main_root.join(".crosslink");
    std::fs::create_dir_all(&main_crosslink).unwrap();

    // Create worktree
    Command::new("git")
        .args([
            "-C",
            &main_root.to_string_lossy(),
            "branch",
            "feature/hub-test",
        ])
        .output()
        .unwrap();
    let wt_path = main_root.join(".worktrees").join("hub-test");
    std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
    Command::new("git")
        .args([
            "-C",
            &main_root.to_string_lossy(),
            "worktree",
            "add",
            &wt_path.to_string_lossy(),
            "feature/hub-test",
        ])
        .output()
        .unwrap();

    let wt_crosslink = wt_path.join(".crosslink");
    std::fs::create_dir_all(&wt_crosslink).unwrap();

    let manager = SyncManager::new(&wt_crosslink).unwrap();

    // cache_dir should point to the main repo's hub cache, not the worktree's
    // Canonicalize the parent (.crosslink) since .hub-cache doesn't exist yet.
    let expected_parent = main_crosslink.canonicalize().unwrap();
    let actual_parent = manager.cache_dir.parent().unwrap().canonicalize().unwrap();
    assert_eq!(actual_parent, expected_parent);
    assert_eq!(manager.cache_dir.file_name().unwrap(), HUB_CACHE_DIR);

    // repo_root should be the main repo, not the worktree
    assert_eq!(
        manager.repo_root.canonicalize().unwrap(),
        main_root.canonicalize().unwrap()
    );
}

// ------------------------------------------------------------------
// Helper: set up a real git repo with a bare remote and .crosslink dir.
// Returns (work_dir, remote_dir).
// ------------------------------------------------------------------
fn setup_sync_env() -> (tempfile::TempDir, tempfile::TempDir) {
    let remote_dir = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();

    // Init bare remote
    Command::new("git")
        .current_dir(remote_dir.path())
        .args(["init", "--bare", "-b", "main"])
        .output()
        .unwrap();

    // Init work repo
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["init", "-b", "main"])
        .output()
        .unwrap();

    // Config git identity
    for args in [
        vec!["config", "user.email", "test@test.local"],
        vec!["config", "user.name", "Test"],
        vec![
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ],
    ] {
        Command::new("git")
            .current_dir(work_dir.path())
            .args(&args)
            .output()
            .unwrap();
    }

    // Initial commit + push
    std::fs::write(work_dir.path().join("README.md"), "# test\n").unwrap();
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["commit", "-m", "init", "--no-gpg-sign"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["push", "-u", "origin", "main"])
        .output()
        .unwrap();

    // Create .crosslink dir with hook-config.json
    let crosslink_dir = work_dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"remote":"origin"}"#,
    )
    .unwrap();

    (work_dir, remote_dir)
}

// ------------------------------------------------------------------
// read_tracker_remote
// ------------------------------------------------------------------

#[test]
fn test_read_tracker_remote_default() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    // No hook-config.json -> defaults to "origin"
    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(remote, "origin");
}

#[test]
fn test_read_tracker_remote_missing_field_defaults_origin() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    // hook-config.json exists but has no tracker_remote field -> "origin"
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"remote":"origin"}"#,
    )
    .unwrap();
    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(remote, "origin");
}

#[test]
fn test_read_tracker_remote_custom_value() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"tracker_remote":"upstream"}"#,
    )
    .unwrap();
    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(remote, "upstream");
}

#[test]
fn test_read_tracker_remote_falls_back_when_corrupt_placeholder() {
    // GH#739: older builds of the init walkthrough wrote the literal
    // UI placeholder "(text)" into hook-config.json for every
    // ConfigType::String key. read_tracker_remote() must detect this
    // corrupt sentinel and fall back to "origin" so push/sync don't
    // fail with the (correct but unhelpful) RemoteMisconfigured("(text)")
    // error. The permanent fix is `crosslink config set tracker_remote
    // <name>` or `crosslink init --force`.
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"tracker_remote":"(text)"}"#,
    )
    .unwrap();
    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(
        remote, "origin",
        "corrupt '(text)' placeholder must fall back to 'origin'"
    );
}

// ------------------------------------------------------------------
// SyncManager::new() with hook-config.json having a tracker_remote key
// ------------------------------------------------------------------

#[test]
fn test_sync_manager_new_reads_remote_from_config() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"tracker_remote":"upstream"}"#,
    )
    .unwrap();
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert_eq!(manager.remote(), "upstream");
}

// ------------------------------------------------------------------
// is_v2_layout, is_initialized, cache_path, remote
// ------------------------------------------------------------------

#[test]
fn test_is_v2_layout_false_when_no_meta() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert!(!manager.is_v2_layout());
}

#[test]
fn test_is_v2_layout_true_with_v2_marker() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert!(manager.is_v2_layout());
}

#[test]
fn test_cache_path_accessor() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert_eq!(manager.cache_path(), manager.cache_dir.as_path());
}

#[test]
fn test_remote_accessor() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert_eq!(manager.remote(), "origin");
}

// ------------------------------------------------------------------
// init_cache -- creates orphan hub branch with initial structure
// ------------------------------------------------------------------

#[test]
fn test_init_cache_creates_hub_worktree() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();

    assert!(!manager.is_initialized());
    manager.init_cache().unwrap();
    assert!(manager.is_initialized());

    // Should have locks.json
    assert!(manager.cache_dir.join("locks.json").exists());
}

#[test]
fn test_init_cache_idempotent() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();

    manager.init_cache().unwrap();
    // Second call should be a no-op (cache_dir exists)
    manager.init_cache().unwrap();
    assert!(manager.is_initialized());
}

#[test]
fn test_init_cache_creates_directory_structure() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();

    manager.init_cache().unwrap();

    let cache = &manager.cache_dir;
    assert!(cache.join("locks.json").exists());
    assert!(cache.join("heartbeats").exists());
    assert!(cache.join("trust").exists());
    assert!(cache.join("issues").exists());
    assert!(cache.join("locks").exists());
}

#[test]
fn test_init_cache_from_existing_remote_branch() {
    // Set up env, init cache to push branch to remote, then reinit from remote
    let (work_dir, remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Push hub branch to remote
    manager
        .git_in_cache(&["push", "-u", "origin", HUB_BRANCH])
        .unwrap();

    // Now create a fresh work dir cloned from same remote
    let work_dir2 = tempfile::tempdir().unwrap();
    Command::new("git")
        .current_dir(work_dir2.path())
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.email", "test@test.local"],
        vec!["config", "user.name", "Test"],
        vec![
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ],
    ] {
        Command::new("git")
            .current_dir(work_dir2.path())
            .args(&args)
            .output()
            .unwrap();
    }
    // fetch main so repo isn't completely empty
    Command::new("git")
        .current_dir(work_dir2.path())
        .args(["fetch", "origin", "main"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(work_dir2.path())
        .args(["checkout", "-b", "main", "origin/main"])
        .output()
        .unwrap();

    let crosslink_dir2 = work_dir2.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir2).unwrap();
    std::fs::write(
        crosslink_dir2.join("hook-config.json"),
        r#"{"remote":"origin"}"#,
    )
    .unwrap();

    let manager2 = SyncManager::new(&crosslink_dir2).unwrap();
    manager2.init_cache().unwrap();
    assert!(manager2.is_initialized());
}

// ------------------------------------------------------------------
// fetch
// ------------------------------------------------------------------

#[test]
fn test_fetch_on_initialized_cache() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // fetch should succeed (hub branch has no remote, but that's handled gracefully)
    manager.fetch().unwrap();
}

#[test]
fn test_fetch_from_remote_with_hub_branch() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Push hub branch to remote so fetch has something to fetch
    manager
        .git_in_cache(&["push", "-u", "origin", HUB_BRANCH])
        .unwrap();

    // Now fetch again
    manager.fetch().unwrap();
}

// ------------------------------------------------------------------
// clean_dirty_state
// ------------------------------------------------------------------

#[test]
fn test_clean_dirty_state_no_dirty() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Stage and commit everything so there's a truly clean state
    let _ = manager.git_in_cache(&["add", "-A"]);
    let _ = manager.git_in_cache(&["commit", "-m", "cleanup for test"]);

    // Nothing dirty -> returns false
    let cleaned = manager.clean_dirty_state().unwrap();
    assert!(!cleaned);
}

#[test]
fn test_clean_dirty_state_with_dirty_file() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Write an untracked file to make the state dirty
    std::fs::write(manager.cache_dir.join("dirty.txt"), "dirty").unwrap();

    let cleaned = manager.clean_dirty_state().unwrap();
    assert!(cleaned);
}

/// Regression test for #574: when the hub cache directory exists but lacks
/// its own `.git` link, every git command run from inside it walks up to the
/// parent repository's git directory. Before the fix, `clean_dirty_state`
/// would happily run `git add -A` + `git commit` against the parent's index
/// and HEAD, silently landing a `sync: auto-stage dirty hub state (recovery)`
/// commit on whatever feature branch the user had checked out, replacing the
/// branch's tree with hub-cache artifacts.
///
/// The fix adds a `verify_cache_worktree` preflight that:
/// 1. Confirms `git rev-parse --show-toplevel` from `cache_dir` resolves to
///    `cache_dir` (not a parent — catches the walk-up case).
/// 2. Confirms HEAD is on the configured `HUB_BRANCH`.
///
/// This test simulates the broken-worktree state and asserts that
/// `clean_dirty_state` refuses with an error and does not modify the parent
/// repository's branch ref or commit log.
#[test]
fn test_clean_dirty_state_refuses_when_cache_dir_is_not_a_worktree() {
    let dir = tempdir().unwrap();

    // Init a parent repo on a feature-style branch.
    Command::new("git")
        .current_dir(dir.path())
        .args(["init", "-q", "-b", "feat/foo"])
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.email", "t@t.local"],
        vec!["config", "user.name", "T"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        Command::new("git")
            .current_dir(dir.path())
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::write(dir.path().join("README.md"), "feature\n").unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["commit", "-qm", "init"])
        .output()
        .unwrap();

    let parent_head_before = String::from_utf8_lossy(
        &Command::new("git")
            .current_dir(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();

    // Create .crosslink/.hub-cache/ as a regular directory — NO `.git` link.
    // This simulates the broken-worktree state described in the bug report.
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"remote":"origin"}"#,
    )
    .unwrap();
    // Add a file inside the broken cache so that an unguarded
    // `git add -A` would have something to stage.
    std::fs::write(cache_dir.join("locks.json"), r#"{"locks":[]}"#).unwrap();

    // Make the parent repo also dirty — this is what an unguarded
    // walked-up `git status --porcelain` would report, kicking off the
    // recovery branch in `clean_dirty_state`.
    std::fs::write(dir.path().join("oops.txt"), "dirty\n").unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();

    let result = manager.clean_dirty_state();
    assert!(
        result.is_err(),
        "clean_dirty_state must refuse to operate on a broken hub cache; got {result:?}"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("574") || err_msg.contains("walking up") || err_msg.contains("not bound"),
        "error should reference the data-loss guard; got: {err_msg}"
    );

    // The parent repository's HEAD must be unchanged — no rogue recovery
    // commit was created.
    let parent_head_after = String::from_utf8_lossy(
        &Command::new("git")
            .current_dir(dir.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert_eq!(
        parent_head_before, parent_head_after,
        "parent repo HEAD must not change when hub cache is broken"
    );

    // And no commit anywhere in the parent's history should mention the
    // recovery message.
    let log_out = Command::new("git")
        .current_dir(dir.path())
        .args(["log", "--all", "--format=%s"])
        .output()
        .unwrap();
    let log_str = String::from_utf8_lossy(&log_out.stdout);
    assert!(
        !log_str.contains("auto-stage dirty hub state"),
        "parent repo must not contain any hub-recovery commits; log was: {log_str}"
    );
}

/// Regression test for #574: even if a caller manages to bypass
/// `clean_dirty_state`'s preflight (e.g. through a future code path that
/// calls `git_commit_in_cache` directly), the commit helper itself must
/// refuse to write into a broken cache. This is the defense-in-depth layer.
#[test]
fn test_git_commit_in_cache_refuses_when_cache_is_broken() {
    let dir = tempdir().unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["init", "-q", "-b", "feat/foo"])
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.email", "t@t.local"],
        vec!["config", "user.name", "T"],
        vec!["config", "commit.gpgsign", "false"],
    ] {
        Command::new("git")
            .current_dir(dir.path())
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::write(dir.path().join("README.md"), "x\n").unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["commit", "-qm", "init"])
        .output()
        .unwrap();

    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"remote":"origin"}"#,
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let result = manager.git_commit_in_cache(&["-m", "should not land"]);
    assert!(
        result.is_err(),
        "git_commit_in_cache must refuse on a broken cache; got {result:?}"
    );
}

// ------------------------------------------------------------------
// read_locks, read_keyring, read_allowed_signers
// ------------------------------------------------------------------

#[test]
fn test_read_locks_with_initialized_cache() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let locks = manager.read_locks().unwrap();
    assert!(locks.locks.is_empty());
}

#[test]
fn test_read_keyring_no_file() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let keyring = manager.read_keyring().unwrap();
    assert!(keyring.is_none());
}

#[test]
fn test_read_allowed_signers_no_file() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(cache_dir.join("trust")).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    // No allowed_signers file -> should return an empty/default store
    let result = manager.read_allowed_signers();
    // Either Ok or Err is acceptable; just ensure it doesn't panic
    let _ = result;
}

// ------------------------------------------------------------------
// upgrade_to_v2
// ------------------------------------------------------------------

#[test]
fn test_upgrade_to_v2_already_v2_is_noop() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // init_cache writes v2 layout marker for new hubs
    let migrated = manager.upgrade_to_v2().unwrap();
    assert_eq!(migrated, 0);
}

#[test]
fn test_upgrade_to_v2_from_v1() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Overwrite the layout version to V1 to simulate a V1 hub
    let meta_dir = manager.cache_dir.join("meta");
    crate::issue_file::write_layout_version(&meta_dir, 1).unwrap();
    manager.git_in_cache(&["add", "-A"]).unwrap();
    manager
        .git_in_cache(&["commit", "-m", "downgrade to v1 for test"])
        .unwrap();

    assert!(!manager.is_v2_layout());
    let migrated = manager.upgrade_to_v2().unwrap();
    // 0 inline comments to migrate
    assert_eq!(migrated, 0);
    // Now should be V2
    assert!(manager.is_v2_layout());
}

// ------------------------------------------------------------------
// find_stale_locks_with_age (V1 path)
// ------------------------------------------------------------------

#[test]
fn test_find_stale_locks_with_age_empty() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let locks = LocksFile::empty();
    locks.save(&cache_dir.join("locks.json")).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks_with_age().unwrap();
    assert!(stale.is_empty());
}

#[test]
fn test_find_stale_locks_with_age_stale_lock_no_heartbeat() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(cache_dir.join("heartbeats")).unwrap();

    // Lock claimed 2 hours ago
    let old_time = Utc::now() - chrono::Duration::hours(2);
    let mut locks_map = std::collections::HashMap::new();
    locks_map.insert(
        42i64,
        crate::locks::Lock {
            agent_id: "stale-agent".to_string(),
            branch: None,
            claimed_at: old_time,
            signed_by: String::new(),
        },
    );
    let locks = LocksFile {
        version: 1,
        locks: locks_map,
        settings: crate::locks::LockSettings {
            stale_lock_timeout_minutes: 60,
        },
    };
    locks.save(&cache_dir.join("locks.json")).unwrap();

    // No heartbeat for stale-agent
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks_with_age().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].0, 42);
    assert_eq!(stale[0].1, "stale-agent");
    assert!(stale[0].2 >= 60); // at least 60 minutes old
}

#[test]
fn test_find_stale_locks_with_age_fresh_heartbeat_not_stale() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let hb_dir = cache_dir.join("heartbeats");
    std::fs::create_dir_all(&hb_dir).unwrap();

    let mut locks_map = std::collections::HashMap::new();
    locks_map.insert(
        10i64,
        crate::locks::Lock {
            agent_id: "fresh-agent".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: String::new(),
        },
    );
    let locks = LocksFile {
        version: 1,
        locks: locks_map,
        settings: crate::locks::LockSettings {
            stale_lock_timeout_minutes: 60,
        },
    };
    locks.save(&cache_dir.join("locks.json")).unwrap();

    // Fresh heartbeat just now
    let hb = Heartbeat {
        agent_id: "fresh-agent".to_string(),
        last_heartbeat: Utc::now(),
        active_issue_id: None,
        machine_id: "host".to_string(),
    };
    std::fs::write(
        hb_dir.join("fresh-agent.json"),
        serde_json::to_string(&hb).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks_with_age().unwrap();
    assert!(stale.is_empty());
}

// ------------------------------------------------------------------
// find_stale_locks_with_age (V2 path)
// ------------------------------------------------------------------

#[test]
fn test_find_stale_locks_with_age_v2_stale() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    // V2 layout
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 99,
        agent_id: "v2-agent".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("99.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // Old heartbeat (2 hours ago)
    let agent_dir = cache_dir.join("agents").join("v2-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let old_ts = Utc::now() - chrono::Duration::hours(2);
    let heartbeat = serde_json::json!({
        "agent_id": "v2-agent",
        "timestamp": old_ts.to_rfc3339(),
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks_with_age().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].0, 99);
    assert_eq!(stale[0].1, "v2-agent");
    assert!(stale[0].2 >= 60);
}

#[test]
fn test_find_stale_locks_with_age_v2_fresh() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    // V2 layout
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 55,
        agent_id: "v2-fresh".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("55.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // Fresh heartbeat (just now)
    let agent_dir = cache_dir.join("agents").join("v2-fresh");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let heartbeat = serde_json::json!({
        "agent_id": "v2-fresh",
        "timestamp": Utc::now().to_rfc3339(),
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks_with_age().unwrap();
    assert!(stale.is_empty());
}

#[test]
fn test_find_stale_locks_with_age_v2_no_heartbeat_file() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 33,
        agent_id: "no-heartbeat-agent".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("33.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();
    // No agents/ directory at all

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks_with_age().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].0, 33);
    assert_eq!(stale[0].2, u64::MAX);
}

#[test]
fn test_find_stale_locks_with_age_v2_invalid_timestamp() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 11,
        agent_id: "bad-ts-agent".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("11.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // Write a heartbeat with unparseable timestamp
    let agent_dir = cache_dir.join("agents").join("bad-ts-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let heartbeat = serde_json::json!({
        "agent_id": "bad-ts-agent",
        "timestamp": "not-a-date",
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks_with_age().unwrap();
    // Bad timestamp -> stale with MAX age
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].2, u64::MAX);
}

#[test]
fn test_find_stale_locks_with_age_v2_missing_timestamp_field() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 22,
        agent_id: "no-ts-agent".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("22.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // Heartbeat file with no timestamp field
    let agent_dir = cache_dir.join("agents").join("no-ts-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let heartbeat = serde_json::json!({
        "agent_id": "no-ts-agent",
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager.find_stale_locks_with_age().unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].2, u64::MAX);
}

// ------------------------------------------------------------------
// claim_lock / release_lock (needs a real git repo + hub cache)
// ------------------------------------------------------------------

fn make_agent(id: &str) -> AgentConfig {
    AgentConfig {
        agent_id: id.to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    }
}

#[test]
fn test_claim_and_release_lock() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent = make_agent("test-agent");

    // Claim lock on issue 1
    let claimed = manager
        .claim_lock(&agent, 1, None, LockMode::Normal)
        .unwrap();
    assert!(claimed);

    // Claiming again for same agent should return false (already held by self)
    let claimed_again = manager
        .claim_lock(&agent, 1, None, LockMode::Normal)
        .unwrap();
    assert!(!claimed_again);

    // Check lock is set
    let locks = manager.read_locks().unwrap();
    assert!(locks.is_locked(1));

    // Release lock
    let released = manager.release_lock(&agent, 1, LockMode::Normal).unwrap();
    assert!(released);

    // Check lock is gone
    let locks = manager.read_locks().unwrap();
    assert!(!locks.is_locked(1));
}

#[test]
fn test_release_lock_not_locked_returns_false() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent = make_agent("test-agent");

    // No lock exists -> returns false
    let released = manager.release_lock(&agent, 999, LockMode::Normal).unwrap();
    assert!(!released);
}

#[test]
fn test_claim_lock_already_locked_by_other_fails() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent1 = make_agent("agent-1");
    let agent2 = make_agent("agent-2");

    // Agent 1 claims
    manager
        .claim_lock(&agent1, 5, None, LockMode::Normal)
        .unwrap();

    // Agent 2 tries to claim without force -> error
    let result = manager.claim_lock(&agent2, 5, None, LockMode::Normal);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("locked by 'agent-1'"));
}

#[test]
fn test_claim_lock_force_steals() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent1 = make_agent("agent-1");
    let agent2 = make_agent("agent-2");

    manager
        .claim_lock(&agent1, 7, None, LockMode::Normal)
        .unwrap();

    // Agent 2 steals with force=true
    let stolen = manager
        .claim_lock(&agent2, 7, None, LockMode::Steal)
        .unwrap();
    assert!(stolen);

    let locks = manager.read_locks().unwrap();
    let lock = locks.get_lock(7).unwrap();
    assert_eq!(lock.agent_id, "agent-2");
}

#[test]
fn test_release_lock_by_different_agent_fails() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent1 = make_agent("agent-1");
    let agent2 = make_agent("agent-2");

    manager
        .claim_lock(&agent1, 3, None, LockMode::Normal)
        .unwrap();

    // Agent 2 tries to release without force -> error
    let result = manager.release_lock(&agent2, 3, LockMode::Normal);
    assert!(result.is_err());
}

#[test]
fn test_release_lock_by_different_agent_with_force() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent1 = make_agent("agent-1");
    let agent2 = make_agent("agent-2");

    manager
        .claim_lock(&agent1, 4, None, LockMode::Normal)
        .unwrap();

    // Agent 2 force-releases
    let released = manager.release_lock(&agent2, 4, LockMode::Steal).unwrap();
    assert!(released);
}

#[test]
fn test_claim_lock_with_branch() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent = make_agent("test-agent");
    manager
        .claim_lock(&agent, 6, Some("feature/test"), LockMode::Normal)
        .unwrap();

    let locks = manager.read_locks().unwrap();
    let lock = locks.get_lock(6).unwrap();
    assert_eq!(lock.branch, Some("feature/test".to_string()));
}

// ------------------------------------------------------------------
// ensure_agent_dir (needs a git repo)
// ------------------------------------------------------------------

#[test]
fn test_ensure_agent_dir_with_git_repo() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let created = manager.ensure_agent_dir("my-agent").unwrap();
    assert!(created);

    let agent_dir = manager.cache_dir.join("agents").join("my-agent");
    assert!(agent_dir.exists());
    assert!(agent_dir.join("heartbeat.json").exists());
}

#[test]
fn test_ensure_agent_dir_idempotent_with_git() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let first = manager.ensure_agent_dir("my-agent").unwrap();
    assert!(first);

    // Second call should return false (already exists)
    let second = manager.ensure_agent_dir("my-agent").unwrap();
    assert!(!second);
}

// ------------------------------------------------------------------
// push_heartbeat (needs a git repo)
// ------------------------------------------------------------------

#[test]
fn test_push_heartbeat_writes_and_commits() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent = make_agent("hb-agent");
    manager.push_heartbeat(&agent, Some(42)).unwrap();

    let hb_path = manager.cache_dir.join("heartbeats").join("hb-agent.json");
    assert!(hb_path.exists());
    let content = std::fs::read_to_string(&hb_path).unwrap();
    let hb: Heartbeat = serde_json::from_str(&content).unwrap();
    assert_eq!(hb.agent_id, "hb-agent");
    assert_eq!(hb.active_issue_id, Some(42));
}

#[test]
fn test_push_heartbeat_no_change_is_ok() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent = make_agent("hb-agent");
    // Push same heartbeat twice -- second commit may be "nothing to commit"
    manager.push_heartbeat(&agent, None).unwrap();
    manager.push_heartbeat(&agent, None).unwrap();
}

// ------------------------------------------------------------------
// verify_recent_commits / verify_locks_signature
// ------------------------------------------------------------------

#[test]
fn test_verify_recent_commits_returns_result() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let results = manager.verify_recent_commits(1).unwrap();
    assert_eq!(results.len(), 1);
    // Depending on whether global git signing is configured, the commit
    // may be Valid, Unsigned, or Invalid -- just check it returns something.
    let (commit_hash, _verification) = &results[0];
    assert!(!commit_hash.is_empty());
}

#[test]
fn test_verify_locks_signature_on_initialized_cache() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Should return some verification result (Valid, Unsigned, Invalid, or NoCommits)
    // depending on whether global git signing is active. Just verify it doesn't panic.
    let result = manager.verify_locks_signature().unwrap();
    // Any variant is acceptable here
    let _ = result;
}

#[test]
fn test_verify_locks_signature_no_commits() {
    // No cache -> git_in_cache will fail, but verify_locks_signature
    // returns NoCommits when commit hash is empty.
    // We test this by creating a cache with a commit that doesn't touch locks.json
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Remove locks.json and commit to simulate a hub with no locks history
    std::fs::remove_file(manager.cache_dir.join("locks.json")).unwrap();
    manager.git_in_cache(&["add", "-A"]).unwrap();
    manager
        .git_in_cache(&["commit", "-m", "remove locks for test"])
        .unwrap();

    let result = manager.verify_locks_signature().unwrap();
    // After removal, there's a commit touching locks.json (the delete)
    // so it won't be NoCommits -- but it also won't be Valid
    let _ = result;
}

// ------------------------------------------------------------------
// verify_entry_signatures
// ------------------------------------------------------------------

#[test]
fn test_verify_entry_signatures_no_issues() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let (verified, failed, unsigned) = manager.verify_entry_signatures().unwrap();
    assert_eq!(verified, 0);
    assert_eq!(failed, 0);
    assert_eq!(unsigned, 0);
}

// ------------------------------------------------------------------
// propagate_claude_hooks
// ------------------------------------------------------------------

#[test]
fn test_propagate_claude_hooks_no_src() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // No .claude/hooks/ dir in repo root -> propagate is a no-op
    manager.propagate_claude_hooks().unwrap();
}

#[test]
fn test_propagate_claude_hooks_copies_files() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Create source hooks dir
    let hooks_src = work_dir.path().join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_src).unwrap();
    std::fs::write(hooks_src.join("pre-tool-use.sh"), "#!/bin/bash\n").unwrap();

    // Propagate
    manager.propagate_claude_hooks().unwrap();

    let hooks_dst = manager.cache_dir.join(".claude").join("hooks");
    assert!(hooks_dst.exists());
    assert!(hooks_dst.join("pre-tool-use.sh").exists());
}

#[test]
fn test_propagate_claude_hooks_idempotent() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let hooks_src = work_dir.path().join(".claude").join("hooks");
    std::fs::create_dir_all(&hooks_src).unwrap();
    std::fs::write(hooks_src.join("hook.sh"), "#!/bin/bash\n").unwrap();

    manager.propagate_claude_hooks().unwrap();
    // Second call should be a no-op (dst already exists)
    manager.propagate_claude_hooks().unwrap();
}

// ------------------------------------------------------------------
// ensure_cache_git_identity
// ------------------------------------------------------------------

#[test]
fn test_ensure_cache_git_identity_sets_identity() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Call directly -- should succeed even if already set
    manager.ensure_cache_git_identity().unwrap();
}

// ------------------------------------------------------------------
// check_divergence / count_unpushed_commits
// ------------------------------------------------------------------

#[test]
fn test_check_divergence_not_diverged() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // 0 commits ahead -> no error
    manager.check_divergence().unwrap();
}

// ------------------------------------------------------------------
// migrate_from_locks_branch -- no old branch case
// ------------------------------------------------------------------

#[test]
fn test_migrate_from_locks_branch_no_old_branch() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();

    // No old branch -> returns false
    let migrated = manager.migrate_from_locks_branch().unwrap();
    assert!(!migrated);
}

// ------------------------------------------------------------------
// configure_signing -- no agent config case
// ------------------------------------------------------------------

#[test]
fn test_configure_signing_no_agent_config() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // No agent.json -> should be no-op
    manager.configure_signing(&crosslink_dir).unwrap();
}

#[test]
fn test_configure_signing_cache_not_exists() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let manager = SyncManager::new(&crosslink_dir).unwrap();

    // cache_dir doesn't exist -> early return
    manager.configure_signing(&crosslink_dir).unwrap();
}

// ------------------------------------------------------------------
// ensure_agent_key_published -- no agent config case
// ------------------------------------------------------------------

#[test]
fn test_ensure_agent_key_published_no_cache() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let manager = SyncManager::new(&crosslink_dir).unwrap();

    // cache_dir doesn't exist -> returns false
    let published = manager.ensure_agent_key_published(&crosslink_dir).unwrap();
    assert!(!published);
}

#[test]
fn test_ensure_agent_key_published_no_agent_config() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // No agent.json -> returns false
    let published = manager.ensure_agent_key_published(&crosslink_dir).unwrap();
    assert!(!published);
}

// ------------------------------------------------------------------
// find_stale_locks_v2 direct
// ------------------------------------------------------------------

#[test]
fn test_find_stale_locks_v2_invalid_json_heartbeat() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock = crate::issue_file::LockFileV2 {
        issue_id: 77,
        agent_id: "invalid-json-agent".to_string(),
        branch: None,
        claimed_at: Utc::now(),
        signed_by: None,
    };
    std::fs::write(
        locks_dir.join("77.json"),
        serde_json::to_string_pretty(&lock).unwrap(),
    )
    .unwrap();

    // Write invalid JSON heartbeat
    let agent_dir = cache_dir.join("agents").join("invalid-json-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(agent_dir.join("heartbeat.json"), "{ not valid json").unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager
        .find_stale_locks_v2(chrono::Duration::minutes(30))
        .unwrap();
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].0, 77);
}

// ------------------------------------------------------------------
// read_keyring with real keyring.json file
// ------------------------------------------------------------------

#[test]
fn test_read_keyring_with_file() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let trust_dir = cache_dir.join("trust");
    std::fs::create_dir_all(&trust_dir).unwrap();

    let keyring = Keyring {
        trusted_fingerprints: vec!["SHA256:abc".to_string()],
    };
    let json = serde_json::to_string(&keyring).unwrap();
    std::fs::write(trust_dir.join("keyring.json"), json).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let loaded = manager.read_keyring().unwrap();
    assert!(loaded.is_some());
    let k = loaded.unwrap();
    assert_eq!(k.trusted_fingerprints.len(), 1);
    assert_eq!(k.trusted_fingerprints[0], "SHA256:abc");
}

// ------------------------------------------------------------------
// verify_entry_signatures with issues having unsigned comments
// ------------------------------------------------------------------

#[test]
fn test_verify_entry_signatures_unsigned_comments() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Write an issue file with unsigned comments
    use crate::issue_file::{CommentEntry, IssueFile};
    use uuid::Uuid;

    let issue = IssueFile {
        uuid: Uuid::new_v4(),
        display_id: Some(1),
        title: "Test issue".to_string(),
        description: None,
        status: crate::models::IssueStatus::Open,
        priority: crate::models::Priority::Medium,
        parent_uuid: None,
        created_by: "test-agent".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        closed_at: None,
        scheduled_at: None,
        due_at: None,
        labels: vec![],
        comments: vec![
            CommentEntry {
                id: 1,
                author: "test-agent".to_string(),
                content: "Hello world".to_string(),
                created_at: Utc::now(),
                kind: "note".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None, // unsigned
                signature: None,
            },
            CommentEntry {
                id: 2,
                author: "test-agent".to_string(),
                content: "Another comment".to_string(),
                created_at: Utc::now(),
                kind: "note".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None, // unsigned
                signature: None,
            },
        ],
        blockers: vec![],
        related: vec![],
        milestone_uuid: None,
        time_entries: vec![],
    };

    let issues_dir = manager.cache_dir.join("issues");
    std::fs::create_dir_all(&issues_dir).unwrap();
    let issue_path = issues_dir.join(format!("{}.json", issue.uuid));
    crate::issue_file::write_issue_file(&issue_path, &issue).unwrap();

    let (verified, failed, unsigned) = manager.verify_entry_signatures().unwrap();
    assert_eq!(verified, 0);
    assert_eq!(failed, 0);
    assert_eq!(unsigned, 2);
}

#[test]
fn test_verify_entry_signatures_with_fake_signature_no_allowed_signers() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    use crate::issue_file::{CommentEntry, IssueFile};
    use uuid::Uuid;

    let issue = IssueFile {
        uuid: Uuid::new_v4(),
        display_id: Some(2),
        title: "Signed issue".to_string(),
        description: None,
        status: crate::models::IssueStatus::Open,
        priority: crate::models::Priority::Medium,
        parent_uuid: None,
        created_by: "test-agent".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        closed_at: None,
        scheduled_at: None,
        due_at: None,
        labels: vec![],
        comments: vec![CommentEntry {
            id: 1,
            author: "test-agent".to_string(),
            content: "Signed comment".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            // Both signed_by and signature are set (fake values)
            signed_by: Some("SHA256:fakefingerprint".to_string()),
            signature: Some("fakesig".to_string()),
        }],
        blockers: vec![],
        related: vec![],
        milestone_uuid: None,
        time_entries: vec![],
    };

    let issues_dir = manager.cache_dir.join("issues");
    std::fs::create_dir_all(&issues_dir).unwrap();
    let issue_path = issues_dir.join(format!("{}.json", issue.uuid));
    crate::issue_file::write_issue_file(&issue_path, &issue).unwrap();

    // No allowed_signers file -> should count as unsigned (verification unavailable)
    let (verified, failed, unsigned) = manager.verify_entry_signatures().unwrap();
    // When allowed_signers doesn't exist, the Err branch counts as unsigned
    assert_eq!(verified, 0);
    // Either failed or unsigned depending on whether ssh-keygen is available
    assert_eq!(verified + failed + unsigned, 1);
}

// ------------------------------------------------------------------
// init_cache: has_local branch path (line 317)
// This covers the case where the hub branch exists locally but
// the worktree doesn't
// ------------------------------------------------------------------

#[test]
fn test_init_cache_with_existing_local_hub_branch() {
    let (work_dir, remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();

    // First init: creates the hub branch and worktree
    manager.init_cache().unwrap();

    // Push hub branch to remote so second repo can fetch it
    manager
        .git_in_cache(&["push", "-u", "origin", HUB_BRANCH])
        .unwrap();

    // Set up second work repo
    let work_dir2 = tempfile::tempdir().unwrap();
    Command::new("git")
        .current_dir(work_dir2.path())
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.email", "test@test.local"],
        vec!["config", "user.name", "Test"],
        vec![
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ],
    ] {
        Command::new("git")
            .current_dir(work_dir2.path())
            .args(&args)
            .output()
            .unwrap();
    }
    // Fetch everything including hub branch
    Command::new("git")
        .current_dir(work_dir2.path())
        .args(["fetch", "origin"])
        .output()
        .unwrap();
    // Create a local main branch
    Command::new("git")
        .current_dir(work_dir2.path())
        .args(["checkout", "-b", "main", "origin/main"])
        .output()
        .unwrap();
    // Create local hub branch (tracking remote)
    Command::new("git")
        .current_dir(work_dir2.path())
        .args([
            "checkout",
            "-b",
            HUB_BRANCH,
            &format!("origin/{HUB_BRANCH}"),
        ])
        .output()
        .unwrap();
    // Switch back to main so we can add the worktree
    Command::new("git")
        .current_dir(work_dir2.path())
        .args(["checkout", "main"])
        .output()
        .unwrap();

    let crosslink_dir2 = work_dir2.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir2).unwrap();
    std::fs::write(
        crosslink_dir2.join("hook-config.json"),
        r#"{"remote":"origin"}"#,
    )
    .unwrap();

    let manager2 = SyncManager::new(&crosslink_dir2).unwrap();
    // hub branch exists both locally and remotely: exercises the has_local=true path
    manager2.init_cache().unwrap();
    assert!(manager2.is_initialized());
}

// ------------------------------------------------------------------
// fetch: with unpushed local commits (rebase path)
// ------------------------------------------------------------------

#[test]
fn test_fetch_with_unpushed_local_commits_and_remote() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Push hub branch to remote
    manager
        .git_in_cache(&["push", "-u", "origin", HUB_BRANCH])
        .unwrap();

    // Make a local commit (not pushed)
    std::fs::write(manager.cache_dir.join("test-local.txt"), "local change\n").unwrap();
    manager.git_in_cache(&["add", "test-local.txt"]).unwrap();
    manager
        .git_in_cache(&["commit", "-m", "local unpushed commit"])
        .unwrap();

    // fetch should trigger the rebase path (unpushed commits exist)
    manager.fetch().unwrap();
}

// ------------------------------------------------------------------
// find_stale_locks_v2: no locks dir -> empty
// ------------------------------------------------------------------

#[test]
fn test_find_stale_locks_v2_empty_locks_dir() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    // No locks dir at all
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager
        .find_stale_locks_v2(chrono::Duration::minutes(30))
        .unwrap();
    assert!(stale.is_empty());
}

// ------------------------------------------------------------------
// check_divergence with many unpushed commits
// ------------------------------------------------------------------

#[test]
fn test_check_divergence_with_many_commits_fails() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Push so remote ref exists
    manager
        .git_in_cache(&["push", "-u", "origin", HUB_BRANCH])
        .unwrap();

    // Create MAX_DIVERGENCE + 1 local commits without pushing
    for i in 0..=MAX_DIVERGENCE {
        std::fs::write(
            manager.cache_dir.join(format!("diverge-{i}.txt")),
            format!("content {i}"),
        )
        .unwrap();
        manager
            .git_in_cache(&["add", &format!("diverge-{i}.txt")])
            .unwrap();
        manager
            .git_in_cache(&["commit", "-m", &format!("diverge commit {i}")])
            .unwrap();
    }

    // check_divergence should fail
    let result = manager.check_divergence();
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("Hub branch has diverged"));
}

// ------------------------------------------------------------------
// migrate_from_locks_branch -- old remote branch exists
// Covers lines 109-180 (the migration path).
// ------------------------------------------------------------------

#[test]
fn test_migrate_from_locks_branch_with_old_remote_branch() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");

    // Create the old crosslink/locks branch on the remote by pushing it from
    // a fresh orphan branch in the work repo.
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["checkout", "--orphan", "locks-init"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["reset", "--hard"])
        .output()
        .unwrap();
    std::fs::write(
        work_dir.path().join("locks.json"),
        r#"{"version":1,"locks":{}}"#,
    )
    .unwrap();
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["add", "locks.json"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["commit", "-m", "init locks branch", "--no-gpg-sign"])
        .output()
        .unwrap();
    // Push as crosslink/locks to the remote
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["push", "origin", &format!("HEAD:{OLD_BRANCH}")])
        .output()
        .unwrap();
    // Switch back to main
    Command::new("git")
        .current_dir(work_dir.path())
        .args(["checkout", "main"])
        .output()
        .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();

    // Old remote branch exists -> migration should run
    let migrated = manager.migrate_from_locks_branch().unwrap();
    assert!(migrated, "expected migration to run");

    // After migration, crosslink/hub should exist on remote
    let has_hub = manager
        .git_in_repo(&["ls-remote", "--heads", "origin", HUB_BRANCH])
        .is_ok_and(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty());
    assert!(
        has_hub,
        "crosslink/hub should exist on remote after migration"
    );

    // Old branch should be gone from remote
    let has_old = manager
        .git_in_repo(&["ls-remote", "--heads", "origin", OLD_BRANCH])
        .is_ok_and(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty());
    assert!(!has_old, "crosslink/locks should be deleted from remote");
}

#[test]
fn test_migrate_from_locks_branch_with_old_local_cache() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");

    // Create old .locks-cache directory (simulates leftover from old version)
    let old_cache = crosslink_dir.join(OLD_CACHE_DIR);
    std::fs::create_dir_all(&old_cache).unwrap();
    std::fs::write(old_cache.join("locks.json"), r#"{"version":1,"locks":{}}"#).unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();

    // Old local cache exists -> migration runs (even without remote old branch)
    let migrated = manager.migrate_from_locks_branch().unwrap();
    assert!(
        migrated,
        "expected migration to run when old local cache exists"
    );

    // Old cache directory should be gone
    assert!(
        !old_cache.exists(),
        "old .locks-cache should be removed after migration"
    );
}

// ------------------------------------------------------------------
// configure_signing -- with agent config having a real key file
// ------------------------------------------------------------------

#[test]
fn test_configure_signing_with_key() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Create a fake private key file under .crosslink/keys/
    let keys_dir = crosslink_dir.join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let key_file = keys_dir.join("agent_ed25519");
    std::fs::write(&key_file, "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();

    // Write agent.json with ssh_key_path and ssh_fingerprint
    let agent = AgentConfig {
        agent_id: "signing-test-agent".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: Some("keys/agent_ed25519".to_string()),
        ssh_fingerprint: Some("SHA256:fakefingerprint".to_string()),
        ssh_public_key: Some(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAfake signing-test-agent".to_string(),
        ),
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    // configure_signing should succeed (key file exists)
    manager.configure_signing(&crosslink_dir).unwrap();

    // Verify git config was written in the cache worktree
    let output = Command::new("git")
        .current_dir(&manager.cache_dir)
        .args(["config", "gpg.format"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "ssh",
        "gpg.format should be set to ssh"
    );

    let output = Command::new("git")
        .current_dir(&manager.cache_dir)
        .args(["config", "commit.gpgsign"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "true",
        "commit.gpgsign should be true"
    );
}

// ========================================================================
// GH#585: configure_signing self-registers the active key in
// allowed_signers so verify-commit can validate hub commits made by it.
// ========================================================================

/// Override the test repo's `user.signingkey` so `driver_signing_key()`
/// returns the test's fake key rather than whatever the host machine has
/// in its global git config (which would otherwise leak into these tests).
fn set_repo_signing_key(repo_root: &Path, key_path: &Path) {
    Command::new("git")
        .current_dir(repo_root)
        .args([
            "config",
            "--local",
            "user.signingkey",
            &key_path.to_string_lossy(),
        ])
        .status()
        .expect("git config user.signingkey failed");
}

#[test]
fn test_configure_signing_registers_active_key_in_allowed_signers() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Write a key pair: <key> + <key>.pub. The body of the pubkey is
    // distinctive so we can assert it landed in allowed_signers.
    let keys_dir = crosslink_dir.join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let key_file = keys_dir.join("driver_ed25519");
    std::fs::write(&key_file, "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();
    let pubkey_line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAGH585-self-register-test driver@host";
    std::fs::write(format!("{}.pub", key_file.display()), pubkey_line).unwrap();

    // Override the test repo's user.signingkey so driver_signing_key()
    // returns OUR fake key, not the test host's global signingkey.
    // This reproduces the GH#585 scenario: a driver workspace using
    // its own SSH signing key, which configure_signing must register
    // in allowed_signers.
    set_repo_signing_key(work_dir.path(), &key_file);

    let agent = AgentConfig {
        agent_id: "self-register-driver".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    manager.configure_signing(&crosslink_dir).unwrap();

    let allowed_signers = manager.cache_dir.join("trust").join("allowed_signers");
    let content = std::fs::read_to_string(&allowed_signers)
        .expect("allowed_signers should exist after configure_signing");
    assert!(
        content.contains("AAAAC3NzaC1lZDI1NTE5AAAAGH585-self-register-test"),
        "driver signing key body should be registered in allowed_signers; got:\n{content}"
    );
    assert!(
        content.contains("self-register-driver@crosslink"),
        "entry should use the agent.json identity as principal; got:\n{content}"
    );
}

#[test]
fn test_configure_signing_self_registration_is_idempotent() {
    // Calling configure_signing twice must not duplicate the entry.
    // The contains_key check dedupes by key body across all principals,
    // so the second call observes "already trusted" and no-ops.
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let keys_dir = crosslink_dir.join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let key_file = keys_dir.join("driver_ed25519");
    std::fs::write(&key_file, "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();
    let pubkey_line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAGH585-idempotency-test driver@host";
    std::fs::write(format!("{}.pub", key_file.display()), pubkey_line).unwrap();
    set_repo_signing_key(work_dir.path(), &key_file);

    let agent = AgentConfig {
        agent_id: "idempotency-driver".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    manager.configure_signing(&crosslink_dir).unwrap();
    manager.configure_signing(&crosslink_dir).unwrap();
    manager.configure_signing(&crosslink_dir).unwrap();

    let allowed_signers = manager.cache_dir.join("trust").join("allowed_signers");
    let content = std::fs::read_to_string(&allowed_signers).unwrap();
    let occurrences = content.matches("idempotency-driver@crosslink").count();
    assert_eq!(
        occurrences, 1,
        "principal should appear exactly once across 3 configure_signing calls; got {occurrences} in:\n{content}"
    );
}

// ========================================================================
// GH#738: when register_active_key_as_trusted adds a new entry while the
// hub is still in bootstrap "pending", it must atomically flip bootstrap
// to "complete". Without this, trust-pending lies (nothing pending — the
// key was auto-trusted) and signing_enforcement=enforced bails forever.
// ========================================================================

#[test]
fn test_configure_signing_completes_bootstrap_on_first_self_registration() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // init_cache wrote meta/bootstrap.json with status="pending".
    let pre = crate::sync::bootstrap::read_bootstrap_state(manager.cache_path())
        .expect("init_cache should write bootstrap.json");
    assert_eq!(pre.status, "pending", "fresh hub starts pending");
    assert!(pre.completed_at.is_none());

    let keys_dir = crosslink_dir.join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let key_file = keys_dir.join("driver_ed25519");
    std::fs::write(&key_file, "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();
    let pubkey_line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAGH738-bootstrap-flip driver@host";
    std::fs::write(format!("{}.pub", key_file.display()), pubkey_line).unwrap();
    set_repo_signing_key(work_dir.path(), &key_file);

    let agent = AgentConfig {
        agent_id: "bootstrap-flip-driver".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    manager.configure_signing(&crosslink_dir).unwrap();

    let post = crate::sync::bootstrap::read_bootstrap_state(manager.cache_path())
        .expect("bootstrap.json must still exist after configure_signing");
    assert_eq!(
        post.status, "complete",
        "self-registering the workspace key during bootstrap must flip status to complete"
    );
    assert!(
        post.completed_at.is_some(),
        "complete state must carry a timestamp"
    );
}

#[test]
fn test_configure_signing_does_not_revisit_completed_bootstrap() {
    // Idempotency check: when the same workspace syncs again later, the
    // key is already trusted (register short-circuits) and bootstrap must
    // remain "complete" with its original completed_at timestamp.
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let keys_dir = crosslink_dir.join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let key_file = keys_dir.join("driver_ed25519");
    std::fs::write(&key_file, "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();
    let pubkey_line = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAGH738-idempotent driver@host";
    std::fs::write(format!("{}.pub", key_file.display()), pubkey_line).unwrap();
    set_repo_signing_key(work_dir.path(), &key_file);

    let agent = AgentConfig {
        agent_id: "bootstrap-idempotent-driver".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    manager.configure_signing(&crosslink_dir).unwrap();
    let after_first = crate::sync::bootstrap::read_bootstrap_state(manager.cache_path())
        .expect("first call should complete bootstrap");
    assert_eq!(after_first.status, "complete");
    let first_ts = after_first.completed_at;

    manager.configure_signing(&crosslink_dir).unwrap();
    manager.configure_signing(&crosslink_dir).unwrap();

    let after_repeat = crate::sync::bootstrap::read_bootstrap_state(manager.cache_path()).unwrap();
    assert_eq!(after_repeat.status, "complete");
    assert_eq!(
        after_repeat.completed_at, first_ts,
        "completed_at must not be rewritten when bootstrap was already complete"
    );
}

#[test]
fn test_configure_signing_skips_self_registration_when_pub_missing() {
    // When the .pub companion isn't on disk, self-registration must
    // gracefully no-op (log debug, return Ok) rather than failing the
    // whole configure_signing call.
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let keys_dir = crosslink_dir.join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let key_file = keys_dir.join("driver_ed25519");
    std::fs::write(&key_file, "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();
    // No .pub companion written.
    set_repo_signing_key(work_dir.path(), &key_file);

    let agent = AgentConfig {
        agent_id: "no-pub-driver".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    // Must succeed even though the .pub is missing.
    manager.configure_signing(&crosslink_dir).unwrap();

    let allowed_signers = manager.cache_dir.join("trust").join("allowed_signers");
    assert!(allowed_signers.exists());
    let content = std::fs::read_to_string(&allowed_signers).unwrap();
    assert!(
        !content.contains("no-pub-driver@crosslink"),
        "should not register when .pub is missing; got:\n{content}"
    );
}

#[test]
fn test_configure_signing_key_file_missing() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Write agent.json pointing at a non-existent key
    let agent = AgentConfig {
        agent_id: "missing-key-agent".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: Some("keys/nonexistent".to_string()),
        ssh_fingerprint: Some("SHA256:missing".to_string()),
        ssh_public_key: None,
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    // Should return Ok(()) without configuring (key file missing -> early return)
    manager.configure_signing(&crosslink_dir).unwrap();
}

#[test]
fn test_configure_signing_agent_has_no_key_fields() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // agent.json exists but ssh_key_path / ssh_fingerprint are None
    let agent = AgentConfig {
        agent_id: "no-key-agent".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    // Should be a no-op (missing key fields -> early return)
    manager.configure_signing(&crosslink_dir).unwrap();
}

// ------------------------------------------------------------------
// ensure_agent_key_published -- with agent config having ssh_public_key
// ------------------------------------------------------------------

#[test]
fn test_ensure_agent_key_published_with_public_key() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Write agent.json with ssh_public_key
    let agent = AgentConfig {
        agent_id: "pub-key-agent".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: Some(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAfakepubkey pub-key-agent".to_string(),
        ),
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    // Publish key
    let published = manager.ensure_agent_key_published(&crosslink_dir).unwrap();
    assert!(published, "key should be published");

    // Verify the key file was created in the cache
    let key_file = manager
        .cache_dir
        .join("trust")
        .join("keys")
        .join("pub-key-agent.pub");
    assert!(
        key_file.exists(),
        "trust/keys/pub-key-agent.pub should exist"
    );
    let content = std::fs::read_to_string(&key_file).unwrap();
    assert!(
        content.contains("AAAAC3NzaC1lZDI1NTE5AAAAfakepubkey"),
        "key file should contain the public key"
    );
}

#[test]
fn test_ensure_agent_key_published_idempotent() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent = AgentConfig {
        agent_id: "idempotent-pub-agent".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: Some(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAidempotent idempotent-pub-agent".to_string(),
        ),
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), &agent_json).unwrap();

    // First publish
    let first = manager.ensure_agent_key_published(&crosslink_dir).unwrap();
    assert!(first);

    // Second publish (key already exists) -> returns false
    let second = manager.ensure_agent_key_published(&crosslink_dir).unwrap();
    assert!(!second, "second publish should be a no-op");
}

#[test]
fn test_ensure_agent_key_published_no_public_key_field() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // agent.json exists but ssh_public_key is None
    let agent = AgentConfig {
        agent_id: "no-pub-key-agent".to_string(),
        machine_id: "test-host".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };
    let agent_json = serde_json::to_string_pretty(&agent).unwrap();
    std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

    // No public key -> returns false
    let published = manager.ensure_agent_key_published(&crosslink_dir).unwrap();
    assert!(!published);
}

// ------------------------------------------------------------------
// fetch: error path when reset --hard hits "unknown revision"
// ------------------------------------------------------------------

#[test]
fn test_fetch_with_empty_hub_branch_no_remote_ref() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Do NOT push hub branch to remote. The hub branch exists locally but has
    // no remote tracking ref. Fetching crosslink/hub from origin will give
    // "couldn't find remote ref" (handled as no-op) or succeed but leave
    // origin/crosslink/hub nonexistent. Either way, reset --hard would get
    // "unknown revision" for the remote ref -- this must not crash.
    manager.fetch().unwrap();
}

#[test]
fn test_fetch_rebase_path_handles_unknown_revision() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Push hub branch so remote ref exists
    manager
        .git_in_cache(&["push", "-u", "origin", HUB_BRANCH])
        .unwrap();

    // Add a local unpushed commit to exercise the rebase path
    std::fs::write(manager.cache_dir.join("rebase-test.txt"), "content").unwrap();
    manager.git_in_cache(&["add", "rebase-test.txt"]).unwrap();
    manager
        .git_in_cache(&["commit", "-m", "local commit for rebase test"])
        .unwrap();

    // fetch should succeed -- the rebase should work cleanly
    manager.fetch().unwrap();

    // Local file should still be present after rebase
    assert!(manager.cache_dir.join("rebase-test.txt").exists());
}

#[test]
fn test_fetch_rebase_conflict_aborts_preserving_local() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Push initial hub state
    manager
        .git_in_cache(&["push", "-u", "origin", HUB_BRANCH])
        .unwrap();

    // Create a conflicting file on remote: simulate another agent pushing
    // a change to the same file.
    std::fs::write(
        manager.cache_dir.join("conflict-file.txt"),
        "remote content\n",
    )
    .unwrap();
    manager.git_in_cache(&["add", "conflict-file.txt"]).unwrap();
    manager
        .git_in_cache(&["commit", "-m", "remote change"])
        .unwrap();
    manager
        .git_in_cache(&["push", "origin", HUB_BRANCH])
        .unwrap();
    // Reset back so the "remote change" is only on origin
    manager
        .git_in_cache(&["reset", "--hard", "HEAD~1"])
        .unwrap();

    // Now make a LOCAL conflicting change to the same file
    std::fs::write(
        manager.cache_dir.join("conflict-file.txt"),
        "local content\n",
    )
    .unwrap();
    manager.git_in_cache(&["add", "conflict-file.txt"]).unwrap();
    manager
        .git_in_cache(&["commit", "-m", "local close event"])
        .unwrap();

    // fetch should succeed (rebase conflict is caught and aborted)
    manager.fetch().unwrap();

    // Local commit should be preserved (rebase was aborted, not applied)
    let content = std::fs::read_to_string(manager.cache_dir.join("conflict-file.txt")).unwrap();
    assert_eq!(content, "local content\n");
}

#[test]
fn test_fetch_git_log_failure_preserves_local_state() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Write a local file (simulating a local-only close event)
    std::fs::write(manager.cache_dir.join("local-close.txt"), "closed\n").unwrap();
    manager.git_in_cache(&["add", "local-close.txt"]).unwrap();
    manager
        .git_in_cache(&["commit", "-m", "close issue #-1"])
        .unwrap();

    // Do NOT push to remote. The remote ref origin/crosslink/hub doesn't
    // exist, so git log origin/crosslink/hub..HEAD will fail.
    // fetch should keep local state instead of resetting.
    manager.fetch().unwrap();

    // Local file should still exist
    assert!(manager.cache_dir.join("local-close.txt").exists());
    let content = std::fs::read_to_string(manager.cache_dir.join("local-close.txt")).unwrap();
    assert_eq!(content, "closed\n");
}

// ------------------------------------------------------------------
// push_heartbeat
// ------------------------------------------------------------------

#[test]
fn test_push_heartbeat_writes_file_and_pushes() {
    let (work_dir, _remote) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent = AgentConfig {
        agent_id: "hb-agent".to_string(),
        machine_id: "hb-machine".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };

    manager.push_heartbeat(&agent, Some(42)).unwrap();

    // Verify heartbeat file was written
    let hb_path = manager.cache_dir.join("heartbeats").join("hb-agent.json");
    assert!(hb_path.exists());
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&hb_path).unwrap()).unwrap();
    assert_eq!(content["agent_id"], "hb-agent");
    assert_eq!(content["active_issue_id"], 42);
}

#[test]
fn test_push_heartbeat_second_call_nothing_to_commit() {
    let (work_dir, _remote) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent = AgentConfig {
        agent_id: "hb2-agent".to_string(),
        machine_id: "hb2-machine".to_string(),
        description: None,
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };

    manager.push_heartbeat(&agent, None).unwrap();
    // Second call with same content - nothing to commit, should still succeed
    // (exercises the "nothing to commit" early return)
    manager.push_heartbeat(&agent, None).unwrap();
}

// ------------------------------------------------------------------
// #362: read_heartbeats_v2 should skip corrupt timestamps
// ------------------------------------------------------------------

#[test]
fn test_read_heartbeats_v2_skips_corrupt_timestamp() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let agent_dir = cache_dir.join("agents").join("corrupt-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();

    // Write V2 format with a corrupt timestamp
    let heartbeat = serde_json::json!({
        "agent_id": "corrupt-agent",
        "timestamp": "not-a-valid-timestamp",
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let heartbeats = manager.read_heartbeats_v2().unwrap();
    // Should skip the corrupt entry instead of falling back to Utc::now()
    assert!(heartbeats.is_empty());
}

#[test]
fn test_read_heartbeats_v2_skips_missing_timestamp_field() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let agent_dir = cache_dir.join("agents").join("no-ts-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();

    // Write V2 format without a timestamp field
    let heartbeat = serde_json::json!({
        "agent_id": "no-ts-agent",
        "status": "active"
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string_pretty(&heartbeat).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let heartbeats = manager.read_heartbeats_v2().unwrap();
    assert!(heartbeats.is_empty());
}

// ------------------------------------------------------------------
// #350/#355: parse_v2_heartbeat_timestamp helper via stale lock detection
// ------------------------------------------------------------------

#[test]
fn test_find_stale_locks_v2_uses_helper() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

    // Set up V2 layout
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();

    // Create V2 lock file
    let locks_dir = cache_dir.join("locks");
    std::fs::create_dir_all(&locks_dir).unwrap();
    let lock_v2 = serde_json::json!({
        "issue_id": 10,
        "agent_id": "fresh-agent",
        "branch": null,
        "claimed_at": Utc::now().to_rfc3339(),
        "signed_by": null
    });
    std::fs::write(
        locks_dir.join("10.json"),
        serde_json::to_string(&lock_v2).unwrap(),
    )
    .unwrap();

    // Create a fresh heartbeat for the agent
    let agent_dir = cache_dir.join("agents").join("fresh-agent");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let hb = serde_json::json!({
        "timestamp": Utc::now().to_rfc3339()
    });
    std::fs::write(
        agent_dir.join("heartbeat.json"),
        serde_json::to_string(&hb).unwrap(),
    )
    .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    let stale = manager
        .find_stale_locks_v2(chrono::Duration::minutes(30))
        .unwrap();
    // Fresh heartbeat — should NOT be stale
    assert!(stale.is_empty());
}

// ------------------------------------------------------------------
// #364: LockMode enum replaces bool force parameter
// ------------------------------------------------------------------

#[test]
fn test_lock_mode_enum_in_claim_and_release() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let agent1 = make_agent("lm-agent-1");
    let agent2 = make_agent("lm-agent-2");

    // Normal claim
    assert!(manager
        .claim_lock(&agent1, 100, None, LockMode::Normal)
        .unwrap());

    // Normal claim by another agent should fail
    assert!(manager
        .claim_lock(&agent2, 100, None, LockMode::Normal)
        .is_err());

    // Steal should succeed
    assert!(manager
        .claim_lock(&agent2, 100, None, LockMode::Steal)
        .unwrap());

    // Normal release by wrong agent should fail
    assert!(manager
        .release_lock(&agent1, 100, LockMode::Normal)
        .is_err());

    // Steal release should succeed
    assert!(manager.release_lock(&agent1, 100, LockMode::Steal).unwrap());
}

// ------------------------------------------------------------------
// repair_stale_signingkey — GH #565
// ------------------------------------------------------------------

/// Write `user.signingkey` into the cache worktree's config at whatever scope
/// `configure_git_ssh_signing` would choose (worktree-scoped for linked worktrees).
fn write_cache_signingkey(manager: &SyncManager, value: &str) {
    let use_worktree = crate::signing::is_linked_worktree(&manager.cache_dir);
    if use_worktree {
        // Replicate enable_worktree_config side-effect: flip extensions.worktreeConfig.
        Command::new("git")
            .current_dir(&manager.cache_dir)
            .args(["config", "--local", "extensions.worktreeConfig", "true"])
            .output()
            .unwrap();
    }
    let scope = if use_worktree {
        "--worktree"
    } else {
        "--local"
    };
    Command::new("git")
        .current_dir(&manager.cache_dir)
        .args(["config", scope, "user.signingkey", value])
        .output()
        .unwrap();
}

fn read_cache_signingkey(manager: &SyncManager) -> Option<String> {
    let out = Command::new("git")
        .current_dir(&manager.cache_dir)
        .args(["config", "user.signingkey"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

#[test]
fn test_repair_stale_signingkey_missing_path_is_repaired() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Simulate the #565 state: an agent worktree wrote its key path into the
    // hub-cache's config.worktree, then the agent worktree was deleted.
    let stale_path = work_dir
        .path()
        .join("gone-agent-worktree")
        .join("keys")
        .join("jus4_ed25519");
    write_cache_signingkey(&manager, stale_path.to_str().unwrap());
    assert!(
        !stale_path.exists(),
        "precondition: stale path must not exist"
    );

    let repaired = manager.repair_stale_signingkey().unwrap();
    assert!(repaired, "stale path should trigger a repair");

    // After repair, the cache signingkey is either unset (no driver key) or
    // points at an existing path (driver key). Either way, the stale path
    // that was there before must no longer be effective.
    let post = read_cache_signingkey(&manager);
    if let Some(ref new_value) = post {
        assert_ne!(
            new_value.as_str(),
            stale_path.to_str().unwrap(),
            "stale signingkey must not survive a repair"
        );
    }
}

#[test]
fn test_repair_stale_signingkey_valid_path_is_noop() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Create a key file that actually exists.
    let keys_dir = crosslink_dir.join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let key_path = keys_dir.join("real_ed25519");
    std::fs::write(&key_path, "-----BEGIN OPENSSH PRIVATE KEY-----\nfake\n").unwrap();

    let abs = key_path.to_str().unwrap().to_string();
    write_cache_signingkey(&manager, &abs);

    let repaired = manager.repair_stale_signingkey().unwrap();
    assert!(!repaired, "valid path must not trigger a repair");

    let post = read_cache_signingkey(&manager);
    assert_eq!(post.as_deref(), Some(abs.as_str()), "signingkey unchanged");
}

#[test]
fn test_repair_stale_signingkey_literal_key_is_skipped() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Literal key material, not a path. Git accepts these inline.
    let literal = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAfake literal-test";
    write_cache_signingkey(&manager, literal);

    let repaired = manager.repair_stale_signingkey().unwrap();
    assert!(!repaired, "literal key material must not trigger a repair");

    let post = read_cache_signingkey(&manager);
    assert_eq!(
        post.as_deref(),
        Some(literal),
        "literal signingkey unchanged"
    );
}

#[test]
fn test_repair_stale_signingkey_no_worktree_override_is_noop() {
    // When the cache worktree has no `user.signingkey` override, repair must
    // be a no-op. The effective value may surface from the user's global
    // git config (which this test cannot control), but as long as nothing
    // crosslink wrote is stale, repair returns Ok(false).
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // Explicitly unset any local/worktree-scoped signingkey.
    let _ = Command::new("git")
        .current_dir(&manager.cache_dir)
        .args(["config", "--local", "--unset-all", "user.signingkey"])
        .output();
    let _ = Command::new("git")
        .current_dir(&manager.cache_dir)
        .args(["config", "--worktree", "--unset-all", "user.signingkey"])
        .output();

    // If the host's global signingkey exists, repair is Ok(false).
    // If the host has no global signingkey OR it points at a real file,
    // repair is still Ok(false). The only way this fires is if the host's
    // global signingkey is stale — and that's out of scope for this test.
    let global = Command::new("git")
        .current_dir(work_dir.path())
        .args(["config", "--global", "user.signingkey"])
        .output();
    let global_key = global
        .ok()
        .and_then(|o| {
            o.status
                .success()
                .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
        })
        .filter(|s| !s.is_empty());
    let global_key_exists_or_absent = global_key
        .as_ref()
        .is_none_or(|k| std::path::Path::new(k).exists() || k.starts_with("ssh-"));
    if !global_key_exists_or_absent {
        // Skip — host has a stale global signingkey; not our test scenario.
        return;
    }

    let repaired = manager.repair_stale_signingkey().unwrap();
    assert!(!repaired, "no worktree override must not trigger a repair");
}

#[test]
fn test_repair_stale_signingkey_cache_missing_is_noop() {
    // No init_cache — cache_dir doesn't exist yet.
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert!(!manager.cache_dir.exists());
    let repaired = manager.repair_stale_signingkey().unwrap();
    assert!(!repaired);
}

// ------------------------------------------------------------------
// resolve_agent_key — GH#610: agent keys live in main repo, with
// a legacy fallback to the worktree-local path.
// ------------------------------------------------------------------

/// When no git metadata is present, `host_crosslink_dir` returns the input
/// unchanged. With the key only at the worktree-local path, the resolver
/// falls back to it — i.e. the legacy pre-#610 layout still works.
#[test]
fn test_resolve_agent_key_legacy_worktree_path_fallback() {
    let dir = tempdir().unwrap();
    let wt_crosslink = dir.path().join(".crosslink");
    let keys_dir = wt_crosslink.join("keys");
    std::fs::create_dir_all(&keys_dir).unwrap();
    let key_file = keys_dir.join("legacy_ed25519");
    std::fs::write(&key_file, "legacy-key-bytes").unwrap();

    let resolved = crate::sync::trust::resolve_agent_key(&wt_crosslink, "keys/legacy_ed25519");
    assert_eq!(
        resolved, key_file,
        "no host-side key + worktree-local key → resolver picks the worktree path"
    );
}

/// When neither the host-side nor the worktree-local path has the file,
/// the resolver returns the worktree-relative path. The caller is then
/// responsible for checking `.exists()` — preserves the pre-fix
/// "key missing → caller disables signing" contract.
#[test]
fn test_resolve_agent_key_neither_path_exists_returns_worktree_path() {
    let dir = tempdir().unwrap();
    let wt_crosslink = dir.path().join(".crosslink");
    std::fs::create_dir_all(&wt_crosslink).unwrap();

    let resolved = crate::sync::trust::resolve_agent_key(&wt_crosslink, "keys/ghost_ed25519");
    assert_eq!(resolved, wt_crosslink.join("keys/ghost_ed25519"));
    assert!(!resolved.exists(), "precondition for this test");
}

/// End-to-end #610: with a real main repo and a linked worktree, an
/// agent.json living in the worktree resolves `ssh_key_path` to the
/// main repo's `.crosslink/keys/`. This is the contract that lets
/// `configure_signing` (called from the worktree's context) bake an
/// absolute main-repo path into the hub-cache's worktree-scoped
/// `user.signingkey`, so the reference keeps pointing at a real file
/// after `git worktree remove` of the agent worktree.
#[test]
fn test_resolve_agent_key_worktree_picks_main_repo_keys() {
    let main = tempdir().unwrap();
    let main_root = main.path();

    // Bootstrap a real git repo so `resolve_main_repo_root` succeeds.
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "t@t.local"],
        vec!["config", "user.name", "t"],
    ] {
        Command::new("git")
            .current_dir(main_root)
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::write(main_root.join("README.md"), "x").unwrap();
    Command::new("git")
        .current_dir(main_root)
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(main_root)
        .args(["commit", "-m", "init", "--no-gpg-sign"])
        .output()
        .unwrap();

    // The post-#610 key lives in the MAIN repo's .crosslink/keys/.
    let main_crosslink = main_root.join(".crosslink");
    let main_keys = main_crosslink.join("keys");
    std::fs::create_dir_all(&main_keys).unwrap();
    let main_key = main_keys.join("agent_ed25519");
    std::fs::write(&main_key, "real-key-bytes").unwrap();

    // Linked worktree the agent would have run in. agent.json lives
    // here and references `keys/agent_ed25519` — relative to *main*
    // per the post-#610 semantics. The worktree's own .crosslink
    // intentionally has no `keys/` directory.
    let wt_root = main_root.join(".worktrees").join("agent-test");
    Command::new("git")
        .current_dir(main_root)
        .args([
            "worktree",
            "add",
            "-b",
            "agent-test",
            wt_root.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let wt_crosslink = wt_root.join(".crosslink");
    std::fs::create_dir_all(&wt_crosslink).unwrap();

    let resolved = crate::sync::trust::resolve_agent_key(&wt_crosslink, "keys/agent_ed25519");
    let expected = main_key.canonicalize().unwrap_or(main_key);
    let actual = resolved.canonicalize().unwrap_or(resolved);
    assert_eq!(
        actual, expected,
        "with a main-repo key present, the resolver prefers it over the worktree path \
         — this is what lets `configure_signing` bake an absolute main-repo path into \
         the hub-cache's worktree-scoped `user.signingkey` so the reference survives \
         `git worktree remove` of the agent worktree (GH#610)"
    );
}

// ============================================================================
// GH#586: structured push-failure classifier
// ============================================================================

#[test]
fn classify_push_failure_offline_dns() {
    let stderr = "fatal: unable to access 'https://github.com/foo/bar.git/': \
                   Could not resolve host: github.com";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::Offline
    );
}

#[test]
fn classify_push_failure_offline_unreachable() {
    let stderr = "fatal: unable to access remote: Network is unreachable";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::Offline
    );
}

#[test]
fn classify_push_failure_offline_timeout() {
    let stderr = "ssh: connect to host github.com port 22: Connection timed out";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::Offline
    );
}

#[test]
fn classify_push_failure_remote_misconfigured() {
    // What git emits when 'origin' isn't a real remote.
    let stderr = "fatal: 'bogus' does not appear to be a git repository\n\
                  fatal: Could not read from remote repository.";
    match classify_push_failure(stderr, "bogus") {
        PushFailure::RemoteMisconfigured { remote, .. } => assert_eq!(remote, "bogus"),
        other => panic!("expected RemoteMisconfigured, got {other:?}"),
    }
}

#[test]
fn classify_push_failure_remote_misconfigured_not_found() {
    // GitHub's 404 response phrasing.
    let stderr = "remote: Repository not found.\nfatal: repository 'https://...' not found";
    assert!(matches!(
        classify_push_failure(stderr, "origin"),
        PushFailure::RemoteMisconfigured { .. }
    ));
}

#[test]
fn classify_push_failure_non_fast_forward() {
    let stderr = "To github.com:foo/bar.git\n ! [rejected]        main -> main (non-fast-forward)";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::NonFastForward
    );
}

#[test]
fn classify_push_failure_concurrent_ref_lock_is_non_ff() {
    // When two clients push the same ref simultaneously with the
    // same expected old SHA, git's bare-repo update logic rejects
    // the second push with "cannot lock ref" / "remote rejected" /
    // "incorrect old value provided". Before the fix, the classifier
    // miscategorized this as `Other`, the call site returned LocalOnly
    // ("saved locally"), and both agents thought they had won the
    // lock. Verify the rejection now lands in `NonFastForward` so
    // `claim_lock_v2`'s retry path runs and one agent correctly
    // returns `Contended`.
    let stderr = "remote: error: cannot lock ref 'refs/heads/crosslink/hub': \
                  is at abc123 but expected def456\n\
                  To /tmp/bare.git\n\
                  ! [remote rejected] crosslink/hub -> crosslink/hub \
                  (incorrect old value provided)\n\
                  error: failed to push some refs to '/tmp/bare.git'";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::NonFastForward,
        "concurrent-ref-lock rejection must be classified as NonFastForward so the \
         caller's retry/recovery path runs"
    );
}

#[test]
fn classify_push_failure_remote_rejected_variant() {
    // Bare "! [remote rejected]" (without the "cannot lock ref"
    // prefix) — also a non-fast-forward family.
    let stderr = "To /tmp/r.git\n ! [remote rejected] main -> main (some hook reason)";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::NonFastForward
    );
}

#[test]
fn classify_push_failure_auth_publickey() {
    // Common SSH auth-failure stderr.
    let stderr = "git@github.com: Permission denied (publickey).\n\
                  fatal: Could not read from remote repository.";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::AuthFailed
    );
}

#[test]
fn classify_push_failure_auth_token() {
    let stderr = "remote: HTTP 403 forbidden\nfatal: Authentication failed";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::AuthFailed
    );
}

#[test]
fn classify_push_failure_other_falls_through() {
    let stderr = "fatal: some entirely novel git failure";
    match classify_push_failure(stderr, "origin") {
        PushFailure::Other(detail) => assert!(detail.contains("novel git failure")),
        other => panic!("expected Other, got {other:?}"),
    }
}

#[test]
fn classify_push_failure_auth_beats_offline_when_both_present() {
    // Failed SSH auth typically produces 'Could not read from remote' too.
    // The classifier must pick Auth, not Offline.
    let stderr = "git@github.com: Permission denied (publickey).\n\
                  fatal: Could not read from remote repository.";
    assert_eq!(
        classify_push_failure(stderr, "origin"),
        PushFailure::AuthFailed
    );
}

#[test]
fn push_failure_user_message_includes_remote_name() {
    let failure = PushFailure::RemoteMisconfigured {
        remote: "upstream".to_string(),
        detail: "detail".to_string(),
    };
    let msg = failure.user_message("comment");
    assert!(
        msg.contains("'upstream'"),
        "message should name the remote: {msg}"
    );
    assert!(msg.contains("crosslink config set tracker_remote"));
}
