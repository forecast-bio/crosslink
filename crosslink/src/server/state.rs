use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex, MutexGuard};

use crate::db::Database;
use crate::server::ws::{self, WsEvent};

/// Shared application state accessible by all axum handlers.
///
/// Cloning `AppState` is cheap — all fields are `Arc`-wrapped or `Copy`.
///
/// Fields `db` and `crosslink_dir` are used by API handlers.
#[derive(Clone)]
pub struct AppState {
    /// Shared database handle — wrapped for concurrent async handler access.
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
    /// Bearer token for API authentication.
    pub auth_token: String,
}

impl AppState {
    pub fn new(db: Database, crosslink_dir: PathBuf) -> Self {
        let (ws_tx, _ws_rx) = ws::channel();
        let auth_token = generate_auth_token();
        Self {
            db: Arc::new(Mutex::new(db)),
            crosslink_dir,
            version: env!("CARGO_PKG_VERSION"),
            ws_tx,
            auth_token,
        }
    }

    /// Acquire the database lock asynchronously.
    ///
    /// Uses `tokio::sync::Mutex` which yields the async task while waiting,
    /// instead of blocking the Tokio worker thread.
    pub async fn db(&self) -> MutexGuard<'_, Database> {
        self.db.lock().await
    }
}

/// Generate a random 32-character hex token for API authentication.
fn generate_auth_token() -> String {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id() as u128;
    format!("{:032x}", seed ^ (pid << 64))
}
