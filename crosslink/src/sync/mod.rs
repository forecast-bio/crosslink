mod cache;
mod core;
mod heartbeats;
mod locks;
mod migration;
mod trust;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Once;

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
pub use self::core::SyncManager;
pub use self::locks::LockMode;

/// Read the configured tracker remote name from `.crosslink/hook-config.json`.
///
/// Returns the value of `tracker_remote` if set, otherwise `"origin"`.
/// This is a pure config read — no subprocess calls. Use
/// `SyncManager::remote_exists()` to validate the remote.
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
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            "no tracker_remote configured in {}, defaulting to \"origin\"",
            config_path.display()
        );
    });

    "origin".to_string()
}

/// Check whether a named git remote exists in the given repo directory.
///
/// Separated from `read_tracker_remote` so the config-read path stays
/// free of subprocess calls (#356). Available for callers that need to
/// validate the remote without constructing a full `SyncManager`.
#[allow(dead_code)]
pub fn validate_remote_exists(repo_root: &Path, remote: &str) -> bool {
    std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["remote", "get-url", remote])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
