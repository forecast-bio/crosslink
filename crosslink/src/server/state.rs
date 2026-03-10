use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::db::Database;

/// Shared application state accessible by all axum handlers.
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
}

impl AppState {
    pub fn new(db: Database, crosslink_dir: PathBuf) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            crosslink_dir,
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}
