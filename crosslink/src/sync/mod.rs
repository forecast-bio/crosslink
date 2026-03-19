mod cache;
mod core;
mod heartbeats;
mod locks;
mod migration;
mod trust;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Directory name under .crosslink for the hub cache worktree.
pub(crate) const HUB_CACHE_DIR: &str = ".hub-cache";

/// The coordination branch name.
pub(crate) const HUB_BRANCH: &str = "crosslink/hub";

/// Maximum number of local commits ahead of remote before bailing.
/// Prevents unbounded divergence from repeated rebase-retry cycles.
const MAX_DIVERGENCE: usize = 10;

/// Old directory name (for migration from crosslink/locks).
const OLD_CACHE_DIR: &str = ".locks-cache";

/// Old branch name (for migration from crosslink/locks).
const OLD_BRANCH: &str = "crosslink/locks";

/// Re-export from `signing` module. Use `SignatureVerification` for new code.
pub use crate::signing::SignatureVerification;

/// Deprecated alias — use `SignatureVerification` instead.
pub type GpgVerification = SignatureVerification;

pub use self::core::SyncManager;

/// Read the configured tracker remote name from `.crosslink/hook-config.json`.
///
/// Returns the value of `tracker_remote` if set, otherwise `"origin"`.
pub fn read_tracker_remote(crosslink_dir: &Path) -> String {
    let config_path = crosslink_dir.join("hook-config.json");
    let configured = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|v| {
            v.get("tracker_remote")
                .and_then(|r| r.as_str().map(|s| s.to_string()))
        });

    if let Some(remote) = configured {
        return remote;
    }

    // Warn once when falling back to "origin".
    static WARNED: AtomicBool = AtomicBool::new(false);
    if !WARNED.swap(true, Ordering::Relaxed) {
        eprintln!(
            "warning: no tracker_remote configured in {}, defaulting to \"origin\"",
            config_path.display()
        );

        // Check whether the "origin" remote actually exists.
        let origin_exists = std::process::Command::new("git")
            .args(["remote", "get-url", "origin"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !origin_exists {
            eprintln!("warning: remote \"origin\" does not exist; sync operations will fail");
        }
    }

    "origin".to_string()
}
