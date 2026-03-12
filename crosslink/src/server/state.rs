use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::broadcast;

use crate::db::Database;
use crate::server::ws::{self, WsEvent};

/// Shared application state accessible by all axum handlers.
///
/// Cloning `AppState` is cheap — all fields are `Arc`-wrapped or `Copy`.
///
/// Fields `db` and `crosslink_dir` are used by API handlers.
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

    /// Acquire the database lock, recovering from mutex poisoning.
    ///
    /// `std::sync::Mutex` becomes permanently poisoned if a thread panics while
    /// holding the guard.  Because the `Database` (backed by SQLite) is
    /// transactional, a panic leaves the DB in a consistent state — so we can
    /// safely recover by accepting the poisoned guard via `into_inner`.
    pub fn db(&self) -> MutexGuard<'_, Database> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }
}
