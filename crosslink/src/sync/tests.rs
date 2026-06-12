use super::*;
use crate::identity::{AgentConfig, AgentRole};
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
fn test_read_locks_auto_frozen_v2_is_empty() {
    // 754b: the v2 lock READ path is gone. A non-v3 (frozen / pre-migration)
    // hub has no live lock state — `read_locks_auto` returns empty. Migration
    // reads locks from the compacted checkpoint, not from `locks/*.json`.
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    // Even with a V2 layout marker and a per-issue lock file present, the read
    // path no longer materializes them.
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&meta_dir).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, 2).unwrap();
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
    assert!(locks.locks.is_empty());
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

// ── GH#611: silent inference from git remotes ────────────────────

/// `git remote add` a name pointing at a placeholder URL on `repo`.
/// The URL value doesn't matter for `git remote` (the listing command),
/// so we just need a syntactically valid string.
fn add_git_remote(repo: &Path, name: &str) {
    let url = format!("https://example.invalid/{name}.git");
    let status = Command::new("git")
        .current_dir(repo)
        .args(["remote", "add", name, &url])
        .status()
        .expect("git remote add failed to spawn");
    assert!(status.success(), "git remote add {name} failed");
}

#[test]
fn test_read_tracker_remote_single_origin_no_warn() {
    // The single most common project shape: one git remote called "origin",
    // hook-config.json has no `tracker_remote` field. Before GH#611 this
    // path emitted a WARN once per process; after, it must be silent.
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    // hook-config.json from setup_sync_env only sets {"remote":"origin"},
    // intentionally NOT a tracker_remote field — so we drop into inference.
    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(remote, "origin");
}

#[test]
fn test_read_tracker_remote_single_non_origin_remote() {
    // One remote, not called "origin" — inferred verbatim.
    let dir = tempdir().unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    add_git_remote(dir.path(), "upstream");

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    // hook-config.json exists but has no tracker_remote field
    std::fs::write(crosslink_dir.join("hook-config.json"), "{}").unwrap();

    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(
        remote, "upstream",
        "single non-origin remote should be inferred as the tracker_remote"
    );
}

#[test]
fn test_read_tracker_remote_multi_remotes_prefers_origin() {
    // Multiple remotes including "origin" — origin wins regardless of order.
    let dir = tempdir().unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    // Add in non-alphabetical order to confirm origin wins on name, not order.
    add_git_remote(dir.path(), "zzz");
    add_git_remote(dir.path(), "origin");
    add_git_remote(dir.path(), "fork");

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(remote, "origin", "origin must win in multi-remote setups");
}

#[test]
fn test_read_tracker_remote_multi_remotes_no_origin_picks_first_alphabetical() {
    // Multiple remotes, none called origin → first alphabetically (deterministic).
    let dir = tempdir().unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    add_git_remote(dir.path(), "upstream");
    add_git_remote(dir.path(), "alice");
    add_git_remote(dir.path(), "bob");

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(
        remote, "alice",
        "with no origin and multiple remotes, picks the alphabetically first one"
    );
}

#[test]
fn test_read_tracker_remote_explicit_config_wins_over_inference() {
    // hook-config.json's explicit value beats whatever git remotes say.
    let dir = tempdir().unwrap();
    Command::new("git")
        .current_dir(dir.path())
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    add_git_remote(dir.path(), "origin");
    add_git_remote(dir.path(), "upstream");

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"tracker_remote":"upstream"}"#,
    )
    .unwrap();

    let remote = read_tracker_remote(&crosslink_dir);
    assert_eq!(
        remote, "upstream",
        "explicit hook-config.json value must take precedence over inference"
    );
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
// init_cache -- a fresh hub bootstraps directly into v3 (754b REQ-10)
// ------------------------------------------------------------------

#[test]
fn test_init_cache_bootstraps_v3() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();

    assert!(!manager.is_initialized());
    manager.init_cache().unwrap();
    assert!(manager.is_initialized());

    // Fresh hub is v3: marker refs exist, no legacy locks.json is written.
    assert!(manager.hub_mode().is_v3());
    assert_eq!(
        crate::hub_v3::detect_hub_version(&manager.cache_dir).unwrap(),
        crate::hub_v3::HubVersion::V3 {
            v2_branch_present: false
        }
    );
    assert!(!manager.cache_dir.join("locks.json").exists());
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
    assert!(manager.hub_mode().is_v3());
}

#[test]
fn test_init_cache_bootstrap_pushes_refs_to_remote() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    // The genesis refs were pushed to the configured remote (best-effort).
    let remote_version =
        crate::hub_v3::detect_remote_hub_version(&manager.repo_root, "origin").unwrap();
    assert!(matches!(
        remote_version,
        crate::hub_v3::HubVersion::V3 { .. }
    ));
}

#[test]
fn test_init_cache_fresh_clone_joins_v3_remote() {
    // Machine 1 bootstraps a v3 hub and pushes its refs.
    let (work_dir, remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();
    assert!(manager.hub_mode().is_v3());

    // Machine 2: a fresh clone of the same remote.
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

    // init_cache on machine 2 must JOIN the existing v3 hub (fetch refs), not
    // bootstrap a conflicting genesis.
    let manager2 = SyncManager::new(&crosslink_dir2).unwrap();
    manager2.init_cache().unwrap();
    assert!(manager2.is_initialized());
    assert!(manager2.hub_mode().is_v3());
    assert_eq!(
        crate::hub_v3::detect_hub_version(&manager2.cache_dir).unwrap(),
        crate::hub_v3::HubVersion::V3 {
            v2_branch_present: false
        }
    );
}

#[test]
fn test_fresh_v3_hub_create_issue_end_to_end() {
    // Fresh repo bootstraps v3; a SharedWriter create_issue then yields an id
    // from the deterministic reduction (REQ-4), proving the event-only write +
    // reduction path works on a brand-new hub.
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let agent = make_agent("issuer");
    std::fs::write(
        crosslink_dir.join("agent.json"),
        serde_json::to_string_pretty(&agent).unwrap(),
    )
    .unwrap();

    let sync = SyncManager::new(&crosslink_dir).unwrap();
    sync.init_cache().unwrap();
    assert!(sync.hub_mode().is_v3());

    let db = crate::db::Database::open(&crosslink_dir.join("issues.db")).unwrap();
    let writer = crate::shared_writer::SharedWriter::new(&crosslink_dir)
        .unwrap()
        .unwrap();
    let id = writer
        .create_issue(&db, "First v3 issue", None, "high", None, None)
        .unwrap();
    assert_eq!(id, 1, "first reduction-assigned display id must be 1");

    // The id is materialized by reducing the agent ref (not a counter file).
    let source = crate::hub_source::RefHubSource::new(sync.cache_path()).unwrap();
    let state = crate::compaction::reduce(&source).unwrap().state;
    assert!(
        state.issues.values().any(|i| i.title == "First v3 issue"),
        "the created issue must surface through the v3 reduction"
    );
}

#[test]
fn test_two_machine_v3_join_round_trip() {
    // Machine 1 bootstraps + creates an issue + pushes. Machine 2 clones, joins
    // the v3 hub, creates its own issue, pushes. Machine 1 fetches and sees it.
    let (work_dir, remote_dir) = setup_sync_env();
    let cl1 = work_dir.path().join(".crosslink");
    std::fs::write(
        cl1.join("agent.json"),
        serde_json::to_string_pretty(&make_agent("machine-1")).unwrap(),
    )
    .unwrap();
    let sync1 = SyncManager::new(&cl1).unwrap();
    sync1.init_cache().unwrap();
    let db1 = crate::db::Database::open(&cl1.join("issues.db")).unwrap();
    let w1 = crate::shared_writer::SharedWriter::new(&cl1)
        .unwrap()
        .unwrap();
    w1.create_issue(&db1, "from m1", None, "high", None, None)
        .unwrap();

    // Machine 2: fresh clone of the same remote.
    let work2 = tempfile::tempdir().unwrap();
    for args in [
        vec!["init", "-b", "main"],
        vec!["config", "user.email", "test@test.local"],
        vec!["config", "user.name", "Test"],
        vec![
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ],
        vec!["fetch", "origin", "main"],
        vec!["checkout", "-b", "main", "origin/main"],
    ] {
        Command::new("git")
            .current_dir(work2.path())
            .args(&args)
            .output()
            .unwrap();
    }
    let cl2 = work2.path().join(".crosslink");
    std::fs::create_dir_all(&cl2).unwrap();
    std::fs::write(cl2.join("hook-config.json"), r#"{"remote":"origin"}"#).unwrap();
    std::fs::write(
        cl2.join("agent.json"),
        serde_json::to_string_pretty(&make_agent("machine-2")).unwrap(),
    )
    .unwrap();
    let sync2 = SyncManager::new(&cl2).unwrap();
    sync2.init_cache().unwrap();
    assert!(sync2.hub_mode().is_v3(), "m2 must join the v3 hub");
    let db2 = crate::db::Database::open(&cl2.join("issues.db")).unwrap();
    let w2 = crate::shared_writer::SharedWriter::new(&cl2)
        .unwrap()
        .unwrap();
    w2.create_issue(&db2, "from m2", None, "medium", None, None)
        .unwrap();

    // Machine 1 fetches and reduces: it must now see machine 2's issue.
    sync1.fetch().unwrap();
    let source = crate::hub_source::RefHubSource::new(sync1.cache_path()).unwrap();
    let state = crate::compaction::reduce(&source).unwrap().state;
    assert!(
        state.issues.values().any(|i| i.title == "from m1"),
        "m1 must still see its own issue"
    );
    assert!(
        state.issues.values().any(|i| i.title == "from m2"),
        "m1 must see m2's issue after fetch"
    );
}

#[test]
fn test_v2_fetch_is_read_only_no_new_commits() {
    // A frozen v2 hub's fetch must NOT create any commit on the v2 branch — it
    // is a read-only mirror update only (754b).
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let wp = work_dir.path();

    // Build an explicit v2 `crosslink/hub` worktree (a fresh init would bootstrap
    // v3) so the hub resolves to V2 mode.
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    for args in [
        vec![
            "worktree",
            "add",
            "--orphan",
            "-b",
            "crosslink/hub",
            cache_dir.to_str().unwrap(),
        ],
        vec![
            "-C",
            cache_dir.to_str().unwrap(),
            "config",
            "user.email",
            "t@t",
        ],
        vec![
            "-C",
            cache_dir.to_str().unwrap(),
            "config",
            "user.name",
            "t",
        ],
    ] {
        Command::new("git")
            .current_dir(wp)
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::create_dir_all(cache_dir.join("issues")).unwrap();
    std::fs::write(cache_dir.join("locks.json"), "{}").unwrap();
    Command::new("git")
        .current_dir(&cache_dir)
        .args(["add", "-A"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(&cache_dir)
        .args(["commit", "-m", "v2 init", "--no-gpg-sign"])
        .output()
        .unwrap();

    let manager = SyncManager::new(&crosslink_dir).unwrap();
    assert!(!manager.hub_mode().is_v3(), "must be a v2 hub");

    let tip_before = String::from_utf8(
        Command::new("git")
            .current_dir(&cache_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();

    manager.fetch().unwrap();

    let tip_after = String::from_utf8(
        Command::new("git")
            .current_dir(&cache_dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(
        tip_before, tip_after,
        "v2 fetch must not create any new commit on the frozen v2 branch"
    );
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
fn test_fetch_v3_after_bootstrap_pushed_refs() {
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    // Bootstrap pushes the v3 refs to the remote during init_cache.
    manager.init_cache().unwrap();
    assert!(manager.hub_mode().is_v3());

    // A subsequent fetch takes the v3 ref-adoption path and succeeds.
    manager.fetch().unwrap();
}

// ------------------------------------------------------------------
// read_allowed_signers
// ------------------------------------------------------------------

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
// find_stale_locks_with_age
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// find_stale_locks_with_age (V2 path)
// ------------------------------------------------------------------

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

// ------------------------------------------------------------------
// ensure_agent_dir (needs a git repo)
// ------------------------------------------------------------------

// ------------------------------------------------------------------
// push_heartbeat (needs a git repo)
// ------------------------------------------------------------------

#[test]
fn test_push_heartbeat_writes_to_agent_ref() {
    // A fresh hub bootstraps v3 (754b), so the heartbeat lands on the agent's
    // own ref, not a worktree `heartbeats/*.json` file.
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();
    assert!(manager.hub_mode().is_v3(), "fresh hub must bootstrap v3");

    let agent = make_agent("hb-agent");
    manager.push_heartbeat(&agent, Some(42)).unwrap();

    let beats = crate::hub_v3::read_heartbeats_from_refs(&manager.cache_dir).unwrap();
    let hb = beats
        .iter()
        .find(|(id, _)| id == "hb-agent")
        .map(|(_, hb)| hb)
        .expect("heartbeat for hb-agent must be on its agent ref");
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
fn test_verify_locks_signature_no_commits_on_v3_hub() {
    // A fresh v3 hub (754b) has no `locks.json` history on its host branch, so
    // `verify_locks_signature` (which looks for the commit touching locks.json)
    // reports NoCommits.
    let (work_dir, _remote_dir) = setup_sync_env();
    let crosslink_dir = work_dir.path().join(".crosslink");
    let manager = SyncManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    let result = manager.verify_locks_signature().unwrap();
    assert!(matches!(
        result,
        crate::sync::SignatureVerification::NoCommits
    ));
}

// ------------------------------------------------------------------
// verify_entry_signatures
// ------------------------------------------------------------------

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
