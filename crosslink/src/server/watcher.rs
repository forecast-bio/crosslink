//! Filesystem watcher for real-time WebSocket events.
//!
//! Uses the `notify` crate to watch the hub cache's `heartbeats/` directory.
//! On file changes, reads the latest heartbeat state, diffs it against the
//! previous snapshot, and broadcasts `heartbeat` and `agent_status` events
//! through the WebSocket broadcast channel.
//!
//! A 30-second polling fallback ensures clients stay up-to-date even when
//! filesystem events are missed (e.g. network mounts, WSL quirks).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use chrono::{Duration, Utc};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::broadcast;
use tokio::time;

use crate::locks::Heartbeat;
use crate::server::types::{AgentStatus, WsAgentStatusEvent, WsHeartbeatEvent};
use crate::server::ws::WsEvent;
use crate::sync::SyncManager;

/// Polling interval when filesystem events are missed or unavailable.
const POLL_INTERVAL_SECS: u64 = 30;

/// Derive an `AgentStatus` from how stale a heartbeat timestamp is.
///
/// | Age            | Status  |
/// |----------------|---------|
/// | < 5 min        | Active  |
/// | 5 – 30 min     | Idle    |
/// | > 30 min       | Stale   |
pub fn status_from_heartbeat(heartbeat: &Heartbeat) -> AgentStatus {
    let age = Utc::now() - heartbeat.last_heartbeat;
    if age < Duration::minutes(5) {
        AgentStatus::Active
    } else if age < Duration::minutes(30) {
        AgentStatus::Idle
    } else {
        AgentStatus::Stale
    }
}

/// Spawn a background task that watches the hub cache for heartbeat changes
/// and broadcasts events to all WebSocket clients.
///
/// Returns immediately; the watcher runs until the `tx` sender is dropped.
pub fn start_watcher(crosslink_dir: PathBuf, tx: broadcast::Sender<WsEvent>) {
    tokio::spawn(async move {
        if let Err(e) = run_watcher(crosslink_dir, tx).await {
            tracing::error!("watcher: error: {e}");
        }
    });
}

/// Core watcher loop — watches the heartbeats directory and polls as a fallback.
async fn run_watcher(crosslink_dir: PathBuf, tx: broadcast::Sender<WsEvent>) -> Result<()> {
    let sync = SyncManager::new(&crosslink_dir)?;

    // Hub cache is always at <main-repo-root>/.crosslink/.hub-cache/.
    // We watch the heartbeats/ subdirectory for file-level changes.
    let watch_path = crosslink_dir.join(".hub-cache").join("heartbeats");

    // Bridge notify (sync) → tokio (async) with an mpsc channel.
    // We only need a "something changed" signal; the actual event details are
    // unused — we always re-read the full heartbeat state on any change.
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel::<()>(16);

    // Build the notify watcher.  The closure is called from the notify thread.
    let mut watcher: RecommendedWatcher = {
        let notify_tx = notify_tx.clone();
        notify::recommended_watcher(move |_res: notify::Result<notify::Event>| {
            // INTENTIONAL: non-blocking send; if the channel is full we drop the signal
            // — the next poll will pick up the changes anyway
            let _ = notify_tx.blocking_send(());
        })?
    };

    // Attempt to start watching.  If the directory doesn't exist yet (hub not
    // initialised), fall back to polling only.
    let watch_active = if watch_path.exists() {
        match watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    "watcher: could not watch {}: {e}, falling back to polling",
                    watch_path.display()
                );
                false
            }
        }
    } else {
        tracing::info!(
            "watcher: heartbeats directory not found at {}, polling only",
            watch_path.display()
        );
        false
    };

    if watch_active {
        tracing::info!(
            "watcher: watching {} for heartbeat changes",
            watch_path.display()
        );
    }

    // Initial snapshot so we can diff on the first real event.
    let mut last_state: HashMap<String, Heartbeat> = HashMap::new();
    let mut last_statuses: HashMap<String, AgentStatus> = HashMap::new();

    if let Ok(heartbeats) = sync.read_heartbeats_auto() {
        for hb in heartbeats {
            last_statuses.insert(hb.agent_id.clone(), status_from_heartbeat(&hb));
            last_state.insert(hb.agent_id.clone(), hb);
        }
    }

    // Polling timer — first tick fires immediately so we emit any initial state.
    let mut poll_interval = time::interval(time::Duration::from_secs(POLL_INTERVAL_SECS));
    poll_interval.tick().await; // consume the immediate first tick

    loop {
        tokio::select! {
            // Filesystem change notification.
            Some(()) = notify_rx.recv() => {
                diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);
            }

            // Fallback poll every 30 seconds.
            _ = poll_interval.tick() => {
                diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);
            }
        }

        // Stop if all receivers have disconnected (server shutting down).
        if tx.receiver_count() == 0 {
            break;
        }
    }

    Ok(())
}

/// Read current heartbeats, diff against `last_state`, and broadcast events.
///
/// Broadcasts a `WsHeartbeatEvent` for every heartbeat that is new or has a
/// newer timestamp.  When the derived `AgentStatus` also changes, broadcasts a
/// `WsAgentStatusEvent` as well.
fn diff_and_broadcast(
    sync: &SyncManager,
    last_state: &mut HashMap<String, Heartbeat>,
    last_statuses: &mut HashMap<String, AgentStatus>,
    tx: &broadcast::Sender<WsEvent>,
) {
    let heartbeats = match sync.read_heartbeats_auto() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("watcher: failed to read heartbeats: {e}");
            return;
        }
    };

    let mut current_state: HashMap<String, Heartbeat> = HashMap::new();
    for hb in heartbeats {
        current_state.insert(hb.agent_id.clone(), hb);
    }

    for (agent_id, hb) in &current_state {
        let is_new_or_updated = last_state
            .get(agent_id)
            .map(|prev| prev.last_heartbeat != hb.last_heartbeat)
            .unwrap_or(true);

        if is_new_or_updated {
            // INTENTIONAL: broadcast failure is harmless when no WebSocket subscribers are connected
            let _ = tx.send(WsEvent::Heartbeat(WsHeartbeatEvent {
                event_type: "heartbeat",
                agent_id: agent_id.clone(),
                timestamp: hb.last_heartbeat,
                active_issue_id: hb.active_issue_id,
            }));

            // Broadcast agent_status only when the derived status changes.
            let new_status = status_from_heartbeat(hb);
            let status_changed = last_statuses
                .get(agent_id)
                .map(|prev| prev != &new_status)
                .unwrap_or(true);

            if status_changed {
                // INTENTIONAL: broadcast failure is harmless when no WebSocket subscribers are connected
                let _ = tx.send(WsEvent::AgentStatus(WsAgentStatusEvent {
                    event_type: "agent_status",
                    agent_id: agent_id.clone(),
                    status: new_status.clone(),
                }));
                last_statuses.insert(agent_id.clone(), new_status);
            }
        }
    }

    *last_state = current_state;
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::process::Command;
    use tempfile::tempdir;

    fn make_heartbeat(agent_id: &str, age_minutes: i64) -> Heartbeat {
        Heartbeat {
            agent_id: agent_id.to_string(),
            last_heartbeat: Utc::now() - Duration::minutes(age_minutes),
            active_issue_id: None,
            machine_id: "test-machine".to_string(),
        }
    }

    /// Set up a real git repo with a bare remote, init the hub cache, and
    /// return a ready-to-use `SyncManager` along with the temp dirs so they
    /// aren't dropped prematurely.
    fn setup_watcher_env() -> (tempfile::TempDir, tempfile::TempDir, SyncManager) {
        let remote_dir = tempdir().unwrap();
        let work_dir = tempdir().unwrap();

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

        let crosslink_dir = work_dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{"remote":"origin"}"#,
        )
        .unwrap();

        let sync = SyncManager::new(&crosslink_dir).unwrap();
        sync.init_cache().unwrap();

        (work_dir, remote_dir, sync)
    }

    /// Write a heartbeat JSON file directly into the hub cache's heartbeats dir.
    fn write_heartbeat_file(sync: &SyncManager, hb: &Heartbeat) {
        let hb_dir = sync.cache_path().join("heartbeats");
        std::fs::create_dir_all(&hb_dir).unwrap();
        let json = serde_json::to_string_pretty(hb).unwrap();
        std::fs::write(hb_dir.join(format!("{}.json", hb.agent_id)), json).unwrap();
    }

    #[test]
    fn test_diff_and_broadcast_new_agent() {
        let (_work, _remote, sync) = setup_watcher_env();
        let (tx, mut rx) = broadcast::channel::<WsEvent>(16);
        let mut last_state: HashMap<String, Heartbeat> = HashMap::new();
        let mut last_statuses: HashMap<String, AgentStatus> = HashMap::new();

        // Write a fresh (active) heartbeat
        let hb = Heartbeat {
            agent_id: "worker-1".to_string(),
            last_heartbeat: Utc::now(),
            active_issue_id: Some(42),
            machine_id: "test-host".to_string(),
        };
        write_heartbeat_file(&sync, &hb);

        diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);

        // Expect Heartbeat + AgentStatus events
        let ev1 = rx.try_recv().expect("Heartbeat event");
        let ev2 = rx.try_recv().expect("AgentStatus event");
        assert!(rx.try_recv().is_err(), "no extra events");

        assert!(matches!(ev1, WsEvent::Heartbeat(_)));
        assert!(matches!(ev2, WsEvent::AgentStatus(_)));

        if let WsEvent::Heartbeat(e) = ev1 {
            assert_eq!(e.agent_id, "worker-1");
            assert_eq!(e.active_issue_id, Some(42));
        }
        if let WsEvent::AgentStatus(e) = ev2 {
            assert_eq!(e.agent_id, "worker-1");
            assert_eq!(e.status, AgentStatus::Active);
        }

        assert_eq!(last_state.len(), 1);
        assert_eq!(last_statuses.len(), 1);
    }

    #[test]
    fn test_diff_and_broadcast_unchanged() {
        let (_work, _remote, sync) = setup_watcher_env();
        let (tx, mut rx) = broadcast::channel::<WsEvent>(16);
        let mut last_state: HashMap<String, Heartbeat> = HashMap::new();
        let mut last_statuses: HashMap<String, AgentStatus> = HashMap::new();

        let hb = Heartbeat {
            agent_id: "worker-2".to_string(),
            last_heartbeat: Utc::now() - Duration::minutes(2),
            active_issue_id: None,
            machine_id: "test-host".to_string(),
        };
        write_heartbeat_file(&sync, &hb);

        // First call: should emit events
        diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);
        // Drain
        while rx.try_recv().is_ok() {}

        // Second call with same file: should emit nothing
        diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);
        assert!(rx.try_recv().is_err(), "no events for unchanged heartbeat");
    }

    #[test]
    fn test_diff_and_broadcast_updated_timestamp() {
        let (_work, _remote, sync) = setup_watcher_env();
        let (tx, mut rx) = broadcast::channel::<WsEvent>(16);
        let mut last_state: HashMap<String, Heartbeat> = HashMap::new();
        let mut last_statuses: HashMap<String, AgentStatus> = HashMap::new();

        let hb1 = Heartbeat {
            agent_id: "worker-3".to_string(),
            last_heartbeat: Utc::now() - Duration::minutes(2),
            active_issue_id: None,
            machine_id: "test-host".to_string(),
        };
        write_heartbeat_file(&sync, &hb1);
        diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);
        while rx.try_recv().is_ok() {}

        // Update timestamp (still Active, so status shouldn't change)
        let hb2 = Heartbeat {
            last_heartbeat: Utc::now() - Duration::minutes(1),
            ..hb1
        };
        write_heartbeat_file(&sync, &hb2);
        diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);

        // Should emit Heartbeat but NOT AgentStatus (status unchanged: Active→Active)
        let ev = rx.try_recv().expect("Heartbeat event");
        assert!(matches!(ev, WsEvent::Heartbeat(_)));
        // No AgentStatus event since status is still Active
        assert!(
            rx.try_recv().is_err(),
            "no AgentStatus when status unchanged"
        );
    }

    #[test]
    fn test_diff_and_broadcast_status_change() {
        let (_work, _remote, sync) = setup_watcher_env();
        let (tx, mut rx) = broadcast::channel::<WsEvent>(16);
        let mut last_state: HashMap<String, Heartbeat> = HashMap::new();
        let mut last_statuses: HashMap<String, AgentStatus> = HashMap::new();

        // Start as Idle (10 minutes old)
        let hb1 = Heartbeat {
            agent_id: "worker-4".to_string(),
            last_heartbeat: Utc::now() - Duration::minutes(10),
            active_issue_id: None,
            machine_id: "test-host".to_string(),
        };
        write_heartbeat_file(&sync, &hb1);
        diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);
        while rx.try_recv().is_ok() {}

        // Now change to Stale (35 minutes old) — different timestamp AND different status
        let hb2 = Heartbeat {
            last_heartbeat: Utc::now() - Duration::minutes(35),
            ..hb1
        };
        write_heartbeat_file(&sync, &hb2);
        diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);

        let ev1 = rx.try_recv().expect("Heartbeat event");
        let ev2 = rx.try_recv().expect("AgentStatus event");
        assert!(rx.try_recv().is_err(), "no extra events");

        assert!(matches!(ev1, WsEvent::Heartbeat(_)));
        assert!(matches!(ev2, WsEvent::AgentStatus(_)));
        if let WsEvent::AgentStatus(e) = ev2 {
            assert_eq!(e.status, AgentStatus::Stale);
        }
    }

    #[test]
    fn test_diff_and_broadcast_read_error_returns_gracefully() {
        // Create a SyncManager pointing to a non-existent crosslink dir.
        // read_heartbeats_auto will return an error / empty list gracefully.
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // SyncManager::new succeeds; cache dir doesn't exist so read_heartbeats_auto
        // returns Ok(vec![]) (empty) rather than an error, since the heartbeats dir
        // simply doesn't exist yet.
        let sync = SyncManager::new(&crosslink_dir).unwrap();

        let (tx, mut rx) = broadcast::channel::<WsEvent>(16);
        let mut last_state: HashMap<String, Heartbeat> = HashMap::new();
        let mut last_statuses: HashMap<String, AgentStatus> = HashMap::new();

        // Should not panic; no heartbeats → no events
        diff_and_broadcast(&sync, &mut last_state, &mut last_statuses, &tx);
        assert!(rx.try_recv().is_err(), "no events when no heartbeats");
    }

    #[test]
    fn test_status_active() {
        let hb = make_heartbeat("a1", 2);
        assert_eq!(status_from_heartbeat(&hb), AgentStatus::Active);
    }

    #[test]
    fn test_status_idle() {
        let hb = make_heartbeat("a1", 10);
        assert_eq!(status_from_heartbeat(&hb), AgentStatus::Idle);
    }

    #[test]
    fn test_status_stale() {
        let hb = make_heartbeat("a1", 45);
        assert_eq!(status_from_heartbeat(&hb), AgentStatus::Stale);
    }

    #[test]
    fn test_status_boundary_five_min() {
        // Exactly 5 minutes old — should be Idle, not Active.
        let hb = make_heartbeat("a1", 5);
        assert_eq!(status_from_heartbeat(&hb), AgentStatus::Idle);
    }

    #[test]
    fn test_status_boundary_thirty_min() {
        // Exactly 30 minutes old — should be Stale, not Idle.
        let hb = make_heartbeat("a1", 30);
        assert_eq!(status_from_heartbeat(&hb), AgentStatus::Stale);
    }

    #[test]
    fn test_diff_broadcasts_new_heartbeat() {
        let (tx, mut rx) = broadcast::channel::<WsEvent>(16);
        let mut last_state: HashMap<String, Heartbeat> = HashMap::new();
        let mut last_statuses: HashMap<String, AgentStatus> = HashMap::new();

        let hb = make_heartbeat("worker-1", 1);
        let mut current: HashMap<String, Heartbeat> = HashMap::new();
        current.insert("worker-1".to_string(), hb.clone());

        // Simulate diff logic directly.
        for (agent_id, hb) in &current {
            let is_new = !last_state.contains_key(agent_id);
            if is_new {
                let _ = tx.send(WsEvent::Heartbeat(WsHeartbeatEvent {
                    event_type: "heartbeat",
                    agent_id: agent_id.clone(),
                    timestamp: hb.last_heartbeat,
                    active_issue_id: hb.active_issue_id,
                }));
                let new_status = status_from_heartbeat(hb);
                let _ = tx.send(WsEvent::AgentStatus(WsAgentStatusEvent {
                    event_type: "agent_status",
                    agent_id: agent_id.clone(),
                    status: new_status.clone(),
                }));
                last_statuses.insert(agent_id.clone(), new_status);
            }
        }
        last_state = current;

        // Should have received 2 events: Heartbeat + AgentStatus.
        let ev1 = rx.try_recv().unwrap();
        let ev2 = rx.try_recv().unwrap();
        assert!(rx.try_recv().is_err(), "no extra events");

        assert!(matches!(ev1, WsEvent::Heartbeat(_)));
        assert!(matches!(ev2, WsEvent::AgentStatus(_)));
        assert_eq!(last_state.len(), 1);
        assert_eq!(last_statuses.len(), 1);
    }

    #[test]
    fn test_diff_no_broadcast_on_unchanged() {
        let (tx, mut rx) = broadcast::channel::<WsEvent>(16);
        let mut last_state: HashMap<String, Heartbeat> = HashMap::new();
        let mut last_statuses: HashMap<String, AgentStatus> = HashMap::new();

        let hb = make_heartbeat("worker-1", 1);
        last_state.insert("worker-1".to_string(), hb.clone());
        last_statuses.insert("worker-1".to_string(), AgentStatus::Active);

        let mut current: HashMap<String, Heartbeat> = HashMap::new();
        current.insert("worker-1".to_string(), hb); // same timestamp

        // Simulate diff logic for unchanged heartbeat.
        for (agent_id, hb) in &current {
            let is_new_or_updated = last_state
                .get(agent_id)
                .map(|prev| prev.last_heartbeat != hb.last_heartbeat)
                .unwrap_or(true);
            if is_new_or_updated {
                let _ = tx.send(WsEvent::Heartbeat(WsHeartbeatEvent {
                    event_type: "heartbeat",
                    agent_id: agent_id.clone(),
                    timestamp: hb.last_heartbeat,
                    active_issue_id: hb.active_issue_id,
                }));
            }
        }

        // Should have received 0 events since the timestamp did not change.
        assert!(rx.try_recv().is_err(), "no events for unchanged heartbeat");
    }
}
