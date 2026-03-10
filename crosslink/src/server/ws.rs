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
//! Channel names map to event types:
//! - `"agents"`    → `WsHeartbeatEvent`, `WsAgentStatusEvent`
//! - `"issues"`    → `WsIssueUpdatedEvent`
//! - `"locks"`     → `WsLockChangedEvent`
//! - `"execution"` → `WsExecutionProgressEvent`

use std::collections::HashSet;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
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
/// Some variants (`IssueUpdated`, `LockChanged`, `ExecutionProgress`) are
/// pre-declared for use by later phase agents; the `#[allow(dead_code)]`
/// attribute suppresses premature warnings.
#[allow(dead_code)]
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
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        match self {
            WsEvent::Heartbeat(e) => serde_json::to_string(e),
            WsEvent::AgentStatus(e) => serde_json::to_string(e),
            WsEvent::IssueUpdated(e) => serde_json::to_string(e),
            WsEvent::LockChanged(e) => serde_json::to_string(e),
            WsEvent::ExecutionProgress(e) => serde_json::to_string(e),
        }
    }
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
/// 3. Server forwards matching broadcast events as JSON text frames.
/// 4. Loop ends when the client disconnects or the broadcast sender is dropped.
async fn handle_socket(mut socket: WebSocket, tx: broadcast::Sender<WsEvent>) {
    let mut rx = tx.subscribe();

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

                        if let Ok(json) = ev.to_json() {
                            if socket.send(Message::Text(json.into())).await.is_err() {
                                // Client disconnected mid-send.
                                break;
                            }
                        }
                    }
                    // The broadcast buffer overflowed; some events were dropped
                    // for this receiver.  Continue processing future events.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
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
    fn test_broadcast_channel_capacity() {
        let (tx, rx) = channel();
        // channel() returns one initial receiver; drop it to test from zero.
        drop(rx);
        assert_eq!(tx.receiver_count(), 0);
        let _rx2 = tx.subscribe();
        assert_eq!(tx.receiver_count(), 1);
    }
}
