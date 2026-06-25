pub mod bootstrap;
mod cache;
mod core;
mod heartbeats;
mod locks;
mod migration;
mod trust;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::process::Command;
use std::sync::Once;

/// Directory name under .crosslink for the hub cache worktree.
pub(crate) const HUB_CACHE_DIR: &str = ".hub-cache";

/// The legacy v2 coordination branch name. Its presence is what
/// [`crate::hub_v3::detect_hub_version`] reads as "a v2 hub exists", so a v3 hub
/// must NEVER create this branch as a worktree host.
pub(crate) const HUB_BRANCH: &str = "crosslink/hub";

/// Branch hosting the v3 hub-cache working directory. The branch carries no hub
/// data (v3 state lives in `refs/heads/crosslink/*`); it exists only so the cache is a
/// valid git worktree whose `.git` link shares the main repo's ref namespace.
/// Deliberately distinct from [`HUB_BRANCH`] so detection never mistakes a fresh
/// v3 hub for a v2 one.
pub(crate) const HUB_V3_HOST_BRANCH: &str = "crosslink/hub-v3-host";

/// Old directory name (for migration from crosslink/locks).
const OLD_CACHE_DIR: &str = ".locks-cache";

/// Old branch name (for migration from crosslink/locks).
const OLD_BRANCH: &str = "crosslink/locks";

/// Re-export from `signing` module. Use `SignatureVerification` for new code.
pub use crate::signing::SignatureVerification;

/// Same-machine hub write lock guard. Re-exported so `compaction::compact`
/// can require it as proof that the caller holds the process mutex.
pub use self::cache::HubWriteLock;
// acquire_hub_lock is re-exported for test helpers in compaction and shared_writer.
// In production code, callers acquire the lock via SyncManager::acquire_lock().
#[cfg(test)]
pub use self::cache::acquire_hub_lock;
pub use self::core::SyncManager;

/// Read the configured tracker remote name for hub sync operations.
///
/// Resolution order:
///
/// 1. If `hook-config.json` sets `tracker_remote` to a non-placeholder
///    string, use it verbatim. The GH#739 `"(text)"` corruption sentinel
///    is still detected here and a single WARN is emitted before
///    falling through to inference.
/// 2. Otherwise infer from the project's git remotes (see
///    [`infer_tracker_remote`]). This replaces the noisy per-invocation
///    `"no tracker_remote configured"` warning that used to fire on
///    every command in fresh projects (GH#611). The common case — a
///    single `origin` remote — is now resolved silently.
/// 3. If the repo has no remotes at all, fall back to `"origin"` and
///    emit a single WARN, since `crosslink sync` will fail without a
///    remote and the user needs to know.
///
/// Use [`SyncManager::remote_exists`] to validate the result against
/// the actual git config before any push/fetch.
pub fn read_tracker_remote(crosslink_dir: &Path) -> String {
    static CORRUPT_WARNED: Once = Once::new();

    let config_path = crosslink_dir.join("hook-config.json");
    let configured = std::fs::read_to_string(&config_path)
        .ok()
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
        .and_then(|v| {
            v.get("tracker_remote")
                .and_then(|r| r.as_str().map(std::string::ToString::to_string))
        });

    if let Some(remote) = configured {
        // GH#739 — pre-fix builds of the init walkthrough wrote the
        // TUI placeholder "(text)" into hook-config.json for every
        // `ConfigType::String` key. Detect that here and warn the
        // user once, falling back to inference so sync doesn't bail
        // with a (correct but unhelpful) RemoteMisconfigured error.
        // The permanent fix is `crosslink config set tracker_remote
        // <name>` or `crosslink init --force` (which now auto-repairs
        // the corrupt placeholder).
        if remote == "(text)" {
            CORRUPT_WARNED.call_once(|| {
                tracing::warn!(
                    "tracker_remote in {} is the corrupt placeholder \"(text)\" \
                     (GH#739). Falling back to inferred remote. Repair with: \
                     `crosslink config set tracker_remote <name>` or \
                     `crosslink init --force`.",
                    config_path.display()
                );
            });
            // fall through to inference rather than blindly returning "origin"
        } else {
            return remote;
        }
    }

    // GH#611: silently infer from git remotes instead of WARN-and-default.
    let repo_path = crosslink_dir.parent().unwrap_or(crosslink_dir);
    infer_tracker_remote(repo_path)
}

/// List the names of git remotes configured for `repo_path`, alphabetically.
///
/// Returns an empty vec when the directory isn't a git repo or `git remote`
/// fails for any other reason — the caller treats "no remotes" as a soft
/// signal, not a hard error. Sort is deterministic so callers picking
/// "first alphabetical" get repeatable behaviour across machines.
fn list_git_remotes(repo_path: &Path) -> Vec<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["remote"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let mut remotes: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    remotes.sort();
    remotes
}

/// Infer the tracker remote name from the project's git remotes when
/// `hook-config.json` doesn't set one (GH#611).
///
/// Selection rules — designed so >99% of projects (single `origin`
/// remote) resolve silently:
///
/// - Any remote named `origin` exists → use `"origin"`. Covers
///   single-remote-named-origin and multi-remote-with-origin cases.
/// - Otherwise, if at least one remote exists → use the first
///   alphabetically. Deterministic across machines.
/// - Zero remotes → default to `"origin"` and emit a single WARN.
///   The user will need to `git remote add` before sync works, and
///   this is the one case where a real diagnostic is warranted.
fn infer_tracker_remote(repo_path: &Path) -> String {
    static NO_REMOTE_WARNED: Once = Once::new();

    let remotes = list_git_remotes(repo_path);
    if remotes.iter().any(|r| r == "origin") {
        return "origin".to_string();
    }
    if let Some(first) = remotes.first() {
        return first.clone();
    }

    NO_REMOTE_WARNED.call_once(|| {
        tracing::warn!(
            "no git remote configured in {}; defaulting tracker_remote to \"origin\". \
             Add a remote with `git remote add origin <url>` before `crosslink sync`.",
            repo_path.display()
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
#[must_use]
pub fn validate_remote_exists(repo_root: &Path, remote: &str) -> bool {
    std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["remote", "get-url", remote])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
