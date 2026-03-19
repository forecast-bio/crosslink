//! WebSocket hub for real-time event broadcasting.
//!
//! Clients connect to `/ws`, optionally send a `subscribe` message to filter
//! channels, and receive JSON events pushed by the server.
//!
//! # Architecture
//!
//! A single `tokio::sync::broadcast` channel carries all `WsEvent` variants.
//! Each connected client runs its own task that reads from a
//! `broadcast::Receiver` and forwards matching events as JSON text frames.
//!
//! Every outgoing message is wrapped in an envelope with a monotonically
//! increasing `seq` field so clients can detect gaps caused by backpressure.
//!
//! Channel names map to event types:
//! - `"agents"`    → `WsHeartbeatEvent`, `WsAgentStatusEvent`
//! - `"issues"`    → `WsIssueUpdatedEvent`
//! - `"locks"`     → `WsLockChangedEvent`
//! - `"execution"` → `WsExecutionProgressEvent`

use std::collections::HashSet;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::server::state::AppState;
use crate::server::types::{
    WsAgentStatusEvent, WsExecutionProgressEvent, WsHeartbeatEvent, WsIssueUpdatedEvent,
    WsLockChangedEvent, WsSubscribeMessage,
};

/// Internal channel capacity.  256 slots before lagged receivers start dropping.
pub const BROADCAST_CAPACITY: usize = 256;

/// All events that can be broadcast over the WebSocket hub.
///
/// Each variant carries the concrete event struct defined in `types.rs`.
/// `Clone` is required by `tokio::sync::broadcast`.
///
/// All variants are used by their respective handlers.
#[derive(Debug, Clone)]
pub enum WsEvent {
    Heartbeat(WsHeartbeatEvent),
    AgentStatus(WsAgentStatusEvent),
    IssueUpdated(WsIssueUpdatedEvent),
    LockChanged(WsLockChangedEvent),
    ExecutionProgress(WsExecutionProgressEvent),
}

impl WsEvent {
    /// Returns the channel name for this event (used to filter subscriptions).
    pub fn channel(&self) -> &'static str {
        match self {
            WsEvent::Heartbeat(_) | WsEvent::AgentStatus(_) => "agents",
            WsEvent::IssueUpdated(_) => "issues",
            WsEvent::LockChanged(_) => "locks",
            WsEvent::ExecutionProgress(_) => "execution",
        }
    }

    /// Serialize this event to a JSON string.
    #[cfg(test)]
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        match self {
            WsEvent::Heartbeat(e) => serde_json::to_string(e),
            WsEvent::AgentStatus(e) => serde_json::to_string(e),
            WsEvent::IssueUpdated(e) => serde_json::to_string(e),
            WsEvent::LockChanged(e) => serde_json::to_string(e),
            WsEvent::ExecutionProgress(e) => serde_json::to_string(e),
        }
    }

    /// Serialize this event to a `serde_json::Value` for embedding in a
    /// `WsEnvelope`.
    pub fn to_json_value(&self) -> Result<serde_json::Value, serde_json::Error> {
        match self {
            WsEvent::Heartbeat(e) => serde_json::to_value(e),
            WsEvent::AgentStatus(e) => serde_json::to_value(e),
            WsEvent::IssueUpdated(e) => serde_json::to_value(e),
            WsEvent::LockChanged(e) => serde_json::to_value(e),
            WsEvent::ExecutionProgress(e) => serde_json::to_value(e),
        }
    }
}

/// Envelope wrapping every outgoing WebSocket message.
///
/// The `seq` field is a per-connection monotonically increasing counter that
/// starts at 1.  Clients can detect dropped messages by checking for gaps in
/// the sequence.  When a gap occurs (broadcast buffer overflow), the server
/// sends a synthetic message with `"type": "gap"` so the client knows to
/// re-sync.
#[derive(Debug, Clone, Serialize)]
pub struct WsEnvelope {
    /// Per-connection sequence number (starts at 1, never resets).
    pub seq: u64,
    /// The inner event payload (flattened into this object).
    #[serde(flatten)]
    pub data: serde_json::Value,
}

/// Create a new broadcast channel for WebSocket events.
///
/// Returns `(Sender, Receiver)`.  The `Sender` is stored in `AppState`;
/// each new WebSocket client subscribes from it.
pub fn channel() -> (broadcast::Sender<WsEvent>, broadcast::Receiver<WsEvent>) {
    broadcast::channel(BROADCAST_CAPACITY)
}

/// HTTP handler — upgrades the connection to WebSocket and hands it off to
/// `handle_socket`.
///
/// Registered at `GET /ws` in the router.
pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state.ws_tx))
}

/// Handle a single WebSocket client for the duration of its connection.
///
/// # Protocol
///
/// 1. Client connects.
/// 2. Client **may** send a `subscribe` message to restrict which channels it
///    receives.  If omitted, the client receives all channels.
/// 3. Server forwards matching broadcast events as JSON text frames, each
///    wrapped in a `WsEnvelope` with a monotonically increasing `seq` field.
/// 4. If the broadcast buffer overflows, the server sends a synthetic `gap`
///    message with the number of dropped events so the client can re-sync.
/// 5. Loop ends when the client disconnects or the broadcast sender is dropped.
async fn handle_socket(mut socket: WebSocket, tx: broadcast::Sender<WsEvent>) {
    let mut rx = tx.subscribe();

    // Per-connection sequence counter.  Starts at 1 so clients can use 0 as
    // a sentinel for "no messages received yet".
    let mut seq: u64 = 0;

    // None → client has not filtered; receives all channels.
    let mut subscribed: Option<HashSet<String>> = None;

    loop {
        tokio::select! {
            // Message arriving from the client.
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        // Only act on well-formed `subscribe` messages.
                        if let Ok(sub) = serde_json::from_str::<WsSubscribeMessage>(&text) {
                            if sub.message_type == "subscribe" {
                                subscribed = Some(sub.channels.into_iter().collect());
                            }
                        }
                    }
                    // Client sent Close frame or the stream ended.
                    Some(Ok(Message::Close(_))) | None => break,
                    // Ping/pong and binary frames are not used by this protocol.
                    _ => {}
                }
            }

            // Event arriving from the broadcast channel.
            event = rx.recv() => {
                match event {
                    Ok(ev) => {
                        // If the client subscribed to specific channels, skip
                        // events that are not in the subscriber's set.
                        if let Some(ref channels) = subscribed {
                            if !channels.contains(ev.channel()) {
                                continue;
                            }
                        }

                        if let Ok(data) = ev.to_json_value() {
                            seq += 1;
                            let envelope = WsEnvelope { seq, data };
                            if let Ok(json) = serde_json::to_string(&envelope) {
                                if socket.send(Message::Text(json.into())).await.is_err() {
                                    // Client disconnected mid-send.
                                    break;
                                }
                            }
                        }
                    }
                    // The broadcast buffer overflowed; some events were dropped
                    // for this receiver.  Send a gap notification so the client
                    // knows to re-sync.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("ws: client lagged, {n} events dropped");
                        seq += 1;
                        let gap = serde_json::json!({
                            "seq": seq,
                            "type": "gap",
                            "dropped": n,
                        });
                        if let Ok(json) = serde_json::to_string(&gap) {
                            if socket.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    // The sender was dropped — the server is shutting down.
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::types::{AgentStatus, WsAgentStatusEvent, WsHeartbeatEvent};
    use chrono::Utc;

    #[test]
    fn test_ws_event_channel_heartbeat() {
        let ev = WsEvent::Heartbeat(WsHeartbeatEvent {
            event_type: "heartbeat",
            agent_id: "a1".to_string(),
            timestamp: Utc::now(),
            active_issue_id: None,
        });
        assert_eq!(ev.channel(), "agents");
    }

    #[test]
    fn test_ws_event_channel_agent_status() {
        let ev = WsEvent::AgentStatus(WsAgentStatusEvent {
            event_type: "agent_status",
            agent_id: "a1".to_string(),
            status: AgentStatus::Active,
        });
        assert_eq!(ev.channel(), "agents");
    }

    #[test]
    fn test_ws_event_to_json_heartbeat() {
        let ev = WsEvent::Heartbeat(WsHeartbeatEvent {
            event_type: "heartbeat",
            agent_id: "worker-1".to_string(),
            timestamp: Utc::now(),
            active_issue_id: Some(42),
        });
        let json = ev.to_json().unwrap();
        assert!(json.contains("\"type\":\"heartbeat\""));
        assert!(json.contains("\"agent_id\":\"worker-1\""));
        assert!(json.contains("\"active_issue_id\":42"));
    }

    #[test]
    fn test_ws_event_to_json_agent_status() {
        let ev = WsEvent::AgentStatus(WsAgentStatusEvent {
            event_type: "agent_status",
            agent_id: "worker-2".to_string(),
            status: AgentStatus::Idle,
        });
        let json = ev.to_json().unwrap();
        assert!(json.contains("\"type\":\"agent_status\""));
        assert!(json.contains("\"status\":\"idle\""));
    }

    #[test]
    fn test_ws_event_to_json_value_heartbeat() {
        let ev = WsEvent::Heartbeat(WsHeartbeatEvent {
            event_type: "heartbeat",
            agent_id: "worker-1".to_string(),
            timestamp: Utc::now(),
            active_issue_id: Some(42),
        });
        let val = ev.to_json_value().unwrap();
        assert_eq!(val["type"], "heartbeat");
        assert_eq!(val["agent_id"], "worker-1");
        assert_eq!(val["active_issue_id"], 42);
    }

    #[test]
    fn test_ws_envelope_contains_seq_and_event_fields() {
        let ev = WsEvent::AgentStatus(WsAgentStatusEvent {
            event_type: "agent_status",
            agent_id: "worker-1".to_string(),
            status: AgentStatus::Active,
        });
        let data = ev.to_json_value().unwrap();
        let envelope = WsEnvelope { seq: 7, data };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // The envelope should have `seq` at the top level.
        assert_eq!(parsed["seq"], 7);
        // The inner event fields should be flattened into the top level.
        assert_eq!(parsed["type"], "agent_status");
        assert_eq!(parsed["agent_id"], "worker-1");
        assert_eq!(parsed["status"], "active");
    }

    #[test]
    fn test_ws_envelope_seq_increments() {
        let ev = WsEvent::Heartbeat(WsHeartbeatEvent {
            event_type: "heartbeat",
            agent_id: "a1".to_string(),
            timestamp: Utc::now(),
            active_issue_id: None,
        });

        let data1 = ev.to_json_value().unwrap();
        let env1 = WsEnvelope {
            seq: 1,
            data: data1,
        };
        let data2 = ev.to_json_value().unwrap();
        let env2 = WsEnvelope {
            seq: 2,
            data: data2,
        };

        let j1: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env1).unwrap()).unwrap();
        let j2: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&env2).unwrap()).unwrap();

        assert_eq!(j1["seq"], 1);
        assert_eq!(j2["seq"], 2);
    }

    #[test]
    fn test_broadcast_channel_capacity() {
        let (tx, rx) = channel();
        // channel() returns one initial receiver; drop it to test from zero.
        drop(rx);
        assert_eq!(tx.receiver_count(), 0);
        let _rx2 = tx.subscribe();
        assert_eq!(tx.receiver_count(), 1);
    }

    #[test]
    fn test_ws_event_channel_issue_updated() {
        let ev = WsEvent::IssueUpdated(crate::server::types::WsIssueUpdatedEvent {
            event_type: "issue_updated",
            issue_id: 1,
            field: "status".to_string(),
        });
        assert_eq!(ev.channel(), "issues");
    }

    #[test]
    fn test_ws_event_channel_lock_changed() {
        let ev = WsEvent::LockChanged(crate::server::types::WsLockChangedEvent {
            event_type: "lock_changed",
            issue_id: 1,
            action: crate::server::types::LockAction::Claimed,
            agent_id: "a1".to_string(),
        });
        assert_eq!(ev.channel(), "locks");
    }

    #[test]
    fn test_ws_event_channel_execution_progress() {
        let ev = WsEvent::ExecutionProgress(crate::server::types::WsExecutionProgressEvent {
            event_type: "execution_progress",
            plan_id: "p1".to_string(),
            phase_id: "ph1".to_string(),
            stage_id: "s1".to_string(),
            status: crate::server::types::StageStatus::Running,
            agent_id: None,
        });
        assert_eq!(ev.channel(), "execution");
    }

    #[test]
    fn test_ws_event_to_json_issue_updated() {
        let ev = WsEvent::IssueUpdated(crate::server::types::WsIssueUpdatedEvent {
            event_type: "issue_updated",
            issue_id: 42,
            field: "title".to_string(),
        });
        let json = ev.to_json().unwrap();
        assert!(json.contains("\"issue_id\":42"));
        let val = ev.to_json_value().unwrap();
        assert_eq!(val["type"], "issue_updated");
    }

    #[test]
    fn test_ws_event_to_json_lock_changed() {
        let ev = WsEvent::LockChanged(crate::server::types::WsLockChangedEvent {
            event_type: "lock_changed",
            issue_id: 5,
            action: crate::server::types::LockAction::Released,
            agent_id: "bot".to_string(),
        });
        let json = ev.to_json().unwrap();
        assert!(json.contains("\"action\":\"released\""));
        let val = ev.to_json_value().unwrap();
        assert_eq!(val["type"], "lock_changed");
    }

    #[test]
    fn test_ws_event_to_json_execution_progress() {
        let ev = WsEvent::ExecutionProgress(crate::server::types::WsExecutionProgressEvent {
            event_type: "execution_progress",
            plan_id: "p1".to_string(),
            phase_id: "ph1".to_string(),
            stage_id: "s1".to_string(),
            status: crate::server::types::StageStatus::Done,
            agent_id: Some("agent-x".to_string()),
        });
        let json = ev.to_json().unwrap();
        assert!(json.contains("\"status\":\"done\""));
        let val = ev.to_json_value().unwrap();
        assert_eq!(val["type"], "execution_progress");
        assert_eq!(val["agent_id"], "agent-x");
    }
}
