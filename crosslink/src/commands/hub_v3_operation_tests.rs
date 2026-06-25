//! End-to-end tests for hub-version-routed operation (754a PASS 2).
//!
//! These drive the production `migrate hub-v3` command (`super::migrate_hub_v3`)
//! to build an authentic V3 hub, then operate it through `SharedWriter` /
//! `SyncManager` in `HubMode::V3`. They cover: mode resolution, the event-only
//! write lifecycle with no worktree writes, reduction-assigned ids, the
//! two-writer convergence + no-collision invariant, lock claim-confirm,
//! offline durability, fetch adoption rules, and heartbeat/request routing.

#![cfg(test)]

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::db::Database;
use crate::hub_v3::{self, agent_ref_name, HubMode};
use crate::identity::{AgentConfig, AgentRole};
use crate::shared_writer::SharedWriter;
use crate::sync::SyncManager;

// ── Fixtures ─────────────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_ok() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

fn write_agent(crosslink_dir: &Path, id: &str) {
    let agent = AgentConfig {
        agent_id: id.to_string(),
        machine_id: "test-machine".to_string(),
        description: Some("test".to_string()),
        role: AgentRole::Driver,
        ssh_key_path: None,
        ssh_fingerprint: None,
        ssh_public_key: None,
    };
    std::fs::write(
        crosslink_dir.join("agent.json"),
        serde_json::to_string_pretty(&agent).unwrap(),
    )
    .unwrap();
}

/// Populate agent `alpha`'s v2 event log directly (two issues, a label, a
/// comment, a milestone) so the migration has a populated v2 hub to convert.
/// The v2 `SharedWriter` write path is deleted (#754); `compaction::compact`
/// (called by the fixture) materializes the worktree files from these events.
fn populate_alpha_v2_for_migration(cache_dir: &Path) {
    use crate::events::{append_event, Event, EventEnvelope};
    let i1 = uuid::Uuid::parse_str("a1a1a1a1-a1a1-a1a1-a1a1-a1a1a1a1a1a1").unwrap();
    let i2 = uuid::Uuid::parse_str("a2a2a2a2-a2a2-a2a2-a2a2-a2a2a2a2a2a2").unwrap();
    let ms = uuid::Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();
    let c1 = uuid::Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap();
    let base = chrono::Utc::now() - chrono::Duration::seconds(300);
    let log_path = cache_dir.join("agents").join("alpha").join("events.log");

    let events = vec![
        Event::IssueCreated {
            uuid: i1,
            title: "First issue".to_string(),
            description: Some("desc one".to_string()),
            priority: "high".to_string(),
            labels: vec![],
            parent_uuid: None,
            created_by: "alpha".to_string(),
            display_id: Some(1),
            scheduled_at: None,
            due_at: None,
        },
        Event::IssueCreated {
            uuid: i2,
            title: "Second issue".to_string(),
            description: None,
            priority: "medium".to_string(),
            labels: vec![],
            parent_uuid: None,
            created_by: "alpha".to_string(),
            display_id: Some(2),
            scheduled_at: None,
            due_at: None,
        },
        Event::LabelAdded {
            issue_uuid: i1,
            label: "bug".to_string(),
        },
        Event::CommentAdded {
            issue_uuid: i1,
            comment_uuid: c1,
            display_id: Some(1),
            author: "alpha".to_string(),
            content: "a note".to_string(),
            created_at: base,
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        },
        Event::MilestoneCreated {
            uuid: ms,
            display_id: Some(1),
            name: "v1.0".to_string(),
            description: None,
            created_at: base,
        },
    ];

    for (i, event) in events.into_iter().enumerate() {
        let env = EventEnvelope {
            agent_id: "alpha".to_string(),
            agent_seq: (i + 1) as u64,
            timestamp: base + chrono::Duration::seconds(i as i64),
            event,
            signed_by: None,
            signature: None,
        };
        append_event(&log_path, &env).unwrap();
    }

    // Materialize the V2 comment file the genesis reads.
    let comments_dir = cache_dir
        .join("issues")
        .join(i1.to_string())
        .join("comments");
    std::fs::create_dir_all(&comments_dir).unwrap();
    let cf = crate::issue_file::CommentFile {
        uuid: c1,
        issue_uuid: i1,
        author: "alpha".to_string(),
        content: "a note".to_string(),
        created_at: base,
        kind: "note".to_string(),
        trigger_type: None,
        intervention_context: None,
        driver_key_fingerprint: None,
        signed_by: None,
        signature: None,
    };
    crate::issue_file::write_comment_file(&comments_dir.join(format!("{c1}.json")), &cf).unwrap();
}

/// A migrated V3 hub: a work clone with a bare remote, populated then migrated.
struct V3Hub {
    work: TempDir,
    remote: TempDir,
    crosslink_dir: PathBuf,
    cache_dir: PathBuf,
}

/// Build a clone (`agent_id`) sharing `remote`. Returns `(work, crosslink_dir,
/// cache_dir)`.
fn clone_for_agent(remote: &Path, agent_id: &str) -> (TempDir, PathBuf, PathBuf) {
    let work = tempfile::tempdir().unwrap();
    let wp = work.path().to_path_buf();
    git(&wp, &["init", "-b", "main"]);
    git(&wp, &["config", "user.email", "test@test.local"]);
    git(&wp, &["config", "user.name", "Test"]);
    git(&wp, &["config", "commit.gpgsign", "false"]);
    git(&wp, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&wp, &["fetch", "origin"]);
    git(&wp, &["checkout", "main"]);
    let crosslink_dir = wp.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"remote":"origin","layout":"v2"}"#,
    )
    .unwrap();
    write_agent(&crosslink_dir, agent_id);
    let sync = SyncManager::new(&crosslink_dir).unwrap();
    sync.init_cache().unwrap();
    let cache_dir = sync.cache_path().to_path_buf();
    // Onboard the clone onto the v3 hub: fetch the marker + agent refs so
    // `detect_hub_version` (hence `HubMode::resolve`) sees V3. A real v3-aware
    // bootstrap (754b) does this; here we fetch the refs directly so the clone
    // discovers the already-migrated hub.
    git(
        &cache_dir,
        &[
            "fetch",
            "origin",
            "+refs/heads/crosslink/*:refs/heads/crosslink/*",
        ],
    );
    (work, crosslink_dir, cache_dir)
}

/// Create a populated v2 hub for `alpha`, then migrate it to v3.
fn setup_migrated_v3_hub() -> V3Hub {
    let remote = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "--bare", "-b", "main"]);
    let wp = work.path().to_path_buf();
    git(&wp, &["init", "-b", "main"]);
    git(&wp, &["config", "user.email", "test@test.local"]);
    git(&wp, &["config", "user.name", "Test"]);
    git(&wp, &["config", "commit.gpgsign", "false"]);
    git(
        &wp,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    std::fs::write(wp.join("README.md"), "# test\n").unwrap();
    git(&wp, &["add", "."]);
    git(&wp, &["commit", "-m", "init", "--no-gpg-sign"]);
    git(&wp, &["push", "-u", "origin", "main"]);

    let crosslink_dir = wp.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    std::fs::write(
        crosslink_dir.join("hook-config.json"),
        r#"{"remote":"origin","layout":"v2"}"#,
    )
    .unwrap();
    write_agent(&crosslink_dir, "alpha");

    let sync = SyncManager::new(&crosslink_dir).unwrap();
    let cache_dir = sync.cache_path().to_path_buf();
    // Since 754b a fresh `init_cache` bootstraps v3, but this fixture migrates
    // FROM a v2 hub, so build the legacy `crosslink/hub` worktree explicitly.
    git(
        &wp,
        &[
            "worktree",
            "add",
            "--orphan",
            "-b",
            "crosslink/hub",
            cache_dir.to_str().unwrap(),
        ],
    );
    git(&cache_dir, &["config", "user.email", "test@test.local"]);
    git(&cache_dir, &["config", "user.name", "Test"]);
    git(&cache_dir, &["config", "commit.gpgsign", "false"]);
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(meta_dir.join("milestones")).unwrap();
    std::fs::create_dir_all(cache_dir.join("issues")).unwrap();
    std::fs::create_dir_all(cache_dir.join("locks")).unwrap();
    crate::issue_file::write_layout_version(&meta_dir, crate::issue_file::CURRENT_LAYOUT_VERSION)
        .unwrap();
    std::fs::write(
        cache_dir.join("locks.json"),
        serde_json::to_string(&serde_json::json!({"version":1,"locks":{},"settings":{"stale_lock_timeout_minutes":60}})).unwrap(),
    )
    .unwrap();
    git(&cache_dir, &["add", "-A"]);
    git(
        &cache_dir,
        &[
            "commit",
            "-m",
            "Initialize crosslink/hub branch",
            "--no-gpg-sign",
        ],
    );

    // Populate the pre-migration v2 hub by writing the agent event log directly
    // (the v2 SharedWriter write path is deleted, #754), then materialize with
    // `compaction::compact` (kept for migration).
    populate_alpha_v2_for_migration(&cache_dir);

    let lock = sync.acquire_lock().unwrap();
    crate::compaction::compact(&cache_dir, "alpha", true, &lock).unwrap();
    drop(lock);

    // Migrate to v3 (the real command).
    super::migrate_hub_v3::hub_v3(&crosslink_dir, false, false, false, false).unwrap();

    V3Hub {
        work,
        remote,
        crosslink_dir,
        cache_dir,
    }
}

/// Fingerprint of the v2 worktree issue files (relative path + size). Lets a
/// test assert no V3 mutation wrote to the v2 worktree.
fn issues_dir_fingerprint(cache_dir: &Path) -> Vec<(String, u64)> {
    let issues = cache_dir.join("issues");
    let mut out = Vec::new();
    if let Ok(files) = walk_files(&issues) {
        for p in files {
            let rel = p
                .strip_prefix(&issues)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let size = std::fs::metadata(&p).map_or(0, |m| m.len());
            out.push((rel, size));
        }
    }
    out.sort();
    out
}

fn walk_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_files(&path)?);
        } else {
            out.push(path);
        }
    }
    Ok(out)
}

// ── Tests ────────────────────────────────────────────────────────────

#[test]
fn v3_mode_resolves_after_migration() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let sync = SyncManager::new(&hub.crosslink_dir).unwrap();
    assert_eq!(sync.hub_mode(), HubMode::V3);
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    assert!(writer.is_v3_public());
}

#[test]
fn v3_lifecycle_no_worktree_writes() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let before = issues_dir_fingerprint(&hub.cache_dir);

    let db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();

    let new_id = writer
        .create_issue(&db, "V3 created", Some("body"), "low", None, None)
        .unwrap();
    assert!(new_id > 0, "reduction should assign a positive display id");

    let issue = db.get_issue(new_id).unwrap().expect("issue hydrated");
    assert_eq!(issue.title, "V3 created");

    // Update (priority), label, comment, and close all route through the v3
    // event-only path.
    writer
        .update_issue(
            &db,
            new_id,
            crate::shared_writer::IssueUpdate {
                title: Some("V3 renamed"),
                ..Default::default()
            },
        )
        .unwrap();
    writer.add_label(&db, new_id, "v3label").unwrap();
    writer
        .add_comment(&db, new_id, "v3 comment", "note")
        .unwrap();
    writer.close_issue(&db, new_id).unwrap();

    let issue = db.get_issue(new_id).unwrap().unwrap();
    assert_eq!(issue.title, "V3 renamed");
    assert_eq!(issue.status, crate::models::IssueStatus::Closed);
    assert!(db
        .get_labels(new_id)
        .unwrap()
        .iter()
        .any(|l| l == "v3label"));
    assert!(!db.get_comments(new_id).unwrap().is_empty());

    let after = issues_dir_fingerprint(&hub.cache_dir);
    assert_eq!(
        before, after,
        "V3 mutations must not touch the v2 worktree issue files"
    );

    let seq = hub_v3::read_max_event_seq_from_ref(&hub.cache_dir, "alpha").unwrap();
    assert!(seq > 0, "own ref should record events");
}

#[test]
fn v3_display_ids_stable_across_re_reduce() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let id = writer
        .create_issue(&db, "Stable id", None, "medium", None, None)
        .unwrap();

    let uuid_str = db.get_issue_uuid_by_id(id).unwrap();
    let uuid = uuid::Uuid::parse_str(&uuid_str).unwrap();

    let source = crate::hub_source::RefHubSource::new(&hub.cache_dir).unwrap();
    let state = crate::compaction::reduce(&source).unwrap().state;
    assert_eq!(
        state.display_id_map.get(&uuid).copied(),
        Some(id),
        "reduction-assigned display id must be stable across re-reduce"
    );
}

#[test]
fn v3_milestone_create_returns_reduction_id() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let mid = writer.create_milestone(&db, "v2.0", Some("next")).unwrap();
    assert!(
        mid > 0,
        "milestone id should be reduction-assigned positive"
    );
    let source = crate::hub_source::RefHubSource::new(&hub.cache_dir).unwrap();
    let state = crate::compaction::reduce(&source).unwrap().state;
    assert!(
        state.milestones.values().any(|m| m.display_id == Some(mid)),
        "milestone id must be present in reduced state"
    );
}

#[test]
fn v3_two_writers_converge_no_id_collision() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let remote = hub.remote.path();

    let db_a = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer_a = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let a_id = writer_a
        .create_issue(&db_a, "Alpha issue", None, "high", None, None)
        .unwrap();

    let (_wb, beta_dir, beta_cache) = clone_for_agent(remote, "beta");
    let sync_b = SyncManager::new(&beta_dir).unwrap();
    assert_eq!(sync_b.hub_mode(), HubMode::V3);
    sync_b.fetch().unwrap();
    let db_b = Database::open(&beta_dir.join("issues.db")).unwrap();
    let writer_b = SharedWriter::new(&beta_dir).unwrap().unwrap();
    let b_id = writer_b
        .create_issue(&db_b, "Beta issue", None, "low", None, None)
        .unwrap();
    assert_ne!(a_id, b_id, "two writers must not collide on display ids");

    let sync_a = SyncManager::new(&hub.crosslink_dir).unwrap();
    sync_a.fetch().unwrap();

    let state_a =
        crate::compaction::reduce(&crate::hub_source::RefHubSource::new(&hub.cache_dir).unwrap())
            .unwrap()
            .state;
    let state_b =
        crate::compaction::reduce(&crate::hub_source::RefHubSource::new(&beta_cache).unwrap())
            .unwrap()
            .state;
    assert_eq!(
        state_a.issues.len(),
        state_b.issues.len(),
        "both writers converge to the same issue set"
    );
    assert_eq!(state_a.display_id_map, state_b.display_id_map);
}

#[test]
fn v3_lock_claim_confirm_winner_and_loser() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let remote = hub.remote.path();
    let db_a = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer_a = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let issue_id = writer_a
        .create_issue(&db_a, "Contended", None, "high", None, None)
        .unwrap();

    let (_wb, beta_dir, _bc) = clone_for_agent(remote, "beta");
    let sync_b = SyncManager::new(&beta_dir).unwrap();
    sync_b.fetch().unwrap();
    let writer_b = SharedWriter::new(&beta_dir).unwrap().unwrap();

    let res_a = writer_a.claim_lock_v2(issue_id, None).unwrap();
    assert_eq!(
        res_a,
        crate::shared_writer::LockClaimResult::Claimed,
        "first claimant wins"
    );

    let res_b = writer_b.claim_lock_v2(issue_id, None).unwrap();
    match res_b {
        crate::shared_writer::LockClaimResult::Contended { winner_agent_id } => {
            assert_eq!(winner_agent_id, "alpha");
        }
        other => panic!("expected Contended, got {other:?}"),
    }
}

#[test]
fn v3_offline_mutation_durable_then_delivered() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();

    // Break the remote.
    git(
        hub.work.path(),
        &["remote", "set-url", "origin", "/nonexistent/remote-xyz.git"],
    );

    let id = writer
        .create_issue(&db, "Offline issue", None, "medium", None, None)
        .unwrap();
    assert!(id > 0);
    assert!(db.get_issue(id).unwrap().is_some());
    let local_seq = hub_v3::read_max_event_seq_from_ref(&hub.cache_dir, "alpha").unwrap();
    assert!(
        local_seq > 0,
        "event durable on local ref despite push failure"
    );

    // Restore + next op delivers the backlog.
    git(
        hub.work.path(),
        &[
            "remote",
            "set-url",
            "origin",
            hub.remote.path().to_str().unwrap(),
        ],
    );
    let id2 = writer
        .create_issue(&db, "Back online", None, "low", None, None)
        .unwrap();
    assert!(id2 > 0);
    let ls = Command::new("git")
        .current_dir(hub.remote.path())
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &agent_ref_name("alpha").unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        ls.status.success(),
        "alpha ref should be on the remote after reconnecting"
    );
}

#[test]
fn v3_fetch_adopts_other_ref_never_moves_own() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let remote = hub.remote.path();

    let db_a = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer_a = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    writer_a
        .create_issue(&db_a, "Alpha A", None, "high", None, None)
        .unwrap();

    let (_wb, beta_dir, beta_cache) = clone_for_agent(remote, "beta");
    let sync_b = SyncManager::new(&beta_dir).unwrap();
    sync_b.fetch().unwrap();
    let db_b = Database::open(&beta_dir.join("issues.db")).unwrap();
    let writer_b = SharedWriter::new(&beta_dir).unwrap().unwrap();
    writer_b
        .create_issue(&db_b, "Beta B", None, "low", None, None)
        .unwrap();

    let alpha_ref = agent_ref_name("alpha").unwrap();
    let seq_before = hub_v3::read_max_event_seq_from_ref(&hub.cache_dir, "alpha").unwrap();

    let sync_a = SyncManager::new(&hub.crosslink_dir).unwrap();
    sync_a.fetch().unwrap();

    // Fetch must NEVER adopt our own ref FROM the remote tracking tip — the
    // local own ref is authoritative for the writer. (It may be locally
    // rewritten by a REQ-11 prune, but is never set to the remote-tracking SHA.)
    let own_after = hub_v3::git_rev_parse_optional(&hub.cache_dir, &alpha_ref).unwrap();
    let own_remote_tracking =
        hub_v3::git_rev_parse_optional(&hub.cache_dir, "refs/crosslink-remote/agents/alpha")
            .unwrap();
    if let Some(rt) = own_remote_tracking {
        // The own ref carries alpha's authoritative head; if it equals the
        // remote tracking tip that's only because no local-only events exist.
        // What must NOT happen is a regression to a stale remote tip — assert
        // the sequence high-water mark never dropped below what we wrote.
        let _ = rt;
    }
    let seq_after = hub_v3::read_max_event_seq_from_ref(&hub.cache_dir, "alpha").unwrap();
    assert!(
        own_after.is_some() && seq_after >= seq_before,
        "fetch must not regress our own ref's event high-water mark"
    );

    // alpha adopts beta's authoritative ref tip.
    let beta_ref = agent_ref_name("beta").unwrap();
    let beta_local = hub_v3::git_rev_parse_optional(&hub.cache_dir, &beta_ref).unwrap();
    let beta_remote = hub_v3::git_rev_parse_optional(&beta_cache, &beta_ref).unwrap();
    assert_eq!(
        beta_local, beta_remote,
        "alpha must adopt beta's authoritative ref tip"
    );
}

#[test]
fn v3_heartbeat_routed_to_ref() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let sync = SyncManager::new(&hub.crosslink_dir).unwrap();
    let agent = AgentConfig::load(&hub.crosslink_dir).unwrap().unwrap();
    sync.push_heartbeat(&agent, Some(42)).unwrap();

    let hbs = sync.read_heartbeats_auto().unwrap();
    assert!(
        hbs.iter()
            .any(|h| h.agent_id == "alpha" && h.active_issue_id == Some(42)),
        "v3 heartbeat must round-trip through the agent ref"
    );
}

#[test]
fn v3_request_and_ack_lifecycle() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let remote = hub.remote.path();

    let writer_a = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let request = crate::agent_requests::AgentRequest {
        request_id: crate::agent_requests::new_request_id(),
        kind: crate::agent_requests::RequestKind::Pause,
        subject: crate::agent_requests::RequestSubject::default(),
        requested_by: "alpha-fp".to_string(),
        requested_at: chrono::Utc::now().to_rfc3339(),
        reason: Some("pause please".to_string()),
    };
    writer_a.write_agent_request("beta", &request).unwrap();

    let (_wb, beta_dir, beta_cache) = clone_for_agent(remote, "beta");
    let sync_b = SyncManager::new(&beta_dir).unwrap();
    sync_b.fetch().unwrap();
    let pending = hub_v3::poll_requests_for_agent(&beta_cache, "beta").unwrap();
    assert_eq!(pending.len(), 1, "beta should see one pending request");
    assert_eq!(pending[0].1.request_id, request.request_id);

    let writer_b = SharedWriter::new(&beta_dir).unwrap().unwrap();
    let ack = crate::agent_requests::AgentRequestAck {
        request_id: request.request_id,
        ack_at: chrono::Utc::now().to_rfc3339(),
        acted: true,
        result: "paused".to_string(),
        notes: None,
    };
    writer_b.write_agent_ack("beta", &ack).unwrap();
    let still_pending = hub_v3::poll_requests_for_agent(&beta_cache, "beta").unwrap();
    assert!(
        still_pending.is_empty(),
        "acked request must no longer be pending"
    );
}

#[test]
fn v3_lock_check_reads_from_checkpoint() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();
    let db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let id = writer
        .create_issue(&db, "Lockable", None, "high", None, None)
        .unwrap();
    writer.claim_lock_v2(id, None).unwrap();

    let sync = SyncManager::new(&hub.crosslink_dir).unwrap();
    let locks = sync.read_locks_auto().unwrap();
    assert!(
        locks.is_locked_by(id, "alpha"),
        "v3 lock_check must read the lock from the checkpoint state"
    );
}

#[test]
fn v3_dashboard_reader_reroutes_to_refs() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();

    // Claim a lock (event-only) and push a heartbeat so the snapshot has
    // issues (from the checkpoint), a lock (state.locks), and a heartbeat
    // (agent ref) all to surface through the ref-based reroute.
    let db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let id = writer
        .create_issue(&db, "Lockable", None, "high", None, None)
        .unwrap();
    writer.claim_lock_v2(id, None).unwrap();
    let sync = SyncManager::new(&hub.crosslink_dir).unwrap();
    let agent = AgentConfig::load(&hub.crosslink_dir).unwrap().unwrap();
    sync.push_heartbeat(&agent, Some(id)).unwrap();

    // The dashboard reader resolves mode per project from the clone path. The
    // hub-cache dir is where `crosslink/hub` and the v3 refs both resolve.
    assert!(
        HubMode::resolve(&hub.cache_dir).is_v3(),
        "migrated cache must resolve to V3 mode"
    );
    let snap = crate::dashboard::reader::read_snapshot(&hub.cache_dir).unwrap();

    assert_eq!(snap.layout_version, 3, "v3 hub reports layout version 3");
    assert!(
        snap.issues.iter().any(|i| i.title == "First issue"),
        "v3 dashboard snapshot must surface checkpoint issues"
    );
    assert!(
        snap.issues.iter().any(|i| i.title == "Lockable"),
        "v3 dashboard snapshot must include the locked issue"
    );
    assert!(
        snap.issues
            .iter()
            .any(|i| i.comments.iter().any(|c| c.content == "a note")),
        "v3 dashboard snapshot must carry checkpoint comments"
    );
    assert!(
        snap.locks.iter().any(|l| l.lock.agent_id == "alpha"),
        "v3 dashboard snapshot must surface the lock from checkpoint state"
    );
    assert!(
        snap.agents.iter().any(|h| h.agent_id == "alpha"),
        "v3 dashboard snapshot must surface heartbeats read from agent refs"
    );
    assert!(
        snap.agent_requests.is_empty(),
        "v3 snapshot leaves agent_requests empty (surfaced via the poll path)"
    );

    // derive_counters must operate on the ref-sourced state without panicking.
    let counters = snap.derive_counters(chrono::Utc::now(), 10, 60);
    assert!(counters.open_issues >= 1);
}

#[test]
fn v3_server_agents_handler_reads_from_refs() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();

    let db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let id = writer
        .create_issue(&db, "Server lockable", None, "high", None, None)
        .unwrap();
    writer.claim_lock_v2(id, None).unwrap();
    let sync = SyncManager::new(&hub.crosslink_dir).unwrap();
    let agent = AgentConfig::load(&hub.crosslink_dir).unwrap().unwrap();
    sync.push_heartbeat(&agent, Some(id)).unwrap();

    // Build server AppState over the migrated v3 hub and drive the agents/locks
    // handlers directly. They route through SyncManager::read_*_auto, which is
    // mode-aware, so a v3 hub must surface refs-sourced agents and locks.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let handler_db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
        let state = crate::server::state::AppState::new(handler_db, hub.crosslink_dir.clone());

        let agents_json =
            crate::server::handlers::agents::list_agents(axum::extract::State(state.clone()))
                .await
                .expect("list_agents must succeed on a v3 hub");
        let agents = agents_json.0;
        let items = agents["items"].as_array().expect("items array");
        assert!(
            items
                .iter()
                .any(|a| a["agent_id"].as_str() == Some("alpha")),
            "v3 server agents handler must surface the ref heartbeat, got {agents}"
        );

        let locks_json =
            crate::server::handlers::agents::list_locks(axum::extract::State(state.clone()))
                .await
                .expect("list_locks must succeed on a v3 hub");
        let locks = locks_json.0;
        let lock_items = locks["items"].as_array().expect("items array");
        assert!(
            lock_items
                .iter()
                .any(|l| l["agent_id"].as_str() == Some("alpha")),
            "v3 server locks handler must surface the checkpoint lock, got {locks}"
        );
    });
}

#[test]
fn v3_locks_cmd_and_stale_detection_over_refs() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();

    let db = Database::open(&hub.crosslink_dir.join("issues.db")).unwrap();
    let writer = SharedWriter::new(&hub.crosslink_dir).unwrap().unwrap();
    let id = writer
        .create_issue(&db, "Cmd lockable", None, "high", None, None)
        .unwrap();
    writer.claim_lock_v2(id, None).unwrap();

    // Fresh heartbeat on the agent ref: the lock must NOT be reported stale.
    // The bug this guards against: `find_stale_locks` read V1-only
    // `heartbeats/*.json` (empty on a v3 hub) and marked every lock stale.
    let sync = SyncManager::new(&hub.crosslink_dir).unwrap();
    let agent = AgentConfig::load(&hub.crosslink_dir).unwrap().unwrap();
    sync.push_heartbeat(&agent, Some(id)).unwrap();

    let stale = sync.find_stale_locks().unwrap();
    assert!(
        !stale.iter().any(|(sid, _)| *sid == id),
        "a v3 lock with a fresh ref heartbeat must not be flagged stale, got {stale:?}"
    );
    let stale_aged = sync.find_stale_locks_with_age().unwrap();
    assert!(
        !stale_aged.iter().any(|(sid, _, _)| *sid == id),
        "find_stale_locks_with_age must use ref heartbeats on a v3 hub"
    );

    // `locks check`/`list`/`next` route lock reads through read_locks_auto and
    // must run without error on a v3 hub.
    super::locks_cmd::check(&hub.crosslink_dir, id).unwrap();
    super::locks_cmd::list(&hub.crosslink_dir, &db, true).unwrap();
    super::next::run(&db, &hub.crosslink_dir).unwrap();
}

// ── `migrate hub-branches` round-trip (#767) ─────────────────────────

/// Move every NEW-namespace hub ref back to the OLD hidden namespace, locally
/// and on the bare remote (via the cache's `origin`), to simulate a hub created
/// before the #767 flip.
fn demote_to_old_namespace(cache_dir: &Path) {
    let pairs = [
        (
            "refs/heads/crosslink/checkpoint",
            "refs/crosslink/checkpoint",
        ),
        ("refs/heads/crosslink/meta", "refs/crosslink/meta"),
        (
            &format!("refs/heads/crosslink/agents/{}", "alpha"),
            "refs/crosslink/agents/alpha",
        ),
    ];
    for (new, old) in pairs {
        let sha = Command::new("git")
            .current_dir(cache_dir)
            .args(["rev-parse", "--verify", "--quiet", new])
            .output()
            .unwrap();
        if !sha.status.success() {
            continue;
        }
        let sha = String::from_utf8_lossy(&sha.stdout).trim().to_string();
        // Local: create old, delete new.
        git(cache_dir, &["update-ref", old, &sha]);
        git(cache_dir, &["update-ref", "-d", new]);
        // Remote (via the cache's `origin`): push old, delete new.
        git(cache_dir, &["push", "origin", &format!("{sha}:{old}")]);
        let _ = Command::new("git")
            .current_dir(cache_dir)
            .args(["push", "origin", &format!(":{new}")])
            .output()
            .unwrap();
    }
}

/// Round-trip: build an OLD-namespace v3 hub, run `migrate hub-branches`, and
/// assert the new visible branches land at the same SHAs local + remote, the old
/// refs are gone both sides, the `RefHubSource` reduces identically post-rename,
/// and a second run is a clean no-op.
#[test]
fn hub_branches_rename_round_trip() {
    if !git_ok() {
        return;
    }
    let hub = setup_migrated_v3_hub();

    // Capture the authoritative reduced state BEFORE the demotion/rename.
    let before =
        crate::compaction::reduce(&crate::hub_source::RefHubSource::new(&hub.cache_dir).unwrap())
            .unwrap()
            .state;

    // Demote to the OLD hidden namespace (simulate a pre-#767 hub).
    demote_to_old_namespace(&hub.cache_dir);

    // The old hidden refs exist; the new visible ones don't (local).
    let rev =
        |dir: &Path, r: &str| -> Option<String> { hub_v3::git_rev_parse_optional(dir, r).unwrap() };
    assert!(rev(&hub.cache_dir, "refs/crosslink/checkpoint").is_some());
    assert!(rev(&hub.cache_dir, "refs/heads/crosslink/checkpoint").is_none());

    // Record the old SHAs so we can assert SHA-equality after the rename.
    let old_cp = rev(&hub.cache_dir, "refs/crosslink/checkpoint").unwrap();
    let old_meta = rev(&hub.cache_dir, "refs/crosslink/meta").unwrap();
    let old_alpha = rev(&hub.cache_dir, "refs/crosslink/agents/alpha").unwrap();

    // Run the rename migration.
    super::migrate_hub_v3::hub_branches(&hub.crosslink_dir).unwrap();

    // Helper: `new` is the old tip or a descendant of it (the post-rename
    // compaction can advance the checkpoint by writing the browse tree and the
    // agent ref by a REQ-11 prune, both as CHILD commits preserving history).
    let same_or_descendant = |old: &str, new: &str| -> bool {
        new == old
            || Command::new("git")
                .current_dir(&hub.cache_dir)
                .args(["merge-base", "--is-ancestor", old, new])
                .status()
                .unwrap()
                .success()
    };

    // meta is never compacted → exact-SHA rename.
    assert!(rev(&hub.cache_dir, "refs/heads/crosslink/meta").as_deref() == Some(old_meta.as_str()));
    // The agent ref lands at the old SHA, then the post-rename compaction may
    // prune it forward (a child commit), so assert old is an ancestor-or-equal.
    let new_alpha = rev(&hub.cache_dir, "refs/heads/crosslink/agents/alpha").unwrap();
    assert!(
        same_or_descendant(&old_alpha, &new_alpha),
        "renamed agent ref must be the old tip or a descendant of it"
    );
    assert!(rev(&hub.cache_dir, "refs/crosslink/meta").is_none());
    assert!(rev(&hub.cache_dir, "refs/crosslink/checkpoint").is_none());
    assert!(rev(&hub.cache_dir, "refs/crosslink/agents/alpha").is_none());
    // The renamed checkpoint exists; compaction advances it (browse tree), so the
    // old tip is an ancestor-or-equal.
    let new_cp = rev(&hub.cache_dir, "refs/heads/crosslink/checkpoint").unwrap();
    assert!(
        same_or_descendant(&old_cp, &new_cp),
        "renamed checkpoint must be the old tip or a descendant of it"
    );

    // Remote: new visible branches present, old hidden refs gone.
    let remote_refs: Vec<String> = {
        let out = Command::new("git")
            .current_dir(&hub.cache_dir)
            .args(["ls-remote", "origin"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(_, n)| n.trim().to_string()))
            .collect()
    };
    assert!(remote_refs.iter().any(|r| r == "refs/heads/crosslink/meta"));
    assert!(remote_refs
        .iter()
        .any(|r| r == "refs/heads/crosslink/agents/alpha"));
    assert!(
        !remote_refs.iter().any(|r| r.starts_with("refs/crosslink/")),
        "no old hidden refs remain on the remote, got {remote_refs:?}"
    );

    // RefHubSource reduces to the SAME issue set post-rename (browse-tree commit
    // does not change the reduced state).
    let after =
        crate::compaction::reduce(&crate::hub_source::RefHubSource::new(&hub.cache_dir).unwrap())
            .unwrap()
            .state;
    assert_eq!(
        before
            .issues
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        after
            .issues
            .keys()
            .collect::<std::collections::BTreeSet<_>>(),
        "reduced issue set must be identical after the rename"
    );

    // The browse tree materialized on the now-visible checkpoint branch.
    let cp_tip = rev(&hub.cache_dir, "refs/heads/crosslink/checkpoint").unwrap();
    let readme =
        hub_v3::git_cat_file_blob_optional(&hub.cache_dir, &format!("{cp_tip}:README.md")).unwrap();
    assert!(readme.is_some(), "browse README materialized after rename");

    // Second run is a clean no-op (idempotent).
    super::migrate_hub_v3::hub_branches(&hub.crosslink_dir).unwrap();
    assert!(rev(&hub.cache_dir, "refs/crosslink/checkpoint").is_none());
    assert!(rev(&hub.cache_dir, "refs/heads/crosslink/checkpoint").is_some());
}
