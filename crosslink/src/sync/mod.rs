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

/// Categorization of a `git push` failure for actionable diagnostics.
///
/// Substring matching on the raw stderr is brittle (the three push sites
/// each maintained their own list and silently miscategorized misconfigured-
/// remote failures as `(offline)`); this enum gives callers a single
/// vocabulary to act on. See GH#586.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PushFailure {
    /// Network unreachable, DNS failed, connection timed out. Transient —
    /// the local cache stays consistent and the push will succeed on a
    /// later attempt once connectivity returns. Worth a `tracing::warn!`
    /// so the user knows state didn't propagate, but not an error.
    Offline,
    /// Remote name doesn't resolve to a usable git endpoint. Repeatable
    /// and fixable: the user needs to point `tracker_remote` at an
    /// existing remote or `git remote add` the missing one. `tracing::error!`
    /// with actionable guidance.
    RemoteMisconfigured { remote: String, detail: String },
    /// Push rejected because the local branch is behind. Callers typically
    /// pull-and-retry; only surface as an error when retries are exhausted.
    NonFastForward,
    /// SSH key denied, HTTP 401/403, missing token. Actionable error.
    AuthFailed,
    /// Anything not in the above buckets. Carries the raw stderr so
    /// callers can surface it verbatim.
    Other(String),
}

/// Classify a `git push` stderr blob into a `PushFailure` variant.
///
/// Patterns are matched in **specificity order** — auth phrases first
/// (some of them also match the offline patterns), then
/// remote-misconfigured, then non-fast-forward, then offline, with
/// everything else falling through to `Other`. The `remote` argument
/// is the remote name that was being pushed to; it gets carried into
/// `RemoteMisconfigured` so callers can render a precise hint.
pub(crate) fn classify_push_failure(err_str: &str, remote: &str) -> PushFailure {
    // 1. Auth — check first because failed auth often also produces
    //    "Could not read from remote repository" lower in the stderr,
    //    which would otherwise match the Offline bucket.
    let auth_markers = [
        "Permission denied",
        "Authentication failed",
        "publickey",
        " 403 ",
        " 401 ",
        "fatal: Authentication",
    ];
    if auth_markers.iter().any(|m| err_str.contains(m)) {
        return PushFailure::AuthFailed;
    }

    // 2. Remote misconfigured — explicit diagnostic git emits when the
    //    remote name doesn't resolve to a real git endpoint (either
    //    because it isn't a configured remote, or because its URL is
    //    junk).
    let misconfigured_markers = [
        "does not appear to be a git repository",
        "Repository not found",
        "is not a valid remote name",
    ];
    if misconfigured_markers.iter().any(|m| err_str.contains(m)) {
        return PushFailure::RemoteMisconfigured {
            remote: remote.to_string(),
            detail: err_str.to_string(),
        };
    }

    // 3. Non-fast-forward — local is behind remote. Git emits this
    //    family in several shapes depending on whether the rejection
    //    came from the local pre-push check, the remote update hook,
    //    or the concurrent-update race (two clients pushing the same
    //    ref with the same expected old SHA). All variants map to
    //    the same recovery action (pull/rebase + retry), so they
    //    share a bucket. Pre-fix, the concurrent-claim race
    //    miscategorized "cannot lock ref" / "remote rejected" as
    //    `Other` and silently returned "saved locally" while both
    //    agents thought they won the lock.
    let non_ff_markers = [
        "! [rejected]",
        "! [remote rejected]",
        "non-fast-forward",
        "cannot lock ref",
        "incorrect old value provided",
        "failed to push some refs",
    ];
    if non_ff_markers.iter().any(|m| err_str.contains(m)) {
        return PushFailure::NonFastForward;
    }

    // 4. Offline — network/DNS/route layer.
    let offline_markers = [
        "Could not resolve host",
        "Could not read from remote",
        "Connection timed out",
        "Network is unreachable",
        "Temporary failure in name resolution",
        "No route to host",
    ];
    if offline_markers.iter().any(|m| err_str.contains(m)) {
        return PushFailure::Offline;
    }

    PushFailure::Other(err_str.to_string())
}

impl PushFailure {
    /// Render an actionable user-facing message for non-offline failure
    /// modes. The caller decides log level (`warn!` for offline,
    /// `error!` for everything else); this helper centralizes the text.
    pub(crate) fn user_message(&self, action: &str) -> String {
        match self {
            PushFailure::Offline => {
                format!("push failed (offline), {action} saved locally only")
            }
            PushFailure::RemoteMisconfigured { remote, .. } => format!(
                "push failed: remote '{remote}' is not a valid git endpoint. \
                 Configure it with `crosslink config set tracker_remote <existing-remote>`, \
                 or add a new one with `git remote add {remote} <url>`. \
                 Local state for {action} is preserved."
            ),
            PushFailure::NonFastForward => format!(
                "push failed: local branch is behind remote (non-fast-forward). \
                 Run `crosslink sync` to reconcile. {action} saved locally."
            ),
            PushFailure::AuthFailed => format!(
                "push failed: authentication denied by the remote. \
                 Check your SSH key (`ssh -T git@<host>`) or your auth token. \
                 {action} saved locally."
            ),
            PushFailure::Other(detail) => {
                format!("push failed with unexpected error: {detail}. {action} saved locally.")
            }
        }
    }
}

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
