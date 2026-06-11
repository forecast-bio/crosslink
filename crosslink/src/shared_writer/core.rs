//! Core types and infrastructure for `SharedWriter`.
//!
//! Contains the `SharedWriter` struct, `new()`, the retry-loop
//! (`write_commit_push` / `emit_compact_push`), git helpers,
//! counter management, and issue file resolution.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::db::Database;
use crate::identity::AgentConfig;
use crate::issue_file::{
    read_counters, read_issue_file, read_milestone_file, write_counters, Counters, IssueFile,
    MilestoneEntry,
};
use crate::sync::SyncManager;

// Hub cache write lock is in sync/cache.rs — acquired via self.sync.acquire_lock()

/// Comment kind for intervention comments.
pub(super) const KIND_INTERVENTION: &str = "intervention";
/// SSH signing namespace for crosslink comments.
pub(super) const SIGNING_NAMESPACE: &str = "crosslink-comment";

/// Content to write in a single atomic commit-push operation.
pub(super) struct WriteSet {
    /// Files to write: (relative path in cache, serialized content).
    pub files: Vec<(String, Vec<u8>)>,
    /// Updated counters, if any.
    pub counters: Option<Counters>,
    /// If true, stage removals (`git rm`) instead of additions (`git add`).
    pub use_git_rm: bool,
}

/// Maximum number of push retries on conflict before giving up.
pub(super) const MAX_RETRIES: usize = 3;

/// Maximum time to wait for lock confirmation compaction (design doc section 8).
pub(super) const LOCK_CONFIRM_TIMEOUT_SECS: u64 = 30;

/// Outcome of a `write_commit_push` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Commit was pushed to remote successfully.
    Pushed,
    /// Commit was saved locally but push failed (offline or all retries exhausted).
    LocalOnly,
}

/// Write-side coordinator for multi-agent shared issue tracking.
///
/// Handles: generate UUID -> claim display ID -> write JSON -> commit ->
/// push (with rebase-retry) -> update local `SQLite`.
pub struct SharedWriter {
    pub(super) sync: SyncManager,
    pub(super) agent: AgentConfig,
    pub(super) cache_dir: PathBuf,
    /// Per-session event sequence counter, monotonically increasing.
    pub(super) event_seq: Cell<u64>,
    /// Whether hub v3 dual-write shadow mode is enabled for this session.
    ///
    /// Read once from `.crosslink/hook-config.json` in the constructor so
    /// the per-event path does no file I/O for the flag check.
    pub(super) hub_v3_dual_write: bool,
}

impl SharedWriter {
    /// Create a `SharedWriter` if multi-agent mode is configured.
    ///
    /// When `agent.json` exists, uses the configured identity with signing.
    /// When no `agent.json` exists but the hub branch is available, creates
    /// an anonymous writer that commits unsigned data to the coordination
    /// branch. Returns `None` only if the hub branch cannot be initialized.
    ///
    /// # Errors
    ///
    /// Returns an error if the sync cache is not initialized or agent loading fails.
    pub fn new(crosslink_dir: &Path) -> Result<Option<Self>> {
        let agent = if let Some(a) = AgentConfig::load(crosslink_dir)? {
            a
        } else {
            // No agent configured -- try anonymous hub writes if hub exists
            let sync = SyncManager::new(crosslink_dir)?;
            if !sync.is_initialized() {
                // Only auto-initialize hub cache if the remote actually
                // exists. Without a remote there is nothing to sync with,
                // so fall back to direct SQLite writes.
                if !sync.remote_exists() {
                    return Ok(None);
                }
                if sync.init_cache().is_err() {
                    return Ok(None);
                }
                if !sync.is_initialized() {
                    return Ok(None);
                }
            }
            AgentConfig::anonymous(crosslink_dir)
        };
        let sync = SyncManager::new(crosslink_dir)?;
        if !sync.is_initialized() {
            // If there's no remote, hub sync is impossible — fall back to
            // direct SQLite writes. This covers local-only repos and test
            // environments where no remote is configured.
            if !sync.remote_exists() {
                return Ok(None);
            }
            bail!("Sync cache not initialized. Run `crosslink sync` first.");
        }
        let cache_dir = sync.cache_path().to_path_buf();

        // Ensure directory structure exists
        std::fs::create_dir_all(cache_dir.join("issues"))?;
        std::fs::create_dir_all(cache_dir.join("meta").join("milestones"))?;

        // Initialize event sequence counter from existing log
        let event_seq = Cell::new(Self::read_max_event_seq(&cache_dir, &agent.agent_id));

        // Read the dual-write flag once at construction time; per-event path does
        // no file I/O for the flag check.
        let hub_v3_dual_write = crate::hub_v3::dual_write_enabled(crosslink_dir);

        Ok(Some(Self {
            sync,
            agent,
            cache_dir,
            event_seq,
            hub_v3_dual_write,
        }))
    }

    pub fn agent_id(&self) -> &str {
        &self.agent.agent_id
    }

    /// Derive the `.crosslink/` directory from the cache path.
    pub(super) fn crosslink_dir(&self) -> &Path {
        self.cache_dir.parent().unwrap_or_else(|| {
            tracing::warn!("cache_dir has no parent, falling back to cache_dir itself");
            &self.cache_dir
        })
    }

    /// Hydrate hub cache into `SQLite` with a single retry on failure.
    ///
    /// If the first attempt fails, prints a warning and retries once.
    /// If the retry also fails, warns the user to run `crosslink sync`
    /// so the caller can continue gracefully.
    pub fn hydrate_with_retry(&self, db: &Database) {
        match crate::hydration::hydrate_to_sqlite(&self.cache_dir, db) {
            Ok(_) => {}
            Err(first_err) => {
                tracing::warn!(
                    "Warning: hydration failed ({}), retrying once...",
                    first_err
                );
                if let Err(retry_err) = crate::hydration::hydrate_to_sqlite(&self.cache_dir, db) {
                    tracing::warn!(
                        "Warning: hydration retry failed ({}). Run `crosslink sync` to recover.",
                        retry_err
                    );
                }
            }
        }
    }

    /// Path to the promoted-UUIDs tracking file (machine-local, not shared).
    pub(super) fn promoted_uuids_path(&self) -> PathBuf {
        self.crosslink_dir().join(".promoted-uuids")
    }

    /// Read the set of UUIDs that have already been promoted.
    pub(super) fn read_promoted_uuids(&self) -> HashSet<Uuid> {
        let path = self.promoted_uuids_path();
        std::fs::read_to_string(&path).map_or_else(
            |_| HashSet::new(),
            |content| {
                content
                    .lines()
                    .filter_map(|line| line.trim().parse::<Uuid>().ok())
                    .collect()
            },
        )
    }

    /// Append promoted UUIDs to the tracking file.
    pub(super) fn record_promoted_uuids(&self, uuids: &[Uuid]) -> Result<()> {
        use std::io::Write;
        let path = self.promoted_uuids_path();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Failed to open promoted UUIDs file: {}", path.display()))?;
        for uuid in uuids {
            writeln!(file, "{uuid}")?;
        }
        Ok(())
    }

    /// Check the current hub layout version.
    pub(super) fn layout_version(&self) -> u32 {
        let meta_dir = self.sync.cache_path().join("meta");
        crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1)
    }

    // ---- Event emission infrastructure ----

    /// Read the max `agent_seq` from an existing event log.
    pub(super) fn read_max_event_seq(cache_dir: &Path, agent_id: &str) -> u64 {
        let log_path = cache_dir.join("agents").join(agent_id).join("events.log");
        crate::events::read_events(&log_path).map_or(0, |events| {
            events.iter().map(|e| e.agent_seq).max().unwrap_or(0)
        })
    }

    /// Get the next event sequence number and increment the counter.
    pub(super) fn next_event_seq(&self) -> u64 {
        let seq = self.event_seq.get() + 1;
        self.event_seq.set(seq);
        seq
    }

    /// Path to this agent's event log file.
    pub(super) fn event_log_path(&self) -> PathBuf {
        self.cache_dir
            .join("agents")
            .join(&self.agent.agent_id)
            .join("events.log")
    }

    /// Resolve the agent's SSH private key to an absolute path, if configured.
    pub(super) fn resolve_ssh_key_path(&self) -> Option<PathBuf> {
        let rel = self.agent.ssh_key_path.as_ref()?;
        let crosslink_dir = self
            .sync
            .cache_path()
            .parent()
            .unwrap_or_else(|| self.sync.cache_path());
        let abs = crosslink_dir.join(rel);
        if abs.exists() {
            Some(abs)
        } else {
            None
        }
    }

    /// Create and optionally sign an event envelope.
    pub(super) fn create_envelope(
        &self,
        event: crate::events::Event,
    ) -> crate::events::EventEnvelope {
        let seq = self.next_event_seq();
        let mut envelope = crate::events::EventEnvelope {
            agent_id: self.agent.agent_id.clone(),
            agent_seq: seq,
            timestamp: Utc::now(),
            event,
            signed_by: None,
            signature: None,
        };

        // Sign if key is configured. If signing is configured but fails,
        // log the failure — unsigned events are still valid, but a signing
        // failure is distinguishable from "not configured" (#477).
        if let (Some(key_path), Some(fingerprint)) = (
            self.resolve_ssh_key_path(),
            self.agent.ssh_fingerprint.as_ref(),
        ) {
            if let Err(e) = crate::events::sign_event(&mut envelope, &key_path, fingerprint) {
                tracing::warn!(
                    "event signing failed (key: {}, fingerprint: {}): {}",
                    key_path.display(),
                    fingerprint,
                    e
                );
            }
        }

        envelope
    }

    /// Emit an event, run compaction, and push all changes.
    ///
    /// The event is appended once to the log before the retry loop.
    /// On push conflict, compaction is re-run after rebase to incorporate
    /// any new remote events.
    pub(super) fn emit_compact_push(
        &self,
        event: crate::events::Event,
        message: &str,
    ) -> Result<PushOutcome> {
        // Serialize access to the hub cache via SyncManager's lock (#372)
        let lock_guard = self.sync.acquire_lock()?;

        let envelope = self.create_envelope(event);
        let log_path = self.event_log_path();
        crate::events::append_event(&log_path, &envelope)?;

        // Hub v3 shadow mirror: append the same envelope to the per-agent ref.
        //
        // Failure policy: best-effort. Any error is logged at WARN and the stats
        // counter incremented, but the caller's operation continues unaffected.
        //
        // Repo dir: `self.cache_dir` is a linked git worktree (`git worktree add`
        // in sync/cache.rs:init_cache). Linked worktrees share the main repository's
        // object store and ref namespace, so plumbing commands run with cache_dir as
        // cwd update refs visible repo-globally. Confirmed in sync/cache.rs at the
        // `git worktree add` call sites.
        //
        // Lock: the hub write lock acquired at the top of this function is still
        // held here, so the stats read-modify-write is safe without additional
        // synchronisation.
        if self.hub_v3_dual_write {
            let stats_path = self.crosslink_dir().join("hub-v3-shadow-stats.json");
            let mut stats = crate::hub_v3::ShadowStats::read(&stats_path);

            match crate::hub_v3::append_event_to_ref(
                &self.cache_dir,
                &self.agent.agent_id,
                &envelope,
            ) {
                Ok(_) => {
                    stats.mirrored += 1;
                }
                Err(e) => {
                    let msg = format!("hub v3 shadow mirror failed: {e}");
                    tracing::warn!("{}", msg);
                    stats.mirror_failures += 1;
                    stats.last_failure = Some(msg);
                    stats.last_failure_at = Some(chrono::Utc::now().to_rfc3339());
                }
            }

            if let Err(e) = stats.write(&stats_path) {
                tracing::warn!("hub v3 shadow: failed to persist stats: {e}");
            }
        }

        for attempt in 0..MAX_RETRIES {
            // Run compaction (force=true since we own the write path).
            // Pass &lock_guard as proof we hold the hub write lock (#750).
            let _ = crate::compaction::compact(
                &self.cache_dir,
                &self.agent.agent_id,
                true,
                &lock_guard,
            )?;

            // Stage event log + compaction output
            let rel_log = format!("agents/{}/events.log", self.agent.agent_id);
            self.git_in_cache(&["add", &rel_log])?;
            // Stage compaction output directories that exist (#472)
            for dir in ["checkpoint/", "issues/", "locks/"] {
                if self.cache_dir.join(dir).exists() {
                    self.git_in_cache(&["add", dir])?;
                }
            }

            // Commit (unsigned when no SSH key)
            let commit_msg = format!(
                "{}: {} at {}",
                self.agent.agent_id,
                message,
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
            );
            let commit_result = self.git_commit_in_cache(&commit_msg);
            if let Err(ref e) = commit_result {
                let err_str = e.to_string();
                if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                    return Ok(PushOutcome::Pushed);
                }
            }
            commit_result?;

            // Push
            let remote = self.sync.remote();
            let push_result = self.git_in_cache(&["push", remote, crate::sync::HUB_BRANCH]);
            match push_result {
                Ok(_) => {
                    // Hub v3 shadow push: mirror the per-agent ref to the remote.
                    //
                    // Best-effort: any non-Pushed outcome or error is recorded in
                    // the stats file but does not affect the return value.
                    // NonFastForward on our own ref is impossible under correct
                    // operation — if it occurs it indicates identity collision or
                    // ref tampering (design REQ-1), so it is logged at WARN with
                    // an explicit diagnostic.
                    if self.hub_v3_dual_write {
                        let stats_path = self.crosslink_dir().join("hub-v3-shadow-stats.json");
                        let mut stats = crate::hub_v3::ShadowStats::read(&stats_path);

                        match crate::hub_v3::push_agent_ref(
                            &self.cache_dir,
                            remote,
                            &self.agent.agent_id,
                        ) {
                            Ok(crate::hub_v3::PushOutcome::Pushed) => {
                                stats.pushed += 1;
                            }
                            Ok(crate::hub_v3::PushOutcome::NonFastForward) => {
                                let msg = format!(
                                    "hub v3 shadow push for agent '{}' was rejected as \
                                     non-fast-forward — this indicates identity collision or \
                                     ref tampering (REQ-1); per-agent ref has diverged",
                                    self.agent.agent_id
                                );
                                tracing::warn!("{}", msg);
                                stats.push_failures += 1;
                                stats.last_failure = Some(msg);
                                stats.last_failure_at = Some(chrono::Utc::now().to_rfc3339());
                            }
                            Ok(crate::hub_v3::PushOutcome::NoRemote) => {
                                let msg = format!(
                                    "hub v3 shadow push for agent '{}': remote '{}' not found",
                                    self.agent.agent_id, remote
                                );
                                tracing::warn!("{}", msg);
                                stats.push_failures += 1;
                                stats.last_failure = Some(msg);
                                stats.last_failure_at = Some(chrono::Utc::now().to_rfc3339());
                            }
                            Ok(crate::hub_v3::PushOutcome::Failed(detail)) => {
                                let msg = format!(
                                    "hub v3 shadow push for agent '{}' failed: {}",
                                    self.agent.agent_id, detail
                                );
                                tracing::warn!("{}", msg);
                                stats.push_failures += 1;
                                stats.last_failure = Some(msg);
                                stats.last_failure_at = Some(chrono::Utc::now().to_rfc3339());
                            }
                            Err(e) => {
                                let msg = format!(
                                    "hub v3 shadow push for agent '{}' error: {}",
                                    self.agent.agent_id, e
                                );
                                tracing::warn!("{}", msg);
                                stats.push_failures += 1;
                                stats.last_failure = Some(msg);
                                stats.last_failure_at = Some(chrono::Utc::now().to_rfc3339());
                            }
                        }

                        if let Err(e) = stats.write(&stats_path) {
                            tracing::warn!("hub v3 shadow: failed to persist push stats: {e}");
                        }
                    }
                    return Ok(PushOutcome::Pushed);
                }
                Err(e) => {
                    let err_str = e.to_string();
                    match crate::sync::classify_push_failure(&err_str, remote) {
                        crate::sync::PushFailure::Offline => {
                            tracing::warn!("push failed (offline), {message} saved locally only");
                            return Ok(PushOutcome::LocalOnly);
                        }
                        // Misconfigured / auth / unknown: surface loudly so
                        // the user sees a real config bug rather than a
                        // generic (offline) warning. Local cache state is
                        // consistent so we still return LocalOnly. GH#586.
                        f @ (crate::sync::PushFailure::RemoteMisconfigured { .. }
                        | crate::sync::PushFailure::AuthFailed
                        | crate::sync::PushFailure::Other(_)) => {
                            tracing::error!("{}", f.user_message(message));
                            return Ok(PushOutcome::LocalOnly);
                        }
                        crate::sync::PushFailure::NonFastForward => {
                            if attempt < MAX_RETRIES - 1 {
                                self.check_divergence()?;
                                self.recover_from_push_conflict(remote)?;
                                continue;
                            }
                            tracing::warn!(
                                "push failed after {} retries (conflict), {message} saved locally only",
                                MAX_RETRIES
                            );
                            return Ok(PushOutcome::LocalOnly);
                        }
                    }
                }
            }
        }
        Ok(PushOutcome::Pushed)
    }

    /// Write an agent control request to the hub branch.
    ///
    /// Drops a JSON file at `agents/<target_agent_id>/requests/<request_id>.json`
    /// on `crosslink/hub`, commits it (signed by the driver's key if
    /// available), and pushes. The filename is lex-sortable so the
    /// target agent's poll loop processes requests in arrival order.
    ///
    /// Conflict recovery mirrors [`emit_compact_push`]: on push rejection
    /// we rebase onto remote hub and retry, falling back to `LocalOnly`
    /// after [`MAX_RETRIES`] so the driver is never blocked by a noisy
    /// hub.
    ///
    /// # Errors
    /// Returns an error if the cache can't be prepared, the write/commit
    /// fails for a reason other than push conflict, or the request's
    /// JSON encoding fails.
    pub fn write_agent_request(
        &self,
        target_agent_id: &str,
        request: &crate::agent_requests::AgentRequest,
    ) -> Result<PushOutcome> {
        // Serialize access to the hub cache (#372).
        let _lock_guard = self.sync.acquire_lock()?;

        let rel_path = crate::agent_requests::request_path(target_agent_id, &request.request_id);
        let abs_path = self.cache_dir.join(&rel_path);
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create agent request dir {}", parent.display()))?;
        }

        let body = serde_json::to_vec_pretty(request).context("serialize agent request")?;
        std::fs::write(&abs_path, &body)
            .with_context(|| format!("write agent request {}", abs_path.display()))?;

        let rel_str = rel_path.to_string_lossy().into_owned();

        let commit_msg = format!(
            "{}: agent request {} ({:?}) for {} at {}",
            self.agent.agent_id,
            request.request_id,
            request.kind,
            target_agent_id,
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );

        for attempt in 0..MAX_RETRIES {
            self.git_in_cache(&["add", &rel_str])?;
            let commit_result = self.git_commit_in_cache(&commit_msg);
            if let Err(ref e) = commit_result {
                let err_str = e.to_string();
                if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                    return Ok(PushOutcome::Pushed);
                }
            }
            commit_result?;

            let remote = self.sync.remote();
            let push_result = self.git_in_cache(&["push", remote, crate::sync::HUB_BRANCH]);
            match push_result {
                Ok(_) => return Ok(PushOutcome::Pushed),
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        tracing::warn!(
                            "Warning: push failed (offline), agent request saved locally: {}",
                            request.request_id
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < MAX_RETRIES - 1 {
                            self.check_divergence()?;
                            self.recover_from_push_conflict(remote)?;
                            // Request file survives the rebase — re-add + retry.
                            continue;
                        }
                        tracing::warn!(
                            "Warning: push failed after {} retries (conflict), agent request saved locally: {}",
                            MAX_RETRIES, request.request_id
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    return Err(e);
                }
            }
        }
        Ok(PushOutcome::Pushed)
    }

    /// Write an ack for a previously-received agent request.
    ///
    /// Drops a JSON file at `agents/<target_agent_id>/requests/<request_id>.ack.json`
    /// on `crosslink/hub`, committed + pushed under the current agent's
    /// identity. Drivers (dashboard) diff `requests/*.json` vs
    /// `requests/*.ack.json` to render request state.
    ///
    /// Follows the same rebase-retry / offline-fallback pattern as
    /// [`Self::write_agent_request`] so an offline agent still writes
    /// the ack locally for the next successful sync.
    ///
    /// # Errors
    /// Returns an error if the cache can't be prepared, the write/commit
    /// fails for a reason other than push conflict, or the ack's JSON
    /// encoding fails.
    pub fn write_agent_ack(
        &self,
        target_agent_id: &str,
        ack: &crate::agent_requests::AgentRequestAck,
    ) -> Result<PushOutcome> {
        let _lock_guard = self.sync.acquire_lock()?;

        let rel_path = crate::agent_requests::requests_dir(target_agent_id)
            .join(format!("{}.ack.json", ack.request_id));
        let abs_path = self.cache_dir.join(&rel_path);
        if let Some(parent) = abs_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create agent request dir {}", parent.display()))?;
        }

        let body = serde_json::to_vec_pretty(ack).context("serialize agent request ack")?;
        std::fs::write(&abs_path, &body)
            .with_context(|| format!("write agent request ack {}", abs_path.display()))?;

        let rel_str = rel_path.to_string_lossy().into_owned();
        let commit_msg = format!(
            "{}: ack agent request {} ({}) for {} at {}",
            self.agent.agent_id,
            ack.request_id,
            if ack.acted { "acted" } else { "rejected" },
            target_agent_id,
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        );

        for attempt in 0..MAX_RETRIES {
            self.git_in_cache(&["add", &rel_str])?;
            let commit_result = self.git_commit_in_cache(&commit_msg);
            if let Err(ref e) = commit_result {
                let err_str = e.to_string();
                if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                    return Ok(PushOutcome::Pushed);
                }
            }
            commit_result?;

            let remote = self.sync.remote();
            let push_result = self.git_in_cache(&["push", remote, crate::sync::HUB_BRANCH]);
            match push_result {
                Ok(_) => return Ok(PushOutcome::Pushed),
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        tracing::warn!(
                            "Warning: ack push failed (offline), saved locally: {}",
                            ack.request_id
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < MAX_RETRIES - 1 {
                            self.check_divergence()?;
                            self.recover_from_push_conflict(remote)?;
                            continue;
                        }
                        tracing::warn!(
                            "Warning: ack push failed after {} retries (conflict), saved locally: {}",
                            MAX_RETRIES, ack.request_id
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    return Err(e);
                }
            }
        }
        Ok(PushOutcome::Pushed)
    }

    // ---- Private helpers ----

    /// Sign a comment's canonical content if the agent has an SSH key.
    ///
    /// Returns `(signed_by, signature)` -- both `None` if no key is available.
    pub(super) fn sign_comment(
        &self,
        content: &str,
        author: &str,
        comment_id: i64,
    ) -> (Option<String>, Option<String>) {
        let (key_path, fingerprint) = match (&self.agent.ssh_key_path, &self.agent.ssh_fingerprint)
        {
            (Some(rel), Some(fp)) => {
                // ssh_key_path is relative to .crosslink/; resolve via sync's cache
                let crosslink_dir = self
                    .sync
                    .cache_path()
                    .parent()
                    .unwrap_or_else(|| self.sync.cache_path());
                let abs = crosslink_dir.join(rel);
                (abs, fp.clone())
            }
            _ => return (None, None),
        };

        if !key_path.exists() {
            return (None, None);
        }

        let canonical = crate::signing::canonicalize_for_signing(&[
            ("author", author),
            ("comment_id", &comment_id.to_string()),
            ("content", content),
        ]);

        crate::signing::sign_content(&key_path, &canonical, SIGNING_NAMESPACE)
            .map_or((None, None), |sig| (Some(fingerprint), Some(sig)))
    }

    /// Scan all issue files from the cache, applying a filter predicate.
    ///
    /// Supports both V1 (`issues/{uuid}.json`) and V2 (`issues/{uuid}/issue.json`)
    /// layouts. Shared implementation used by `find_offline_issues` and
    /// `load_issue_by_display_id`.
    pub(super) fn scan_issues<F>(&self, mut filter: F) -> Result<Vec<IssueFile>>
    where
        F: FnMut(&IssueFile) -> bool,
    {
        let issues_dir = self.cache_dir.join("issues");
        let mut results = Vec::new();
        if !issues_dir.exists() {
            return Ok(results);
        }
        for entry in std::fs::read_dir(&issues_dir)? {
            let entry = entry?;
            let path = entry.path();
            // V1: issues/{uuid}.json (flat file)
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(issue) = read_issue_file(&path) {
                    if filter(&issue) {
                        results.push(issue);
                    }
                }
            }
            // V2: issues/{uuid}/issue.json (directory per issue)
            if path.is_dir() {
                let issue_file = path.join("issue.json");
                if issue_file.exists() {
                    if let Ok(issue) = read_issue_file(&issue_file) {
                        if filter(&issue) {
                            results.push(issue);
                        }
                    }
                }
            }
        }
        Ok(results)
    }

    /// Find all issue files in the cache with `display_id: null` created by this agent.
    ///
    /// Supports both v1 (`issues/{uuid}.json`) and v2 (`issues/{uuid}/issue.json`) layouts.
    /// Skips issues whose UUIDs appear in the promoted-UUIDs tracking file to
    /// prevent re-promotion loops (gh#313).
    pub(super) fn find_offline_issues(&self) -> Result<Vec<IssueFile>> {
        // Load the set of already-promoted UUIDs so we never re-promote them.
        let promoted = self.read_promoted_uuids();
        let agent_id = self.agent.agent_id.clone();

        let mut offline = self.scan_issues(|issue| {
            issue.display_id.is_none()
                && issue.created_by == agent_id
                && !promoted.contains(&issue.uuid)
        })?;
        // Sort by created_at for deterministic ID assignment
        offline.sort_by_key(|i| i.created_at);
        Ok(offline)
    }

    /// Claim N sequential display IDs from `meta/counters.json`.
    ///
    /// Returns `(first_claimed_id, updated_counters)`.
    ///
    /// Before claiming, the counter is reconciled against the highest
    /// `display_id` actually present in the hub-cache issue files. This
    /// prevents a class of collision bugs where `counters.json` falls out
    /// of sync with the real state — e.g. a freshly-cloned repo whose
    /// `counters.json` defaults to 1 but whose `issues/` directory
    /// already contains closed issues with larger IDs; or a local cache
    /// whose counter was decremented by a previous offline rollback
    /// without observing that other agents had meanwhile pushed issues
    /// with higher IDs. See `reconcile_display_counter`.
    pub(super) fn claim_display_id(&self, count: i64) -> Result<(i64, Counters)> {
        let mut counters = self.read_counters()?;
        self.reconcile_display_counter(&mut counters)?;
        let first = counters.next_display_id;
        counters.next_display_id += count;
        Ok((first, counters))
    }

    /// Claim a milestone display ID from `meta/counters.json`.
    ///
    /// Returns `(claimed_id, updated_counters)`.
    ///
    /// As with `claim_display_id`, the counter is reconciled against the
    /// highest `display_id` present in the on-disk milestone files
    /// before assignment so that stale `counters.json` does not produce
    /// colliding IDs.
    pub(super) fn claim_milestone_id(&self) -> Result<(i64, Counters)> {
        let mut counters = self.read_counters()?;
        self.reconcile_milestone_counter(&mut counters)?;
        let id = counters.next_milestone_id;
        counters.next_milestone_id += 1;
        Ok((id, counters))
    }

    /// Reconcile `counters.next_display_id` with the actual maximum
    /// `display_id` present in the hub-cache issue files (open OR
    /// closed). If the counter is behind, advance it past the max so
    /// the next claim cannot collide with an existing file.
    ///
    /// Typical scenarios where the counter falls behind:
    /// - Fresh clone: `meta/counters.json` does not yet exist so
    ///   `read_counters` returns `Counters::default()` with
    ///   `next_display_id = 1`, but the hub branch already has issues
    ///   with much higher IDs.
    /// - Offline rollback: `rewrite_as_offline` decrements the counter
    ///   on local push failure. If another agent meanwhile pushed a
    ///   new issue that reuses the same slot, the next online sync
    ///   pulls it in but the local counter still points at the freed
    ///   slot.
    /// - Mixed hub/local state: merges and fast-forwards can leave
    ///   `counters.json` lagging behind the issues directory.
    ///
    /// This is O(N) over issues-in-cache. That cost is paid only when
    /// minting a new `display_id` (typically a handful of times per
    /// command), not on every read.
    pub(super) fn reconcile_display_counter(&self, counters: &mut Counters) -> Result<()> {
        let max_existing = self
            .scan_issues(|_| true)?
            .iter()
            .filter_map(|i| i.display_id)
            .max()
            .unwrap_or(0);
        if counters.next_display_id <= max_existing {
            counters.next_display_id = max_existing + 1;
        }
        Ok(())
    }

    /// Reconcile `counters.next_milestone_id` with the actual maximum
    /// `display_id` present in the on-disk milestone files. See
    /// [`reconcile_display_counter`] for the full rationale; the same
    /// failure modes apply to milestones.
    pub(super) fn reconcile_milestone_counter(&self, counters: &mut Counters) -> Result<()> {
        let milestones_dir = self.cache_dir.join("meta").join("milestones");
        let mut max_existing = 0i64;
        if milestones_dir.exists() {
            for entry in std::fs::read_dir(&milestones_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(ms) = read_milestone_file(&path) {
                    if ms.display_id > max_existing {
                        max_existing = ms.display_id;
                    }
                }
            }
        }
        if counters.next_milestone_id <= max_existing {
            counters.next_milestone_id = max_existing + 1;
        }
        Ok(())
    }

    /// Load a milestone entry by `display_id` from per-file storage.
    pub(super) fn load_milestone_by_id(&self, display_id: i64) -> Result<MilestoneEntry> {
        let milestones_dir = self.cache_dir.join("meta").join("milestones");
        if milestones_dir.exists() {
            for entry in std::fs::read_dir(&milestones_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(ms) = read_milestone_file(&path) {
                    if ms.display_id == display_id {
                        return Ok(ms);
                    }
                }
            }
        }
        bail!("Milestone #{display_id} not found in shared cache")
    }

    /// Read counters from the cache.
    pub(super) fn read_counters(&self) -> Result<Counters> {
        let path = self.cache_dir.join("meta").join("counters.json");
        read_counters(&path)
    }

    /// Write counters to the cache.
    pub(super) fn write_counters_to_cache(&self, counters: &Counters) -> Result<()> {
        let path = self.cache_dir.join("meta").join("counters.json");
        write_counters(&path, counters)
    }

    /// Path to an issue JSON file in the cache.
    ///
    /// V1: `issues/{uuid}.json`
    /// V2: `issues/{uuid}/issue.json`
    pub(super) fn issue_path(&self, uuid: &Uuid) -> PathBuf {
        if self.layout_version() >= 2 {
            self.cache_dir
                .join("issues")
                .join(uuid.to_string())
                .join("issue.json")
        } else {
            self.cache_dir.join("issues").join(format!("{uuid}.json"))
        }
    }

    /// Relative path to an issue JSON file (for `WriteSet` entries and git staging).
    ///
    /// V1: `issues/{uuid}.json`
    /// V2: `issues/{uuid}/issue.json`
    pub(super) fn issue_rel_path(&self, uuid: &Uuid) -> String {
        if self.layout_version() >= 2 {
            format!("issues/{uuid}/issue.json")
        } else {
            format!("issues/{uuid}.json")
        }
    }

    /// Relative path to a comment JSON file (V2 layout only).
    ///
    /// `issues/{issue_uuid}/comments/{comment_uuid}.json`
    pub(super) fn comment_rel_path(issue_uuid: &Uuid, comment_uuid: &Uuid) -> String {
        format!("issues/{issue_uuid}/comments/{comment_uuid}.json")
    }

    /// Load an issue JSON file by its display ID.
    ///
    /// Scans the issues directory for a file matching the display ID.
    /// Supports both v1 (`issues/{uuid}.json`) and v2 (`issues/{uuid}/issue.json`) layouts.
    pub(super) fn load_issue_by_display_id(&self, display_id: i64) -> Result<IssueFile> {
        let mut matches = self.scan_issues(|issue| issue.display_id == Some(display_id))?;
        matches.pop().ok_or_else(|| {
            anyhow::anyhow!(
                "Issue {} not found in shared cache",
                crate::utils::format_issue_id(display_id)
            )
        })
    }

    /// Load an issue by ID, supporting both positive (real) and negative (offline) IDs.
    ///
    /// For negative IDs, consults `SQLite` to resolve the UUID first.
    pub(super) fn load_issue_by_id(&self, id: i64, db: &Database) -> Result<IssueFile> {
        let resolved = db.resolve_id(id);
        if resolved >= 0 {
            self.load_issue_by_display_id(resolved)
        } else {
            let uuid_str = db.get_issue_uuid_by_id(resolved)?;
            let uuid: Uuid = uuid_str.parse().with_context(|| {
                format!("Invalid UUID for local issue L{}", resolved.unsigned_abs())
            })?;
            read_issue_file(&self.issue_path(&uuid))
        }
    }

    /// Resolve an issue ID (positive or negative) to its UUID.
    ///
    /// For positive IDs, scans issue files by `display_id` first, then falls
    /// back to `SQLite` if the JSON cache doesn't have the issue (#427).
    /// For negative IDs, looks up the UUID from `SQLite`.
    pub(super) fn resolve_uuid(&self, id: i64, db: &Database) -> Result<Uuid> {
        // Resolve positive IDs to their local equivalent if needed.
        // Users type "1" meaning "the first issue" regardless of format.
        let resolved = db.resolve_id(id);

        if resolved >= 0 {
            if let Ok(issue) = self.load_issue_by_display_id(resolved) {
                Ok(issue.uuid)
            } else {
                // JSON cache miss — fall back to SQLite (#427)
                let uuid_str = db.get_issue_uuid_by_id(resolved)?;
                uuid_str.parse().with_context(|| {
                    format!("Invalid UUID for issue #{resolved} from SQLite fallback")
                })
            }
        } else {
            let uuid_str = db.get_issue_uuid_by_id(resolved)?;
            uuid_str.parse().with_context(|| {
                format!("Invalid UUID for local issue L{}", resolved.unsigned_abs())
            })
        }
    }

    /// Write files from a `WriteSet` to the cache directory and update counters.
    ///
    /// Uses [`crate::utils::atomic_write`] for the `JSON` writes (#604).
    /// `std::fs::write` is open-truncate-write — if the process dies
    /// mid-write the target file is left half-populated and subsequent
    /// reads fail with a `JSON` parse error until the user runs
    /// `git checkout HEAD -- <path>`. The temp-file + atomic-rename
    /// approach makes the write all-or-nothing relative to process
    /// crashes, which is the only #604 failure mode that doesn't
    /// self-heal on the next successful mutation.
    fn apply_write_set(&self, write_set: &WriteSet) -> Result<()> {
        if !write_set.use_git_rm {
            for (rel_path, content) in &write_set.files {
                // Validate JSON content before writing to prevent corruption
                if std::path::Path::new(rel_path)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                {
                    if let Err(e) = serde_json::from_slice::<serde_json::Value>(content) {
                        bail!("Refusing to write invalid JSON to hub cache: {rel_path} ({e})");
                    }
                }
                let full = self.cache_dir.join(rel_path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                crate::utils::atomic_write(&full, content)?;

                // Clean up stale V1 flat file when writing V2 directory
                // format (#428). The sync-level cleanup_stale_layout_files()
                // is the guarantee; this is opportunistic (#478).
                if rel_path.ends_with("/issue.json") {
                    if let Some(uuid_dir) = rel_path.strip_suffix("/issue.json") {
                        let v1_path = self.cache_dir.join(format!("{uuid_dir}.json"));
                        if v1_path.exists() {
                            if let Err(e) = std::fs::remove_file(&v1_path) {
                                tracing::warn!(
                                    "stale V1 file {} could not be removed (sync cleanup will retry): {}",
                                    v1_path.display(),
                                    e
                                );
                            }
                        }
                    }
                }
            }
        }
        if let Some(ref c) = write_set.counters {
            self.write_counters_to_cache(c)?;
        }
        Ok(())
    }

    /// Generate content, commit, and push with retry.
    ///
    /// The `prepare` closure is called on **every** attempt, so it must
    /// re-read any mutable state (counters, issue files) from the cache
    /// which may have changed after a rebase pull.  This prevents stale
    /// display-ID collisions when two agents race.
    pub(super) fn write_commit_push<F>(&self, mut prepare: F, message: &str) -> Result<PushOutcome>
    where
        F: FnMut(&Self) -> Result<WriteSet>,
    {
        // Serialize access to the hub cache via SyncManager's lock (#400, #457)
        let _lock_guard = self.sync.acquire_lock()?;

        for attempt in 0..MAX_RETRIES {
            // Recover from broken git states before attempting write (#454, #455, #456)
            self.hub_health_check();

            // (Re-)generate content -- reads fresh counters/files after rebase
            let write_set = prepare(self)?;

            // Write files to cache and update counters
            self.apply_write_set(&write_set)?;

            // Collect relative paths for staging
            let mut paths: Vec<String> = write_set.files.iter().map(|(p, _)| p.clone()).collect();
            if write_set.counters.is_some() {
                paths.push("meta/counters.json".to_string());
            }

            // Stage
            for path in &paths {
                if write_set.use_git_rm {
                    // Use `git rm` (not --cached) so files are removed from
                    // both the index AND the working directory atomically.
                    // This prevents split state where the file is gone from
                    // disk but the commit fails (#427). --force handles
                    // modified files; --ignore-unmatch handles retries where
                    // the file is already gone.
                    // -r enables recursive removal for V2 directories (#460)
                    // INTENTIONAL: git rm is best-effort — --ignore-unmatch handles missing files on retry
                    if let Err(e) =
                        self.git_in_cache(&["rm", "-r", "--force", "--ignore-unmatch", path])
                    {
                        tracing::debug!(
                            "git rm for '{}' did not succeed (may be already removed): {}",
                            path,
                            e
                        );
                    }
                } else {
                    self.git_in_cache(&["add", path])?;
                }
            }

            // Commit (unsigned when no SSH key)
            let commit_msg = format!(
                "{}: {} at {}",
                self.agent.agent_id,
                message,
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
            );
            let commit_result = self.git_commit_in_cache(&commit_msg);
            if let Err(e) = &commit_result {
                let err_str = e.to_string();
                if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                    return Ok(PushOutcome::Pushed);
                }
                // Commit failed — if we were deleting files (git rm), restore
                // Commit failed — reset index and working directory to HEAD
                // to prevent split state (#427, #468). This is safe because
                // the commit didn't succeed, so HEAD is the correct state.
                if write_set.use_git_rm {
                    if let Err(reset_err) = self.git_in_cache(&["reset", "--hard", "HEAD"]) {
                        tracing::error!(
                            "hub cache may be corrupt: commit failed and reset failed: {}",
                            reset_err
                        );
                    }
                }
                commit_result?;
            }

            // Push
            let remote = self.sync.remote();
            let push_result = self.git_in_cache(&["push", remote, crate::sync::HUB_BRANCH]);
            match push_result {
                Ok(_) => return Ok(PushOutcome::Pushed),
                Err(e) => {
                    let err_str = e.to_string();
                    // Offline -- commit is local, will push on next sync
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        tracing::warn!(
                            "Warning: push failed (offline), changes saved locally only: {}",
                            message
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    // Conflict -- reset commit AND working directory, pull latest,
                    // then retry. The prepare closure re-reads fresh state on the
                    // next iteration, so losing working dir changes is safe.
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < MAX_RETRIES - 1 {
                            // Bail if local has diverged too far -- sign of a rebase loop
                            self.check_divergence()?;
                            // Escalating recovery: get to a known-good state (#466)
                            self.recover_from_push_conflict(remote)?;
                            continue;
                        }
                        // All retries exhausted -- keep as local-only
                        tracing::warn!(
                            "Warning: push failed after {} retries (conflict), changes saved locally only: {}",
                            MAX_RETRIES, message
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    // Other error -- propagate
                    return Err(e);
                }
            }
        }
        Ok(PushOutcome::Pushed)
    }

    /// Check if local has diverged too far from remote and bail if so.
    /// Delegates to `SyncManager::check_divergence` via the shared `sync` field.
    pub(super) fn check_divergence(&self) -> Result<()> {
        self.sync.check_divergence()
    }

    /// Run hub health checks to recover from broken git states.
    /// Delegates to `SyncManager::hub_health_check` via the shared `sync` field.
    pub(super) fn hub_health_check(&self) {
        self.sync.hub_health_check();
    }

    /// Run a git command in the cache worktree.
    pub(super) fn git_in_cache(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = std::process::Command::new("git")
            .current_dir(&self.cache_dir)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {args:?} in cache"))?;
        if !output.status.success() {
            // Capture BOTH streams (#601). Some git status messages — most
            // notably "nothing to commit, working tree clean" — go to
            // stdout, not stderr. Capturing only stderr produced empty
            // failure messages and silently disabled the substring-based
            // no-op guards in the push paths below.
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "git {args:?} in cache failed ({}):\nstdout: {}\nstderr: {}",
                output.status,
                stdout.trim(),
                stderr.trim(),
            );
        }
        Ok(output)
    }

    /// Escalating recovery from a push conflict (#466).
    ///
    /// Attempts to get the hub cache back to a known-good state so the
    /// retry loop can re-prepare and push again. Each step verifies it
    /// worked before moving on:
    ///
    /// 1. Reset HEAD~1 to undo our commit
    /// 2. Pull --rebase to sync with remote
    /// 3. If rebase conflicts: abort, then reset to remote
    /// 4. Verify we're on the branch and not mid-rebase
    pub(super) fn recover_from_push_conflict(&self, remote: &str) -> Result<()> {
        let remote_ref = format!("{}/{}", remote, crate::sync::HUB_BRANCH);

        // Step 1: undo our commit
        if self.git_in_cache(&["reset", "--hard", "HEAD~1"]).is_err() {
            tracing::warn!("reset HEAD~1 failed, falling back to reset to remote");
            self.git_in_cache(&["reset", "--hard", &remote_ref])?;
            return self.verify_clean_state();
        }

        // Step 2: pull latest from remote
        let pull_result = self.git_in_cache(&["pull", "--rebase", remote, crate::sync::HUB_BRANCH]);

        if let Err(e) = pull_result {
            let err_str = e.to_string();
            if err_str.contains("CONFLICT")
                || err_str.contains("rebase")
                || err_str.contains("could not apply")
            {
                // Step 3: rebase conflicted — abort and force-align to remote
                let _ = self.git_in_cache(&["rebase", "--abort"]);
                self.git_in_cache(&["reset", "--hard", &remote_ref])?;
            } else {
                // Pull failed for non-conflict reason — health check + retry
                self.hub_health_check();
                self.git_in_cache(&["pull", "--rebase", remote, crate::sync::HUB_BRANCH])?;
            }
        }

        // Step 4: verify we're in a known-good state before returning
        self.verify_clean_state()
    }

    /// Verify the hub cache is in a clean, usable state.
    ///
    /// Checks: on the correct branch, not mid-rebase, clean working directory.
    /// Called after recovery operations to confirm they actually worked.
    fn verify_clean_state(&self) -> Result<()> {
        // Must be on the hub branch, not detached
        if self.git_in_cache(&["symbolic-ref", "HEAD"]).is_err() {
            bail!("hub cache recovery failed: HEAD is still detached");
        }

        // Must not be mid-rebase
        let git_dir = self.git_in_cache(&["rev-parse", "--git-dir"]).map_or_else(
            |_| self.cache_dir.join(".git"),
            |o| {
                let raw = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let p = PathBuf::from(&raw);
                if p.is_absolute() {
                    p
                } else {
                    self.cache_dir.join(p)
                }
            },
        );

        if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
            bail!("hub cache recovery failed: still in mid-rebase state");
        }

        Ok(())
    }

    /// Run a git commit in the cache worktree, disabling signing when
    /// the agent has no SSH key (anonymous/pre-init mode).
    pub(super) fn git_commit_in_cache(&self, message: &str) -> Result<std::process::Output> {
        self.git_commit_in_cache_with_args(&["-m", message])
    }

    /// Run a git commit with arbitrary args in the cache worktree,
    /// disabling signing when the agent has no SSH key.
    pub(super) fn git_commit_in_cache_with_args(
        &self,
        args: &[&str],
    ) -> Result<std::process::Output> {
        let has_key = self.agent.ssh_key_path.is_some();
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(&self.cache_dir);
        if !has_key {
            cmd.args(["-c", "commit.gpgsign=false"]);
        }
        cmd.arg("commit").args(args);
        let output = cmd
            .output()
            .with_context(|| format!("Failed to run git commit {args:?} in cache"))?;
        if !output.status.success() {
            // Capture BOTH streams (#601). `git commit`'s "nothing to
            // commit, working tree clean" status message goes to stdout,
            // so capturing only stderr produced empty failure messages.
            // The substring-based no-op guards in `write_commit_push` and
            // `emit_compact_push` rely on this message being visible in
            // the error string they inspect.
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "git commit {args:?} in cache failed ({}):\nstdout: {}\nstderr: {}",
                output.status,
                stdout.trim(),
                stderr.trim(),
            );
        }
        Ok(output)
    }
}
