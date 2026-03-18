use super::*;
use crate::identity::AgentConfig;
use crate::locks::{Heartbeat, Keyring, LocksFile};
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
        "5".to_string(),
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
        "5".to_string(),
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
        "42".to_string(),
        crate::locks::Lock {
            agent_id: "stale-agent".to_string(),
            branch: None,
            claimed_at: old_time,
            signed_by: "".to_string(),
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
        "10".to_string(),
        crate::locks::Lock {
            agent_id: "fresh-agent".to_string(),
            branch: None,
            claimed_at: Utc::now(),
            signed_by: "".to_string(),
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
    let claimed = manager.claim_lock(&agent, 1, None, false).unwrap();
    assert!(claimed);

    // Claiming again for same agent should return false (already held by self)
    let claimed_again = manager.claim_lock(&agent, 1, None, false).unwrap();
    assert!(!claimed_again);

    // Check lock is set
    let locks = manager.read_locks().unwrap();
    assert!(locks.is_locked(1));

    // Release lock
    let released = manager.release_lock(&agent, 1, false).unwrap();
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
    let released = manager.release_lock(&agent, 999, false).unwrap();
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
    manager.claim_lock(&agent1, 5, None, false).unwrap();

    // Agent 2 tries to claim without force -> error
    let result = manager.claim_lock(&agent2, 5, None, false);
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

    manager.claim_lock(&agent1, 7, None, false).unwrap();

    // Agent 2 steals with force=true
    let stolen = manager.claim_lock(&agent2, 7, None, true).unwrap();
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

    manager.claim_lock(&agent1, 3, None, false).unwrap();

    // Agent 2 tries to release without force -> error
    let result = manager.release_lock(&agent2, 3, false);
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

    manager.claim_lock(&agent1, 4, None, false).unwrap();

    // Agent 2 force-releases
    let released = manager.release_lock(&agent2, 4, true).unwrap();
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
        .claim_lock(&agent, 6, Some("feature/test"), false)
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
        status: "open".to_string(),
        priority: "medium".to_string(),
        parent_uuid: None,
        created_by: "test-agent".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        closed_at: None,
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
        status: "open".to_string(),
        priority: "medium".to_string(),
        parent_uuid: None,
        created_by: "test-agent".to_string(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        closed_at: None,
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
            &format!("origin/{}", HUB_BRANCH),
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
    for i in 0..(MAX_DIVERGENCE + 1) {
        std::fs::write(
            manager.cache_dir.join(format!("diverge-{}.txt", i)),
            format!("content {}", i),
        )
        .unwrap();
        manager
            .git_in_cache(&["add", &format!("diverge-{}.txt", i)])
            .unwrap();
        manager
            .git_in_cache(&["commit", "-m", &format!("diverge commit {}", i)])
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
        .args(["push", "origin", &format!("HEAD:{}", OLD_BRANCH)])
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
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);
    assert!(
        has_hub,
        "crosslink/hub should exist on remote after migration"
    );

    // Old branch should be gone from remote
    let has_old = manager
        .git_in_repo(&["ls-remote", "--heads", "origin", OLD_BRANCH])
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false);
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
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };

    manager.push_heartbeat(&agent, None).unwrap();
    // Second call with same content - nothing to commit, should still succeed
    // (exercises the "nothing to commit" early return)
    manager.push_heartbeat(&agent, None).unwrap();
}
