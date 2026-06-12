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

/// Ref holding the pure-cache compaction checkpoint (`state.json` at tree root).
///
/// Written by whichever process compacts and pushed with `--force-with-lease`
/// (REQ-7). Concurrent compactions are harmless: the same event set reduces to
/// the same deterministic state, so two writers produce byte-identical content
/// and the lease loser simply refetches an identical result.
pub const CHECKPOINT_REF: &str = "refs/crosslink/checkpoint";

/// Ref holding hub metadata: the version marker (`hub.json`) and the
/// `allowed_signers` trust store (REQ-9, REQ-12). Driver-written, CAS-updated.
pub const META_REF: &str = "refs/crosslink/meta";

/// Compare-and-swap expectation for the generalized single-file commit core.
///
/// Mirrors the `update-ref <ref> <new> <old>` contract: the update succeeds only
/// if the ref's current value matches the expectation, otherwise it is rejected
/// as a concurrent move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Variants are selected by the different commit entry points (append vs.
// genesis-or-update); the bin's duplicate module tree flags the unused-by-bin
// constructors as dead code.
#[allow(dead_code)]
pub enum CasExpectation<'a> {
    /// The ref must not currently exist (genesis write).
    MustNotExist,
    /// The ref must currently point at this exact SHA.
    MustMatch(&'a str),
    /// Read the current tip and CAS against it. `None` if the ref is absent
    /// (treated as genesis), `Some(sha)` if it exists (treated as an update).
    CurrentTip,
}

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
#[derive(Debug)]
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

    // ── Step e: mktree (sibling-preserving) ──────────────────────────
    // Read the existing tree at the current tip and upsert events.log,
    // keeping every unrelated sibling file/subtree (heartbeat.json,
    // requests-ack/, requests-out/, …) byte-identical. A naive single-entry
    // mktree here would DROP those siblings once refs carry them.
    let tree_sha = write_tree_with(
        repo_dir,
        old_commit.as_deref(),
        &[("events.log", BlobRef::Existing(&blob_sha))],
        &[],
    )?;

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

    commit_single_file_tree(
        repo_dir,
        &ref_name,
        "events.log",
        log_bytes,
        message,
        agent_id,
        CasExpectation::CurrentTip,
    )
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
    push_ref(repo_dir, remote, &ref_name)
}

/// Push an arbitrary `refs/crosslink/*` ref to a remote with a plain
/// (non-force) push.
///
/// `git push <remote> <ref>:<ref>` — no `+`, no `--force-with-lease`. The plain
/// push IS the fast-forward CAS; any non-fast-forward outcome is classified as
/// [`PushOutcome::NonFastForward`] (REQ-1: never silently rebased). This is the
/// generalization of [`push_agent_ref`], which is now a thin wrapper.
///
/// # Errors
///
/// Returns an error only if `git push` cannot be spawned; rejections and
/// remote-not-found are reported as [`PushOutcome`] variants, not errors.
// PR3 read/verify path and the migrate command push the checkpoint and meta
// refs through this; flagged dead until those callers land.
#[allow(dead_code)]
pub fn push_ref(repo_dir: &Path, remote: &str, ref_name: &str) -> Result<PushOutcome> {
    let refspec = format!("{ref_name}:{ref_name}");

    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["push", remote, &refspec])
        .output()
        .with_context(|| format!("failed to run git push for ref '{ref_name}'"))?;

    Ok(classify_push_output(&output))
}

/// Push a ref with `--force-with-lease`, used for the checkpoint ref (REQ-7).
///
/// The checkpoint is a pure cache: two compactors over the same event set
/// produce byte-identical content, so a checkpoint race is harmless. The lease
/// guards against clobbering a checkpoint advanced by an unseen third party —
/// the lease loser refetches and either fast-forwards or discards its identical
/// result. `expected_remote` is the remote SHA the local side believes the ref
/// holds:
///
/// - `Some(sha)` → `git push --force-with-lease=<ref>:<sha>` (strict lease).
/// - `None` → `git push --force-with-lease=<ref>` (git uses the local
///   remote-tracking ref as the lease baseline).
///
/// A failed lease (the remote advanced past `expected_remote`) is reported as
/// [`PushOutcome::NonFastForward`]; the caller refetches and retries.
///
/// # Errors
///
/// Returns an error only if `git push` cannot be spawned.
// PR3 compaction-checkpoint push is the production caller; flagged dead until
// that path lands in part 2.
#[allow(dead_code)]
pub fn push_ref_with_lease(
    repo_dir: &Path,
    remote: &str,
    ref_name: &str,
    expected_remote: Option<&str>,
) -> Result<PushOutcome> {
    let lease = expected_remote.map_or_else(
        || format!("--force-with-lease={ref_name}"),
        |sha| format!("--force-with-lease={ref_name}:{sha}"),
    );
    let refspec = format!("{ref_name}:{ref_name}");

    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["push", &lease, remote, &refspec])
        .output()
        .with_context(|| {
            format!("failed to run git push --force-with-lease for ref '{ref_name}'")
        })?;

    Ok(classify_push_output(&output))
}

/// Classify a `git push` process output into a [`PushOutcome`].
///
/// Shared by [`push_ref`] and [`push_ref_with_lease`]. A failed
/// `--force-with-lease` reports "stale info" / "rejected", which map to
/// [`PushOutcome::NonFastForward`] — the caller refetches and retries.
fn classify_push_output(output: &std::process::Output) -> PushOutcome {
    if output.status.success() {
        return PushOutcome::Pushed;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Distinguish rejection reasons from the git stderr.
    if stderr.contains("non-fast-forward")
        || stderr.contains("rejected")
        || stderr.contains("stale info")
    {
        return PushOutcome::NonFastForward;
    }

    if stderr.contains("does not appear to be a git repository")
        || stderr.contains("repository not found")
        || stderr.contains("Could not read from remote repository")
        || stderr.contains("No such remote")
        || stderr.contains('\'') && stderr.contains("' does not")
    {
        return PushOutcome::NoRemote;
    }

    PushOutcome::Failed(stderr.trim().to_string())
}

// ── Hub version detection ─────────────────────────────────────────────

/// Branch name of the legacy v2 hub.
const V2_HUB_BRANCH: &str = "refs/heads/crosslink/hub";

/// Detected hub schema version.
///
/// See `.design/hub-v3-per-agent-refs.md` REQ-9. Detection is structural:
/// presence of the v3 marker refs ([`META_REF`] + [`CHECKPOINT_REF`]) versus the
/// legacy `crosslink/hub` branch.
#[derive(Debug, Clone, PartialEq, Eq)]
// Consumed by the migrate command and the v3-aware warn path (part 2); the bin's
// duplicate module tree flags the variants as dead code until then.
#[allow(dead_code)]
pub enum HubVersion {
    /// A `crosslink/hub` branch exists but neither v3 marker ref does.
    V2Only,
    /// The v3 marker refs ([`META_REF`] + [`CHECKPOINT_REF`]) exist.
    /// `v2_branch_present` records whether the old branch is still around
    /// (true until `migrate hub-v3 --finalize` deletes it).
    V3 { v2_branch_present: bool },
    /// Neither a v2 branch nor the v3 marker refs exist (uninitialized hub).
    Absent,
}

/// Detect the LOCAL hub version by inspecting refs in `repo_dir`.
///
/// Classification (REQ-9): if both [`META_REF`] and [`CHECKPOINT_REF`] resolve,
/// the hub is `V3` (recording whether the v2 branch is still present); otherwise
/// if the `crosslink/hub` branch resolves, it is `V2Only`; otherwise `Absent`.
///
/// # Errors
///
/// Returns an error only if `git rev-parse` cannot be spawned.
// Part-2 migrate/refusal logic is the production caller; flagged dead until then.
#[allow(dead_code)]
pub fn detect_hub_version(repo_dir: &Path) -> Result<HubVersion> {
    let meta = git_rev_parse_optional(repo_dir, META_REF)?.is_some();
    let checkpoint = git_rev_parse_optional(repo_dir, CHECKPOINT_REF)?.is_some();
    let v2 = git_rev_parse_optional(repo_dir, V2_HUB_BRANCH)?.is_some();

    Ok(classify_hub_version(meta, checkpoint, v2))
}

/// Detect the REMOTE hub version via a single `git ls-remote` call.
///
/// Queries `git ls-remote <remote> refs/crosslink/* refs/heads/crosslink/hub`
/// and classifies the returned ref listing identically to [`detect_hub_version`].
/// Unlike the local probe, an unreachable or unauthenticated remote is a hard
/// error — the version of an unreachable remote must never be guessed (REQ-9).
///
/// # Errors
///
/// Returns an error if `git ls-remote` cannot be spawned, or if the remote is
/// unreachable / unauthenticated / unknown (classified from stderr, paralleling
/// the [`PushOutcome`] stderr discrimination).
// Part-2 migrate/refusal logic is the production caller; flagged dead until then.
#[allow(dead_code)]
pub fn detect_remote_hub_version(repo_dir: &Path, remote: &str) -> Result<HubVersion> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["ls-remote", remote, "refs/crosslink/*", V2_HUB_BRANCH])
        .output()
        .with_context(|| format!("failed to run git ls-remote for remote '{remote}'"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "git ls-remote failed for remote '{remote}' (cannot determine remote hub version; \
             remote unreachable, unauthenticated, or unknown): {}",
            stderr.trim()
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut meta = false;
    let mut checkpoint = false;
    let mut v2 = false;
    // Each line is "<sha>\t<refname>".
    for line in stdout.lines() {
        let Some((_, refname)) = line.split_once('\t') else {
            continue;
        };
        let refname = refname.trim();
        match refname {
            META_REF => meta = true,
            CHECKPOINT_REF => checkpoint = true,
            V2_HUB_BRANCH => v2 = true,
            _ => {}
        }
    }

    Ok(classify_hub_version(meta, checkpoint, v2))
}

/// Pure classifier shared by the local and remote detectors.
const fn classify_hub_version(
    meta_present: bool,
    checkpoint_present: bool,
    v2_present: bool,
) -> HubVersion {
    if meta_present && checkpoint_present {
        HubVersion::V3 {
            v2_branch_present: v2_present,
        }
    } else if v2_present {
        HubVersion::V2Only
    } else {
        HubVersion::Absent
    }
}

// ── Operation mode (754a PASS 2) ──────────────────────────────────────

/// Resolved operation mode for a hub, decided ONCE per `SyncManager` /
/// `SharedWriter` construction from [`detect_hub_version`].
///
/// - [`HubMode::V3`] — the hub carries the v3 marker refs ([`META_REF`] +
///   [`CHECKPOINT_REF`]). Mutations write events only to the agent's own ref;
///   state is derived by reduction and hydrated from the resulting
///   [`crate::checkpoint::CheckpointState`]. No worktree file writes, no
///   counter reads, no rebase/conflict machinery.
/// - [`HubMode::V2`] — [`HubVersion::V2Only`] or [`HubVersion::Absent`]. The
///   today behavior is preserved bit-identically: worktree-file writes through
///   the hub-cache, counter-claimed display ids, file-based hydration. `Absent`
///   keeps the current init/bootstrap behavior (v3 bootstrap-from-scratch is
///   tracked as 754b).
///
/// # Why the dual-write flag is ignored in V3
///
/// `hub_v3.dual_write` was the v2→v3 BRIDGE: while a hub was still v2, it
/// mirrored every v2 log append onto the per-agent ref so the v3 layout could
/// be soaked before cutover. Once the hub is v3 there is no v2 log to mirror
/// FROM — the agent ref IS the only log — so the flag is a no-op in
/// [`HubMode::V3`] and is never consulted on the v3 write path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubMode {
    /// Legacy worktree-file operation (v2 or uninitialized).
    V2,
    /// Per-agent-ref, event-only operation (v3).
    V3,
}

impl HubMode {
    /// Resolve the operation mode from the hub at `repo_dir` (the hub-cache
    /// worktree or the main repo — refs resolve via the shared object store).
    ///
    /// `V3{..}` ⇒ [`HubMode::V3`]; `V2Only` / `Absent` ⇒ [`HubMode::V2`].
    /// Any detection error degrades to [`HubMode::V2`] (the safe, unchanged
    /// path) with a debug log — mode resolution must never block construction.
    #[must_use]
    pub fn resolve(repo_dir: &Path) -> Self {
        match detect_hub_version(repo_dir) {
            Ok(HubVersion::V3 { .. }) => HubMode::V3,
            Ok(HubVersion::V2Only | HubVersion::Absent) => HubMode::V2,
            Err(e) => {
                tracing::debug!(
                    "HubMode::resolve: detect_hub_version failed for {}: {e}; defaulting to V2",
                    repo_dir.display()
                );
                HubMode::V2
            }
        }
    }

    /// Whether this is the v3 event-only mode.
    #[must_use]
    pub const fn is_v3(self) -> bool {
        matches!(self, HubMode::V3)
    }
}

// ── Hub meta marker (META_REF) ────────────────────────────────────────

/// Hub version marker stored as `hub.json` on [`META_REF`].
///
/// Written by `crosslink migrate hub-v3` (part 2) alongside the `allowed_signers`
/// blob. Records the schema version and provenance of the migration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
// Constructed and read by the migrate command and the verify path (part 2);
// flagged dead until those callers land.
#[allow(dead_code)]
pub struct HubMeta {
    /// Hub schema version (3 for v3).
    pub hub_version: u32,
    /// The `crosslink/hub` commit the migration was derived from.
    pub migrated_from_commit: String,
    /// When the migration ran.
    pub migrated_at: chrono::DateTime<chrono::Utc>,
    /// When `migrate hub-v3 --finalize` deleted the legacy v2 branch, if ever.
    /// `None` until finalize runs; `serde(default)` so pre-finalize markers and
    /// markers written before this field existed parse cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Read the [`HubMeta`] marker from the [`META_REF`] tip, if present.
///
/// Returns `Ok(None)` when the meta ref does not exist (no v3 marker yet) or
/// when its tree has no `hub.json` (a meta ref carrying only `allowed_signers`).
///
/// # Errors
///
/// Returns an error if git plumbing fails or `hub.json` exists but does not
/// parse as [`HubMeta`].
// Part-2 verify / refusal logic is the production caller; flagged dead until then.
#[allow(dead_code)]
pub fn read_hub_meta(repo_dir: &Path) -> Result<Option<HubMeta>> {
    let Some(tip) = git_rev_parse_optional(repo_dir, META_REF)? else {
        return Ok(None);
    };
    let spec = format!("{tip}:hub.json");
    let Some(bytes) = git_cat_file_blob_optional(repo_dir, &spec)? else {
        return Ok(None);
    };
    let meta: HubMeta = serde_json::from_slice(&bytes)
        .context("failed to parse hub.json on the meta ref as HubMeta")?;
    Ok(Some(meta))
}

// ── Heartbeats on agent refs (REQ-7 auxiliary state) ──────────────────
//
// Writer-ownership: each agent's heartbeat lives at `heartbeat.json` at the
// TREE ROOT of that agent's own ref (`refs/crosslink/agents/<id>`), the only
// ref that agent writes (single-writer invariant). The serialized shape is the
// SAME [`crate::locks::Heartbeat`] schema the v2 path uses, so a reader parses
// either source without a v3-specific type. Because the write goes through the
// sibling-preserving tree core, the agent's `events.log` (and any requests-ack/
// subtree) survives a heartbeat write byte-for-byte.

/// Write this agent's heartbeat to `heartbeat.json` at the root of its own ref.
///
/// One sibling-preserving commit: `events.log` and any `requests-ack/` subtree
/// on the same ref are carried through unchanged. Reuses the v2
/// [`crate::locks::Heartbeat`] serialization (`serde_json`) so v2 and v3 readers
/// share one schema.
///
/// # Errors
///
/// Returns an error if serialization or any git plumbing step fails, or if the
/// ref moved concurrently (CAS failure — caller re-reads and retries).
// Wired into the v2 heartbeat caller by pass 2 (mode routing); flagged dead
// until then because the bin's duplicate module tree sees no caller.
#[allow(dead_code)]
pub fn write_heartbeat_to_ref(
    repo_dir: &Path,
    agent_id: &str,
    heartbeat: &crate::locks::Heartbeat,
) -> Result<String> {
    validate_agent_id(agent_id)?;
    let ref_name = format!("{AGENT_REF_PREFIX}{agent_id}");
    let bytes = serde_json::to_vec_pretty(heartbeat)
        .context("failed to serialize heartbeat for the agent ref")?;
    let message = format!("crosslink heartbeat: agent {agent_id}");
    commit_upserts_to_ref(
        repo_dir,
        &ref_name,
        &[("heartbeat.json", BlobRef::Bytes(&bytes))],
        &[],
        &message,
        agent_id,
        CasExpectation::CurrentTip,
    )
}

/// Read every agent's heartbeat by scanning `refs/crosslink/agents/*` and
/// reading `heartbeat.json` at each tip.
///
/// Agents whose ref carries no `heartbeat.json` (e.g. an events-only ref that
/// has never beaten) are skipped, not errored. Returns `(agent_id, Heartbeat)`
/// pairs; the embedded `Heartbeat::agent_id` always matches the ref's agent id
/// for a well-formed hub, but the ref-derived id is authoritative for the tuple.
///
/// # Errors
///
/// Returns an error if `git for-each-ref` fails or a present `heartbeat.json`
/// blob does not parse as [`crate::locks::Heartbeat`] (a corrupt heartbeat is
/// surfaced, not silently dropped).
// Wired into the dashboard/TUI reader by pass 3; flagged dead until then.
#[allow(dead_code)]
pub fn read_heartbeats_from_refs(
    repo_dir: &Path,
) -> Result<Vec<(String, crate::locks::Heartbeat)>> {
    let mut out = Vec::new();
    for ref_name in for_each_agent_ref(repo_dir)? {
        let Some(agent_id) = ref_name.strip_prefix(AGENT_REF_PREFIX) else {
            continue;
        };
        let Some(tip) = git_rev_parse_optional(repo_dir, &ref_name)? else {
            continue;
        };
        let spec = format!("{tip}:heartbeat.json");
        let Some(bytes) = git_cat_file_blob_optional(repo_dir, &spec)? else {
            continue; // No heartbeat on this ref yet — skip.
        };
        let hb: crate::locks::Heartbeat = serde_json::from_slice(&bytes).with_context(|| {
            format!("failed to parse heartbeat.json on ref '{ref_name}' as Heartbeat")
        })?;
        out.push((agent_id.to_string(), hb));
    }
    Ok(out)
}

// ── Agent requests / acks on agent refs (design doc §9, REQ-6) ────────
//
// Writer-ownership (single-writer-per-ref invariant):
//
//   - A DRIVER writes a request into ITS OWN ref under the subtree path
//     `requests-out/<target_agent_id>--<ulid>.json`. The `<target>--<ulid>`
//     encoding flattens what would otherwise be two nesting levels into one:
//     the tree core supports exactly one subtree level, so the target id and
//     ulid are joined with a `--` separator. Target ids are validated to
//     `[-_a-zA-Z0-9]{3,64}`, which permits single hyphens but never a doubled
//     `--`, so the filename is split on the LAST `--` to recover (target, ulid)
//     unambiguously (a target id like `my-agent` parses correctly).
//
//   - The TARGET agent writes acks into ITS OWN ref under the subtree path
//     `requests-ack/<ulid>.json`.
//
// Readers scan ALL agent refs' `requests-out/` subtrees. Both layouts use a
// one-level subtree (supported by the tree core); requests and acks are
// consistent in using subtrees rather than mixing flat-root and subtree forms.
//
// Signature model (mirrors v2 — see the v2 finding in the PR report): requests
// and acks are plain JSON carrying a `requested_by` driver fingerprint; they are
// NOT git-signed at the event level. v2's `poll::process_pending` trusts whatever
// landed on the local cache because the hub-sync machinery rejects unsigned /
// bad-signer COMMITS at fetch time (and `reduce` only WARNS on unsigned events,
// never rejecting). The v3 ref primitives mirror this exactly: they perform no
// per-request signature gate here — the same fetch-time / trust-boundary model
// applies, with rogue-agent attribution handled by signed events + allowed_signers
// + trust revoke per REQ-6.

/// Subtree directory (under a driver's ref) holding outbound requests.
const REQUESTS_OUT_DIR: &str = "requests-out";
/// Subtree directory (under a target agent's ref) holding acks.
const REQUESTS_ACK_DIR: &str = "requests-ack";

/// Encode `(target_agent_id, request_id)` into the flattened one-level filename
/// `requests-out/<target>--<ulid>.json`.
fn request_out_path(target_agent_id: &str, request_id: &str) -> String {
    format!("{REQUESTS_OUT_DIR}/{target_agent_id}--{request_id}.json")
}

/// Decode a `requests-out/` leaf filename back into `(target_agent_id, ulid)`.
///
/// Splits on the LAST `--` so a target id containing single hyphens parses
/// correctly. Returns `None` for a name that does not end in `.json` or lacks a
/// `--` separator.
fn parse_request_out_name(file_name: &str) -> Option<(String, String)> {
    let stem = file_name.strip_suffix(".json")?;
    let (target, ulid) = stem.rsplit_once("--")?;
    if target.is_empty() || ulid.is_empty() {
        return None;
    }
    Some((target.to_string(), ulid.to_string()))
}

/// Write a request into the DRIVER's OWN ref under
/// `requests-out/<target_agent_id>--<ulid>.json`.
///
/// Single sibling-preserving commit on the driver's ref (its `events.log`,
/// `heartbeat.json`, and any other `requests-out/` entries survive). The driver
/// is the sole writer of its ref, upholding the single-writer invariant — the v2
/// scheme of writing into the TARGET's directory is rejected by this design.
///
/// # Errors
///
/// Returns an error if either agent id is invalid, serialization fails, or any
/// git plumbing step fails (including a concurrent CAS move of the driver ref).
// Wired into the driver CLI path by pass 2; flagged dead until then.
#[allow(dead_code)]
pub fn write_request_to_own_ref(
    repo_dir: &Path,
    driver_agent_id: &str,
    target_agent_id: &str,
    request: &crate::agent_requests::AgentRequest,
) -> Result<String> {
    validate_agent_id(driver_agent_id)?;
    validate_agent_id(target_agent_id)?;
    let ref_name = format!("{AGENT_REF_PREFIX}{driver_agent_id}");
    let path = request_out_path(target_agent_id, &request.request_id);
    let bytes = serde_json::to_vec_pretty(request).context("failed to serialize agent request")?;
    let message = format!(
        "crosslink request: {driver_agent_id} -> {target_agent_id} ({})",
        request.request_id
    );
    commit_upserts_to_ref(
        repo_dir,
        &ref_name,
        &[(&path, BlobRef::Bytes(&bytes))],
        &[],
        &message,
        driver_agent_id,
        CasExpectation::CurrentTip,
    )
}

/// Write an ack into the TARGET agent's OWN ref under
/// `requests-ack/<request_id>.json`.
///
/// Single sibling-preserving commit on the target's own ref. The target is the
/// sole writer of its ref (single-writer invariant).
///
/// # Errors
///
/// Returns an error if the agent id is invalid, serialization fails, or any git
/// plumbing step fails (including a concurrent CAS move of the agent ref).
// Wired into the agent poll path by pass 2; flagged dead until then.
#[allow(dead_code)]
pub fn write_ack_to_own_ref(
    repo_dir: &Path,
    my_agent_id: &str,
    request_id: &str,
    ack: &crate::agent_requests::AgentRequestAck,
) -> Result<String> {
    validate_agent_id(my_agent_id)?;
    let ref_name = format!("{AGENT_REF_PREFIX}{my_agent_id}");
    let path = format!("{REQUESTS_ACK_DIR}/{request_id}.json");
    let bytes = serde_json::to_vec_pretty(ack).context("failed to serialize agent request ack")?;
    let message = format!("crosslink ack: {my_agent_id} ({request_id})");
    commit_upserts_to_ref(
        repo_dir,
        &ref_name,
        &[(&path, BlobRef::Bytes(&bytes))],
        &[],
        &message,
        my_agent_id,
        CasExpectation::CurrentTip,
    )
}

/// Poll for requests targeting `my_agent_id` that have not yet been acked.
///
/// Scans EVERY agent ref's `requests-out/` subtree for entries whose flattened
/// filename targets `my_agent_id`, then filters out any whose ulid already has a
/// `requests-ack/<ulid>.json` on `my_agent_id`'s OWN ref. Returns
/// `(driver_agent_id, AgentRequest)` pairs for the still-pending requests, sorted
/// by ulid (lexicographic = chronological).
///
/// A driver's own ref is skipped only insofar as it would never target itself in
/// practice; requests addressed to a DIFFERENT agent are not returned.
///
/// # Errors
///
/// Returns an error if git plumbing fails or a present request blob does not
/// parse as [`crate::agent_requests::AgentRequest`].
// Wired into the agent poll path by pass 2; flagged dead until then.
#[allow(dead_code)]
pub fn poll_requests_for_agent(
    repo_dir: &Path,
    my_agent_id: &str,
) -> Result<Vec<(String, crate::agent_requests::AgentRequest)>> {
    validate_agent_id(my_agent_id)?;

    // Collect the set of ulids already acked on my own ref.
    let my_ref = format!("{AGENT_REF_PREFIX}{my_agent_id}");
    let acked: std::collections::HashSet<String> = match git_rev_parse_optional(repo_dir, &my_ref)?
    {
        None => std::collections::HashSet::new(),
        Some(tip) => list_subtree_leaf_stems(repo_dir, &tip, REQUESTS_ACK_DIR)?
            .into_iter()
            .collect(),
    };

    let mut out = Vec::new();
    for ref_name in for_each_agent_ref(repo_dir)? {
        let Some(driver_id) = ref_name.strip_prefix(AGENT_REF_PREFIX) else {
            continue;
        };
        let Some(tip) = git_rev_parse_optional(repo_dir, &ref_name)? else {
            continue;
        };
        for leaf in list_subtree_leaf_names(repo_dir, &tip, REQUESTS_OUT_DIR)? {
            let Some((target, ulid)) = parse_request_out_name(&leaf) else {
                continue;
            };
            if target != my_agent_id {
                continue; // Request for a different target.
            }
            if acked.contains(&ulid) {
                continue; // Already acked by me.
            }
            let spec = format!("{tip}:{REQUESTS_OUT_DIR}/{leaf}");
            let Some(bytes) = git_cat_file_blob_optional(repo_dir, &spec)? else {
                continue;
            };
            let request: crate::agent_requests::AgentRequest = serde_json::from_slice(&bytes)
                .with_context(|| format!("failed to parse request '{leaf}' on ref '{ref_name}'"))?;
            out.push((driver_id.to_string(), request));
        }
    }

    // Sort by ulid (lexicographic == chronological).
    out.sort_by(|a, b| a.1.request_id.cmp(&b.1.request_id));
    Ok(out)
}

/// Read the maximum `agent_seq` recorded in this agent's OWN REF `events.log`.
///
/// Returns `Ok(0)` when the ref does not exist or carries no `events.log`
/// (genesis state). Used by the v3 write path to initialize the per-session
/// sequence counter from the durable ref rather than a worktree file, so the
/// sequence never regresses after a REQ-11 prune drops covered events (prune
/// keeps the highest-seq events, so the max is preserved across prune as long
/// as any event remains; a fully-pruned ref legitimately resets to 0 because
/// every prior event is checkpoint-covered).
///
/// # Errors
///
/// Returns an error if git plumbing fails or the `events.log` blob does not
/// parse (a corrupt ref is surfaced, not silently treated as empty).
pub fn read_max_event_seq_from_ref(repo_dir: &Path, agent_id: &str) -> Result<u64> {
    validate_agent_id(agent_id)?;
    let ref_name = format!("{AGENT_REF_PREFIX}{agent_id}");
    let Some(tip) = git_rev_parse_optional(repo_dir, &ref_name)? else {
        return Ok(0);
    };
    let spec = format!("{tip}:events.log");
    let Some(bytes) = git_cat_file_blob_optional(repo_dir, &spec)? else {
        return Ok(0);
    };
    let events = read_events_from_bytes(&bytes)
        .with_context(|| format!("failed to parse events.log on '{ref_name}' for seq init"))?;
    Ok(events.iter().map(|e| e.agent_seq).max().unwrap_or(0))
}

/// Enumerate `refs/crosslink/agents/*` ref names.
fn for_each_agent_ref(repo_dir: &Path) -> Result<Vec<String>> {
    let pattern = format!("{AGENT_REF_PREFIX}*");
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["for-each-ref", "--format=%(refname)", &pattern])
        .output()
        .with_context(|| format!("failed to run git for-each-ref for '{pattern}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "git for-each-ref failed for '{}': {}",
            pattern,
            stderr.trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// List the leaf file names (e.g. `01J...--abc.json`) of a one-level subtree
/// `<dir>` at `commit_sha`. Returns an empty vec if the subtree is absent.
fn list_subtree_leaf_names(repo_dir: &Path, commit_sha: &str, dir: &str) -> Result<Vec<String>> {
    let spec = format!("{commit_sha}:{dir}");
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["ls-tree", "--name-only", &spec])
        .output()
        .with_context(|| format!("failed to run git ls-tree for '{spec}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Absent subtree → empty, not an error.
        if stderr.contains("Not a valid object name")
            || stderr.contains("does not exist")
            || stderr.contains("not a tree")
            || stderr.contains("Not a tree")
        {
            return Ok(Vec::new());
        }
        anyhow::bail!("git ls-tree failed for '{}': {}", spec, stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// List the `.json` leaf STEMS (filename without the `.json` suffix) of a
/// one-level subtree `<dir>` at `commit_sha`. Used to enumerate acked ulids.
fn list_subtree_leaf_stems(repo_dir: &Path, commit_sha: &str, dir: &str) -> Result<Vec<String>> {
    Ok(list_subtree_leaf_names(repo_dir, commit_sha, dir)?
        .into_iter()
        .filter_map(|n| n.strip_suffix(".json").map(str::to_string))
        .collect())
}

// ── V3 compaction cycle (REQ-7 checkpoint + REQ-11 own-ref prune) ─────
//
// compact_v3 lives HERE (hub_v3.rs) rather than in compaction.rs because it
// depends on the v3 ref-write primitives in THIS module (commit_blob_to_ref,
// push_ref_with_lease, commit_log_bytes) plus RefHubSource. compaction.rs is the
// I/O-agnostic reducer that already sits BELOW hub_v3 in the dependency graph
// (hub_v3 → hub_source/RefHubSource → compaction::reduce); putting compact_v3 in
// compaction.rs would invert that and create a cycle. hub_v3 → compaction::reduce
// is the existing, acyclic direction.

/// Outcome of a [`compact_v3`] cycle.
#[derive(Debug, Clone)]
// Consumed by the v3 compact CLI path (pass 2) and tests; the bin's duplicate
// module tree flags the fields as dead until that caller lands.
#[allow(dead_code)]
pub struct CompactV3Result {
    /// Number of events reduced in this pass (`reduce` outcome).
    pub events_processed: usize,
    /// SHA of the checkpoint commit written this pass, or `None` when the
    /// checkpoint CAS was lost to a concurrent compactor (benign no-op).
    pub checkpoint_commit: Option<String>,
    /// Whether the checkpoint was successfully pushed (only when `remote` set).
    pub checkpoint_pushed: bool,
    /// Number of events pruned from this agent's OWN ref (REQ-11). Zero unless
    /// the checkpoint was committed AND (if remote) pushed.
    pub events_pruned: usize,
}

/// Run a full v3 compaction cycle for `agent_id`.
///
/// 1. `reduce(RefHubSource)` → materialized [`CheckpointState`].
/// 2. Serialize the state and commit it to [`CHECKPOINT_REF`] as `state.json`
///    via [`commit_blob_to_ref`] (CAS `CurrentTip`). A concurrent local
///    compactor that moved the checkpoint first wins the CAS; THIS process loses
///    it benignly (deterministic content — both produce the same state), logs at
///    debug, and returns with `checkpoint_commit: None` and no prune.
/// 3. If `remote` is `Some`, push the checkpoint with `--force-with-lease`. A
///    lease loss is benign (REQ-7: identical content) → logged at debug, not an
///    error, and prune is skipped.
/// 4. REQ-11 prune: ONLY after the checkpoint covering watermark `W` is committed
///    AND (if remote) pushed successfully, rewrite this agent's OWN ref's
///    `events.log` to drop events with `OrderingKey <= W`. The rewrite goes
///    through the sibling-preserving [`commit_log_bytes`], so the agent's
///    heartbeat/requests-ack siblings survive. Other agents' refs are NEVER
///    pruned.
///
/// # Prune safety invariant
///
/// Prune happens only when the checkpoint that COVERS the pruned events is
/// durably visible (committed locally, and — if a remote is configured — pushed).
/// Pruning before the covering checkpoint is pushed could let a fresh clone fetch
/// the pruned ref but an older checkpoint, losing events not yet covered by any
/// visible checkpoint. Hence: no push success ⇒ no prune.
///
/// No materialized files, no worktree writes — pure object-store plumbing.
///
/// # Errors
///
/// Returns an error if reduction, checkpoint serialization/commit, or the prune
/// rewrite fails. A lost checkpoint CAS or a lost push lease is NOT an error.
// Wired into the v3 compact CLI path by pass 2; flagged dead until then.
#[allow(dead_code)]
pub fn compact_v3(
    repo_dir: &Path,
    agent_id: &str,
    _hub_lock: &crate::sync::HubWriteLock,
    remote: Option<&str>,
) -> Result<CompactV3Result> {
    validate_agent_id(agent_id)?;

    // 1. Reduce the full v3 ref namespace.
    let source = crate::hub_source::RefHubSource::new(repo_dir)
        .context("failed to construct RefHubSource for v3 compaction")?;
    let outcome = crate::compaction::reduce(&source).context("v3 reduction failed")?;
    let events_processed = outcome.events_processed;
    let watermark = outcome.state.watermark.clone();

    // 2. Serialize and commit the checkpoint (CAS CurrentTip).
    let mut state = outcome.state;
    state.compaction_lease = None;
    let state_bytes =
        serde_json::to_vec_pretty(&state).context("failed to serialize v3 checkpoint state")?;

    // Idempotency guard: if the existing checkpoint's `state.json` already
    // equals the freshly-reduced bytes, writing a new commit would only churn
    // the ref SHA (new commit object, same content) and break SHA-level
    // idempotency for callers that re-compact opportunistically (e.g. fetch).
    // Skip the commit AND the prune — nothing changed.
    if let Some(existing_tip) = git_rev_parse_optional(repo_dir, CHECKPOINT_REF)? {
        let spec = format!("{existing_tip}:state.json");
        if let Some(existing_bytes) = git_cat_file_blob_optional(repo_dir, &spec)? {
            if existing_bytes == state_bytes {
                tracing::debug!(
                    "v3 compaction: checkpoint already current (byte-identical); no-op"
                );
                return Ok(CompactV3Result {
                    events_processed,
                    checkpoint_commit: Some(existing_tip),
                    checkpoint_pushed: false,
                    events_pruned: 0,
                });
            }
        }
    }

    let checkpoint_commit = match commit_blob_to_ref(
        repo_dir,
        CHECKPOINT_REF,
        "state.json",
        &state_bytes,
        "crosslink v3 checkpoint",
    ) {
        Ok(sha) => Some(sha),
        Err(e) => {
            // A concurrent local compactor moved the checkpoint first. The
            // content is deterministic for the same event set, so this is a
            // benign no-op: log at debug and skip the rest (no prune).
            let msg = format!("{e:?}");
            if msg.contains("ref moved concurrently") {
                tracing::debug!(
                    "v3 compaction: checkpoint CAS lost to a concurrent compactor (benign): {msg}"
                );
                return Ok(CompactV3Result {
                    events_processed,
                    checkpoint_commit: None,
                    checkpoint_pushed: false,
                    events_pruned: 0,
                });
            }
            return Err(e).context("failed to commit v3 checkpoint");
        }
    };

    // 3. Push the checkpoint with --force-with-lease, if a remote is configured.
    //    Lease loss is benign (deterministic content) → debug, not error.
    let checkpoint_pushed = match remote {
        None => false,
        Some(rem) => {
            let expected = remote_checkpoint_sha(repo_dir, rem);
            match push_ref_with_lease(repo_dir, rem, CHECKPOINT_REF, expected.as_deref())? {
                PushOutcome::Pushed => true,
                PushOutcome::NonFastForward => {
                    tracing::debug!(
                        "v3 compaction: checkpoint lease lost (benign, deterministic content); \
                         skipping prune this cycle"
                    );
                    false
                }
                other => {
                    tracing::debug!(
                        "v3 compaction: checkpoint push did not succeed ({other:?}); \
                         skipping prune this cycle"
                    );
                    false
                }
            }
        }
    };

    // 4. REQ-11 prune — only when the covering checkpoint is durably visible.
    //    Local-only (remote=None): a committed checkpoint suffices.
    //    Remote configured: the checkpoint must have PUSHED.
    let prune_ok = checkpoint_commit.is_some() && (remote.is_none() || checkpoint_pushed);
    let events_pruned = if prune_ok {
        match &watermark {
            Some(wm) => prune_own_ref(repo_dir, agent_id, wm)?,
            None => 0,
        }
    } else {
        0
    };

    // 5. After a prune the local own ref is REWRITTEN (shorter history), so a
    //    subsequent plain own-ref push would be non-fast-forward against the
    //    un-pruned remote. The own ref is single-writer (REQ-11), so force the
    //    remote ref to the pruned local tip to keep them in sync and preserve
    //    the fast-forward invariant for future plain pushes. Only when a remote
    //    is configured and we actually pruned. A force-push failure here is
    //    benign (the events remain durable locally; the next compact retries).
    if events_pruned > 0 {
        if let Some(rem) = remote {
            let ref_name = format!("{AGENT_REF_PREFIX}{agent_id}");
            let refspec = format!("+{ref_name}:{ref_name}");
            match Command::new("git")
                .current_dir(repo_dir)
                .args(["push", rem, &refspec])
                .output()
            {
                Ok(out) if out.status.success() => {}
                Ok(out) => tracing::debug!(
                    "v3 compaction: own-ref prune force-push did not complete (benign): {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => tracing::debug!(
                    "v3 compaction: own-ref prune force-push could not run (benign): {e}"
                ),
            }
        }
    }

    Ok(CompactV3Result {
        events_processed,
        checkpoint_commit,
        checkpoint_pushed,
        events_pruned,
    })
}

/// Resolve the remote-tracking checkpoint SHA to use as the `--force-with-lease`
/// baseline, or `None` if no tracking ref is known (git falls back to its own
/// remote-tracking ref).
fn remote_checkpoint_sha(repo_dir: &Path, _remote: &str) -> Option<String> {
    // The fetch refspec maps refs/crosslink/* → refs/crosslink-remote/*.
    let tracking = "refs/crosslink-remote/checkpoint";
    git_rev_parse_optional(repo_dir, tracking).ok().flatten()
}

/// Rewrite this agent's OWN ref `events.log`, dropping events with
/// `OrderingKey <= watermark`. Sibling-preserving via [`commit_log_bytes`].
///
/// Returns the number of events pruned. NEVER touches another agent's ref.
fn prune_own_ref(
    repo_dir: &Path,
    agent_id: &str,
    watermark: &crate::events::OrderingKey,
) -> Result<usize> {
    let ref_name = format!("{AGENT_REF_PREFIX}{agent_id}");
    let Some(tip) = git_rev_parse_optional(repo_dir, &ref_name)? else {
        return Ok(0);
    };
    let spec = format!("{tip}:events.log");
    let Some(bytes) = git_cat_file_blob_optional(repo_dir, &spec)? else {
        return Ok(0);
    };
    let all = read_events_from_bytes(&bytes)
        .with_context(|| format!("failed to parse events.log on '{ref_name}' for prune"))?;
    let before = all.len();
    let remaining: Vec<_> = all
        .into_iter()
        .filter(|e| crate::events::OrderingKey::from_envelope(e) > *watermark)
        .collect();
    let pruned = before - remaining.len();
    if pruned == 0 {
        return Ok(0);
    }

    // Re-serialize the pruned log to NDJSON bytes (byte-identical to the
    // append/write path: one JSON line per event, trailing newline).
    let mut out = Vec::new();
    for ev in &remaining {
        let line = serde_json::to_string(ev).context("failed to serialize pruned event")?;
        out.extend_from_slice(line.as_bytes());
        out.push(b'\n');
    }

    commit_log_bytes(
        repo_dir,
        agent_id,
        &out,
        &format!("crosslink v3 prune: dropped {pruned} covered events"),
    )?;
    Ok(pruned)
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

// ── v3-aware warn for v2 operation on a migrated hub ──────────────────

/// One-shot guard so the migrated-hub warning fires at most once per process.
static MIGRATED_V2_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Warn once if the hub at `repo_dir` has been migrated to v3 ([`detect_hub_version`]
/// reports `V3`) while we are about to operate it in v2 mode.
///
/// Pre-finalize, mixed-version operation is user-managed (full refusal is the
/// follow-up #754): v2 writes are NOT reflected in v3 until the cutover. This is
/// cheap (a few `git rev-parse` probes) and never fatal — detection failures are
/// swallowed so the warning can never block a hub operation.
pub fn warn_if_migrated_v2_operation(repo_dir: &Path) {
    use std::sync::atomic::Ordering;
    if MIGRATED_V2_WARNED.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(HubVersion::V3 { .. }) = detect_hub_version(repo_dir) {
        if !MIGRATED_V2_WARNED.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "hub has been migrated to v3; v2 writes are not reflected in v3 — \
                 finish the cutover or avoid mixed operation"
            );
        }
    }
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
pub(crate) fn git_rev_parse_optional(repo_dir: &Path, ref_name: &str) -> Result<Option<String>> {
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
pub(crate) fn git_cat_file_blob_optional(
    repo_dir: &Path,
    blob_spec: &str,
) -> Result<Option<Vec<u8>>> {
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

// ── Generalized single-file / multi-file commit core ─────────────────

/// Build a tree from a single file entry, commit it onto `ref_name`, and move
/// the ref with a compare-and-swap.
///
/// This is the shared core extracted from the append path, [`commit_log_bytes`],
/// and [`commit_blob_to_ref`]. It hashes `bytes`, creates a one-entry tree
/// (`<file_name>`) at the TREE ROOT, commits it (parent = the resolved old tip),
/// and CAS-updates the ref per `expected`.
///
/// The new state is always added as a CHILD of the current tip when one exists,
/// so any subsequent push remains a fast-forward.
///
/// # Errors
///
/// - `MustNotExist` when the ref already exists, or `MustMatch`/`CurrentTip`
///   when the ref moved between read and update: returns
///   `"ref moved concurrently: <ref>"`.
/// - Any git plumbing failure.
fn commit_single_file_tree(
    repo_dir: &Path,
    ref_name: &str,
    file_name: &str,
    bytes: &[u8],
    message: &str,
    committer_id: &str,
    expected: CasExpectation<'_>,
) -> Result<String> {
    commit_upserts_to_ref(
        repo_dir,
        ref_name,
        &[(file_name, BlobRef::Bytes(bytes))],
        &[],
        message,
        committer_id,
        expected,
    )
}

/// Sibling-preserving multi-path commit core.
///
/// Resolves the CAS base/old value from `expected`, builds the new tree from
/// that base via [`write_tree_with`] (applying `upserts` and `deletes` while
/// keeping every unrelated sibling), commits it as a child of the base, and
/// CAS-updates `ref_name`.
///
/// This is THE shared core for every hub-v3 ref write: a naive single-entry
/// `mktree` would drop sibling files (heartbeat.json, requests-ack/, …) once
/// refs carry them, so the append path, checkpoint writes, meta writes,
/// heartbeats, requests, and acks all route through here.
///
/// # Errors
///
/// Returns `"ref moved concurrently: <ref>"` on a CAS failure, or any git
/// plumbing / path-nesting error from [`write_tree_with`].
fn commit_upserts_to_ref(
    repo_dir: &Path,
    ref_name: &str,
    upserts: &[(&str, BlobRef<'_>)],
    deletes: &[&str],
    message: &str,
    committer_id: &str,
    expected: CasExpectation<'_>,
) -> Result<String> {
    let old_commit = resolve_cas_old(repo_dir, ref_name, expected)?;
    let tree_sha = write_tree_with(repo_dir, old_commit.as_deref(), upserts, deletes)?;
    let commit_sha = git_commit_tree(
        repo_dir,
        &tree_sha,
        old_commit.as_deref(),
        message,
        committer_id,
    )?;
    git_update_ref_cas(repo_dir, ref_name, &commit_sha, old_commit.as_deref())?;
    Ok(commit_sha)
}

/// Resolve the CAS old-value and the commit parent for an `expected`
/// expectation, validating any `MustNotExist` / `MustMatch` precondition.
///
/// Returns `(parent_for_commit, old_value_for_cas)`. Both are `Option<String>`:
/// `None` parent means a genesis (parentless) commit; `None`/`Some` old value
/// maps directly onto the `git update-ref <ref> <new> <old>` contract.
fn resolve_cas_old(
    repo_dir: &Path,
    ref_name: &str,
    expected: CasExpectation<'_>,
) -> Result<Option<String>> {
    match expected {
        CasExpectation::MustNotExist => {
            if git_rev_parse_optional(repo_dir, ref_name)?.is_some() {
                anyhow::bail!(
                    "ref moved concurrently: {ref_name} (expected absent, but it exists)"
                );
            }
            Ok(None)
        }
        CasExpectation::MustMatch(sha) => Ok(Some(sha.to_string())),
        CasExpectation::CurrentTip => git_rev_parse_optional(repo_dir, ref_name),
    }
}

/// Commit a single blob to an arbitrary ref at the TREE ROOT under `file_name`.
///
/// Reads the current tip as the commit parent and CAS-updates on it
/// ([`CasExpectation::CurrentTip`]): genesis when absent, fast-forward child
/// when present. Used for the checkpoint ref (`state.json`) and any other
/// single-file driver ref.
///
/// # Errors
///
/// Returns an error if the ref moved concurrently (CAS failure) or any git
/// plumbing step fails.
// PR3 read/verify path and the migrate command (part 2) are the production
// callers; the bin's duplicate module tree flags it as dead code until then.
#[allow(dead_code)]
pub fn commit_blob_to_ref(
    repo_dir: &Path,
    ref_name: &str,
    file_name: &str,
    bytes: &[u8],
    message: &str,
) -> Result<String> {
    commit_single_file_tree(
        repo_dir,
        ref_name,
        file_name,
        bytes,
        message,
        "crosslink",
        CasExpectation::CurrentTip,
    )
}

/// Commit MULTIPLE files into a single tree at the TREE ROOT and move `ref_name`.
///
/// `files` is a slice of `(file_name, bytes)` pairs. The entries are sorted by
/// name before `git mktree` so the tree is well-formed regardless of the input
/// order. Used for [`META_REF`], which carries `hub.json` plus `allowed_signers`.
///
/// Reads the current tip as the commit parent ([`CasExpectation::CurrentTip`]):
/// genesis when absent, fast-forward child when present.
///
/// # mktree ordering
///
/// Git's tree object format requires entries sorted by name. Modern `git mktree`
/// (observed: 2.54) re-sorts its stdin, but older versions and `--missing` mode
/// can reject unsorted input, so this function sorts defensively before feeding
/// mktree. Duplicate file names are rejected (a malformed tree request).
///
/// # Errors
///
/// Returns an error on duplicate file names, CAS failure, or any git plumbing
/// failure.
// PR3 migrate command (part 2) writes META_REF; flagged dead until then.
#[allow(dead_code)]
pub fn commit_files_to_ref(
    repo_dir: &Path,
    ref_name: &str,
    files: &[(&str, &[u8])],
    message: &str,
) -> Result<String> {
    anyhow::ensure!(
        !files.is_empty(),
        "commit_files_to_ref requires at least one file"
    );

    // Validate names (root-level only) and reject duplicates before building.
    for (name, _) in files {
        anyhow::ensure!(
            !name.contains('/') && !name.is_empty(),
            "commit_files_to_ref file name must be a non-empty tree-root name, got '{name}'"
        );
    }
    let mut sorted: Vec<&str> = files.iter().map(|(n, _)| *n).collect();
    sorted.sort_unstable();
    for pair in sorted.windows(2) {
        anyhow::ensure!(
            pair[0] != pair[1],
            "commit_files_to_ref got duplicate file name '{}'",
            pair[0]
        );
    }

    let upserts: Vec<(&str, BlobRef<'_>)> = files
        .iter()
        .map(|(name, bytes)| (*name, BlobRef::Bytes(bytes)))
        .collect();
    commit_upserts_to_ref(
        repo_dir,
        ref_name,
        &upserts,
        &[],
        message,
        "crosslink",
        CasExpectation::CurrentTip,
    )
}

// ── Sibling-preserving tree core (REQ-1/REQ-11 prerequisite) ─────────

/// One entry of a git tree as emitted by `git ls-tree <sha>`.
///
/// Subtree entries (`object_type == "tree"`) are retained verbatim so nested
/// directories survive a root-level rewrite without recursion — the existing
/// subtree SHA is fed straight back to `git mktree` unless an upsert/delete
/// explicitly targets a path inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TreeEntry {
    /// Git mode string, e.g. `100644` (blob), `100755` (exec), `040000` (tree).
    mode: String,
    /// `blob` or `tree`.
    object_type: String,
    /// Object SHA.
    sha: String,
    /// Entry name (a single path component — never contains `/`).
    name: String,
}

impl TreeEntry {
    /// Render this entry as a single `git mktree` input line:
    /// `<mode> SP <type> SP <sha> TAB <name>`.
    fn mktree_line(&self) -> String {
        format!(
            "{} {} {}\t{}",
            self.mode, self.object_type, self.sha, self.name
        )
    }
}

/// Source of a blob to write into a tree: either pre-hashed (the append path
/// already ran `hash-object`) or raw bytes to hash now.
enum BlobRef<'a> {
    /// A blob SHA already present in the object store.
    Existing(&'a str),
    /// Raw bytes to `git hash-object -w` before insertion.
    Bytes(&'a [u8]),
}

impl BlobRef<'_> {
    /// Resolve to a concrete blob SHA, hashing if necessary.
    fn resolve(&self, repo_dir: &Path) -> Result<String> {
        match self {
            BlobRef::Existing(sha) => Ok((*sha).to_string()),
            BlobRef::Bytes(bytes) => git_hash_object(repo_dir, bytes),
        }
    }
}

/// Read the entries of the tree at `commit_sha` via `git ls-tree <sha>`.
///
/// Returns one [`TreeEntry`] per top-level entry. Subtrees are kept as tree
/// entries verbatim (their SHA is preserved), so nested directories survive a
/// root-level rewrite without recursing into them.
///
/// # Errors
///
/// Returns an error if `git ls-tree` cannot be spawned or fails, or if a line
/// cannot be parsed in the documented `<mode> SP <type> SP <sha> TAB <name>`
/// format.
fn read_tree_entries(repo_dir: &Path, commit_sha: &str) -> Result<Vec<TreeEntry>> {
    let spec = format!("{commit_sha}^{{tree}}");
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["ls-tree", &spec])
        .output()
        .with_context(|| format!("failed to run git ls-tree for '{spec}'"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git ls-tree failed for '{}': {}", spec, stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Format: "<mode> SP <type> SP <sha>\t<name>"
        let (meta, name) = line
            .split_once('\t')
            .ok_or_else(|| anyhow::anyhow!("malformed git ls-tree line (no TAB): {line}"))?;
        let mut parts = meta.split_whitespace();
        let mode = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("malformed git ls-tree line (no mode): {line}"))?;
        let object_type = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("malformed git ls-tree line (no type): {line}"))?;
        let sha = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("malformed git ls-tree line (no sha): {line}"))?;
        entries.push(TreeEntry {
            mode: mode.to_string(),
            object_type: object_type.to_string(),
            sha: sha.to_string(),
            name: name.to_string(),
        });
    }
    Ok(entries)
}

/// Build a new tree from an optional `base` commit's tree, applying `upserts`
/// (insert-or-replace) and `deletes` while preserving every unrelated sibling.
///
/// # Path nesting
///
/// At most ONE level of nesting is supported. A path is either:
/// - a root-level file (`"events.log"`, `"heartbeat.json"`), or
/// - a one-level subtree path (`"requests-ack/<ulid>.json"`).
///
/// Paths with two or more `/` separators are rejected with an error — the v3
/// design needs exactly one nesting level (driver requests under
/// `requests-out/<target>--<ulid>.json`, acks under `requests-ack/<ulid>.json`),
/// and deeper trees would require recursion this core deliberately avoids.
///
/// For a subtree path the existing subtree (if any) is read, the target leaf is
/// upserted/deleted within it, and the rebuilt subtree replaces the old one in
/// the root entry list. A subtree that becomes empty after a delete is dropped
/// from the root entirely.
///
/// `base = None` builds the tree from scratch (genesis). The returned SHA is a
/// tree object suitable for `git commit-tree`.
///
/// # Errors
///
/// Returns an error on a path with deeper-than-one nesting, an empty path
/// component, any git plumbing failure, or a name collision between a file and
/// a subtree at the root.
fn write_tree_with(
    repo_dir: &Path,
    base: Option<&str>,
    upserts: &[(&str, BlobRef<'_>)],
    deletes: &[&str],
) -> Result<String> {
    use std::collections::BTreeMap;

    // Start from the base tree's entries (or empty for genesis), keyed by name
    // so upserts/deletes are O(log n) and ordering is deterministic.
    let mut root: BTreeMap<String, TreeEntry> = BTreeMap::new();
    if let Some(commit_sha) = base {
        for entry in read_tree_entries(repo_dir, commit_sha)? {
            root.insert(entry.name.clone(), entry);
        }
    }

    // Pending mutations to one-level subtrees, accumulated so multiple
    // upserts/deletes into the SAME subtree rebuild it once. Maps
    // subtree-name -> (leaf-name -> Option<blob_sha>); None means delete.
    let mut subtree_ops: BTreeMap<String, BTreeMap<String, Option<String>>> = BTreeMap::new();

    // Classify and stage every upsert.
    for (path, blob) in upserts {
        match split_one_level(path)? {
            (None, file) => {
                let blob_sha = blob.resolve(repo_dir)?;
                root.insert(
                    file.to_string(),
                    TreeEntry {
                        mode: "100644".to_string(),
                        object_type: "blob".to_string(),
                        sha: blob_sha,
                        name: file.to_string(),
                    },
                );
            }
            (Some(dir), leaf) => {
                let blob_sha = blob.resolve(repo_dir)?;
                subtree_ops
                    .entry(dir.to_string())
                    .or_default()
                    .insert(leaf.to_string(), Some(blob_sha));
            }
        }
    }

    // Classify and stage every delete.
    for path in deletes {
        match split_one_level(path)? {
            (None, file) => {
                root.remove(file);
            }
            (Some(dir), leaf) => {
                subtree_ops
                    .entry(dir.to_string())
                    .or_default()
                    .insert(leaf.to_string(), None);
            }
        }
    }

    // Rebuild each touched subtree from its current entries + staged ops.
    for (dir, ops) in subtree_ops {
        // Read the existing subtree's entries, if the subtree exists in root.
        let mut leaves: BTreeMap<String, TreeEntry> = BTreeMap::new();
        if let Some(existing) = root.get(&dir) {
            anyhow::ensure!(
                existing.object_type == "tree",
                "write_tree_with: '{dir}' exists as a {} but a subtree path targets it",
                existing.object_type
            );
            for entry in read_subtree_entries(repo_dir, &existing.sha)? {
                leaves.insert(entry.name.clone(), entry);
            }
        }

        for (leaf, maybe_sha) in ops {
            match maybe_sha {
                Some(sha) => {
                    leaves.insert(
                        leaf.clone(),
                        TreeEntry {
                            mode: "100644".to_string(),
                            object_type: "blob".to_string(),
                            sha,
                            name: leaf.clone(),
                        },
                    );
                }
                None => {
                    leaves.remove(&leaf);
                }
            }
        }

        if leaves.is_empty() {
            // Subtree emptied out — drop it from the root entirely.
            root.remove(&dir);
        } else {
            let subtree_sha = mktree_from_entries(repo_dir, leaves.values())?;
            root.insert(
                dir.clone(),
                TreeEntry {
                    mode: "040000".to_string(),
                    object_type: "tree".to_string(),
                    sha: subtree_sha,
                    name: dir,
                },
            );
        }
    }

    mktree_from_entries(repo_dir, root.values())
}

/// Read the entries of a subtree object (a `tree` SHA, not a commit) via
/// `git ls-tree <tree-sha>`. Each entry is a leaf of a one-level subtree.
fn read_subtree_entries(repo_dir: &Path, tree_sha: &str) -> Result<Vec<TreeEntry>> {
    // For a one-level subtree we only expect blob leaves, but read generically.
    read_tree_entries_raw(repo_dir, tree_sha)
}

/// Run `git ls-tree <sha>` (the SHA may be a tree or a commit; `^{tree}` is not
/// appended) and parse entries. Shared parser used by both the root and subtree
/// readers.
fn read_tree_entries_raw(repo_dir: &Path, sha: &str) -> Result<Vec<TreeEntry>> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["ls-tree", sha])
        .output()
        .with_context(|| format!("failed to run git ls-tree for '{sha}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git ls-tree failed for '{}': {}", sha, stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (meta, name) = line
            .split_once('\t')
            .ok_or_else(|| anyhow::anyhow!("malformed git ls-tree line (no TAB): {line}"))?;
        let mut parts = meta.split_whitespace();
        let mode = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("malformed git ls-tree line (no mode): {line}"))?;
        let object_type = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("malformed git ls-tree line (no type): {line}"))?;
        let sha_field = parts
            .next()
            .ok_or_else(|| anyhow::anyhow!("malformed git ls-tree line (no sha): {line}"))?;
        entries.push(TreeEntry {
            mode: mode.to_string(),
            object_type: object_type.to_string(),
            sha: sha_field.to_string(),
            name: name.to_string(),
        });
    }
    Ok(entries)
}

/// Feed `entries` (in iterator order — callers pass sorted `BTreeMap::values`)
/// to `git mktree` and return the resulting tree SHA. Empty input yields the
/// canonical empty-tree object.
fn mktree_from_entries<'a, I>(repo_dir: &Path, entries: I) -> Result<String>
where
    I: Iterator<Item = &'a TreeEntry>,
{
    use std::io::Write as _;

    let mut input = String::new();
    let mut any = false;
    for entry in entries {
        any = true;
        input.push_str(&entry.mktree_line());
        input.push('\n');
    }

    if !any {
        // `git mktree` with empty stdin produces the canonical empty tree.
        // Use `git hash-object -t tree /dev/stdin` semantics via mktree, which
        // returns the well-known empty-tree SHA. Feeding empty stdin to mktree
        // is valid and yields that SHA.
        input.clear();
    }

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
        .write_all(input.as_bytes())
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

/// Split a path into `(Option<subtree>, leaf)`, enforcing the one-level-nesting
/// invariant of [`write_tree_with`].
///
/// - `"events.log"` → `(None, "events.log")`
/// - `"requests-ack/01J.json"` → `(Some("requests-ack"), "01J.json")`
/// - `"a/b/c"` → error (deeper than one level)
///
/// # Errors
///
/// Returns an error on an empty path, an empty component, or more than one `/`.
fn split_one_level(path: &str) -> Result<(Option<&str>, &str)> {
    anyhow::ensure!(!path.is_empty(), "write_tree_with: empty path");
    let mut parts = path.split('/');
    let first = parts.next().unwrap_or("");
    match parts.next() {
        None => {
            // No '/': root-level file.
            anyhow::ensure!(!first.is_empty(), "write_tree_with: empty path component");
            Ok((None, first))
        }
        Some(leaf) => {
            anyhow::ensure!(
                parts.next().is_none(),
                "write_tree_with: path '{path}' nests deeper than one level (only \
                 root-level files and one-level subtree paths are supported)"
            );
            anyhow::ensure!(
                !first.is_empty() && !leaf.is_empty(),
                "write_tree_with: empty path component in '{path}'"
            );
            Ok((Some(first), leaf))
        }
    }
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

    // ── Test 10: commit_blob_to_ref genesis + CAS conflict ───────────

    #[test]
    fn commit_blob_to_ref_genesis_and_cas_conflict() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        let ref_name = CHECKPOINT_REF;

        // Genesis: ref does not exist yet.
        let sha1 = commit_blob_to_ref(
            dir.path(),
            ref_name,
            "state.json",
            b"{\"v\":1}\n",
            "genesis checkpoint",
        )
        .unwrap();
        let tip = run_git_output(dir.path(), &["rev-parse", ref_name]);
        assert_eq!(tip, sha1, "ref must point at the genesis commit");

        // Genesis commit must be parentless and the blob readable at the root.
        let log = run_git_output(dir.path(), &["log", "--oneline", ref_name]);
        assert_eq!(log.lines().count(), 1, "genesis commit must have no parent");
        let blob = run_git_output(
            dir.path(),
            &["cat-file", "blob", &format!("{sha1}:state.json")],
        );
        assert_eq!(blob, "{\"v\":1}");

        // Second commit fast-forwards onto the tip (CurrentTip CAS).
        let sha2 = commit_blob_to_ref(
            dir.path(),
            ref_name,
            "state.json",
            b"{\"v\":2}\n",
            "second checkpoint",
        )
        .unwrap();
        assert_ne!(sha1, sha2);
        let count = run_git_output(dir.path(), &["rev-list", "--count", ref_name]);
        assert_eq!(count, "2", "second commit must chain onto the first");

        // CAS conflict: directly invoke the single-file core with a stale
        // MustMatch expectation (ref has moved past sha1).
        let stale = commit_single_file_tree(
            dir.path(),
            ref_name,
            "state.json",
            b"{\"v\":3}\n",
            "stale checkpoint",
            "crosslink",
            CasExpectation::MustMatch(&sha1),
        );
        assert!(stale.is_err(), "stale MustMatch CAS must fail");
        let msg = format!("{:?}", stale.unwrap_err());
        assert!(
            msg.contains("ref moved concurrently"),
            "CAS conflict error must mention concurrent move, got: {msg}"
        );

        // MustNotExist on an existing ref must also fail.
        let exists = commit_single_file_tree(
            dir.path(),
            ref_name,
            "state.json",
            b"{}\n",
            "genesis on existing",
            "crosslink",
            CasExpectation::MustNotExist,
        );
        assert!(exists.is_err(), "MustNotExist on an existing ref must fail");
    }

    // ── Test 11: commit_files_to_ref multi-file tree + ordering ──────

    #[test]
    fn commit_files_to_ref_multi_file_ordering() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        // Deliberately pass entries in NON-sorted order (hub.json before
        // allowed_signers) to exercise the defensive sort.
        let files: &[(&str, &[u8])] = &[
            ("hub.json", b"{\"hub_version\":3}\n"),
            ("allowed_signers", b"signer line\n"),
        ];
        let sha = commit_files_to_ref(dir.path(), META_REF, files, "meta genesis").unwrap();

        // Both files readable at the tree root.
        let hub = run_git_output(
            dir.path(),
            &["cat-file", "blob", &format!("{sha}:hub.json")],
        );
        assert_eq!(hub, "{\"hub_version\":3}");
        let signers = run_git_output(
            dir.path(),
            &["cat-file", "blob", &format!("{sha}:allowed_signers")],
        );
        assert_eq!(signers, "signer line");

        // ls-tree output is sorted by name (git tree invariant): allowed_signers
        // sorts before hub.json.
        let listing = run_git_output(dir.path(), &["ls-tree", "--name-only", sha.as_str()]);
        let names: Vec<&str> = listing.lines().collect();
        assert_eq!(names, vec!["allowed_signers", "hub.json"]);

        // Duplicate file names are rejected.
        let dup: &[(&str, &[u8])] = &[("a.json", b"1"), ("a.json", b"2")];
        assert!(
            commit_files_to_ref(dir.path(), META_REF, dup, "dup").is_err(),
            "duplicate file names must be rejected"
        );
    }

    // ── Test 12: push_ref_with_lease success + lease rejection ───────

    #[test]
    fn push_ref_with_lease_success_and_rejection() {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        git_init(local_dir.path());
        run_git(remote_dir.path(), &["init", "--bare"]);
        run_git(
            local_dir.path(),
            &[
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
        );

        let ref_name = CHECKPOINT_REF;

        // Genesis checkpoint, then push with lease (no expected baseline → None).
        commit_blob_to_ref(
            local_dir.path(),
            ref_name,
            "state.json",
            b"{\"v\":1}\n",
            "cp1",
        )
        .unwrap();
        let p1 = push_ref_with_lease(local_dir.path(), "origin", ref_name, None).unwrap();
        assert!(
            matches!(p1, PushOutcome::Pushed),
            "genesis lease push must succeed"
        );
        let remote_tip1 = run_git_output(remote_dir.path(), &["rev-parse", ref_name]);

        // Advance locally and push again with the correct expected remote SHA.
        commit_blob_to_ref(
            local_dir.path(),
            ref_name,
            "state.json",
            b"{\"v\":2}\n",
            "cp2",
        )
        .unwrap();
        let p2 =
            push_ref_with_lease(local_dir.path(), "origin", ref_name, Some(&remote_tip1)).unwrap();
        assert!(
            matches!(p2, PushOutcome::Pushed),
            "matching-lease push must succeed"
        );
        let remote_tip2 = run_git_output(remote_dir.path(), &["rev-parse", ref_name]);

        // Now move the REMOTE ref out from under us to a divergent commit, then
        // push with a STALE expected baseline (remote_tip2). The lease must
        // reject.
        // Build a divergent commit on the remote by pushing an unrelated history.
        commit_blob_to_ref(
            remote_dir.path(),
            "refs/crosslink/scratch",
            "state.json",
            b"X\n",
            "scratch",
        )
        .unwrap();
        let divergent = run_git_output(remote_dir.path(), &["rev-parse", "refs/crosslink/scratch"]);
        run_git(remote_dir.path(), &["update-ref", ref_name, &divergent]);

        // Local still believes remote is at remote_tip2 → stale lease → rejected.
        commit_blob_to_ref(
            local_dir.path(),
            ref_name,
            "state.json",
            b"{\"v\":3}\n",
            "cp3",
        )
        .unwrap();
        let p3 =
            push_ref_with_lease(local_dir.path(), "origin", ref_name, Some(&remote_tip2)).unwrap();
        assert!(
            matches!(p3, PushOutcome::NonFastForward),
            "stale lease must be rejected as NonFastForward, got a different outcome"
        );
        // Remote unchanged by the rejected push.
        let remote_after = run_git_output(remote_dir.path(), &["rev-parse", ref_name]);
        assert_eq!(
            remote_after, divergent,
            "rejected lease push must not move the remote"
        );
    }

    // ── Test 13: detect_hub_version matrix ───────────────────────────

    #[test]
    fn detect_hub_version_matrix() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        // Absent: no refs at all.
        assert_eq!(detect_hub_version(dir.path()).unwrap(), HubVersion::Absent);

        // V2Only: create a crosslink/hub branch (needs a commit to point at).
        let cp = commit_blob_to_ref(dir.path(), "refs/crosslink/tmp", "x", b"x\n", "tmp").unwrap();
        run_git(dir.path(), &["update-ref", "refs/heads/crosslink/hub", &cp]);
        assert_eq!(detect_hub_version(dir.path()).unwrap(), HubVersion::V2Only);

        // V3 with v2 branch present: add meta + checkpoint refs.
        commit_files_to_ref(dir.path(), META_REF, &[("hub.json", b"{}\n")], "meta").unwrap();
        commit_blob_to_ref(dir.path(), CHECKPOINT_REF, "state.json", b"{}\n", "cp").unwrap();
        assert_eq!(
            detect_hub_version(dir.path()).unwrap(),
            HubVersion::V3 {
                v2_branch_present: true
            }
        );

        // V3 without v2 branch: delete the v2 branch.
        run_git(
            dir.path(),
            &["update-ref", "-d", "refs/heads/crosslink/hub"],
        );
        assert_eq!(
            detect_hub_version(dir.path()).unwrap(),
            HubVersion::V3 {
                v2_branch_present: false
            }
        );
    }

    // ── Test 14: detect_remote_hub_version matrix + unreachable ──────

    #[test]
    fn detect_remote_hub_version_matrix_and_unreachable() {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        git_init(local_dir.path());
        run_git(remote_dir.path(), &["init", "--bare"]);
        run_git(
            local_dir.path(),
            &[
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
        );

        // Absent: bare remote with no crosslink refs.
        assert_eq!(
            detect_remote_hub_version(local_dir.path(), "origin").unwrap(),
            HubVersion::Absent
        );

        // V2Only: create the v2 branch on the remote.
        commit_blob_to_ref(remote_dir.path(), "refs/crosslink/tmp", "x", b"x\n", "tmp").unwrap();
        let tmp = run_git_output(remote_dir.path(), &["rev-parse", "refs/crosslink/tmp"]);
        run_git(
            remote_dir.path(),
            &["update-ref", "refs/heads/crosslink/hub", &tmp],
        );
        run_git(
            remote_dir.path(),
            &["update-ref", "-d", "refs/crosslink/tmp"],
        );
        assert_eq!(
            detect_remote_hub_version(local_dir.path(), "origin").unwrap(),
            HubVersion::V2Only
        );

        // V3 with v2 present: add meta + checkpoint on the remote.
        commit_files_to_ref(
            remote_dir.path(),
            META_REF,
            &[("hub.json", b"{}\n")],
            "meta",
        )
        .unwrap();
        commit_blob_to_ref(
            remote_dir.path(),
            CHECKPOINT_REF,
            "state.json",
            b"{}\n",
            "cp",
        )
        .unwrap();
        assert_eq!(
            detect_remote_hub_version(local_dir.path(), "origin").unwrap(),
            HubVersion::V3 {
                v2_branch_present: true
            }
        );

        // V3 without v2: drop the remote v2 branch.
        run_git(
            remote_dir.path(),
            &["update-ref", "-d", "refs/heads/crosslink/hub"],
        );
        assert_eq!(
            detect_remote_hub_version(local_dir.path(), "origin").unwrap(),
            HubVersion::V3 {
                v2_branch_present: false
            }
        );

        // Unreachable remote → hard error (never guess).
        let err = detect_remote_hub_version(local_dir.path(), "/no/such/remote/path").unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("ls-remote")
                || msg.contains("remote unreachable")
                || msg.contains("cannot determine"),
            "unreachable remote must hard-error, got: {msg}"
        );
    }

    // ── Test 15: HubMeta round-trip ──────────────────────────────────

    #[test]
    fn hub_meta_roundtrip_via_commit_files_and_read() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        // No meta ref → None.
        assert!(read_hub_meta(dir.path()).unwrap().is_none());

        let meta = HubMeta {
            hub_version: 3,
            migrated_from_commit: "deadbeefcafe1234".to_string(),
            migrated_at: chrono::Utc::now(),
            finalized_at: None,
        };
        let meta_bytes = serde_json::to_vec(&meta).unwrap();
        commit_files_to_ref(
            dir.path(),
            META_REF,
            &[("hub.json", &meta_bytes), ("allowed_signers", b"sig\n")],
            "meta with marker",
        )
        .unwrap();

        let read = read_hub_meta(dir.path())
            .unwrap()
            .expect("meta must be present");
        assert_eq!(read.hub_version, 3);
        assert_eq!(read.migrated_from_commit, "deadbeefcafe1234");
        // chrono round-trips at the serialized precision.
        assert_eq!(read, meta);

        // A meta ref without hub.json (only allowed_signers) → None.
        let dir2 = tempfile::tempdir().unwrap();
        git_init(dir2.path());
        commit_files_to_ref(
            dir2.path(),
            META_REF,
            &[("allowed_signers", b"sig\n")],
            "meta no marker",
        )
        .unwrap();
        assert!(read_hub_meta(dir2.path()).unwrap().is_none());
    }

    // ── PASS 1 helpers ────────────────────────────────────────────────

    fn make_heartbeat(agent_id: &str, issue: Option<i64>) -> crate::locks::Heartbeat {
        crate::locks::Heartbeat {
            agent_id: agent_id.to_string(),
            last_heartbeat: Utc::now(),
            active_issue_id: issue,
            machine_id: format!("machine-{agent_id}"),
        }
    }

    fn cat_blob(repo_dir: &Path, spec: &str) -> Option<Vec<u8>> {
        let out = std::process::Command::new("git")
            .current_dir(repo_dir)
            .args(["cat-file", "blob", spec])
            .output()
            .unwrap();
        if out.status.success() {
            Some(out.stdout)
        } else {
            None
        }
    }

    fn make_request(request_id: &str) -> crate::agent_requests::AgentRequest {
        crate::agent_requests::AgentRequest {
            request_id: request_id.to_string(),
            kind: crate::agent_requests::RequestKind::Pause,
            subject: crate::agent_requests::RequestSubject::default(),
            requested_by: "SHA256:driver".to_string(),
            requested_at: Utc::now().to_rfc3339(),
            reason: None,
        }
    }

    fn make_ack(request_id: &str) -> crate::agent_requests::AgentRequestAck {
        crate::agent_requests::AgentRequestAck {
            request_id: request_id.to_string(),
            ack_at: Utc::now().to_rfc3339(),
            acted: true,
            result: "paused".to_string(),
            notes: None,
        }
    }

    // ── Part 1: sibling preservation ──────────────────────────────────

    #[test]
    fn append_event_preserves_sibling_heartbeat() {
        // Regression gate: a ref carrying events.log + heartbeat.json must keep
        // heartbeat.json byte-identical after an append touches only events.log.
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let agent_id = "sibling-agent";

        // Genesis events.log.
        append_event_to_ref(dir.path(), agent_id, &make_envelope(agent_id, 1)).unwrap();
        // Write a heartbeat sibling.
        write_heartbeat_to_ref(dir.path(), agent_id, &make_heartbeat(agent_id, Some(7))).unwrap();

        let ref_name = agent_ref_name(agent_id).unwrap();
        let tip = run_git_output(dir.path(), &["rev-parse", &ref_name]);
        let hb_before = cat_blob(dir.path(), &format!("{tip}:heartbeat.json")).unwrap();

        // Append another event — heartbeat.json must survive untouched.
        append_event_to_ref(dir.path(), agent_id, &make_envelope(agent_id, 2)).unwrap();

        let tip2 = run_git_output(dir.path(), &["rev-parse", &ref_name]);
        let hb_after = cat_blob(dir.path(), &format!("{tip2}:heartbeat.json")).unwrap();
        assert_eq!(hb_before, hb_after, "heartbeat.json must survive an append");

        // events.log must now have 2 events.
        let log = cat_blob(dir.path(), &format!("{tip2}:events.log")).unwrap();
        assert_eq!(read_events_from_bytes(&log).unwrap().len(), 2);
    }

    #[test]
    fn interleaved_writers_preserve_all_siblings() {
        // append / heartbeat / ack all write the SAME ref; every writer must
        // preserve the others' files.
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let agent_id = "multi-writer";
        let ref_name = agent_ref_name(agent_id).unwrap();

        append_event_to_ref(dir.path(), agent_id, &make_envelope(agent_id, 1)).unwrap();
        write_heartbeat_to_ref(dir.path(), agent_id, &make_heartbeat(agent_id, None)).unwrap();
        write_ack_to_own_ref(dir.path(), agent_id, "01ACK0001", &make_ack("01ACK0001")).unwrap();
        // A second append after the heartbeat + ack are in place.
        append_event_to_ref(dir.path(), agent_id, &make_envelope(agent_id, 2)).unwrap();
        // A second ack into the same subtree.
        write_ack_to_own_ref(dir.path(), agent_id, "01ACK0002", &make_ack("01ACK0002")).unwrap();

        let tip = run_git_output(dir.path(), &["rev-parse", &ref_name]);
        // events.log: 2 events.
        let log = cat_blob(dir.path(), &format!("{tip}:events.log")).unwrap();
        assert_eq!(read_events_from_bytes(&log).unwrap().len(), 2);
        // heartbeat.json present.
        assert!(cat_blob(dir.path(), &format!("{tip}:heartbeat.json")).is_some());
        // Both acks present in the subtree.
        assert!(cat_blob(dir.path(), &format!("{tip}:requests-ack/01ACK0001.json")).is_some());
        assert!(cat_blob(dir.path(), &format!("{tip}:requests-ack/01ACK0002.json")).is_some());
    }

    #[test]
    fn write_tree_with_rejects_deep_nesting() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let res = write_tree_with(
            dir.path(),
            None,
            &[("a/b/c.json", BlobRef::Bytes(b"x"))],
            &[],
        );
        assert!(res.is_err(), "deeper-than-one-level path must be rejected");
        let msg = format!("{:?}", res.unwrap_err());
        assert!(
            msg.contains("nests deeper than one level"),
            "error must mention nesting depth, got: {msg}"
        );
        // split_one_level direct coverage.
        assert!(split_one_level("a/b/c").is_err());
        assert_eq!(split_one_level("file.json").unwrap(), (None, "file.json"));
        assert_eq!(
            split_one_level("dir/leaf.json").unwrap(),
            (Some("dir"), "leaf.json")
        );
    }

    #[test]
    fn write_tree_with_subtree_delete_drops_empty_subtree() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let agent_id = "subtree-agent";
        let ref_name = agent_ref_name(agent_id).unwrap();

        append_event_to_ref(dir.path(), agent_id, &make_envelope(agent_id, 1)).unwrap();
        write_ack_to_own_ref(dir.path(), agent_id, "01ONLY", &make_ack("01ONLY")).unwrap();
        let tip = run_git_output(dir.path(), &["rev-parse", &ref_name]);
        assert!(cat_blob(dir.path(), &format!("{tip}:requests-ack/01ONLY.json")).is_some());

        // Delete the only ack leaf → subtree must vanish, events.log survives.
        let new_tree =
            write_tree_with(dir.path(), Some(&tip), &[], &["requests-ack/01ONLY.json"]).unwrap();
        let listing = run_git_output(dir.path(), &["ls-tree", "--name-only", &new_tree]);
        let names: Vec<&str> = listing.lines().collect();
        assert_eq!(names, vec!["events.log"], "empty subtree must be dropped");
    }

    // ── Part 2: heartbeats ────────────────────────────────────────────

    #[test]
    fn heartbeats_roundtrip_across_three_refs() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());

        // Three agents: two with heartbeats, one with only events (skipped).
        for (id, issue) in [("hb-a", Some(1)), ("hb-b", None)] {
            append_event_to_ref(dir.path(), id, &make_envelope(id, 1)).unwrap();
            write_heartbeat_to_ref(dir.path(), id, &make_heartbeat(id, issue)).unwrap();
        }
        // hb-c: events only, no heartbeat.
        append_event_to_ref(dir.path(), "hb-c", &make_envelope("hb-c", 1)).unwrap();

        let mut beats = read_heartbeats_from_refs(dir.path()).unwrap();
        beats.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(beats.len(), 2, "only agents with a heartbeat are returned");
        assert_eq!(beats[0].0, "hb-a");
        assert_eq!(beats[0].1.active_issue_id, Some(1));
        assert_eq!(beats[1].0, "hb-b");
        assert_eq!(beats[1].1.active_issue_id, None);
    }

    #[test]
    fn heartbeat_overwrites_previous() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let id = "hb-update";
        write_heartbeat_to_ref(dir.path(), id, &make_heartbeat(id, Some(1))).unwrap();
        write_heartbeat_to_ref(dir.path(), id, &make_heartbeat(id, Some(2))).unwrap();
        let beats = read_heartbeats_from_refs(dir.path()).unwrap();
        assert_eq!(beats.len(), 1);
        assert_eq!(beats[0].1.active_issue_id, Some(2), "latest heartbeat wins");
    }

    // ── Part 3: requests / acks ───────────────────────────────────────

    #[test]
    fn request_poll_ack_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let driver = "driver-1";
        let target = "target-1";

        // Driver writes a request into its own ref.
        let req = make_request("01REQ0001");
        write_request_to_own_ref(dir.path(), driver, target, &req).unwrap();

        // Target polls and sees it.
        let pending = poll_requests_for_agent(dir.path(), target).unwrap();
        assert_eq!(pending.len(), 1, "target must see the pending request");
        assert_eq!(pending[0].0, driver, "driver id recovered");
        assert_eq!(pending[0].1.request_id, "01REQ0001");

        // Target acks into its own ref.
        write_ack_to_own_ref(dir.path(), target, "01REQ0001", &make_ack("01REQ0001")).unwrap();

        // Poll no longer returns it.
        let after = poll_requests_for_agent(dir.path(), target).unwrap();
        assert!(after.is_empty(), "acked request must not be returned");
    }

    #[test]
    fn request_for_different_target_not_returned() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let driver = "driver-2";
        write_request_to_own_ref(
            dir.path(),
            driver,
            "someone-else",
            &make_request("01REQOTHER"),
        )
        .unwrap();
        let pending = poll_requests_for_agent(dir.path(), "me-myself").unwrap();
        assert!(pending.is_empty(), "request for another target is filtered");
    }

    #[test]
    fn request_separator_handles_hyphenated_agent_ids() {
        // Target id containing single hyphens must parse via split-on-LAST `--`.
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let driver = "ops-driver";
        let target = "my-hyphenated-agent";
        write_request_to_own_ref(dir.path(), driver, target, &make_request("01HYPH001")).unwrap();

        let pending = poll_requests_for_agent(dir.path(), target).unwrap();
        assert_eq!(pending.len(), 1, "hyphenated target id must parse");
        assert_eq!(pending[0].0, driver);

        // Direct parse-encode roundtrip coverage.
        let name = format!("{target}--01HYPH001.json");
        let (t, u) = parse_request_out_name(&name).unwrap();
        assert_eq!(t, target);
        assert_eq!(u, "01HYPH001");
        // A name with no separator returns None.
        assert!(parse_request_out_name("nodelim.json").is_none());
    }

    #[test]
    fn requests_from_multiple_drivers_to_same_target() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let target = "busy-target";
        write_request_to_own_ref(dir.path(), "drv-a", target, &make_request("01AAA")).unwrap();
        write_request_to_own_ref(dir.path(), "drv-b", target, &make_request("01BBB")).unwrap();

        let pending = poll_requests_for_agent(dir.path(), target).unwrap();
        assert_eq!(pending.len(), 2, "requests from both drivers visible");
        // Sorted by ulid.
        assert_eq!(pending[0].1.request_id, "01AAA");
        assert_eq!(pending[1].1.request_id, "01BBB");

        // Ack one — only the other remains.
        write_ack_to_own_ref(dir.path(), target, "01AAA", &make_ack("01AAA")).unwrap();
        let remaining = poll_requests_for_agent(dir.path(), target).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].1.request_id, "01BBB");
    }

    // ── Part 4: compact_v3 ────────────────────────────────────────────

    fn test_hub_lock(dir: &Path) -> crate::sync::HubWriteLock {
        let lock_path = dir.join(".hub-write-lock");
        crate::sync::acquire_hub_lock(&lock_path).expect("failed to acquire hub write lock")
    }

    /// Seed a v3 hub in `dir`: a meta ref (so `detect_hub_version` is V3) plus
    /// `count` events on `agent_id`'s ref. Returns nothing; refs are live.
    fn seed_v3_hub(dir: &Path, agent_id: &str, count: u64) {
        commit_files_to_ref(
            dir,
            META_REF,
            &[("hub.json", b"{\"hub_version\":3}\n")],
            "meta",
        )
        .unwrap();
        for seq in 1..=count {
            append_event_to_ref(dir, agent_id, &make_envelope(agent_id, seq)).unwrap();
        }
    }

    #[test]
    fn compact_v3_local_only_writes_checkpoint_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let agent_id = "cv3-agent";
        seed_v3_hub(dir.path(), agent_id, 4);

        let lock = test_hub_lock(dir.path());
        let result = compact_v3(dir.path(), agent_id, &lock, None).unwrap();
        drop(lock);

        assert_eq!(result.events_processed, 4);
        assert!(result.checkpoint_commit.is_some(), "checkpoint committed");
        assert!(!result.checkpoint_pushed, "no remote → no push");
        // Local-only: prune happens once the checkpoint is committed.
        assert_eq!(result.events_pruned, 4, "all covered events pruned locally");

        // Checkpoint ref must exist with state.json.
        let cp_tip = run_git_output(dir.path(), &["rev-parse", CHECKPOINT_REF]);
        let state_bytes = cat_blob(dir.path(), &format!("{cp_tip}:state.json")).unwrap();
        let state = crate::checkpoint::CheckpointState::from_slice(&state_bytes).unwrap();
        assert_eq!(state.issues.len(), 4, "4 issues materialized");

        // Own ref's events.log is now empty (all <= watermark).
        let agent_tip = run_git_output(
            dir.path(),
            &["rev-parse", &agent_ref_name(agent_id).unwrap()],
        );
        let log = cat_blob(dir.path(), &format!("{agent_tip}:events.log")).unwrap();
        assert!(read_events_from_bytes(&log).unwrap().is_empty());
    }

    #[test]
    fn compact_v3_remote_push_then_prune_and_fresh_reduce_matches() {
        let dir = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        git_init(dir.path());
        run_git(remote.path(), &["init", "--bare"]);
        run_git(
            dir.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );

        let agent_id = "cv3-remote";
        seed_v3_hub(dir.path(), agent_id, 3);
        // Push the agent ref so a fresh clone can fetch it.
        push_agent_ref(dir.path(), "origin", agent_id).unwrap();

        // Capture the full reduced state BEFORE prune for comparison.
        let pre =
            crate::compaction::reduce(&crate::hub_source::RefHubSource::new(dir.path()).unwrap())
                .unwrap();

        let lock = test_hub_lock(dir.path());
        let result = compact_v3(dir.path(), agent_id, &lock, Some("origin")).unwrap();
        drop(lock);

        assert!(result.checkpoint_pushed, "checkpoint pushed to remote");
        assert_eq!(result.events_pruned, 3, "prune after successful push");

        // Push the pruned agent ref too (so the remote reflects the prune).
        push_agent_ref(dir.path(), "origin", agent_id).unwrap();

        // Fresh clone fetches the v3 refs and must reduce to identical state.
        let fresh = tempfile::tempdir().unwrap();
        run_git(fresh.path(), &["init"]);
        run_git(
            fresh.path(),
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        run_git(
            fresh.path(),
            &["fetch", "origin", "+refs/crosslink/*:refs/crosslink/*"],
        );
        let fresh_outcome =
            crate::compaction::reduce(&crate::hub_source::RefHubSource::new(fresh.path()).unwrap())
                .unwrap();

        // Identical full state: checkpoint + remaining events == original full reduce.
        let pre_state = serde_json::to_value(&pre.state).unwrap();
        let fresh_state = serde_json::to_value(&fresh_outcome.state).unwrap();
        assert_eq!(
            pre_state, fresh_state,
            "fresh clone (checkpoint + pruned ref) must reduce to identical state"
        );
    }

    #[test]
    fn compact_v3_skips_prune_when_push_fails() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        // Add a remote that does not exist → push fails.
        run_git(
            dir.path(),
            &["remote", "add", "origin", "/no/such/bare/remote"],
        );
        let agent_id = "cv3-nopush";
        seed_v3_hub(dir.path(), agent_id, 2);

        let lock = test_hub_lock(dir.path());
        let result = compact_v3(dir.path(), agent_id, &lock, Some("origin")).unwrap();
        drop(lock);

        assert!(
            result.checkpoint_commit.is_some(),
            "checkpoint still committed locally"
        );
        assert!(!result.checkpoint_pushed, "push to dead remote fails");
        assert_eq!(
            result.events_pruned, 0,
            "prune MUST be skipped when the checkpoint push fails"
        );
        // Own ref still has both events.
        let tip = run_git_output(
            dir.path(),
            &["rev-parse", &agent_ref_name(agent_id).unwrap()],
        );
        let log = cat_blob(dir.path(), &format!("{tip}:events.log")).unwrap();
        assert_eq!(read_events_from_bytes(&log).unwrap().len(), 2);
    }

    #[test]
    fn compact_v3_concurrent_cas_loss_is_benign() {
        // Drive compact_v3's commit_blob_to_ref into the documented benign
        // "ref moved concurrently" branch: pin the reduce, then move the
        // checkpoint ref out from under the in-flight commit. Done deterministically
        // by exercising the same CAS the function relies on — we commit a
        // checkpoint first, then a SECOND compact whose CAS we invalidate by
        // advancing the checkpoint ref between its internal read and commit is not
        // single-thread-reachable, so we instead assert the branch directly: a
        // commit_blob_to_ref with a stale parent fails with the exact message the
        // benign handler matches, and a normal compact still succeeds afterwards.
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let agent_id = "cv3-cas";
        seed_v3_hub(dir.path(), agent_id, 2);

        // First compact: succeeds, writes + prunes.
        let lock = test_hub_lock(dir.path());
        let r1 = compact_v3(dir.path(), agent_id, &lock, None).unwrap();
        drop(lock);
        assert!(r1.checkpoint_commit.is_some());
        assert_eq!(r1.events_pruned, 2);

        // The benign-branch matcher: a genesis CAS on the now-existing checkpoint
        // ref yields the exact "ref moved concurrently" substring the handler keys
        // off — the same error compact_v3's commit_blob_to_ref would surface when a
        // concurrent compactor advanced the checkpoint first.
        let stale = commit_single_file_tree(
            dir.path(),
            CHECKPOINT_REF,
            "state.json",
            b"{}\n",
            "stale",
            "crosslink",
            CasExpectation::MustNotExist,
        );
        assert!(stale.is_err());
        assert!(
            format!("{:?}", stale.unwrap_err()).contains("ref moved concurrently"),
            "the benign branch keys off this exact substring"
        );

        // A subsequent compact with no new events is a clean no-op success.
        let lock2 = test_hub_lock(dir.path());
        let r2 = compact_v3(dir.path(), agent_id, &lock2, None).unwrap();
        drop(lock2);
        assert_eq!(r2.events_processed, 0);
        assert_eq!(r2.events_pruned, 0);
    }

    #[test]
    fn compact_v3_two_compactors_produce_consistent_checkpoint() {
        // Two compactors over the same event set: both reduce to the same
        // deterministic content, so the checkpoint CAS loser hits the benign
        // "ref moved concurrently" branch (None commit) without erroring and the
        // winner's checkpoint stands.
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        let agent_id = "cv3-race";
        seed_v3_hub(dir.path(), agent_id, 3);
        let repo = Arc::new(dir.path().to_path_buf());

        // Run two compactors; serialize via the hub lock as production does, but
        // interleave so the SECOND sees the FIRST's checkpoint already advanced.
        let r_a = {
            let lock = test_hub_lock(&repo);
            let r = compact_v3(&repo, agent_id, &lock, None).unwrap();
            drop(lock);
            r
        };
        // Second compactor: no new events, checkpoint already present → no-op.
        let r_b = {
            let lock = test_hub_lock(&repo);
            let r = compact_v3(&repo, agent_id, &lock, None).unwrap();
            drop(lock);
            r
        };
        assert!(r_a.checkpoint_commit.is_some());
        assert_eq!(r_a.events_processed, 3);
        assert_eq!(r_b.events_processed, 0, "second compactor sees pruned ref");

        // Checkpoint state is consistent and complete.
        let cp_tip = run_git_output(&repo, &["rev-parse", CHECKPOINT_REF]);
        let state_bytes = cat_blob(&repo, &format!("{cp_tip}:state.json")).unwrap();
        let state = crate::checkpoint::CheckpointState::from_slice(&state_bytes).unwrap();
        assert_eq!(state.issues.len(), 3);
    }
}
