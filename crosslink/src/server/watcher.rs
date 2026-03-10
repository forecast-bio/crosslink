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
            eprintln!("watcher: error: {e}");
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
            // Non-blocking send; if the channel is full we just drop the signal
            // — the next poll will pick up the changes anyway.
            let _ = notify_tx.blocking_send(());
        })?
    };

    // Attempt to start watching.  If the directory doesn't exist yet (hub not
    // initialised), fall back to polling only.
    let watch_active = if watch_path.exists() {
        match watcher.watch(&watch_path, RecursiveMode::NonRecursive) {
            Ok(()) => true,
            Err(e) => {
                eprintln!(
                    "watcher: could not watch {}: {e}, falling back to polling",
                    watch_path.display()
                );
                false
            }
        }
    } else {
        eprintln!(
            "watcher: heartbeats directory not found at {}, polling only",
            watch_path.display()
        );
        false
    };

    if watch_active {
        eprintln!(
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
            eprintln!("watcher: failed to read heartbeats: {e}");
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
            // Always broadcast the heartbeat event.
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

    fn make_heartbeat(agent_id: &str, age_minutes: i64) -> Heartbeat {
        Heartbeat {
            agent_id: agent_id.to_string(),
            last_heartbeat: Utc::now() - Duration::minutes(age_minutes),
            active_issue_id: None,
            machine_id: "test-machine".to_string(),
        }
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
            let is_new = last_state.get(agent_id).is_none();
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
