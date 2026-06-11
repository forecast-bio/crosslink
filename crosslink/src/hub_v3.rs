//! PR2 of hub v3 (`.design/hub-v3-per-agent-refs.md` REQ-1/REQ-2) — plumbing-only
//! writes to per-agent refs; no index, no worktree, no checkout; always-fast-forward
//! pushes; dual-write shadow mode pending integration.
//!
//! Each agent writes exclusively to `refs/crosslink/agents/<agent-id>` via the git
//! plumbing commands `hash-object`, `mktree`, `commit-tree`, and `update-ref`. No
//! shared worktree, no `git add`, no index mutations. A crash anywhere before the
//! final `update-ref` leaves loose orphan objects in the repository but the ref
//! itself unmoved — the repository remains consistent and the next call succeeds
//! from the last committed state.
//!
//! Integration into [`crate::shared_writer`], config flags, and the integrity
//! command are tracked in a separate follow-up task.
//!
//! # Design reference
//!
//! See `.design/hub-v3-per-agent-refs.md`, REQ-1, REQ-2, and the Write path
//! section.

use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::events::{read_events_from_bytes, EventEnvelope};
use crate::utils::is_windows_reserved_name;

// ── Constants and ref name helpers ───────────────────────────────────

/// Namespace prefix for per-agent hub refs.
pub const AGENT_REF_PREFIX: &str = "refs/crosslink/agents/";

/// Build the full ref name for an agent.
///
/// Validates the agent ID (3–64 characters, alphanumeric plus `-` and `_`;
/// same rules as [`crate::identity::AgentConfig`]) and returns the qualified
/// ref `refs/crosslink/agents/<agent_id>`.
///
/// # Errors
///
/// Returns an error if `agent_id` is empty, too short, too long, contains
/// invalid characters, or is a Windows-reserved filename.
pub fn agent_ref_name(agent_id: &str) -> Result<String> {
    validate_agent_id(agent_id)?;
    Ok(format!("{AGENT_REF_PREFIX}{agent_id}"))
}

/// Validate an agent ID using the same rules as `identity::AgentConfig`.
///
/// Rules: non-empty, 3–64 characters, all chars alphanumeric, `-`, or `_`,
/// not a Windows-reserved filename.
fn validate_agent_id(agent_id: &str) -> Result<()> {
    anyhow::ensure!(!agent_id.is_empty(), "agent_id cannot be empty");
    anyhow::ensure!(
        agent_id.len() >= 3,
        "agent_id must be at least 3 characters"
    );
    anyhow::ensure!(agent_id.len() <= 64, "agent_id must be <= 64 characters");
    anyhow::ensure!(
        agent_id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
        "agent_id must be alphanumeric with hyphens/underscores only, got: {agent_id}"
    );
    anyhow::ensure!(
        !is_windows_reserved_name(agent_id),
        "agent_id '{agent_id}' is a Windows reserved filename and cannot be used"
    );
    Ok(())
}

// ── Public outcome types ─────────────────────────────────────────────

/// Result of a successful [`append_event_to_ref`] call.
// The fields are read by tests and will be used by PR3 callers; the bin's
// duplicate module tree flags them as dead code because the shadow-write
// production caller currently discards the return value with Ok(_).
#[derive(Debug)]
#[allow(dead_code)]
pub struct RefAppendOutcome {
    /// SHA of the newly created commit.
    pub new_commit: String,
    /// SHA of the previous ref tip, or `None` for a genesis write.
    pub old_commit: Option<String>,
    /// Total number of events in the log after the append (existing + 1).
    pub events_in_log: usize,
}

/// Outcome of a [`push_agent_ref`] call.
pub enum PushOutcome {
    /// The push succeeded and the remote ref was updated.
    Pushed,
    /// The push was rejected because it would not be a fast-forward.
    ///
    /// Per REQ-1 this indicates identity collision or history tampering and
    /// must never be silently rebased. The caller decides how loud to be.
    NonFastForward,
    /// The named remote does not exist in this repository.
    NoRemote,
    /// The push failed for any other reason. The message contains git stderr.
    Failed(String),
}

// ── AbortPoint (test-only) ───────────────────────────────────────────

/// Points at which [`append_event_to_ref_with_abort`] can inject an early
/// return, simulating a crash or kill signal during the plumbing sequence.
///
/// Used by the crash-injection harness (AC-2).
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum AbortPoint {
    /// Abort after `git hash-object -w` but before `git mktree`.
    HashObject,
    /// Abort after `git mktree` but before `git commit-tree`.
    Mktree,
    /// Abort after `git commit-tree` but before `git update-ref`.
    CommitTree,
}

// ── Core write path ──────────────────────────────────────────────────

/// Append a single event to a per-agent ref using git plumbing.
///
/// The plumbing sequence (steps a–f) only creates loose objects. A crash
/// anywhere before step g leaves the ref untouched and the repository
/// consistent; the next call will re-read the current tip and proceed from
/// there.
///
/// Steps a–f: create loose objects only.
/// Step g: atomic CAS `update-ref` — the ONLY step that moves the ref.
///
/// # Errors
///
/// - Returns `"ref moved concurrently: <ref>"` when the CAS fails because
///   another writer updated the ref between the read (step a) and the
///   update (step g). The caller must re-read and retry; this function does
///   NOT retry internally.
/// - Returns an error if any plumbing command fails, or if the existing log
///   fails to parse (corrupt ref is refused, not silently extended).
pub fn append_event_to_ref(
    repo_dir: &Path,
    agent_id: &str,
    envelope: &EventEnvelope,
) -> Result<RefAppendOutcome> {
    #[cfg(test)]
    return append_inner(repo_dir, agent_id, envelope, None);
    #[cfg(not(test))]
    append_inner(repo_dir, agent_id, envelope)
}

/// Inner plumbing sequence, shared between the production function and the
/// crash-injection variant.
///
/// Under `#[cfg(test)]` the `abort` parameter allows the test harness to
/// inject early exits after any plumbing step. Under `#[cfg(not(test))]`
/// the parameter is absent and the compiler optimises the match away entirely.
#[cfg(test)]
fn append_inner(
    repo_dir: &Path,
    agent_id: &str,
    envelope: &EventEnvelope,
    abort: Option<AbortPoint>,
) -> Result<RefAppendOutcome> {
    append_inner_impl(repo_dir, agent_id, envelope, abort)
}

#[cfg(not(test))]
fn append_inner(
    repo_dir: &Path,
    agent_id: &str,
    envelope: &EventEnvelope,
) -> Result<RefAppendOutcome> {
    append_inner_impl(repo_dir, agent_id, envelope, None::<()>)
}

/// The single plumbing sequence implementation.
///
/// `abort_opt` is `Option<AbortPoint>` under `#[cfg(test)]` and
/// `Option<()>` (always `None`) under `#[cfg(not(test))]`. The
/// inner abort checks are dead-code-eliminated in the release build.
fn append_inner_impl<A: IntoAbortPoint>(
    repo_dir: &Path,
    agent_id: &str,
    envelope: &EventEnvelope,
    abort_opt: A,
) -> Result<RefAppendOutcome> {
    validate_agent_id(agent_id)?;
    let ref_name = format!("{AGENT_REF_PREFIX}{agent_id}");

    // ── Step a: resolve current tip ─────────────────────────────────
    let old_commit = git_rev_parse_optional(repo_dir, &ref_name)?;

    // ── Step b: read and validate the existing log ───────────────────
    let existing_bytes: Vec<u8> = match &old_commit {
        None => Vec::new(),
        Some(sha) => {
            let spec = format!("{sha}:events.log");
            git_cat_file_blob_optional(repo_dir, &spec)?.unwrap_or_default()
        }
    };

    // Validate the existing log before appending — a corrupt ref must error,
    // not be silently extended.
    let existing_events = read_events_from_bytes(&existing_bytes)
        .with_context(|| format!("corrupt events.log on ref '{ref_name}'; refusing to extend"))?;
    let events_in_log = existing_events.len() + 1;

    // ── Step c: serialise the new line ───────────────────────────────
    let new_line = serde_json::to_string(envelope).context("failed to serialise event envelope")?;
    let mut new_bytes = existing_bytes;
    new_bytes.extend_from_slice(new_line.as_bytes());
    new_bytes.push(b'\n');

    // ── Step d: hash-object -w ───────────────────────────────────────
    let blob_sha = git_hash_object(repo_dir, &new_bytes)?;

    if abort_opt.should_abort_after_hash_object() {
        // Test-only early exit after hash-object. Ref is unmoved; loose blob
        // is an orphan but `git fsck --strict` exits 0 (dangling objects are
        // warnings, not errors).
        return Ok(RefAppendOutcome {
            new_commit: String::new(),
            old_commit,
            events_in_log,
        });
    }

    // ── Step e: mktree ───────────────────────────────────────────────
    let tree_sha = git_mktree(repo_dir, &blob_sha)?;

    if abort_opt.should_abort_after_mktree() {
        return Ok(RefAppendOutcome {
            new_commit: String::new(),
            old_commit,
            events_in_log,
        });
    }

    // ── Step f: commit-tree ──────────────────────────────────────────
    let commit_msg = format!(
        "crosslink event: agent {} seq {}",
        agent_id, envelope.agent_seq
    );
    let commit_sha = git_commit_tree(
        repo_dir,
        &tree_sha,
        old_commit.as_deref(),
        &commit_msg,
        agent_id,
    )?;

    if abort_opt.should_abort_after_commit_tree() {
        return Ok(RefAppendOutcome {
            new_commit: String::new(),
            old_commit,
            events_in_log,
        });
    }

    // ── Step g: atomic CAS update-ref ───────────────────────────────
    git_update_ref_cas(repo_dir, &ref_name, &commit_sha, old_commit.as_deref())?;

    Ok(RefAppendOutcome {
        new_commit: commit_sha,
        old_commit,
        events_in_log,
    })
}

/// Commit a complete `events.log` byte image onto the agent's ref.
///
/// Used by offline display-id claim rewrites (`set_issue_created_claim_in_log`):
/// when the v2 log is rewritten in place, the shadow ref must be brought back
/// to byte parity or `integrity hubv3` would flag a mismatch at the rewritten
/// seq. The new state is added as a CHILD commit of the current tip — history
/// is preserved and any subsequent push remains fast-forward (the parity check
/// reads only the tip's `events.log`).
///
/// The bytes are validated as a parseable event log before anything is
/// written. Same crash invariant as [`append_event_to_ref`]: the ref only
/// moves at the final CAS `update-ref`.
///
/// # Errors
///
/// Returns an error if the bytes do not parse as an event log, if any git
/// plumbing step fails, or if the ref moved concurrently (CAS failure).
pub fn commit_log_bytes(
    repo_dir: &Path,
    agent_id: &str,
    log_bytes: &[u8],
    message: &str,
) -> Result<String> {
    validate_agent_id(agent_id)?;
    let ref_name = format!("{AGENT_REF_PREFIX}{agent_id}");

    read_events_from_bytes(log_bytes)
        .context("refusing to commit unparseable events.log bytes to the agent ref")?;

    let old_commit = git_rev_parse_optional(repo_dir, &ref_name)?;
    let blob_sha = git_hash_object(repo_dir, log_bytes)?;
    let tree_sha = git_mktree(repo_dir, &blob_sha)?;
    let commit_sha = git_commit_tree(
        repo_dir,
        &tree_sha,
        old_commit.as_deref(),
        message,
        agent_id,
    )?;
    git_update_ref_cas(repo_dir, &ref_name, &commit_sha, old_commit.as_deref())?;
    Ok(commit_sha)
}

// ── AbortPoint trait (sealed helper) ────────────────────────────────

/// Sealed helper trait that lets `append_inner_impl` query the abort point
/// without a `cfg`-guarded match in prod (the `None::<Option<()>>` impl
/// returns `false` for every query and gets optimised out).
trait IntoAbortPoint: Copy {
    fn should_abort_after_hash_object(self) -> bool;
    fn should_abort_after_mktree(self) -> bool;
    fn should_abort_after_commit_tree(self) -> bool;
}

/// Production stub — always `false`.
impl IntoAbortPoint for Option<()> {
    fn should_abort_after_hash_object(self) -> bool {
        false
    }
    fn should_abort_after_mktree(self) -> bool {
        false
    }
    fn should_abort_after_commit_tree(self) -> bool {
        false
    }
}

/// Test variant — inspects the `Option<AbortPoint>`.
#[cfg(test)]
impl IntoAbortPoint for Option<AbortPoint> {
    fn should_abort_after_hash_object(self) -> bool {
        matches!(self, Some(AbortPoint::HashObject))
    }
    fn should_abort_after_mktree(self) -> bool {
        matches!(self, Some(AbortPoint::Mktree))
    }
    fn should_abort_after_commit_tree(self) -> bool {
        matches!(self, Some(AbortPoint::CommitTree))
    }
}

// ── Push helper ──────────────────────────────────────────────────────

/// Push a per-agent ref to a remote using a plain (non-force) push.
///
/// `git push <remote> <ref>:<ref>` — no `+`, no `--force-with-lease`. The
/// plain push IS the fast-forward CAS; any non-fast-forward outcome is
/// classified as [`PushOutcome::NonFastForward`] (REQ-1: identity collision
/// or history tampering — never silently rebased).
pub fn push_agent_ref(repo_dir: &Path, remote: &str, agent_id: &str) -> Result<PushOutcome> {
    validate_agent_id(agent_id)?;
    let ref_name = agent_ref_name(agent_id)?;
    let refspec = format!("{ref_name}:{ref_name}");

    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["push", remote, &refspec])
        .output()
        .with_context(|| format!("failed to run git push for agent '{agent_id}'"))?;

    if output.status.success() {
        return Ok(PushOutcome::Pushed);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Distinguish rejection reasons from the git stderr.
    if stderr.contains("non-fast-forward")
        || stderr.contains("rejected")
        || stderr.contains("stale info")
    {
        return Ok(PushOutcome::NonFastForward);
    }

    if stderr.contains("does not appear to be a git repository")
        || stderr.contains("repository not found")
        || stderr.contains("Could not read from remote repository")
        || stderr.contains("No such remote")
        || stderr.contains('\'') && stderr.contains("' does not")
    {
        return Ok(PushOutcome::NoRemote);
    }

    Ok(PushOutcome::Failed(stderr.trim().to_string()))
}

// ── Config helper ────────────────────────────────────────────────────

/// Read the `hub_v3.dual_write` config flag.
///
/// Reads `.crosslink/hook-config.json` and returns the boolean value of the
/// flat key `"hub_v3.dual_write"`. Any unreadable or invalid state (missing
/// file, missing key, non-bool value, JSON parse error) returns `false` with
/// a `tracing::debug` log. This function never propagates errors — dual-write
/// is a shadow mode and must never prevent the user operation from proceeding.
pub fn dual_write_enabled(crosslink_dir: &Path) -> bool {
    let config_path = crosslink_dir.join("hook-config.json");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                "hub_v3::dual_write_enabled: cannot read {}: {}",
                config_path.display(),
                e
            );
            return false;
        }
    };
    let val: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(
                "hub_v3::dual_write_enabled: cannot parse {}: {}",
                config_path.display(),
                e
            );
            return false;
        }
    };
    val.get("hub_v3.dual_write")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

// ── Shadow stats ──────────────────────────────────────────────────────

/// Counters written to `.crosslink/hub-v3-shadow-stats.json` during dual-write
/// soak mode. Updated under the hub write lock that is already held in
/// `emit_compact_push`; no additional synchronization is required.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ShadowStats {
    /// Events successfully mirrored to the per-agent ref.
    pub mirrored: u64,
    /// Events for which the shadow `append_event_to_ref` returned an error.
    pub mirror_failures: u64,
    /// Agent-ref pushes that succeeded.
    pub pushed: u64,
    /// Agent-ref pushes that returned a non-`Pushed` outcome or an error.
    pub push_failures: u64,
    /// Description of the last mirror or push failure, if any.
    pub last_failure: Option<String>,
    /// RFC 3339 timestamp of the last failure, if any.
    pub last_failure_at: Option<String>,
}

impl ShadowStats {
    /// Read stats from `path`, returning a zero-valued struct on any error.
    pub fn read(path: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    /// Atomically persist stats to `path`.
    pub fn write(&self, path: &Path) -> std::result::Result<(), anyhow::Error> {
        let bytes =
            serde_json::to_vec_pretty(self).context("failed to serialize hub-v3 shadow stats")?;
        crate::utils::atomic_write(path, &bytes)
    }
}

// ── Test-only crash-injection variant ────────────────────────────────

/// Variant of [`append_event_to_ref`] that accepts an optional
/// [`AbortPoint`] for crash-injection tests.
///
/// Passing `None` is equivalent to calling [`append_event_to_ref`].
/// Passing `Some(point)` causes the function to return an
/// `Ok(RefAppendOutcome)` with an empty `new_commit` string after the
/// indicated step, leaving the ref unmoved.
///
/// Only compiled in test builds.
#[cfg(test)]
pub(crate) fn append_event_to_ref_with_abort(
    repo_dir: &Path,
    agent_id: &str,
    envelope: &EventEnvelope,
    abort: Option<AbortPoint>,
) -> Result<RefAppendOutcome> {
    append_inner(repo_dir, agent_id, envelope, abort)
}

// ── Private git plumbing helpers ─────────────────────────────────────

/// Run `git rev-parse --verify --quiet <ref>` and return `Some(sha)` if the
/// ref exists, or `None` if it doesn't.
fn git_rev_parse_optional(repo_dir: &Path, ref_name: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["rev-parse", "--verify", "--quiet", ref_name])
        .output()
        .with_context(|| format!("failed to run git rev-parse for '{ref_name}'"))?;

    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Ok(None);
        }
        Ok(Some(sha))
    } else {
        // Non-zero exit = ref does not exist (with --quiet, no stderr for missing refs).
        Ok(None)
    }
}

/// Read a blob by `<commit>:<path>` spec. Returns `None` if the object does
/// not exist (missing path); returns an error for other failures.
fn git_cat_file_blob_optional(repo_dir: &Path, blob_spec: &str) -> Result<Option<Vec<u8>>> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["cat-file", "blob", blob_spec])
        .output()
        .with_context(|| format!("failed to run git cat-file for '{blob_spec}'"))?;

    if output.status.success() {
        return Ok(Some(output.stdout));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    // Distinguish "object not found" from real errors.
    if stderr.contains("does not exist")
        || stderr.contains("Not a valid object name")
        || stderr.contains("not found")
        || stderr.contains("could not get object info")
    {
        return Ok(None);
    }

    anyhow::bail!("git cat-file failed for '{}': {}", blob_spec, stderr.trim())
}

/// Write bytes to the object store via `git hash-object -w --stdin`.
///
/// Returns the blob SHA.
fn git_hash_object(repo_dir: &Path, data: &[u8]) -> Result<String> {
    use std::io::Write as _;

    let mut child = Command::new("git")
        .current_dir(repo_dir)
        .args(["hash-object", "-w", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn git hash-object")?;

    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("git hash-object stdin pipe unavailable"))?
        .write_all(data)
        .context("failed to write to git hash-object stdin")?;

    let output = child
        .wait_with_output()
        .context("failed to wait for git hash-object")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git hash-object failed: {}", stderr.trim());
    }

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        anyhow::bail!("git hash-object returned empty SHA");
    }
    Ok(sha)
}

/// Create a tree from a single `events.log` blob via `git mktree`.
///
/// The input line is `100644 blob <blob_sha>\tevents.log`.
fn git_mktree(repo_dir: &Path, blob_sha: &str) -> Result<String> {
    use std::io::Write as _;

    let tree_line = format!("100644 blob {blob_sha}\tevents.log\n");

    let mut child = Command::new("git")
        .current_dir(repo_dir)
        .args(["mktree"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn git mktree")?;

    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("git mktree stdin pipe unavailable"))?
        .write_all(tree_line.as_bytes())
        .context("failed to write to git mktree stdin")?;

    let output = child
        .wait_with_output()
        .context("failed to wait for git mktree")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git mktree failed: {}", stderr.trim());
    }

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        anyhow::bail!("git mktree returned empty SHA");
    }
    Ok(sha)
}

/// Create a commit object via `git commit-tree`.
///
/// Sets deterministic author/committer identity from `agent_id`. Parent is
/// optional (None for genesis commits).
fn git_commit_tree(
    repo_dir: &Path,
    tree_sha: &str,
    parent_sha: Option<&str>,
    message: &str,
    agent_id: &str,
) -> Result<String> {
    let author_name = agent_id;
    let author_email = format!("{agent_id}@crosslink");

    let mut args: Vec<&str> = vec!["commit-tree", tree_sha];
    let parent_arg;
    if let Some(p) = parent_sha {
        parent_arg = p.to_string();
        args.push("-p");
        args.push(&parent_arg);
    }
    args.push("-m");
    args.push(message);

    let mut child = Command::new("git")
        .current_dir(repo_dir)
        .args(&args)
        .env("GIT_AUTHOR_NAME", author_name)
        .env("GIT_AUTHOR_EMAIL", &author_email)
        .env("GIT_COMMITTER_NAME", author_name)
        .env("GIT_COMMITTER_EMAIL", &author_email)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to spawn git commit-tree")?;

    // commit-tree reads message from -m; stdin is null.
    let _ = child.stdin.take();

    let output = child
        .wait_with_output()
        .context("failed to wait for git commit-tree")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git commit-tree failed: {}", stderr.trim());
    }

    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() {
        anyhow::bail!("git commit-tree returned empty SHA");
    }
    Ok(sha)
}

/// Atomic compare-and-swap ref update via `git update-ref`.
///
/// For genesis (no old value) uses `git update-ref <ref> <new> ''` which
/// asserts the ref did not exist. For updates uses `git update-ref <ref>
/// <new> <old>`. On CAS failure (the ref was updated concurrently) returns
/// an error containing "ref moved concurrently" — the caller must re-read
/// and retry.
fn git_update_ref_cas(
    repo_dir: &Path,
    ref_name: &str,
    new_sha: &str,
    old_sha: Option<&str>,
) -> Result<()> {
    let old_value = old_sha.unwrap_or("");
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["update-ref", ref_name, new_sha, old_value])
        .output()
        .with_context(|| format!("failed to run git update-ref for '{ref_name}'"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    // `git update-ref` exits non-zero when the old value doesn't match.
    // Classify it as a concurrent-writer conflict.
    anyhow::bail!(
        "ref moved concurrently: {ref_name} (git update-ref failed: {})",
        stderr.trim()
    )
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{append_event, Event, EventEnvelope};
    use chrono::Utc;
    use uuid::Uuid;

    // ── Test helpers ─────────────────────────────────────────────────

    /// Initialise a git repo at `path` with a test identity configured.
    fn git_init(path: &Path) {
        run_git(path, &["init"]);
        run_git(path, &["config", "user.email", "test@crosslink.test"]);
        run_git(path, &["config", "user.name", "Test"]);
    }

    fn run_git(repo_dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .current_dir(repo_dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to run: {e}"));
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn run_git_output(repo_dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .current_dir(repo_dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to run: {e}"));
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn make_envelope(agent_id: &str, seq: u64) -> EventEnvelope {
        EventEnvelope {
            agent_id: agent_id.to_string(),
            agent_seq: seq,
            timestamp: Utc::now(),
            event: Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: format!("Issue seq {seq}"),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: agent_id.to_string(),
                display_id: None,
                scheduled_at: None,
                due_at: None,
            },
            signed_by: None,
            signature: None,
        }
    }

    // ── Test 1: genesis append ────────────────────────────────────────

    #[test]
    fn genesis_append_creates_ref_with_single_event() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        let agent_id = "test-agent";
        let envelope = make_envelope(agent_id, 1);

        let outcome = append_event_to_ref(dir.path(), agent_id, &envelope).unwrap();

        assert!(
            outcome.old_commit.is_none(),
            "genesis write must have no parent"
        );
        assert_eq!(outcome.events_in_log, 1);
        assert!(!outcome.new_commit.is_empty());

        // The ref must exist and point at the new commit.
        let ref_name = agent_ref_name(agent_id).unwrap();
        let sha = run_git_output(dir.path(), &["rev-parse", &ref_name]);
        assert_eq!(sha, outcome.new_commit);

        // The commit must have no parent.
        let parent_count_output = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["log", "--oneline", &ref_name])
            .output()
            .unwrap();
        let log = String::from_utf8_lossy(&parent_count_output.stdout);
        assert_eq!(log.lines().count(), 1, "genesis commit must have no parent");

        // The events.log blob must be the NDJSON line for the envelope.
        let blob_spec = format!("{}:events.log", outcome.new_commit);
        let blob = run_git_output(dir.path(), &["cat-file", "blob", &blob_spec]);
        let expected_line = serde_json::to_string(&envelope).unwrap();
        assert_eq!(blob, expected_line.trim());
    }

    // ── Test 2: sequential appends ───────────────────────────────────

    #[test]
    fn sequential_appends_chain_commits_and_preserve_order() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        let agent_id = "chain-agent";
        let e1 = make_envelope(agent_id, 1);
        let e2 = make_envelope(agent_id, 2);
        let e3 = make_envelope(agent_id, 3);

        let r1 = append_event_to_ref(dir.path(), agent_id, &e1).unwrap();
        let r2 = append_event_to_ref(dir.path(), agent_id, &e2).unwrap();
        let r3 = append_event_to_ref(dir.path(), agent_id, &e3).unwrap();

        assert_eq!(r1.events_in_log, 1);
        assert_eq!(r2.events_in_log, 2);
        assert_eq!(r3.events_in_log, 3);

        // Verify 3 commits chained via rev-list.
        let ref_name = agent_ref_name(agent_id).unwrap();
        let rev_list = run_git_output(dir.path(), &["rev-list", "--count", &ref_name]);
        assert_eq!(rev_list.trim(), "3", "must have exactly 3 commits in chain");

        // Parse the log blob and verify all 3 events in order.
        let blob_spec = format!("{}:events.log", r3.new_commit);
        let blob_bytes = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["cat-file", "blob", &blob_spec])
            .output()
            .unwrap()
            .stdout;

        let parsed = read_events_from_bytes(&blob_bytes).unwrap();
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].agent_seq, 1);
        assert_eq!(parsed[1].agent_seq, 2);
        assert_eq!(parsed[2].agent_seq, 3);

        // Verify byte-for-byte parity with events::append_event.
        let tmp = tempfile::tempdir().unwrap();
        let log_path = tmp.path().join("events.log");
        append_event(&log_path, &e1).unwrap();
        append_event(&log_path, &e2).unwrap();
        append_event(&log_path, &e3).unwrap();
        let file_bytes = std::fs::read(&log_path).unwrap();
        assert_eq!(
            blob_bytes, file_bytes,
            "hub_v3 log bytes must be byte-identical to events::append_event output"
        );
    }

    // ── Test 3: CAS conflict ─────────────────────────────────────────

    #[test]
    fn stale_cas_loses_loudly_and_winning_state_survives() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        let agent_id = "cas-agent";

        // First append establishes the ref.
        let r1 = append_event_to_ref(dir.path(), agent_id, &make_envelope(agent_id, 1)).unwrap();
        let tip_after_first = r1.new_commit;

        // Second append moves the ref forward.
        append_event_to_ref(dir.path(), agent_id, &make_envelope(agent_id, 2)).unwrap();

        // Now simulate a stale writer that still has the first tip as "old".
        // We directly call git_update_ref_cas with the stale old value to
        // simulate the race without needing threads.
        let ref_name = agent_ref_name(agent_id).unwrap();

        // Craft a dummy commit pointing at the same tree as r1.
        let current_tip = run_git_output(dir.path(), &["rev-parse", &ref_name]);

        let stale_result = git_update_ref_cas(
            dir.path(),
            &ref_name,
            &tip_after_first,       // wrong new (doesn't matter, CAS will fail)
            Some(&tip_after_first), // stale old value — ref has moved past this
        );

        assert!(stale_result.is_err(), "stale CAS must fail with an error");
        let err_msg = format!("{:?}", stale_result.unwrap_err());
        assert!(
            err_msg.contains("ref moved concurrently"),
            "error must mention concurrent move, got: {err_msg}"
        );

        // The ref must still point at the winning (current) commit.
        let tip_now = run_git_output(dir.path(), &["rev-parse", &ref_name]);
        assert_eq!(
            tip_now, current_tip,
            "winning state must survive the stale CAS attempt"
        );
    }

    // ── Test 4: crash injection ───────────────────────────────────────

    fn run_crash_injection_test(abort: AbortPoint, label: &str) {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        let agent_id = "crash-agent";
        let ref_name = agent_ref_name(agent_id).unwrap();
        let envelope = make_envelope(agent_id, 1);

        // Inject the crash — function returns Ok with empty new_commit, ref unmoved.
        let result = append_event_to_ref_with_abort(dir.path(), agent_id, &envelope, Some(abort));
        assert!(result.is_ok(), "{label}: abort should return Ok");
        let outcome = result.unwrap();
        assert!(
            outcome.new_commit.is_empty(),
            "{label}: aborted outcome must have empty new_commit"
        );

        // The ref must NOT have moved.
        let ref_exists = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["rev-parse", "--verify", "--quiet", &ref_name])
            .output()
            .unwrap();
        assert!(
            !ref_exists.status.success(),
            "{label}: ref must not exist after aborted genesis write"
        );

        // git fsck --strict must exit 0 (dangling objects produce warnings,
        // not errors; exit code is 0).
        let fsck = std::process::Command::new("git")
            .current_dir(dir.path())
            .args(["fsck", "--strict"])
            .output()
            .unwrap();
        assert_eq!(
            fsck.status.code(),
            Some(0),
            "{label}: git fsck --strict must exit 0; stderr: {}",
            String::from_utf8_lossy(&fsck.stderr)
        );

        // A subsequent normal append must succeed with the correct genesis chain.
        let normal = append_event_to_ref(dir.path(), agent_id, &envelope).unwrap();
        assert_eq!(
            normal.events_in_log, 1,
            "{label}: recovery must have 1 event"
        );
        assert!(
            normal.old_commit.is_none(),
            "{label}: recovery must be a genesis commit"
        );
    }

    #[test]
    fn crash_after_hash_object_leaves_ref_unmoved() {
        run_crash_injection_test(AbortPoint::HashObject, "HashObject");
    }

    #[test]
    fn crash_after_mktree_leaves_ref_unmoved() {
        run_crash_injection_test(AbortPoint::Mktree, "Mktree");
    }

    #[test]
    fn crash_after_commit_tree_leaves_ref_unmoved() {
        run_crash_injection_test(AbortPoint::CommitTree, "CommitTree");
    }

    // ── Test 5: two-agent concurrency in one repo ────────────────────

    #[test]
    fn two_agents_no_contention_on_separate_refs() {
        use std::sync::Arc;

        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let repo_dir = Arc::new(dir.path().to_path_buf());

        const EVENTS_PER_AGENT: u64 = 25;

        let repo_a = Arc::clone(&repo_dir);
        let handle_a = std::thread::spawn(move || {
            let agent_id = "concurrent-agent-a";
            for seq in 1..=EVENTS_PER_AGENT {
                append_event_to_ref(&repo_a, agent_id, &make_envelope(agent_id, seq))
                    .unwrap_or_else(|e| panic!("agent-a seq {seq} failed: {e}"));
            }
        });

        let repo_b = Arc::clone(&repo_dir);
        let handle_b = std::thread::spawn(move || {
            let agent_id = "concurrent-agent-b";
            for seq in 1..=EVENTS_PER_AGENT {
                append_event_to_ref(&repo_b, agent_id, &make_envelope(agent_id, seq))
                    .unwrap_or_else(|e| panic!("agent-b seq {seq} failed: {e}"));
            }
        });

        handle_a.join().expect("agent-a thread panicked");
        handle_b.join().expect("agent-b thread panicked");

        // Verify both refs have exactly 25 events each.
        for agent_id in &["concurrent-agent-a", "concurrent-agent-b"] {
            let ref_name = agent_ref_name(agent_id).unwrap();
            let rev_count = run_git_output(&repo_dir, &["rev-list", "--count", &ref_name]);
            assert_eq!(
                rev_count.trim(),
                "25",
                "agent {agent_id} must have 25 commits"
            );

            let tip = run_git_output(&repo_dir, &["rev-parse", &ref_name]);
            let blob_spec = format!("{tip}:events.log");
            let blob_bytes = std::process::Command::new("git")
                .current_dir(repo_dir.as_ref())
                .args(["cat-file", "blob", &blob_spec])
                .output()
                .unwrap()
                .stdout;
            let events = read_events_from_bytes(&blob_bytes).unwrap();
            assert_eq!(events.len(), 25, "agent {agent_id} log must have 25 events");
            for (i, ev) in events.iter().enumerate() {
                assert_eq!(
                    ev.agent_seq,
                    (i + 1) as u64,
                    "agent {agent_id} event at position {i} must have seq {}",
                    i + 1
                );
            }
        }
    }

    // ── Test 6: push outcomes ─────────────────────────────────────────

    #[test]
    fn push_outcomes_genesis_fast_forward_nff_noremote() {
        // Set up a local repo and a bare "remote".
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();

        git_init(local_dir.path());

        // Initialise a bare remote.
        run_git(remote_dir.path(), &["init", "--bare"]);

        // Add remote.
        run_git(
            local_dir.path(),
            &[
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
        );

        let agent_id = "push-agent";

        // (a) genesis write then push → Pushed.
        append_event_to_ref(local_dir.path(), agent_id, &make_envelope(agent_id, 1)).unwrap();
        let push1 = push_agent_ref(local_dir.path(), "origin", agent_id).unwrap();
        assert!(
            matches!(push1, PushOutcome::Pushed),
            "genesis push must be Pushed"
        );

        // Verify remote has the ref.
        let ref_name = agent_ref_name(agent_id).unwrap();
        run_git_output(remote_dir.path(), &["rev-parse", &ref_name]);

        // (b) append + push again → Pushed (fast-forward).
        append_event_to_ref(local_dir.path(), agent_id, &make_envelope(agent_id, 2)).unwrap();
        let push2 = push_agent_ref(local_dir.path(), "origin", agent_id).unwrap();
        assert!(
            matches!(push2, PushOutcome::Pushed),
            "second push must be Pushed (fast-forward)"
        );

        // (c) Move the REMOTE ref forward to a divergent commit, then push → NonFastForward.
        // We append a third event locally but also forge a different commit on the remote.
        append_event_to_ref(local_dir.path(), agent_id, &make_envelope(agent_id, 3)).unwrap();
        let local_tip = run_git_output(local_dir.path(), &["rev-parse", &ref_name]);

        // Create a divergent commit on the remote by writing a different blob.
        let divergent_bytes = b"divergent log content\n";
        // Write a temporary file into the bare remote to create a divergent object.
        // The simplest way: use git fast-import to create a divergent commit on the remote.
        // Instead, move the remote ref to a non-ancestor of local tip using update-ref
        // after creating a dummy orphan commit there.
        // Approach: create a second agent to get a real commit SHA on the remote, then
        // forcibly move the remote ref for push-agent to that SHA.
        let other_agent = "divergent-helper";
        append_event_to_ref(
            local_dir.path(),
            other_agent,
            &make_envelope(other_agent, 1),
        )
        .unwrap();
        let other_ref = agent_ref_name(other_agent).unwrap();
        // Push the helper agent to get an object on the remote.
        push_agent_ref(local_dir.path(), "origin", other_agent).unwrap();
        let remote_other_tip = run_git_output(remote_dir.path(), &["rev-parse", &other_ref]);

        // Now forcibly move the remote's push-agent ref to the helper commit (divergent).
        run_git(
            remote_dir.path(),
            &["update-ref", &ref_name, &remote_other_tip],
        );

        // Push should now be rejected as non-fast-forward.
        let push3 = push_agent_ref(local_dir.path(), "origin", agent_id).unwrap();
        assert!(
            matches!(push3, PushOutcome::NonFastForward),
            "divergent remote must yield NonFastForward"
        );

        // Remote ref must remain at the divergent (winning) value, not local tip.
        let remote_tip_after = run_git_output(remote_dir.path(), &["rev-parse", &ref_name]);
        assert_eq!(
            remote_tip_after, remote_other_tip,
            "remote must be unchanged after rejected push"
        );
        assert_ne!(
            remote_tip_after, local_tip,
            "local tip must not have been force-pushed"
        );

        // (d) Non-existent remote → NoRemote or Failed with useful message.
        let push4 = push_agent_ref(local_dir.path(), "nonexistent-remote", agent_id).unwrap();
        assert!(
            matches!(push4, PushOutcome::NoRemote | PushOutcome::Failed(_)),
            "unknown remote must be NoRemote or Failed"
        );
        // Log what we got for visibility (no assert on the exact variant since
        // git wording differs between versions).
        let _ = divergent_bytes; // suppress unused warning
    }

    // ── Test 7: agent_ref_name validation ────────────────────────────

    #[test]
    fn agent_ref_name_validation() {
        // Valid IDs pass.
        assert!(agent_ref_name("abc").is_ok());
        assert!(agent_ref_name("my-agent-42").is_ok());
        assert!(agent_ref_name("agent_xyz").is_ok());
        assert!(agent_ref_name(&"a".repeat(64)).is_ok());

        // Too short.
        assert!(agent_ref_name("ab").is_err());
        assert!(agent_ref_name("").is_err());

        // Too long.
        assert!(agent_ref_name(&"a".repeat(65)).is_err());
        assert!(agent_ref_name(&"a".repeat(200)).is_err());

        // Path-traversal / invalid chars.
        assert!(agent_ref_name("../x").is_err(), "../x must be rejected");
        assert!(agent_ref_name("a/b").is_err(), "slash must be rejected");
        assert!(agent_ref_name("a b").is_err(), "space must be rejected");
        assert!(agent_ref_name("a@b").is_err(), "@ must be rejected");
        assert!(
            agent_ref_name("a.b").is_err(),
            "dot (path-traversal) must be rejected"
        );

        // Windows reserved names.
        assert!(agent_ref_name("CON").is_err(), "CON must be rejected");
        assert!(agent_ref_name("NUL").is_err(), "NUL must be rejected");
        assert!(agent_ref_name("PRN").is_err(), "PRN must be rejected");
    }

    // ── Test 8: dual_write_enabled ────────────────────────────────────

    #[test]
    fn dual_write_enabled_missing_file_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        // No hook-config.json exists in the dir.
        assert!(
            !dual_write_enabled(dir.path()),
            "missing file must return false"
        );
    }

    #[test]
    fn dual_write_enabled_flag_true_returns_true() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hook-config.json");
        std::fs::write(
            &config_path,
            r#"{"hub_v3.dual_write": true, "tracking_mode": "strict"}"#,
        )
        .unwrap();
        assert!(
            dual_write_enabled(dir.path()),
            "hub_v3.dual_write=true must return true"
        );
    }

    #[test]
    fn dual_write_enabled_flag_false_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hook-config.json");
        std::fs::write(&config_path, r#"{"hub_v3.dual_write": false}"#).unwrap();
        assert!(
            !dual_write_enabled(dir.path()),
            "hub_v3.dual_write=false must return false"
        );
    }

    #[test]
    fn dual_write_enabled_missing_key_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hook-config.json");
        std::fs::write(&config_path, r#"{"tracking_mode": "strict"}"#).unwrap();
        assert!(
            !dual_write_enabled(dir.path()),
            "missing key must return false"
        );
    }

    #[test]
    fn dual_write_enabled_garbage_json_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hook-config.json");
        std::fs::write(&config_path, b"not valid json at all {{{{").unwrap();
        assert!(
            !dual_write_enabled(dir.path()),
            "garbage JSON must return false"
        );
    }

    #[test]
    fn dual_write_enabled_wrong_type_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("hook-config.json");
        // Value is a string, not a bool.
        std::fs::write(&config_path, r#"{"hub_v3.dual_write": "yes"}"#).unwrap();
        assert!(
            !dual_write_enabled(dir.path()),
            "non-bool value must return false"
        );
    }

    // ── Test 9: ShadowStats round-trip ───────────────────────────────

    #[test]
    fn shadow_stats_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hub-v3-shadow-stats.json");
        let stats = ShadowStats::read(&path);
        assert_eq!(stats.mirrored, 0);
        assert_eq!(stats.mirror_failures, 0);
        assert_eq!(stats.pushed, 0);
        assert_eq!(stats.push_failures, 0);
        assert!(stats.last_failure.is_none());
    }

    #[test]
    fn shadow_stats_write_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hub-v3-shadow-stats.json");
        let written = ShadowStats {
            mirrored: 42,
            mirror_failures: 3,
            pushed: 40,
            push_failures: 2,
            last_failure: Some("test failure".to_string()),
            last_failure_at: Some("2026-01-01T00:00:00Z".to_string()),
        };
        written.write(&path).unwrap();
        let read = ShadowStats::read(&path);
        assert_eq!(read.mirrored, 42);
        assert_eq!(read.mirror_failures, 3);
        assert_eq!(read.pushed, 40);
        assert_eq!(read.push_failures, 2);
        assert_eq!(read.last_failure.as_deref(), Some("test failure"));
    }
}
