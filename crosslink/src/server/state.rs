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
    /// Path to the `.crosslink` directory (used to construct `SyncManager` on demand).
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
    /// Path to the per-user dashboard DB (`~/.crosslink/dashboard.db`),
    /// populated only when the process was launched via `crosslink
    /// dashboard serve`. `None` for the deprecated `crosslink serve`
    /// path. Dashboard API handlers open fresh connections from this
    /// path per request (`SQLite` opens are cheap).
    pub dashboard_db_path: Option<PathBuf>,
    /// In-process registry of live PTY sessions backing the embedded
    /// terminal. Empty until the first `/api/v1/pty` POST.
    pub pty_registry: crate::dashboard::pty::SessionRegistry,
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
            dashboard_db_path: None,
            pty_registry: crate::dashboard::pty::SessionRegistry::new(),
        }
    }

    /// Attach a dashboard DB path for the dashboard API handlers.
    /// Returns `self` to enable builder-style chaining at server startup.
    #[must_use]
    pub fn with_dashboard_db(mut self, path: PathBuf) -> Self {
        self.dashboard_db_path = Some(path);
        self
    }

    /// Acquire the database lock asynchronously.
    ///
    /// Uses `tokio::sync::Mutex` which yields the async task while waiting,
    /// instead of blocking the Tokio worker thread.
    pub async fn db(&self) -> MutexGuard<'_, Database> {
        self.db.lock().await
    }
}

/// Produce the auth token the server should bind this run:
///
/// 1. If `~/.crosslink/.dashboard-token` exists and contains a valid
///    32-char lowercase-hex string, reuse it. This is what keeps
///    open browser tabs working across binary rebuilds.
/// 2. Otherwise generate a fresh 128-bit random token, write it to
///    that path with `0600` perms, and return it.
///
/// Any step can fall back to in-memory-only generation (if `$HOME`
/// can't be resolved, if /dev/urandom fails, etc.) — the server
/// still works in that degraded mode, tabs just 401 on restart.
fn generate_auth_token() -> String {
    if let Some(path) = token_path() {
        if let Ok(saved) = std::fs::read_to_string(&path) {
            let trimmed = saved.trim();
            if is_valid_hex_token(trimmed) {
                return trimmed.to_string();
            }
        }
        let fresh = fresh_hex_token();
        let _ = std::fs::create_dir_all(path.parent().unwrap_or_else(|| std::path::Path::new("/")));
        if std::fs::write(&path, &fresh).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
        }
        return fresh;
    }
    fresh_hex_token()
}

/// Rotate the persisted token: delete the file and return a fresh
/// one. Called when the operator passes `--rotate-token`.
pub fn rotate_auth_token() -> String {
    if let Some(path) = token_path() {
        let _ = std::fs::remove_file(&path);
    }
    generate_auth_token()
}

fn token_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".crosslink").join(".dashboard-token"))
}

fn is_valid_hex_token(s: &str) -> bool {
    s.len() == 32
        && s.chars()
            .all(|c| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_ascii_lowercase()))
}

fn fresh_hex_token() -> String {
    let mut buf = [0u8; 16];
    #[cfg(unix)]
    {
        use std::io::Read;
        // Bounded read — `/dev/urandom` has no EOF; `std::fs::read`
        // would loop forever (same root cause as #706).
        if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
            let _ = f.read_exact(&mut buf);
        }
    }
    if buf == [0u8; 16] {
        // urandom unavailable — fall back to time+pid mixed. Not
        // cryptographically ideal, but better than an all-zero token.
        use std::time::SystemTime;
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = u128::from(std::process::id());
        let mixed = nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(pid);
        buf.copy_from_slice(&mixed.to_le_bytes());
    }
    let mut hex = String::with_capacity(32);
    for byte in buf {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}
