use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::db::Database;
use crate::server::ws::{self, WsEvent};

/// Shared application state accessible by all axum handlers.
///
/// Cloning `AppState` is cheap — all fields are `Arc`-wrapped or `Copy`.
///
/// Fields `db` and `crosslink_dir` are used by API handlers added in later
/// phases; `#[allow(dead_code)]` suppresses the false-positive until then.
#[allow(dead_code)]
#[derive(Clone)]
pub struct AppState {
    /// Shared database handle — wrapped for concurrent handler access.
    pub db: Arc<Mutex<Database>>,
    /// Path to the `.crosslink` directory (used to construct SyncManager on demand).
    pub crosslink_dir: PathBuf,
    /// Crosslink version string for health/info responses.
    pub version: &'static str,
    /// Sender side of the WebSocket broadcast channel.
    ///
    /// Handlers that mutate state (e.g. issues, sessions) can push events here
    /// to notify all connected WebSocket clients in real-time.
    pub ws_tx: broadcast::Sender<WsEvent>,
}

impl AppState {
    pub fn new(db: Database, crosslink_dir: PathBuf) -> Self {
        let (ws_tx, _ws_rx) = ws::channel();
        Self {
            db: Arc::new(Mutex::new(db)),
            crosslink_dir,
            version: env!("CARGO_PKG_VERSION"),
            ws_tx,
        }
    }
}
