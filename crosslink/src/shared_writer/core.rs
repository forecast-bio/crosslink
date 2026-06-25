//! Core types and infrastructure for `SharedWriter`.
//!
//! Contains the `SharedWriter` struct, `new()`, the v3 event-only write path
//! (`write_commit_push` / `emit_compact_push` -> `commit_v3`), envelope
//! signing, and issue/milestone resolution from the reduced v3 state. Mutations
//! on a legacy v2 hub are refused (#754, REQ-10).

use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::db::Database;
use crate::identity::AgentConfig;
use crate::issue_file::{IssueFile, MilestoneEntry};
use crate::sync::SyncManager;

// Hub cache write lock is in sync/cache.rs — acquired via self.sync.acquire_lock()

/// Comment kind for intervention comments.
pub(super) const KIND_INTERVENTION: &str = "intervention";
/// SSH signing namespace for crosslink comments.
pub(super) const SIGNING_NAMESPACE: &str = "crosslink-comment";

/// The events a mutation emits in `HubMode::V3`.
///
/// Historically this carried the v2 worktree files and counters too; the v2
/// write path is deleted (#754, REQ-10), so a mutation now produces events
/// only. The `prepare` closure builds these once and `commit_v3` appends them
/// to the agent's own ref.
pub(super) struct WriteSet {
    /// Events to append to the agent's own ref (REQ-1).
    pub events: Vec<crate::events::Event>,
}

/// Refusal message emitted when a mutation is attempted on a legacy v2 hub.
///
/// The v2 write path is deleted (#754, REQ-10). The v2 branch is kept as a
/// read-only escape hatch until `crosslink migrate hub-v3 --finalize`, so the
/// fix is to migrate, not to repair the v2 layout.
pub(super) const V2_WRITE_REFUSAL: &str = "this hub uses the legacy v2 layout; run `crosslink migrate hub-v3` to migrate (the v2 branch is kept as an escape hatch until --finalize)";

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
/// Handles: build events -> append to the agent's own ref -> fast-forward push
/// -> reduce -> hydrate local `SQLite` (hub v3, #754). Display ids are assigned
/// by the reduction, not claimed from a counter.
pub struct SharedWriter {
    pub(super) sync: SyncManager,
    pub(super) agent: AgentConfig,
    pub(super) cache_dir: PathBuf,
    /// Per-session event sequence counter, monotonically increasing.
    pub(super) event_seq: Cell<u64>,
    /// The most recent reduced state from a v3 `commit_v3` / fetch. The
    /// create/comment/milestone flows read the reduction-assigned display id
    /// from here (`state.display_id_map[uuid]`, REQ-4) for CLI output; `None`
    /// before the first v3 mutation.
    pub(super) last_v3_state: std::cell::RefCell<Option<crate::checkpoint::CheckpointState>>,
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

        // Initialize event sequence counter from existing log. In V3 the
        // authoritative log is the agent's OWN REF (read via git cat-file);
        // in V2 it is the worktree `events.log` file. read_max_event_seq
        // dispatches by mode so a fresh worktree (post-prune) does not reset
        // the sequence below the ref's tip.
        let event_seq = Cell::new(Self::read_max_event_seq(
            &cache_dir,
            &agent.agent_id,
            sync.hub_mode(),
        ));

        // Minimal v3-aware warn: if the hub has already been migrated to v3 but
        // we resolved v2 mode (an exotic concurrent-migration race), warn once.
        // Cheap (a rev-parse), non-fatal — never blocks the operation. No-op
        // when this client resolved V3 mode (clients route by hub version).
        crate::hub_v3::warn_if_migrated_v2_operation(&cache_dir, sync.hub_mode());

        Ok(Some(Self {
            sync,
            agent,
            cache_dir,
            event_seq,
            last_v3_state: std::cell::RefCell::new(None),
        }))
    }

    /// The configured agent id for this writer.
    ///
    /// Used only by tests since the v2 write path (the production consumer) was
    /// deleted (#754); kept as a tested accessor.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn agent_id(&self) -> &str {
        &self.agent.agent_id
    }

    /// The resolved operation mode (V2 worktree-file or V3 event-only) of the
    /// underlying `SyncManager`.
    pub(super) fn hub_mode(&self) -> crate::hub_v3::HubMode {
        self.sync.hub_mode()
    }

    /// Whether this writer operates a v3 hub (event-only, per-agent refs).
    pub(super) fn is_v3(&self) -> bool {
        self.hub_mode().is_v3()
    }

    /// Public accessor for v3 mode, for cross-module callers (`agent_requests`).
    #[must_use]
    pub fn is_v3_public(&self) -> bool {
        self.is_v3()
    }

    /// Public accessor for the hub-cache directory (the v3 ref repo dir), for
    /// cross-module callers (`agent_requests` v3 poll).
    #[must_use]
    pub fn cache_dir_public(&self) -> &Path {
        &self.cache_dir
    }

    /// Derive the `.crosslink/` directory from the cache path.
    ///
    /// Used only by tests since the v2 write path (the production consumer) was
    /// deleted (#754); kept as a tested accessor.
    #[cfg_attr(not(test), allow(dead_code))]
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
        // V3: hydrate from the reduced state cached by the last commit_v3 /
        // refresh_v3_state (event-only operation — no worktree issue files to
        // read). If no state is cached yet (first call before any v3 mutation),
        // reduce now so SQLite still reflects the hub.
        if self.is_v3() {
            if self.last_v3_state.borrow().is_none() {
                if let Err(e) = self.refresh_v3_state() {
                    tracing::warn!("v3 hydrate: state refresh failed: {e}");
                    return;
                }
            }
            if let Some(state) = self.last_v3_state.borrow().as_ref() {
                if let Err(e) = crate::hydration::hydrate_from_state(state, db) {
                    tracing::warn!(
                        "v3 hydrate_from_state failed ({e}). Run `crosslink sync` to recover."
                    );
                }
            }
            return;
        }
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

    // ---- Event emission infrastructure ----

    /// Read the max `agent_seq` from this agent's existing event log.
    ///
    /// V2: reads the worktree file `agents/<id>/events.log`. V3: reads the
    /// agent's OWN REF (`refs/heads/crosslink/agents/<id>` -> `events.log`) via git
    /// cat-file, since there is no worktree log in v3 and the ref is the only
    /// durable record of the sequence high-water mark (including after a prune).
    pub(super) fn read_max_event_seq(
        cache_dir: &Path,
        agent_id: &str,
        mode: crate::hub_v3::HubMode,
    ) -> u64 {
        if mode.is_v3() {
            return crate::hub_v3::read_max_event_seq_from_ref(cache_dir, agent_id).unwrap_or(0);
        }
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

    /// Emit an event to the agent's own ref and push it (fast-forward).
    ///
    /// V3-only (#754, REQ-10): lock claim/release route through the event-only
    /// own-ref path. `commit_v3` reduces locks from events into the checkpoint;
    /// the claim-confirm read (`read_lock_v2`) then resolves the winner from the
    /// reduced state. A mutation on a legacy v2 hub is refused — locks are
    /// mutations, so they cannot be claimed against a v2 layout.
    pub(super) fn emit_compact_push(
        &self,
        event: crate::events::Event,
        _message: &str,
    ) -> Result<PushOutcome> {
        // Serialize access to the hub cache via SyncManager's lock (#372)
        let lock_guard = self.sync.acquire_lock()?;

        if !self.is_v3() {
            bail!(V2_WRITE_REFUSAL);
        }
        self.commit_v3(vec![event], &lock_guard)
    }

    /// Write an agent control request to the driver's own ref.
    ///
    /// V3-only (#754, REQ-10): the DRIVER writes the request into ITS OWN ref
    /// under `requests-out/<target>--<ulid>.json` (single-writer invariant) and
    /// pushes the ref (fast-forward). A v2 hub is refused.
    ///
    /// # Errors
    /// Returns an error if the cache can't be prepared, the ref write fails, or
    /// the request's JSON encoding fails.
    pub fn write_agent_request(
        &self,
        target_agent_id: &str,
        request: &crate::agent_requests::AgentRequest,
    ) -> Result<PushOutcome> {
        // Serialize access to the hub cache (#372).
        let _lock_guard = self.sync.acquire_lock()?;

        if !self.is_v3() {
            bail!(V2_WRITE_REFUSAL);
        }
        crate::hub_v3::write_request_to_own_ref(
            &self.cache_dir,
            &self.agent.agent_id,
            target_agent_id,
            request,
        )?;
        Ok(self.push_own_ref_outcome())
    }

    /// Write an ack for a previously-received agent request.
    ///
    /// V3-only (#754, REQ-10): the TARGET agent writes the ack into ITS OWN ref
    /// under `requests-ack/<ulid>.json` (single-writer invariant) and pushes the
    /// ref. `target_agent_id` here IS the acking agent (the poll passes its own
    /// id). A v2 hub is refused.
    ///
    /// # Errors
    /// Returns an error if the cache can't be prepared, the ref write fails, or
    /// the ack's JSON encoding fails.
    pub fn write_agent_ack(
        &self,
        _target_agent_id: &str,
        ack: &crate::agent_requests::AgentRequestAck,
    ) -> Result<PushOutcome> {
        let _lock_guard = self.sync.acquire_lock()?;

        if !self.is_v3() {
            bail!(V2_WRITE_REFUSAL);
        }
        crate::hub_v3::write_ack_to_own_ref(
            &self.cache_dir,
            &self.agent.agent_id,
            &ack.request_id,
            ack,
        )?;
        Ok(self.push_own_ref_outcome())
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

    /// Load a milestone entry by `display_id`, reconstructed from the reduced
    /// v3 state's `CompactMilestone` (v3-only; no worktree milestone files).
    pub(super) fn load_milestone_by_id(&self, display_id: i64) -> Result<MilestoneEntry> {
        if self.last_v3_state.borrow().is_none() {
            self.refresh_v3_state()?;
        }
        let state = self.last_v3_state.borrow();
        let state = state.as_ref().ok_or_else(|| {
            anyhow::anyhow!("v3 state unavailable while loading milestone {display_id}")
        })?;
        let cm = state
            .milestones
            .values()
            .find(|m| m.display_id == Some(display_id))
            .ok_or_else(|| anyhow::anyhow!("Milestone #{display_id} not found in v3 state"))?;
        Ok(MilestoneEntry {
            uuid: cm.uuid,
            display_id,
            name: cm.name.clone(),
            description: cm.description.clone(),
            status: cm.status,
            created_at: cm.created_at,
            closed_at: cm.closed_at,
        })
    }

    /// Load an issue by its display ID, reconstructed from the reduced v3 state
    /// (v3-only; no worktree issue files). The prepare closures use the loaded
    /// `IssueFile` only to read the issue's uuid and current fields.
    pub(super) fn load_issue_by_display_id(&self, display_id: i64) -> Result<IssueFile> {
        if self.last_v3_state.borrow().is_none() {
            self.refresh_v3_state()?;
        }
        let state = self.last_v3_state.borrow();
        let state = state.as_ref().ok_or_else(|| {
            anyhow::anyhow!("v3 state unavailable while loading issue {display_id}")
        })?;
        let ci = state
            .issues
            .values()
            .find(|i| i.display_id == Some(display_id))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Issue {} not found in v3 state",
                    crate::utils::format_issue_id(display_id)
                )
            })?;
        Ok(IssueFile {
            uuid: ci.uuid,
            display_id: ci.display_id,
            title: ci.title.clone(),
            description: ci.description.clone(),
            status: ci.status,
            priority: ci.priority,
            parent_uuid: ci.parent_uuid,
            created_by: ci.created_by.clone(),
            created_at: ci.created_at,
            updated_at: ci.updated_at,
            closed_at: ci.closed_at,
            scheduled_at: ci.scheduled_at,
            due_at: ci.due_at,
            labels: ci.labels.iter().cloned().collect(),
            comments: vec![],
            blockers: ci.blockers.iter().copied().collect(),
            related: ci.related.iter().copied().collect(),
            milestone_uuid: ci.milestone_uuid,
            time_entries: vec![],
        })
    }

    /// Load an issue by its display ID from the reduced v3 state.
    ///
    /// Negative (offline) ids are a v2-only concept — v3 reduction assigns every
    /// issue an authoritative id, so a negative id cannot be resolved here.
    pub(super) fn load_issue_by_id(&self, id: i64, db: &Database) -> Result<IssueFile> {
        let resolved = db.resolve_id(id);
        if resolved >= 0 {
            self.load_issue_by_display_id(resolved)
        } else {
            bail!(
                "negative (offline) issue id L{} is not valid on a v3 hub",
                resolved.unsigned_abs()
            )
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

    // ──────────────────────────── V3 write path ─────────────────────────
    //
    // 754a PASS 2. In `HubMode::V3` a mutation writes EVENTS ONLY to the
    // agent's own ref. No worktree files, no counter reads, no rebase/conflict
    // machinery: pushes to the own ref are fast-forward by construction
    // (single-writer-per-ref), and ids are reduction-assigned so there is no
    // offline-promotion or counter-revert dance. The entire v2 offline/promote
    // path is UNNECESSARY in v3 because (a) ids come from reduction (no
    // double-mint to revert) and (b) every push is an own-ref fast-forward (no
    // rebase). A failed push leaves the events durable on the LOCAL ref; the
    // next successful push delivers them.

    /// Normalize a mutation's events for the v3 write path: drop any
    /// `display_id` so the reducer assigns the authoritative id (REQ-4). The
    /// prepare closures emit `display_id: None` already; this is a belt-and-
    /// braces pass over `IssueCreated` / `CommentAdded` / `TimeEntryAdded` /
    /// `MilestoneCreated` so any stray claimed id becomes a pure-v3 emitter.
    fn normalize_events_for_v3(events: Vec<crate::events::Event>) -> Vec<crate::events::Event> {
        use crate::events::Event;
        events
            .into_iter()
            .map(|e| match e {
                Event::IssueCreated {
                    uuid,
                    title,
                    description,
                    priority,
                    labels,
                    parent_uuid,
                    created_by,
                    display_id: _,
                    scheduled_at,
                    due_at,
                } => Event::IssueCreated {
                    uuid,
                    title,
                    description,
                    priority,
                    labels,
                    parent_uuid,
                    created_by,
                    display_id: None,
                    scheduled_at,
                    due_at,
                },
                Event::CommentAdded {
                    issue_uuid,
                    comment_uuid,
                    display_id: _,
                    author,
                    content,
                    created_at,
                    kind,
                    trigger_type,
                    intervention_context,
                    driver_key_fingerprint,
                    signed_by,
                    signature,
                } => Event::CommentAdded {
                    issue_uuid,
                    comment_uuid,
                    display_id: None,
                    author,
                    content,
                    created_at,
                    kind,
                    trigger_type,
                    intervention_context,
                    driver_key_fingerprint,
                    signed_by,
                    signature,
                },
                Event::TimeEntryAdded {
                    issue_uuid,
                    entry_uuid,
                    display_id: _,
                    started_at,
                    ended_at,
                    duration_seconds,
                } => Event::TimeEntryAdded {
                    issue_uuid,
                    entry_uuid,
                    display_id: None,
                    started_at,
                    ended_at,
                    duration_seconds,
                },
                Event::MilestoneCreated {
                    uuid,
                    display_id: _,
                    name,
                    description,
                    created_at,
                } => Event::MilestoneCreated {
                    uuid,
                    display_id: None,
                    name,
                    description,
                    created_at,
                },
                other => other,
            })
            .collect()
    }

    /// Append `events` to this agent's OWN REF, push it (fast-forward), then
    /// reduce + hydrate so `SQLite` reflects the mutation immediately.
    ///
    /// Caller MUST already hold the hub write lock (`sync.acquire_lock()`,
    /// REQ-8 single local lock). Returns the [`PushOutcome`]: `Pushed` when the
    /// own ref reached the remote (or no remote is configured), `LocalOnly`
    /// when the push failed benignly (offline / transient) — the events are
    /// durable on the local ref and the next successful push delivers them.
    ///
    /// On success the reduced [`crate::checkpoint::CheckpointState`] is cached
    /// in `self.last_v3_state` so create/comment/milestone flows can read the
    /// reduction-assigned display id (REQ-4).
    fn commit_v3(
        &self,
        events: Vec<crate::events::Event>,
        _lock: &crate::sync::HubWriteLock,
    ) -> Result<PushOutcome> {
        let agent_id = self.agent.agent_id.clone();
        let normalized = Self::normalize_events_for_v3(events);

        // 1. Envelope + append each event to the OWN REF (sibling-preserving).
        //    Sequence numbers come from `self.event_seq`, initialized in `new`
        //    from the ref's log (read_max_event_seq in V3 mode). No worktree
        //    `events.log` is written — the ref is the only log.
        for event in normalized {
            let envelope = self.create_envelope(event);
            crate::hub_v3::append_event_to_ref(&self.cache_dir, &agent_id, &envelope)
                .context("v3: failed to append event to agent ref")?;
        }

        // 2. Push the own ref (plain fast-forward CAS). A non-Pushed outcome is
        //    benign: the events stay durable on the local ref. NonFastForward on
        //    our OWN ref would indicate identity collision / tampering (REQ-1)
        //    — surfaced loudly but still treated as LocalOnly (state is durable).
        let remote = self.sync.remote();
        let mut outcome = PushOutcome::Pushed;
        if self.sync.remote_exists() {
            match crate::hub_v3::push_agent_ref(&self.cache_dir, remote, &agent_id)? {
                crate::hub_v3::PushOutcome::Pushed => {}
                crate::hub_v3::PushOutcome::NonFastForward => {
                    tracing::error!(
                        "v3 own-ref push for agent '{agent_id}' was rejected as non-fast-forward \
                         — identity collision or ref tampering (REQ-1); events remain durable \
                         on the local ref"
                    );
                    outcome = PushOutcome::LocalOnly;
                }
                crate::hub_v3::PushOutcome::NoRemote => {
                    outcome = PushOutcome::LocalOnly;
                }
                crate::hub_v3::PushOutcome::Failed(detail) => {
                    tracing::warn!(
                        "v3 own-ref push for agent '{agent_id}' did not complete ({detail}); \
                         events saved locally only"
                    );
                    outcome = PushOutcome::LocalOnly;
                }
            }
        } else {
            outcome = PushOutcome::LocalOnly;
        }

        // 3. Fetch + adopt OTHER agents' refs BEFORE reducing, so the reduced
        //    state (and the checkpoint we write) reflects the full event set —
        //    not just our local view. This is what makes the lock claim-confirm
        //    correct: an earlier-ordered claim from another agent that arrives
        //    here is seen now, rather than being masked by a checkpoint we
        //    advanced from a partial view. The hub write lock is already held
        //    (we are inside write_commit_push / emit_compact_push), so we use the
        //    lock-free fetch_and_adopt_v3_refs rather than sync.fetch() (which
        //    would re-acquire the non-reentrant lock and deadlock).
        if self.sync.remote_exists() {
            self.sync.fetch_and_adopt_v3_refs();
        }

        // 4. Reduce -> cache state for display-id lookup + hydration. Write +
        //    push the checkpoint (pure cache, REQ-7). The write path does NOT
        //    prune the own ref: pruning every mutation would rewrite the own ref
        //    each time (and a prune followed by a plain push is non-fast-forward).
        //    REQ-11 prune is confined to the explicit `compact` command.
        self.refresh_v3_state()?;
        self.write_and_push_v3_checkpoint();

        Ok(outcome)
    }

    /// Reduce-free checkpoint refresh for the write path: serialize the cached
    /// `last_v3_state`, write it to the local checkpoint ref (idempotent), and
    /// push it (best-effort). NO prune. A failure is logged, never fatal — the
    /// checkpoint is a pure cache (REQ-7) and readers reduce on demand.
    fn write_and_push_v3_checkpoint(&self) {
        let bytes = {
            let state = self.last_v3_state.borrow();
            let Some(state) = state.as_ref() else {
                return;
            };
            let mut state = state.clone();
            state.compaction_lease = None;
            match serde_json::to_vec_pretty(&state) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!("v3: checkpoint serialization failed (non-fatal): {e}");
                    return;
                }
            }
        };
        // Idempotent: skip when the local checkpoint already matches.
        if let Ok(Some(tip)) =
            crate::hub_v3::git_rev_parse_optional(&self.cache_dir, crate::hub_v3::CHECKPOINT_REF)
        {
            let spec = format!("{tip}:state.json");
            if let Ok(Some(existing)) =
                crate::hub_v3::git_cat_file_blob_optional(&self.cache_dir, &spec)
            {
                if existing == bytes {
                    return;
                }
            }
        }
        if let Err(e) = crate::hub_v3::commit_blob_to_ref(
            &self.cache_dir,
            crate::hub_v3::CHECKPOINT_REF,
            "state.json",
            &bytes,
            "crosslink v3 checkpoint",
        ) {
            tracing::warn!("v3: checkpoint write failed (non-fatal): {e}");
            return;
        }
        if self.sync.remote_exists() {
            let expected = crate::hub_v3::git_rev_parse_optional(
                &self.cache_dir,
                "refs/crosslink-remote/checkpoint",
            )
            .ok()
            .flatten();
            match crate::hub_v3::push_ref_with_lease(
                &self.cache_dir,
                self.sync.remote(),
                crate::hub_v3::CHECKPOINT_REF,
                expected.as_deref(),
            ) {
                Ok(
                    crate::hub_v3::PushOutcome::Pushed | crate::hub_v3::PushOutcome::NonFastForward,
                ) => {}
                Ok(other) => tracing::debug!("v3: checkpoint push did not complete: {other:?}"),
                Err(e) => tracing::debug!("v3: checkpoint push error (benign): {e}"),
            }
        }
    }

    /// Reduce the current v3 ref namespace and cache the materialized state in
    /// `self.last_v3_state` (for display-id lookup). Does NOT touch `SQLite` —
    /// the caller drives hydration onto its own `&Database` via
    /// [`Self::hydrate_with_retry`], which dispatches to `hydrate_from_state`
    /// under V3 using this cached state.
    fn refresh_v3_state(&self) -> Result<()> {
        let source = crate::hub_source::RefHubSource::new(&self.cache_dir)
            .context("v3: failed to construct RefHubSource for state refresh")?;
        let outcome =
            crate::compaction::reduce(&source).context("v3: reduction for state refresh failed")?;
        *self.last_v3_state.borrow_mut() = Some(outcome.state);
        Ok(())
    }

    /// Push this agent's OWN ref and map the result to a [`PushOutcome`].
    /// Shared by the v3 request/ack writers. A non-`Pushed` result is benign:
    /// the data is durable on the local ref and delivers on the next push.
    fn push_own_ref_outcome(&self) -> PushOutcome {
        if !self.sync.remote_exists() {
            return PushOutcome::LocalOnly;
        }
        match crate::hub_v3::push_agent_ref(
            &self.cache_dir,
            self.sync.remote(),
            &self.agent.agent_id,
        ) {
            Ok(crate::hub_v3::PushOutcome::Pushed) => PushOutcome::Pushed,
            Ok(other) => {
                tracing::warn!(
                    "v3 own-ref push for '{}' did not complete: {other:?}; saved locally",
                    self.agent.agent_id
                );
                PushOutcome::LocalOnly
            }
            Err(e) => {
                tracing::warn!("v3 own-ref push for '{}' error: {e}", self.agent.agent_id);
                PushOutcome::LocalOnly
            }
        }
    }

    /// V3 lock claim-confirm helper: fetch every other agent's ref, reduce, and
    /// re-cache the state so a subsequent `read_lock_v2` sees the full event set
    /// (first-claim-wins winner). `sync.fetch()` is the v3 fetch (adopts other
    /// agents' refs + checkpoint, then compacts), after which `refresh_v3_state`
    /// re-reduces and caches. A fetch failure (offline) is non-fatal — we then
    /// confirm against the local view, which is the best available.
    pub(super) fn confirm_v3_locks(&self) -> Result<()> {
        if let Err(e) = self.sync.fetch() {
            tracing::warn!("v3 lock confirm: fetch failed ({e}); confirming against local view");
        }
        self.refresh_v3_state()
    }

    /// Look up the reduction-assigned display id for `uuid` from the last
    /// cached v3 state (`display_id_map`, REQ-4). Returns `None` when the id is
    /// not yet frozen by reduction (provisional) or no state is cached.
    pub(super) fn v3_assigned_display_id(&self, uuid: &Uuid) -> Option<i64> {
        self.last_v3_state
            .borrow()
            .as_ref()
            .and_then(|s| s.display_id_map.get(uuid).copied())
    }

    /// Look up the reduction-assigned comment display id from the last cached
    /// v3 state, by the comment's host issue display id and the comment uuid.
    /// Returns `None` when the comment's id is provisional (not yet frozen) or
    /// the state/issue/comment is not present.
    pub(super) fn v3_assigned_comment_id(
        &self,
        issue_display_id: i64,
        comment_uuid: &Uuid,
    ) -> Option<i64> {
        let state = self.last_v3_state.borrow();
        let state = state.as_ref()?;
        let issue = state
            .issues
            .values()
            .find(|i| i.display_id == Some(issue_display_id))?;
        issue.comments.get(comment_uuid).and_then(|c| c.display_id)
    }

    /// Look up the reduction-assigned milestone display id for `uuid` from the
    /// last cached v3 state. Returns `None` when not yet assigned by reduction.
    pub(super) fn v3_assigned_milestone_id(&self, uuid: &Uuid) -> Option<i64> {
        self.last_v3_state
            .borrow()
            .as_ref()
            .and_then(|s| s.milestones.get(uuid).and_then(|m| m.display_id))
    }

    /// Generate content, commit, and push with retry.
    ///
    /// The `prepare` closure is called on **every** attempt, so it must
    /// re-read any mutable state (counters, issue files) from the cache
    /// which may have changed after a rebase pull.  This prevents stale
    /// display-ID collisions when two agents race.
    ///
    /// V3-only (#754, REQ-10): `prepare` is run ONCE to produce the events,
    /// which are appended to the agent's own ref and pushed fast-forward (see
    /// [`Self::commit_v3`]). A mutation on a legacy v2 hub is refused — the v2
    /// worktree-file write path is deleted.
    pub(super) fn write_commit_push<F>(&self, mut prepare: F, _message: &str) -> Result<PushOutcome>
    where
        F: FnMut(&Self) -> Result<WriteSet>,
    {
        // Serialize access to the hub cache via SyncManager's lock (#400, #457)
        let lock_guard = self.sync.acquire_lock()?;

        if !self.is_v3() {
            bail!(V2_WRITE_REFUSAL);
        }

        // Run prepare ONCE: it produces the events that drive the ref-only write.
        let write_set = prepare(self)?;
        self.commit_v3(write_set.events, &lock_guard)
    }
}
