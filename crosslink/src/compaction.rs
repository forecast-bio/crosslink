//! Compaction engine for the event-sourced CRDT system.
//!
//! Reads append-only event logs from all agents, applies deterministic
//! reduction rules, and materializes the result as checkpoint state plus
//! per-entity JSON files (issues, locks).

use anyhow::{Context, Result};
use chrono::Utc;
use std::collections::{BTreeSet, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::checkpoint::{
    read_checkpoint, read_watermark, write_checkpoint, CheckpointState, CompactIssue, LockEntry,
    SkewWarning, UnsignedEventWarning,
};
use crate::events::{Event, EventEnvelope, OrderingKey};
use crate::issue_file::{IssueFile, LockFileV2};

/// Compaction lease duration in seconds.
///
/// Used by `CompactionLockGuard` to determine when a lock file is stale
/// (age > 2 × this value). Also used by the test-only lease helper for
/// in-memory lease expiry. The value must exceed the longest expected
/// compaction run to avoid premature expiry; 30 seconds is sufficient for
/// typical repos with <10k events.
const LEASE_DURATION_SECS: i64 = 30;

/// Lock file name inside the checkpoint directory.
const COMPACTION_LOCK_FILE: &str = "compaction.lock";

/// RAII guard for the compaction file lock.
///
/// On creation, atomically creates a lock file using `create_new(true)` which
/// fails if the file already exists. On drop, removes the lock file.
/// This ensures only one compaction process runs at a time.
struct CompactionLockGuard {
    path: PathBuf,
}

/// Information parsed from an existing compaction lock file to determine
/// whether the lock is stale (held by a dead or timed-out process) or
/// whether the current agent already owns it and can safely reclaim.
struct StaleLockInfo {
    /// The agent ID that created the lock.
    agent_id: String,
    /// When the lock was originally acquired.
    acquired_at: chrono::DateTime<Utc>,
}

impl CompactionLockGuard {
    /// Try to acquire the compaction lock by atomically creating a lock file.
    fn try_acquire(cache_dir: &Path, agent_id: &str, force: bool) -> Result<Option<Self>> {
        let lock_dir = cache_dir.join("checkpoint");
        fs::create_dir_all(&lock_dir)
            .with_context(|| format!("Failed to create checkpoint dir: {}", lock_dir.display()))?;
        let path = lock_dir.join(COMPACTION_LOCK_FILE);

        match Self::try_create(&path, agent_id) {
            Ok(guard) => return Ok(Some(guard)),
            Err(e) => {
                // If the file doesn't exist, the error is not AlreadyExists —
                // it's a real filesystem error (permissions, disk full, etc.).
                // Propagate it instead of falling through to stale-lock logic.
                if !path.exists() {
                    return Err(e);
                }
                // File exists → another process holds the lock. Fall through
                // to stale-lock detection below.
            }
        }

        if let Some(info) = Self::read_lock_info(&path) {
            let age = Utc::now() - info.acquired_at;
            let is_stale = age.num_seconds() > LEASE_DURATION_SECS * 2;
            let is_self = info.agent_id == agent_id;

            if is_stale || (force && is_self) {
                let _ = fs::remove_file(&path);
                return Self::try_create(&path, agent_id).map(Some).or(Ok(None));
            }
        } else if force {
            let _ = fs::remove_file(&path);
            return Self::try_create(&path, agent_id).map(Some).or(Ok(None));
        }

        Ok(None)
    }

    fn try_create(path: &Path, agent_id: &str) -> Result<Self> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| "Compaction lock file already exists")?;

        let content = serde_json::json!({
            "agent_id": agent_id,
            "acquired_at": Utc::now().to_rfc3339(),
            "pid": std::process::id(),
        });
        file.write_all(content.to_string().as_bytes())
            .with_context(|| "Failed to write compaction lock file")?;

        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    fn read_lock_info(path: &Path) -> Option<StaleLockInfo> {
        let content = fs::read_to_string(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;
        let agent_id = value.get("agent_id")?.as_str()?.to_string();
        let acquired_str = value.get("acquired_at")?.as_str()?;
        let acquired_at = chrono::DateTime::parse_from_rfc3339(acquired_str)
            .ok()?
            .with_timezone(&Utc);
        Some(StaleLockInfo {
            agent_id,
            acquired_at,
        })
    }
}

impl Drop for CompactionLockGuard {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_file(&self.path) {
            // Log but don't panic — the lock file will be detected as stale
            // on the next compaction run and cleaned up then.
            tracing::warn!(
                "failed to remove compaction lock file {}: {}",
                self.path.display(),
                e
            );
        }
    }
}

/// Clock skew threshold in seconds.
const SKEW_THRESHOLD_SECS: i64 = 60;

/// Result of a compaction run.
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub events_processed: usize,
    pub issues_materialized: usize,
    pub locks_materialized: usize,
    pub skew_warnings: usize,
    pub unsigned_warnings: usize,
    pub git_skew_violations: usize,
}

/// Run compaction on the hub cache.
///
/// Reads all agent event logs, applies reduction rules in deterministic order,
/// writes checkpoint state and materializes issue/lock files.
///
/// Uses a filesystem lock file (`checkpoint/compaction.lock`) for mutual
/// exclusion. The lock is created atomically with `create_new(true)` so that
/// concurrent compaction attempts safely fail rather than racing.
///
/// If `force` is false, returns `None` when the lock is held by another agent.
/// If `force` is true, stale or self-owned locks are removed before retrying.
pub fn compact(cache_dir: &Path, agent_id: &str, force: bool) -> Result<Option<CompactionResult>> {
    // Acquire filesystem lock — this is the real mutual exclusion mechanism.
    let _lock_guard = match CompactionLockGuard::try_acquire(cache_dir, agent_id, force)? {
        Some(guard) => guard,
        None => return Ok(None),
    };

    let mut state = read_checkpoint(cache_dir)?;

    // Read watermark for incremental compaction.
    // Prefer embedded watermark from checkpoint state; fall back to legacy file.
    let watermark = match state.watermark.clone() {
        Some(wm) => Some(wm),
        None => read_watermark(cache_dir)?,
    };

    // Collect events from all agent logs
    let agents_dir = cache_dir.join("agents");
    let mut all_events = Vec::new();

    if agents_dir.exists() {
        for entry in std::fs::read_dir(&agents_dir)
            .with_context(|| format!("Failed to read agents dir: {}", agents_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let log_path = entry.path().join("events.log");
            let events = if let Some(ref wm) = watermark {
                crate::events::read_events_after(&log_path, wm)?
            } else {
                crate::events::read_events(&log_path)?
            };
            all_events.extend(events);
        }
    }

    // If we fell back to a legacy file-based watermark, migrate it into the
    // in-memory checkpoint state so future compactions use the embedded path.
    // No need to write + re-read from disk — the checkpoint is written at the
    // end of compaction with the watermark already embedded (#332).
    if state.watermark.is_none() {
        if let Some(ref wm) = watermark {
            state.watermark = Some(wm.clone());
        }
    }

    if all_events.is_empty() && watermark.is_some() {
        // Still run git-based skew detection even with no new events
        let git_violations = crate::clock_skew::detect_git_skew_violations(cache_dir)
            .unwrap_or_else(|e| {
                tracing::warn!("git skew detection failed, defaulting to no violations: {e}");
                Vec::new()
            });
        let git_skew_violations = git_violations.len();
        crate::clock_skew::write_skew_violations(cache_dir, &git_violations)?;

        state.compaction_lease = None;
        write_checkpoint(cache_dir, &state)?;
        // _lock_guard dropped here, removing the lock file
        return Ok(Some(CompactionResult {
            events_processed: 0,
            issues_materialized: 0,
            locks_materialized: 0,
            skew_warnings: state.skew_warnings.len(),
            unsigned_warnings: state.unsigned_event_warnings.len(),
            git_skew_violations,
        }));
    }

    // If no watermark, we're doing a full compaction — reset state
    if watermark.is_none() {
        state = CheckpointState::default();
    }

    // Sort by ordering key for deterministic reduction.
    // Uses sort_by_cached_key to compute the OrderingKey once per envelope
    // rather than on every comparison (#340).
    all_events.sort_by_cached_key(OrderingKey::from_envelope);

    let events_processed = all_events.len();
    let mut changed_issues: HashSet<Uuid> = HashSet::new();
    let mut changed_locks: HashSet<i64> = HashSet::new();

    // For full compaction (no watermark), clear warnings since we reprocess
    // everything. For incremental compaction, keep existing warnings and
    // accumulate new ones from the incremental events (#339).
    if watermark.is_none() {
        state.skew_warnings.clear();
        state.unsigned_event_warnings.clear();
    }

    let allowed_signers_path = cache_dir.join("trust").join("allowed_signers");

    // Apply each event
    for envelope in &all_events {
        detect_clock_skew(&mut state, envelope);
        check_unsigned(&mut state, envelope, &allowed_signers_path);
        apply(
            &mut state,
            envelope,
            &mut changed_issues,
            &mut changed_locks,
        );
    }

    // Update watermark to last processed event (written atomically with checkpoint)
    if let Some(last) = all_events.last() {
        state.watermark = Some(OrderingKey::from_envelope(last));
    }

    // Materialize changed entities to disk
    materialize(cache_dir, &state, &changed_issues, &changed_locks)?;

    // Run git-based clock skew detection
    let git_violations =
        crate::clock_skew::detect_git_skew_violations(cache_dir).unwrap_or_else(|e| {
            tracing::warn!("git skew detection failed, defaulting to no violations: {e}");
            Vec::new()
        });
    let git_skew_violations = git_violations.len();
    crate::clock_skew::write_skew_violations(cache_dir, &git_violations)?;

    let issues_materialized = changed_issues.len();
    let locks_materialized = changed_locks.len();
    let skew_warnings = state.skew_warnings.len();
    let unsigned_warnings = state.unsigned_event_warnings.len();

    state.compaction_lease = None;
    write_checkpoint(cache_dir, &state)?;
    // _lock_guard dropped here, removing the lock file

    Ok(Some(CompactionResult {
        events_processed,
        issues_materialized,
        locks_materialized,
        skew_warnings,
        unsigned_warnings,
        git_skew_violations,
    }))
}

/// Prune (flush) compacted events from an agent's log.
///
/// Removes events at or below the current watermark.
/// Returns the number of events pruned.
pub fn prune_events(cache_dir: &Path, agent_id: &str) -> Result<usize> {
    let watermark = match read_watermark(cache_dir)? {
        Some(wm) => wm,
        None => return Ok(0),
    };

    let log_path = cache_dir.join("agents").join(agent_id).join("events.log");
    if !log_path.exists() {
        return Ok(0);
    }

    let all_events = crate::events::read_events(&log_path)?;
    let before_count = all_events.len();
    let remaining: Vec<_> = all_events
        .into_iter()
        .filter(|e| OrderingKey::from_envelope(e) > watermark)
        .collect();

    let pruned = before_count - remaining.len();
    if pruned > 0 {
        let codec = crate::events::NdjsonCodec;
        let bytes = <crate::events::NdjsonCodec as crate::events::EventCodec>::encode_batch(
            &codec, &remaining,
        )?;
        crate::utils::atomic_write(&log_path, &bytes)
            .with_context(|| format!("Failed to write pruned log: {}", log_path.display()))?;
    }

    Ok(pruned)
}

// ── Internal functions ───────────────────────────────────────────────

/// Deterministic reduction: apply a single event to checkpoint state.
fn apply(
    state: &mut CheckpointState,
    envelope: &EventEnvelope,
    changed_issues: &mut HashSet<Uuid>,
    changed_locks: &mut HashSet<i64>,
) {
    match &envelope.event {
        Event::IssueCreated {
            uuid,
            title,
            description,
            priority,
            labels,
            parent_uuid,
            created_by,
        } => {
            // Skip if UUID already exists (idempotent)
            if state.issues.contains_key(uuid) {
                return;
            }
            let display_id = state.next_display_id;
            state.next_display_id += 1;
            state.display_id_map.insert(*uuid, display_id);
            state.issues.insert(
                *uuid,
                CompactIssue {
                    uuid: *uuid,
                    display_id: Some(display_id),
                    title: title.clone(),
                    description: description.clone(),
                    status: crate::models::IssueStatus::Open,
                    priority: priority.parse().unwrap_or(crate::models::Priority::Medium),
                    parent_uuid: *parent_uuid,
                    created_by: created_by.clone(),
                    created_at: envelope.timestamp,
                    updated_at: envelope.timestamp,
                    closed_at: None,
                    labels: labels.iter().cloned().collect(),
                    blockers: BTreeSet::new(),
                    related: BTreeSet::new(),
                    milestone_uuid: None,
                },
            );
            changed_issues.insert(*uuid);
        }

        Event::LockClaimed {
            issue_display_id,
            branch,
        } => {
            // First-claim-wins: reject if a *different* agent holds it.
            // When the *same* agent re-claims, the lock is refreshed with the
            // new branch and timestamp — this is the intended "reclaim"
            // behavior for agents that restart or switch branches.
            if let Some(existing) = state.locks.get(issue_display_id) {
                if existing.agent_id != envelope.agent_id {
                    return;
                }
            }
            state.locks.insert(
                *issue_display_id,
                LockEntry {
                    agent_id: envelope.agent_id.clone(),
                    branch: branch.clone(),
                    claimed_at: envelope.timestamp,
                },
            );
            changed_locks.insert(*issue_display_id);
        }

        Event::LockReleased { issue_display_id } => {
            // Only release if held by this agent
            if let Some(existing) = state.locks.get(issue_display_id) {
                if existing.agent_id == envelope.agent_id {
                    state.locks.remove(issue_display_id);
                    changed_locks.insert(*issue_display_id);
                }
            }
        }

        Event::IssueUpdated {
            uuid,
            title,
            description,
            priority,
        } => {
            if let Some(issue) = state.issues.get_mut(uuid) {
                // Last-writer-wins per field
                if let Some(t) = title {
                    issue.title = t.clone();
                }
                if let Some(d) = description {
                    issue.description = Some(d.clone());
                }
                if let Some(p) = priority {
                    if let Ok(parsed) = p.parse() {
                        issue.priority = parsed;
                    }
                }
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid);
            }
        }

        Event::StatusChanged {
            uuid,
            new_status,
            closed_at,
        } => {
            if let Some(issue) = state.issues.get_mut(uuid) {
                // Last-writer-wins (latest timestamp)
                issue.status = new_status.parse().unwrap_or(issue.status);
                issue.closed_at = *closed_at;
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid);
            }
        }

        Event::DependencyAdded {
            blocked_uuid,
            blocker_uuid,
        } => {
            if let Some(issue) = state.issues.get_mut(blocked_uuid) {
                issue.blockers.insert(*blocker_uuid);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*blocked_uuid);
            }
        }

        Event::DependencyRemoved {
            blocked_uuid,
            blocker_uuid,
        } => {
            if let Some(issue) = state.issues.get_mut(blocked_uuid) {
                issue.blockers.remove(blocker_uuid);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*blocked_uuid);
            }
        }

        Event::RelationAdded { uuid_a, uuid_b } => {
            if let Some(issue) = state.issues.get_mut(uuid_a) {
                issue.related.insert(*uuid_b);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid_a);
            }
            if let Some(issue) = state.issues.get_mut(uuid_b) {
                issue.related.insert(*uuid_a);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid_b);
            }
        }

        Event::RelationRemoved { uuid_a, uuid_b } => {
            if let Some(issue) = state.issues.get_mut(uuid_a) {
                issue.related.remove(uuid_b);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid_a);
            }
            if let Some(issue) = state.issues.get_mut(uuid_b) {
                issue.related.remove(uuid_a);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*uuid_b);
            }
        }

        Event::MilestoneAssigned {
            issue_uuid,
            milestone_uuid,
        } => {
            if let Some(issue) = state.issues.get_mut(issue_uuid) {
                issue.milestone_uuid = *milestone_uuid;
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*issue_uuid);
            }
        }

        Event::LabelAdded { issue_uuid, label } => {
            if let Some(issue) = state.issues.get_mut(issue_uuid) {
                issue.labels.insert(label.clone());
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*issue_uuid);
            }
        }

        Event::LabelRemoved { issue_uuid, label } => {
            if let Some(issue) = state.issues.get_mut(issue_uuid) {
                issue.labels.remove(label);
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*issue_uuid);
            }
        }

        Event::ParentChanged {
            issue_uuid,
            new_parent_uuid,
        } => {
            if let Some(issue) = state.issues.get_mut(issue_uuid) {
                issue.parent_uuid = *new_parent_uuid;
                issue.updated_at = envelope.timestamp;
                changed_issues.insert(*issue_uuid);
            }
        }
    }
}

/// Materialize checkpoint state to disk (issue.json + lock files).
///
/// Respects the hub layout version: writes V1 flat files or V2 directory
/// files accordingly. Cleans up stale V1 flat files when writing V2 (#428).
/// Writes `meta/version.json` if missing to prevent layout drift.
fn materialize(
    cache_dir: &Path,
    state: &CheckpointState,
    changed_issues: &HashSet<Uuid>,
    changed_locks: &HashSet<i64>,
) -> Result<()> {
    let issues_dir = cache_dir.join("issues");
    let locks_dir = cache_dir.join("locks");
    let meta_dir = cache_dir.join("meta");

    let layout_version = crate::issue_file::read_layout_version(&meta_dir)
        .unwrap_or(crate::issue_file::CURRENT_LAYOUT_VERSION);

    // Materialize changed issues
    for uuid in changed_issues {
        if let Some(compact) = state.issues.get(uuid) {
            let issue_file = compact_to_issue_file(compact);
            let content = serde_json::to_string_pretty(&issue_file)?;

            if layout_version >= 2 {
                let issue_dir = issues_dir.join(uuid.to_string());
                std::fs::create_dir_all(&issue_dir).with_context(|| {
                    format!("Failed to create issue dir: {}", issue_dir.display())
                })?;
                let path = issue_dir.join("issue.json");
                crate::utils::atomic_write(&path, content.as_bytes())?;

                // Clean up stale V1 flat file if it exists (#428)
                let v1_path = issues_dir.join(format!("{}.json", uuid));
                if v1_path.exists() {
                    let _ = std::fs::remove_file(&v1_path);
                }
            } else {
                let path = issues_dir.join(format!("{}.json", uuid));
                crate::utils::atomic_write(&path, content.as_bytes())?;
            }
        }
    }

    // Ensure version marker exists to prevent layout drift (#428)
    if !meta_dir.join("version.json").exists() {
        let _ = crate::issue_file::write_layout_version(&meta_dir, layout_version);
    }

    // Materialize changed locks
    std::fs::create_dir_all(&locks_dir)?;
    for display_id in changed_locks {
        let lock_path = locks_dir.join(format!("{}.json", display_id));
        if let Some(lock_entry) = state.locks.get(display_id) {
            let lock_file = LockFileV2 {
                issue_id: *display_id,
                agent_id: lock_entry.agent_id.clone(),
                branch: lock_entry.branch.clone(),
                claimed_at: lock_entry.claimed_at,
                signed_by: None,
            };
            let content = serde_json::to_string_pretty(&lock_file)?;
            crate::utils::atomic_write(&lock_path, content.as_bytes())?;
        } else {
            // Lock was released — remove the file
            if lock_path.exists() {
                std::fs::remove_file(&lock_path).with_context(|| {
                    format!("Failed to remove lock file: {}", lock_path.display())
                })?;
            }
        }
    }

    Ok(())
}

/// Convert a CompactIssue to an IssueFile for materialization.
///
/// Delegates to the `From<&CompactIssue>` impl on `IssueFile`.
fn compact_to_issue_file(compact: &CompactIssue) -> IssueFile {
    IssueFile::from(compact)
}

/// Detect clock skew: flag events whose timestamp is in the future relative
/// to the current wall-clock time by more than the threshold.
///
/// Only future-dated events indicate a skewed clock. Past events are expected
/// during incremental compaction (events may have been written hours or days
/// ago). Comparing against `now()` for past events produced false positives
/// (#330).
fn detect_clock_skew(state: &mut CheckpointState, envelope: &EventEnvelope) {
    let now = Utc::now();
    let future_skew = (envelope.timestamp - now).num_seconds();
    if future_skew > SKEW_THRESHOLD_SECS {
        state.skew_warnings.push(SkewWarning {
            agent_id: envelope.agent_id.clone(),
            skew_seconds: future_skew,
            event_timestamp: envelope.timestamp,
        });
    }
}

/// Check for unsigned events and verify signatures when possible.
fn check_unsigned(
    state: &mut CheckpointState,
    envelope: &EventEnvelope,
    allowed_signers_path: &Path,
) {
    if envelope.signed_by.is_none() || envelope.signature.is_none() {
        state.unsigned_event_warnings.push(UnsignedEventWarning {
            agent_id: envelope.agent_id.clone(),
            agent_seq: envelope.agent_seq,
            timestamp: envelope.timestamp,
        });
    } else if allowed_signers_path.exists() {
        // Verify the signature against the trust store
        if let Ok(false) = crate::events::verify_event_signature(envelope, allowed_signers_path) {
            state.unsigned_event_warnings.push(UnsignedEventWarning {
                agent_id: envelope.agent_id.clone(),
                agent_seq: envelope.agent_seq,
                timestamp: envelope.timestamp,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::CompactionLease;
    use crate::events::{append_event, Event, EventEnvelope};
    use chrono::Duration;

    /// Try to acquire the compaction lease. Returns true if acquired.
    /// (Test-only helper — production code uses CompactionLockGuard.)
    fn try_acquire_lease(state: &mut CheckpointState, agent_id: &str) -> bool {
        let now = Utc::now();
        if let Some(ref lease) = state.compaction_lease {
            if lease.agent_id == agent_id {
                // We already hold it — refresh
            } else if lease.expires_at > now {
                // Another agent holds an unexpired lease — but check if the
                // holding process is still alive. If the PID is dead, treat
                // the lease as stale regardless of expiry time.
                let holder_dead = lease.pid.map(|pid| !is_pid_alive(pid)).unwrap_or(false);
                if !holder_dead {
                    return false;
                }
                // PID is dead — fall through to take the stale lease
            }
            // Expired lease from another agent — take it
        }

        state.compaction_lease = Some(CompactionLease {
            agent_id: agent_id.to_string(),
            acquired_at: now,
            expires_at: now + Duration::seconds(LEASE_DURATION_SECS),
            pid: Some(std::process::id()),
        });
        true
    }

    /// Check if a process with the given PID is still alive.
    #[cfg(windows)]
    fn is_pid_alive(pid: u32) -> bool {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|output| {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }

    /// Check if a process with the given PID is still alive.
    #[cfg(not(windows))]
    fn is_pid_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Release the compaction lease.
    /// (Test-only helper — production code uses CompactionLockGuard.)
    fn release_lease(state: &mut CheckpointState) {
        state.compaction_lease = None;
    }

    fn make_envelope(agent_id: &str, seq: u64, event: Event) -> EventEnvelope {
        EventEnvelope {
            agent_id: agent_id.to_string(),
            agent_seq: seq,
            timestamp: Utc::now(),
            event,
            signed_by: None,
            signature: None,
        }
    }

    fn setup_cache(dir: &Path) {
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::create_dir_all(dir.join("issues")).unwrap();
        std::fs::create_dir_all(dir.join("locks")).unwrap();
        std::fs::create_dir_all(dir.join("checkpoint")).unwrap();
        // Write V2 layout marker — matches real hub initialization
        crate::issue_file::write_layout_version(
            &dir.join("meta"),
            crate::issue_file::CURRENT_LAYOUT_VERSION,
        )
        .unwrap();
    }

    #[test]
    fn test_compact_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let result = compact(cache_dir, "agent-1", false).unwrap().unwrap();
        assert_eq!(result.events_processed, 0);
        assert_eq!(result.issues_materialized, 0);
    }

    #[test]
    fn test_compact_issue_created() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log_path = cache_dir.join("agents/agent-1/events.log");
        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Test issue".to_string(),
                description: Some("A description".to_string()),
                priority: "high".to_string(),
                labels: vec!["bug".to_string()],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        append_event(&log_path, &env).unwrap();

        let result = compact(cache_dir, "agent-1", false).unwrap().unwrap();
        assert_eq!(result.events_processed, 1);
        assert_eq!(result.issues_materialized, 1);

        // Verify materialized file
        let issue_path = cache_dir
            .join("issues")
            .join(uuid.to_string())
            .join("issue.json");
        assert!(issue_path.exists());
        let issue: IssueFile =
            serde_json::from_str(&std::fs::read_to_string(&issue_path).unwrap()).unwrap();
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.display_id, Some(1));
        assert_eq!(issue.priority, crate::models::Priority::High);
        assert_eq!(issue.labels, vec!["bug".to_string()]);
    }

    #[test]
    fn test_display_id_stability() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid1 = Uuid::new_v4();
        let uuid2 = Uuid::new_v4();
        let log_path = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: uuid1,
                title: "First".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::IssueCreated {
                uuid: uuid2,
                title: "Second".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );

        append_event(&log_path, &e1).unwrap();
        append_event(&log_path, &e2).unwrap();

        // First compaction
        compact(cache_dir, "agent-1", true).unwrap();

        // Delete watermark to force full re-compaction
        let wm_path = cache_dir.join("checkpoint/watermark.json");
        if wm_path.exists() {
            std::fs::remove_file(&wm_path).unwrap();
        }

        // Second compaction — IDs should be the same
        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.display_id_map[&uuid1], 1);
        assert_eq!(state.display_id_map[&uuid2], 2);
    }

    #[test]
    fn test_idempotent_issue_created() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log_path = cache_dir.join("agents/agent-1/events.log");

        // Write the same IssueCreated twice (duplicate)
        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Issue".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(1);
        let e2 = make_envelope(
            "agent-1",
            2,
            Event::IssueCreated {
                uuid,
                title: "Issue duplicate".to_string(),
                description: None,
                priority: "high".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );

        append_event(&log_path, &e1).unwrap();
        append_event(&log_path, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.issues.len(), 1);
        assert_eq!(state.issues[&uuid].title, "Issue"); // First one wins
        assert_eq!(state.next_display_id, 2); // Only one ID consumed
    }

    #[test]
    fn test_lock_contention_first_wins() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log1 = cache_dir.join("agents/agent-1/events.log");
        let log2 = cache_dir.join("agents/agent-2/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/a".to_string()),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let mut e2 = make_envelope(
            "agent-2",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/b".to_string()),
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(1);

        append_event(&log1, &e1).unwrap();
        append_event(&log2, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.locks[&1].agent_id, "agent-1"); // First in order wins
    }

    #[test]
    fn test_lock_release() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.locks.is_empty());
        // Lock file should not exist
        assert!(!cache_dir.join("locks/1.json").exists());
    }

    #[test]
    fn test_issue_update_last_writer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log1 = cache_dir.join("agents/agent-1/events.log");
        let log2 = cache_dir.join("agents/agent-2/events.log");

        let mut e_create = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Original".to_string(),
                description: None,
                priority: "low".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e_create.timestamp = Utc::now() - Duration::seconds(10);

        let mut e_update1 = make_envelope(
            "agent-1",
            2,
            Event::IssueUpdated {
                uuid,
                title: Some("Agent 1 title".to_string()),
                description: None,
                priority: None,
            },
        );
        e_update1.timestamp = Utc::now() - Duration::seconds(5);

        let mut e_update2 = make_envelope(
            "agent-2",
            1,
            Event::IssueUpdated {
                uuid,
                title: Some("Agent 2 title".to_string()),
                description: Some("Agent 2 desc".to_string()),
                priority: None,
            },
        );
        e_update2.timestamp = Utc::now() - Duration::seconds(3);

        append_event(&log1, &e_create).unwrap();
        append_event(&log1, &e_update1).unwrap();
        append_event(&log2, &e_update2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let issue = &state.issues[&uuid];
        assert_eq!(issue.title, "Agent 2 title"); // Later timestamp wins
        assert_eq!(issue.description.as_deref(), Some("Agent 2 desc"));
    }

    #[test]
    fn test_label_add_remove_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e_create = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Test".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e_create.timestamp = Utc::now() - Duration::seconds(10);

        let mut e_add1 = make_envelope(
            "agent-1",
            2,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "bug".to_string(),
            },
        );
        e_add1.timestamp = Utc::now() - Duration::seconds(8);

        let mut e_add2 = make_envelope(
            "agent-1",
            3,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "bug".to_string(),
            },
        );
        e_add2.timestamp = Utc::now() - Duration::seconds(6);

        let e_remove = make_envelope(
            "agent-1",
            4,
            Event::LabelRemoved {
                issue_uuid: uuid,
                label: "bug".to_string(),
            },
        );

        append_event(&log, &e_create).unwrap();
        append_event(&log, &e_add1).unwrap();
        append_event(&log, &e_add2).unwrap();
        append_event(&log, &e_remove).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.issues[&uuid].labels.is_empty());
    }

    #[test]
    fn test_dependency_add_remove() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let blocked = Uuid::new_v4();
        let blocker = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: blocked,
                title: "Blocked".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::DependencyAdded {
                blocked_uuid: blocked,
                blocker_uuid: blocker,
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(5);

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.issues[&blocked].blockers.contains(&blocker));
    }

    #[test]
    fn test_relation_bidirectional() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid_a = Uuid::new_v4();
        let uuid_b = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: uuid_a,
                title: "A".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::IssueCreated {
                uuid: uuid_b,
                title: "B".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(9);

        let e3 = make_envelope("agent-1", 3, Event::RelationAdded { uuid_a, uuid_b });

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();
        append_event(&log, &e3).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.issues[&uuid_a].related.contains(&uuid_b));
        assert!(state.issues[&uuid_b].related.contains(&uuid_a));
    }

    #[test]
    fn test_lease_acquire_release() {
        let mut state = CheckpointState::default();

        assert!(try_acquire_lease(&mut state, "agent-1"));
        assert!(state.compaction_lease.is_some());
        assert_eq!(state.compaction_lease.as_ref().unwrap().agent_id, "agent-1");

        // Same agent can re-acquire
        assert!(try_acquire_lease(&mut state, "agent-1"));

        // Different agent cannot while lease is active
        assert!(!try_acquire_lease(&mut state, "agent-2"));

        release_lease(&mut state);
        assert!(state.compaction_lease.is_none());

        // Now agent-2 can acquire
        assert!(try_acquire_lease(&mut state, "agent-2"));
    }

    #[test]
    fn test_lease_expiry() {
        let mut state = CheckpointState {
            compaction_lease: Some(CompactionLease {
                agent_id: "agent-1".to_string(),
                acquired_at: Utc::now() - Duration::seconds(60),
                expires_at: Utc::now() - Duration::seconds(30),
                pid: None,
            }),
            ..Default::default()
        };

        // Different agent can take expired lease
        assert!(try_acquire_lease(&mut state, "agent-2"));
        assert_eq!(state.compaction_lease.as_ref().unwrap().agent_id, "agent-2");
    }

    #[test]
    fn test_lease_stale_by_dead_pid() {
        // Use a PID that is almost certainly not running (max u32 - 1)
        let dead_pid = u32::MAX - 1;

        let mut state = CheckpointState {
            compaction_lease: Some(CompactionLease {
                agent_id: "agent-1".to_string(),
                acquired_at: Utc::now(),
                expires_at: Utc::now() + Duration::seconds(300), // far from expired
                pid: Some(dead_pid),
            }),
            ..Default::default()
        };

        // Another agent can take the lease because the PID is dead
        assert!(try_acquire_lease(&mut state, "agent-2"));
        assert_eq!(state.compaction_lease.as_ref().unwrap().agent_id, "agent-2");
    }

    #[test]
    fn test_lease_not_stale_when_pid_alive() {
        // Use current process PID — definitely alive
        let mut state = CheckpointState {
            compaction_lease: Some(CompactionLease {
                agent_id: "agent-1".to_string(),
                acquired_at: Utc::now(),
                expires_at: Utc::now() + Duration::seconds(300),
                pid: Some(std::process::id()),
            }),
            ..Default::default()
        };

        // Another agent cannot take the lease because the PID is alive
        assert!(!try_acquire_lease(&mut state, "agent-2"));
    }

    #[test]
    fn test_lease_no_pid_falls_back_to_expiry() {
        // Lease without PID (backward compat) — uses expiry-based check only
        let mut state = CheckpointState {
            compaction_lease: Some(CompactionLease {
                agent_id: "agent-1".to_string(),
                acquired_at: Utc::now(),
                expires_at: Utc::now() + Duration::seconds(300),
                pid: None,
            }),
            ..Default::default()
        };

        // Cannot take because lease is unexpired and no PID to check
        assert!(!try_acquire_lease(&mut state, "agent-2"));
    }

    #[test]
    fn test_clock_skew_detection() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Skewed".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        // Set timestamp far in the future to trigger skew warning
        env.timestamp = Utc::now() + Duration::seconds(120);

        append_event(&log, &env).unwrap();

        let result = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert!(result.skew_warnings > 0);
    }

    #[test]
    fn test_unsigned_event_warning() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Unsigned".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        // env has signed_by = None, signature = None

        append_event(&log, &env).unwrap();

        let result = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert!(result.unsigned_warnings > 0);
    }

    #[test]
    fn test_prune_events() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Prunable".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "bug".to_string(),
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        // Compact to create watermark
        compact(cache_dir, "agent-1", true).unwrap();

        // Prune should remove events at/below watermark
        let pruned = prune_events(cache_dir, "agent-1").unwrap();
        assert_eq!(pruned, 2);

        // Log should be empty now
        let remaining = crate::events::read_events(&log).unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_deterministic_reduction_order() {
        // Two agents emit events; regardless of file read order, the state
        // should be the same because we sort by OrderingKey.
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log1 = cache_dir.join("agents/agent-1/events.log");
        let log2 = cache_dir.join("agents/agent-2/events.log");

        let mut e_create = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Test".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e_create.timestamp = Utc::now() - Duration::seconds(20);

        let mut e_label1 = make_envelope(
            "agent-2",
            1,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "feature".to_string(),
            },
        );
        e_label1.timestamp = Utc::now() - Duration::seconds(10);

        let mut e_label2 = make_envelope(
            "agent-1",
            2,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "urgent".to_string(),
            },
        );
        e_label2.timestamp = Utc::now() - Duration::seconds(5);

        append_event(&log1, &e_create).unwrap();
        append_event(&log2, &e_label1).unwrap();
        append_event(&log1, &e_label2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let issue = &state.issues[&uuid];
        assert!(issue.labels.contains("feature"));
        assert!(issue.labels.contains("urgent"));
        assert_eq!(issue.labels.len(), 2);
    }

    #[test]
    fn test_status_changed() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "To close".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let closed_at = Utc::now();
        let e2 = make_envelope(
            "agent-1",
            2,
            Event::StatusChanged {
                uuid,
                new_status: "closed".to_string(),
                closed_at: Some(closed_at),
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(
            state.issues[&uuid].status,
            crate::models::IssueStatus::Closed
        );
        assert!(state.issues[&uuid].closed_at.is_some());
    }

    #[test]
    fn test_compact_respects_file_lock() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        // Set an active lease by another agent (use current PID so it looks alive)
        let lock_path = cache_dir.join("checkpoint").join(COMPACTION_LOCK_FILE);
        let content = serde_json::json!({
            "agent_id": "agent-2",
            "acquired_at": Utc::now().to_rfc3339(),
            "pid": std::process::id(),
        });
        std::fs::write(&lock_path, content.to_string()).unwrap();

        // Try to compact as agent-1 without force — should fail
        let result = compact(cache_dir, "agent-1", false).unwrap();
        assert!(result.is_none());

        // Lock file should still exist
        assert!(lock_path.exists());
    }

    #[test]
    fn test_compact_force_overrides_stale_lock() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        // Create a stale lock file (acquired long ago)
        let lock_path = cache_dir.join("checkpoint").join(COMPACTION_LOCK_FILE);
        let stale_time = Utc::now() - Duration::seconds(LEASE_DURATION_SECS * 3);
        let content = serde_json::json!({
            "agent_id": "agent-2",
            "acquired_at": stale_time.to_rfc3339(),
            "pid": 99999,
        });
        std::fs::write(&lock_path, content.to_string()).unwrap();

        // Force should override the stale lock
        let result = compact(cache_dir, "agent-1", true).unwrap();
        assert!(result.is_some());

        // Lock file should be cleaned up after compaction
        assert!(!lock_path.exists());
    }

    #[test]
    fn test_file_lock_guard_acquire_release() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let lock_path = cache_dir.join("checkpoint").join(COMPACTION_LOCK_FILE);

        // Acquire lock
        {
            let guard = CompactionLockGuard::try_acquire(cache_dir, "agent-1", false)
                .unwrap()
                .unwrap();
            assert!(lock_path.exists());

            // Second acquire by different agent should fail
            let result = CompactionLockGuard::try_acquire(cache_dir, "agent-2", false).unwrap();
            assert!(result.is_none());

            drop(guard);
        }

        // After drop, lock file should be removed
        assert!(!lock_path.exists());

        // Now agent-2 can acquire
        let guard = CompactionLockGuard::try_acquire(cache_dir, "agent-2", false)
            .unwrap()
            .unwrap();
        assert!(lock_path.exists());
        drop(guard);
    }

    #[test]
    fn test_file_lock_same_agent_force() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        // Create a lock file owned by agent-1
        let lock_path = cache_dir.join("checkpoint").join(COMPACTION_LOCK_FILE);
        let content = serde_json::json!({
            "agent_id": "agent-1",
            "acquired_at": Utc::now().to_rfc3339(),
            "pid": 99999,
        });
        std::fs::write(&lock_path, content.to_string()).unwrap();

        // Same agent with force should be able to re-acquire
        let guard = CompactionLockGuard::try_acquire(cache_dir, "agent-1", true)
            .unwrap()
            .unwrap();
        assert!(lock_path.exists());
        drop(guard);
    }

    #[test]
    fn test_stale_lock_auto_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        // Create a stale lock file (well past 2x lease duration)
        let lock_path = cache_dir.join("checkpoint").join(COMPACTION_LOCK_FILE);
        let stale_time = Utc::now() - Duration::seconds(LEASE_DURATION_SECS * 3);
        let content = serde_json::json!({
            "agent_id": "agent-old",
            "acquired_at": stale_time.to_rfc3339(),
            "pid": 99999,
        });
        std::fs::write(&lock_path, content.to_string()).unwrap();

        // Even without force, stale locks should be auto-cleared
        let guard = CompactionLockGuard::try_acquire(cache_dir, "agent-new", false)
            .unwrap()
            .unwrap();
        assert!(lock_path.exists());
        drop(guard);
    }

    #[test]
    fn test_materialized_issue_file_format() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Materialized".to_string(),
                description: Some("desc".to_string()),
                priority: "critical".to_string(),
                labels: vec!["bug".to_string(), "urgent".to_string()],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        append_event(&log, &env).unwrap();
        compact(cache_dir, "agent-1", true).unwrap();

        // Read back the materialized issue.json
        let path = cache_dir
            .join("issues")
            .join(uuid.to_string())
            .join("issue.json");
        let content = std::fs::read_to_string(&path).unwrap();
        let issue: IssueFile = serde_json::from_str(&content).unwrap();

        assert_eq!(issue.uuid, uuid);
        assert_eq!(issue.display_id, Some(1));
        assert_eq!(issue.title, "Materialized");
        assert_eq!(issue.description.as_deref(), Some("desc"));
        assert_eq!(issue.status, crate::models::IssueStatus::Open);
        assert_eq!(issue.priority, crate::models::Priority::Critical);
        assert!(issue.comments.is_empty());
        assert!(issue.time_entries.is_empty());
    }

    #[test]
    fn test_parent_changed() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: child,
                title: "Child".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::ParentChanged {
                issue_uuid: child,
                new_parent_uuid: Some(parent),
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.issues[&child].parent_uuid, Some(parent));
    }

    #[test]
    fn test_milestone_assigned() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let issue_uuid = Uuid::new_v4();
        let milestone_uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: issue_uuid,
                title: "Milestone test".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::MilestoneAssigned {
                issue_uuid,
                milestone_uuid: Some(milestone_uuid),
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(
            state.issues[&issue_uuid].milestone_uuid,
            Some(milestone_uuid)
        );
    }

    // ── Lock claim / release / contention tests ─────────────────────

    #[test]
    fn test_lock_release_by_non_holder_ignored() {
        // Agent B cannot release a lock held by Agent A
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");

        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(3);

        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        // Lock should still be held by agent-a since agent-b cannot release it
        assert_eq!(state.locks[&1].agent_id, "agent-a");
        assert!(cache_dir.join("locks/1.json").exists());
    }

    #[test]
    fn test_lock_claim_release_reclaim_cycle() {
        // Agent claims, releases, then reclaims the same lock
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/first".to_string()),
            },
        );
        e1.timestamp = now - Duration::seconds(3);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e2.timestamp = now - Duration::seconds(2);

        let mut e3 = make_envelope(
            "agent-1",
            3,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/second".to_string()),
            },
        );
        e3.timestamp = now - Duration::seconds(1);

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();
        append_event(&log, &e3).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.agent_id, "agent-1");
        assert_eq!(lock.branch, Some("feature/second".to_string()));
    }

    #[test]
    fn test_three_way_lock_contention() {
        // Three agents all claim the same lock — earliest timestamp wins
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let now = Utc::now();

        for (agent, seq_offset) in &[("agent-a", 3), ("agent-b", 2), ("agent-c", 1)] {
            let log = cache_dir.join(format!("agents/{}/events.log", agent));
            let mut e = make_envelope(
                agent,
                1,
                Event::LockClaimed {
                    issue_display_id: 1,
                    branch: None,
                },
            );
            e.timestamp = now - Duration::seconds(*seq_offset);
            append_event(&log, &e).unwrap();
        }

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        // agent-c has the earliest timestamp (now - 1), but agent-a has (now - 3)
        assert_eq!(state.locks[&1].agent_id, "agent-a");
    }

    #[test]
    fn test_lock_contention_timestamp_tiebreaker_uses_agent_id() {
        // When timestamps are identical, agent_id string ordering breaks the tie
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let same_time = Utc::now() - Duration::seconds(5);

        let log_a = cache_dir.join("agents/agent-alpha/events.log");
        let log_b = cache_dir.join("agents/agent-beta/events.log");

        let mut e1 = make_envelope(
            "agent-alpha",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = same_time;

        let mut e2 = make_envelope(
            "agent-beta",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e2.timestamp = same_time;

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();

        compact(cache_dir, "agent-alpha", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        // "agent-alpha" < "agent-beta" lexicographically, so alpha wins
        assert_eq!(state.locks[&1].agent_id, "agent-alpha");
    }

    #[test]
    fn test_concurrent_claims_on_different_issues() {
        // Two agents claim different issues — both should succeed
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");

        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/a".to_string()),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 2,
                branch: Some("feature/b".to_string()),
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();

        let result = compact(cache_dir, "agent-a", true).unwrap().unwrap();
        assert_eq!(result.locks_materialized, 2);

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.locks.len(), 2);
        assert_eq!(state.locks[&1].agent_id, "agent-a");
        assert_eq!(state.locks[&2].agent_id, "agent-b");

        // Both lock files should exist
        assert!(cache_dir.join("locks/1.json").exists());
        assert!(cache_dir.join("locks/2.json").exists());
    }

    #[test]
    fn test_lock_branch_metadata_preserved_through_contention() {
        // Winner's branch metadata should be preserved, loser's discarded
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");

        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/winner-branch".to_string()),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/loser-branch".to_string()),
            },
        );
        e2.timestamp = Utc::now() - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.agent_id, "agent-a");
        assert_eq!(lock.branch, Some("feature/winner-branch".to_string()));

        // Verify materialized lock file also has correct branch
        let lock_content = std::fs::read_to_string(cache_dir.join("locks/1.json")).unwrap();
        let lock_file: LockFileV2 = serde_json::from_str(&lock_content).unwrap();
        assert_eq!(lock_file.branch, Some("feature/winner-branch".to_string()));
    }

    #[test]
    fn test_lock_release_removes_materialized_file() {
        // After claim + release, the lock file should be removed
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 5,
                branch: None,
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(2);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 5,
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        // Lock should be absent from checkpoint and disk
        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.locks.is_empty());
        assert!(!cache_dir.join("locks/5.json").exists());
    }

    #[test]
    fn test_lock_release_of_nonexistent_is_noop() {
        // Releasing a lock that was never claimed should be harmless
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");

        let e = make_envelope(
            "agent-1",
            1,
            Event::LockReleased {
                issue_display_id: 99,
            },
        );
        append_event(&log, &e).unwrap();

        let result = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert_eq!(result.events_processed, 1);
        assert_eq!(result.locks_materialized, 0);

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.locks.is_empty());
    }

    #[test]
    fn test_incremental_compaction_with_lock_changes() {
        // First compaction claims, second (incremental) releases
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        // First round: claim
        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/x".to_string()),
            },
        );
        e1.timestamp = now - Duration::seconds(3);
        append_event(&log, &e1).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        // Verify lock is held
        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.locks[&1].agent_id, "agent-1");
        assert!(cache_dir.join("locks/1.json").exists());

        // Second round: release (incremental — watermark set from first round)
        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e2.timestamp = now - Duration::seconds(1);
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        // Lock should be gone
        let state = read_checkpoint(cache_dir).unwrap();
        assert!(state.locks.is_empty());
        assert!(!cache_dir.join("locks/1.json").exists());
    }

    #[test]
    fn test_contention_loser_then_winner_releases() {
        // Agent A wins, Agent B loses. Then Agent A releases.
        // After compaction, the lock should be gone entirely.
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");
        let now = Utc::now();

        // Agent A claims first
        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = now - Duration::seconds(4);

        // Agent B claims second (will lose)
        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e2.timestamp = now - Duration::seconds(3);

        // Agent A releases
        let mut e3 = make_envelope(
            "agent-a",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e3.timestamp = now - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();
        append_event(&log_a, &e3).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        // Lock should be gone — agent-a won and then released
        assert!(state.locks.is_empty());
        assert!(!cache_dir.join("locks/1.json").exists());
    }

    #[test]
    fn test_same_agent_reclaim_after_contention_loss() {
        // Agent A claims at t=1, Agent B claims at t=2 (loses to A).
        // Agent A releases at t=3. Agent B reclaims at t=4 (now unopposed).
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = now - Duration::seconds(4);

        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e2.timestamp = now - Duration::seconds(3);

        let mut e3 = make_envelope(
            "agent-a",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e3.timestamp = now - Duration::seconds(2);

        let mut e4 = make_envelope(
            "agent-b",
            2,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/retry".to_string()),
            },
        );
        e4.timestamp = now - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();
        append_event(&log_a, &e3).unwrap();
        append_event(&log_b, &e4).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.agent_id, "agent-b");
        assert_eq!(lock.branch, Some("feature/retry".to_string()));
    }

    #[test]
    fn test_multiple_issues_independent_contention() {
        // Agent A and B each claim issue 1 and issue 2
        // Agent A wins issue 1 (earlier), Agent B wins issue 2 (earlier)
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log_a = cache_dir.join("agents/agent-a/events.log");
        let log_b = cache_dir.join("agents/agent-b/events.log");
        let now = Utc::now();

        // Issue 1: Agent A claims first
        let mut e1 = make_envelope(
            "agent-a",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = now - Duration::seconds(4);

        // Issue 2: Agent B claims first
        let mut e2 = make_envelope(
            "agent-b",
            1,
            Event::LockClaimed {
                issue_display_id: 2,
                branch: None,
            },
        );
        e2.timestamp = now - Duration::seconds(3);

        // Issue 1: Agent B claims second (loses)
        let mut e3 = make_envelope(
            "agent-b",
            2,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e3.timestamp = now - Duration::seconds(2);

        // Issue 2: Agent A claims second (loses)
        let mut e4 = make_envelope(
            "agent-a",
            2,
            Event::LockClaimed {
                issue_display_id: 2,
                branch: None,
            },
        );
        e4.timestamp = now - Duration::seconds(1);

        append_event(&log_a, &e1).unwrap();
        append_event(&log_b, &e2).unwrap();
        append_event(&log_b, &e3).unwrap();
        append_event(&log_a, &e4).unwrap();

        compact(cache_dir, "agent-a", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(state.locks.len(), 2);
        assert_eq!(state.locks[&1].agent_id, "agent-a");
        assert_eq!(state.locks[&2].agent_id, "agent-b");
    }

    #[test]
    fn test_prune_preserves_unpruned_lock_events() {
        // Claim at seq=1, release at seq=2. Watermark covers seq=1 only.
        // After prune, seq=2 (release) should remain.
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e1.timestamp = now - Duration::seconds(3);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 1,
            },
        );
        e2.timestamp = now - Duration::seconds(1);

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        // Set watermark to cover only the first event
        let watermark = OrderingKey {
            timestamp: now - Duration::seconds(2),
            agent_id: "agent-1".to_string(),
            agent_seq: 1,
        };
        crate::checkpoint::write_watermark(cache_dir, &watermark).unwrap();

        let pruned = prune_events(cache_dir, "agent-1").unwrap();
        assert_eq!(pruned, 1);

        // Remaining event should be the release
        let remaining = crate::events::read_events(&log).unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(matches!(
            remaining[0].event,
            Event::LockReleased {
                issue_display_id: 1
            }
        ));
    }

    #[test]
    fn test_lock_claimed_at_timestamp_matches_event() {
        // The claimed_at field in the lock entry should match the event timestamp
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let claim_time = Utc::now() - Duration::seconds(10);

        let mut e = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 1,
                branch: None,
            },
        );
        e.timestamp = claim_time;
        append_event(&log, &e).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let lock = state.locks.get(&1).unwrap();
        assert_eq!(lock.claimed_at, claim_time);

        // Check materialized file too
        let lock_content = std::fs::read_to_string(cache_dir.join("locks/1.json")).unwrap();
        let lock_file: LockFileV2 = serde_json::from_str(&lock_content).unwrap();
        assert_eq!(lock_file.claimed_at, claim_time);
    }

    // ── Coverage gap tests ──────────────────────────────────────────────

    #[test]
    fn test_no_clock_skew_within_threshold() {
        // Events with timestamps within the threshold should NOT produce skew warnings
        let mut state = CheckpointState::default();
        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Recent".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        // timestamp is Utc::now(), well within the 60s threshold
        detect_clock_skew(&mut state, &env);
        assert!(state.skew_warnings.is_empty());
    }

    #[test]
    fn test_check_unsigned_with_signed_event_no_trust_file() {
        // A signed event when there is no allowed_signers file should NOT produce a warning
        let mut state = CheckpointState::default();
        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Signed".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        env.signed_by = Some("agent-1".to_string());
        env.signature = Some("fake-signature".to_string());

        let nonexistent = PathBuf::from("/tmp/nonexistent_trust_dir/allowed_signers");
        check_unsigned(&mut state, &env, &nonexistent);
        assert!(
            state.unsigned_event_warnings.is_empty(),
            "Signed event without trust file should not warn"
        );
    }

    #[test]
    fn test_apply_events_on_nonexistent_issue_are_noop() {
        // All mutation events referencing unknown UUIDs should be no-ops
        let mut state = CheckpointState::default();
        let unknown = Uuid::new_v4();
        let mut changed_issues = HashSet::new();
        let mut changed_locks = HashSet::new();

        // IssueUpdated on nonexistent
        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueUpdated {
                uuid: unknown,
                title: Some("Ghost".to_string()),
                description: None,
                priority: None,
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());
        assert!(changed_issues.is_empty());

        // StatusChanged on nonexistent
        let env = make_envelope(
            "agent-1",
            2,
            Event::StatusChanged {
                uuid: unknown,
                new_status: "closed".to_string(),
                closed_at: Some(Utc::now()),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // DependencyAdded on nonexistent
        let env = make_envelope(
            "agent-1",
            3,
            Event::DependencyAdded {
                blocked_uuid: unknown,
                blocker_uuid: Uuid::new_v4(),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // DependencyRemoved on nonexistent
        let env = make_envelope(
            "agent-1",
            4,
            Event::DependencyRemoved {
                blocked_uuid: unknown,
                blocker_uuid: Uuid::new_v4(),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // RelationAdded on nonexistent (both sides)
        let env = make_envelope(
            "agent-1",
            5,
            Event::RelationAdded {
                uuid_a: unknown,
                uuid_b: Uuid::new_v4(),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // RelationRemoved on nonexistent
        let env = make_envelope(
            "agent-1",
            6,
            Event::RelationRemoved {
                uuid_a: unknown,
                uuid_b: Uuid::new_v4(),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // MilestoneAssigned on nonexistent
        let env = make_envelope(
            "agent-1",
            7,
            Event::MilestoneAssigned {
                issue_uuid: unknown,
                milestone_uuid: Some(Uuid::new_v4()),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // LabelAdded on nonexistent
        let env = make_envelope(
            "agent-1",
            8,
            Event::LabelAdded {
                issue_uuid: unknown,
                label: "ghost-label".to_string(),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // LabelRemoved on nonexistent
        let env = make_envelope(
            "agent-1",
            9,
            Event::LabelRemoved {
                issue_uuid: unknown,
                label: "ghost-label".to_string(),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // ParentChanged on nonexistent
        let env = make_envelope(
            "agent-1",
            10,
            Event::ParentChanged {
                issue_uuid: unknown,
                new_parent_uuid: Some(Uuid::new_v4()),
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.issues.is_empty());

        // No issues or locks should have been marked as changed
        assert!(changed_issues.is_empty());
        assert!(changed_locks.is_empty());
    }

    #[test]
    fn test_dependency_removed() {
        // Test the DependencyRemoved branch through full compaction
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let blocked = Uuid::new_v4();
        let blocker = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: blocked,
                title: "Blocked".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = now - Duration::seconds(10);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::DependencyAdded {
                blocked_uuid: blocked,
                blocker_uuid: blocker,
            },
        );
        e2.timestamp = now - Duration::seconds(5);

        let e3 = make_envelope(
            "agent-1",
            3,
            Event::DependencyRemoved {
                blocked_uuid: blocked,
                blocker_uuid: blocker,
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();
        append_event(&log, &e3).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(
            state.issues[&blocked].blockers.is_empty(),
            "Dependency should be removed after DependencyRemoved event"
        );
    }

    #[test]
    fn test_relation_removed() {
        // Test the RelationRemoved branch through full compaction
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid_a = Uuid::new_v4();
        let uuid_b = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: uuid_a,
                title: "A".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = now - Duration::seconds(10);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::IssueCreated {
                uuid: uuid_b,
                title: "B".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e2.timestamp = now - Duration::seconds(9);

        let mut e3 = make_envelope("agent-1", 3, Event::RelationAdded { uuid_a, uuid_b });
        e3.timestamp = now - Duration::seconds(5);

        let e4 = make_envelope("agent-1", 4, Event::RelationRemoved { uuid_a, uuid_b });

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();
        append_event(&log, &e3).unwrap();
        append_event(&log, &e4).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert!(
            state.issues[&uuid_a].related.is_empty(),
            "Relation should be removed from A"
        );
        assert!(
            state.issues[&uuid_b].related.is_empty(),
            "Relation should be removed from B"
        );
    }

    #[test]
    fn test_issue_update_description_and_priority() {
        // Cover the description and priority update branches in IssueUpdated
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let mut e_create = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Original".to_string(),
                description: None,
                priority: "low".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e_create.timestamp = Utc::now() - Duration::seconds(10);

        let e_update = make_envelope(
            "agent-1",
            2,
            Event::IssueUpdated {
                uuid,
                title: None,
                description: Some("New description".to_string()),
                priority: Some("critical".to_string()),
            },
        );

        append_event(&log, &e_create).unwrap();
        append_event(&log, &e_update).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        let issue = &state.issues[&uuid];
        assert_eq!(issue.title, "Original", "Title should be unchanged");
        assert_eq!(
            issue.description.as_deref(),
            Some("New description"),
            "Description should be updated"
        );
        assert_eq!(
            issue.priority,
            crate::models::Priority::Critical,
            "Priority should be updated"
        );
    }

    #[test]
    fn test_prune_events_no_watermark() {
        // prune_events should return 0 when no watermark exists
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Unprunable".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        append_event(&log, &env).unwrap();

        // No compaction done, so no watermark
        let pruned = prune_events(cache_dir, "agent-1").unwrap();
        assert_eq!(pruned, 0);

        // Events should still be there
        let remaining = crate::events::read_events(&log).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_prune_events_no_log_file() {
        // prune_events should return 0 when the agent's log file doesn't exist
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        // Create a watermark so we pass the first check
        let watermark = OrderingKey {
            timestamp: Utc::now(),
            agent_id: "agent-1".to_string(),
            agent_seq: 1,
        };
        crate::checkpoint::write_watermark(cache_dir, &watermark).unwrap();

        // Agent dir exists but no events.log
        std::fs::create_dir_all(cache_dir.join("agents/agent-1")).unwrap();

        let pruned = prune_events(cache_dir, "agent-1").unwrap();
        assert_eq!(pruned, 0);
    }

    #[test]
    fn test_prune_events_nothing_to_prune() {
        // When all events are after the watermark, nothing should be pruned
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let env = make_envelope(
            "agent-1",
            5,
            Event::LabelAdded {
                issue_uuid: Uuid::new_v4(),
                label: "test".to_string(),
            },
        );
        append_event(&log, &env).unwrap();

        // Set watermark before the event
        let watermark = OrderingKey {
            timestamp: now - Duration::seconds(100),
            agent_id: "agent-1".to_string(),
            agent_seq: 1,
        };
        crate::checkpoint::write_watermark(cache_dir, &watermark).unwrap();

        let pruned = prune_events(cache_dir, "agent-1").unwrap();
        assert_eq!(pruned, 0);

        let remaining = crate::events::read_events(&log).unwrap();
        assert_eq!(remaining.len(), 1);
    }

    #[test]
    fn test_compact_no_agents_dir() {
        // compact should succeed when agents dir doesn't exist (no events)
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        // Only set up checkpoint dir, not agents dir
        std::fs::create_dir_all(cache_dir.join("checkpoint")).unwrap();
        std::fs::create_dir_all(cache_dir.join("issues")).unwrap();
        std::fs::create_dir_all(cache_dir.join("locks")).unwrap();

        let result = compact(cache_dir, "agent-1", false).unwrap().unwrap();
        assert_eq!(result.events_processed, 0);
        assert_eq!(result.issues_materialized, 0);
    }

    #[test]
    fn test_read_lock_info_malformed_json() {
        // read_lock_info should return None for malformed lock files
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("compaction.lock");

        // Empty file
        std::fs::write(&lock_path, "").unwrap();
        assert!(CompactionLockGuard::read_lock_info(&lock_path).is_none());

        // Not JSON
        std::fs::write(&lock_path, "not json at all").unwrap();
        assert!(CompactionLockGuard::read_lock_info(&lock_path).is_none());

        // JSON missing agent_id
        std::fs::write(&lock_path, r#"{"acquired_at": "2025-01-01T00:00:00Z"}"#).unwrap();
        assert!(CompactionLockGuard::read_lock_info(&lock_path).is_none());

        // JSON missing acquired_at
        std::fs::write(&lock_path, r#"{"agent_id": "test"}"#).unwrap();
        assert!(CompactionLockGuard::read_lock_info(&lock_path).is_none());

        // JSON with bad date format
        std::fs::write(
            &lock_path,
            r#"{"agent_id": "test", "acquired_at": "not-a-date"}"#,
        )
        .unwrap();
        assert!(CompactionLockGuard::read_lock_info(&lock_path).is_none());
    }

    #[test]
    fn test_read_lock_info_nonexistent_file() {
        let nonexistent = PathBuf::from("/tmp/does_not_exist_lock_file");
        assert!(CompactionLockGuard::read_lock_info(&nonexistent).is_none());
    }

    #[test]
    fn test_force_acquire_with_malformed_lock_file() {
        // force=true with an unreadable lock should remove it and acquire
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let lock_path = cache_dir.join("checkpoint").join(COMPACTION_LOCK_FILE);
        std::fs::write(&lock_path, "totally broken json").unwrap();

        // Without force, should fail since lock file exists but can't be read
        let result = CompactionLockGuard::try_acquire(cache_dir, "agent-1", false).unwrap();
        assert!(
            result.is_none(),
            "Should not acquire when lock has unreadable info and force=false"
        );

        // With force, should remove the malformed lock and acquire
        let guard = CompactionLockGuard::try_acquire(cache_dir, "agent-1", true)
            .unwrap()
            .unwrap();
        assert!(lock_path.exists());
        drop(guard);
    }

    #[test]
    fn test_compact_to_issue_file_with_blockers_and_related() {
        // Verify that compact_to_issue_file correctly maps blockers and related
        let uuid = Uuid::new_v4();
        let blocker = Uuid::new_v4();
        let related = Uuid::new_v4();

        let compact = CompactIssue {
            uuid,
            display_id: Some(42),
            title: "Full issue".to_string(),
            description: Some("With all fields".to_string()),
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::High,
            parent_uuid: Some(Uuid::new_v4()),
            created_by: "agent-1".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: Some(Utc::now()),
            labels: {
                let mut s = BTreeSet::new();
                s.insert("bug".to_string());
                s.insert("urgent".to_string());
                s
            },
            blockers: {
                let mut s = BTreeSet::new();
                s.insert(blocker);
                s
            },
            related: {
                let mut s = BTreeSet::new();
                s.insert(related);
                s
            },
            milestone_uuid: Some(Uuid::new_v4()),
        };

        let issue_file = compact_to_issue_file(&compact);
        assert_eq!(issue_file.uuid, uuid);
        assert_eq!(issue_file.display_id, Some(42));
        assert_eq!(issue_file.title, "Full issue");
        assert_eq!(issue_file.description.as_deref(), Some("With all fields"));
        assert_eq!(issue_file.priority, crate::models::Priority::High);
        assert!(issue_file.closed_at.is_some());
        assert_eq!(issue_file.blockers, vec![blocker]);
        assert_eq!(issue_file.related, vec![related]);
        assert_eq!(
            issue_file.labels,
            vec!["bug".to_string(), "urgent".to_string()]
        );
        assert!(issue_file.comments.is_empty());
        assert!(issue_file.time_entries.is_empty());
        assert_eq!(issue_file.milestone_uuid, compact.milestone_uuid);
        assert_eq!(issue_file.parent_uuid, compact.parent_uuid);
        assert_eq!(issue_file.created_by, "agent-1");
    }

    #[test]
    fn test_incremental_compaction_no_new_events() {
        // After initial compaction with a watermark, a second compact with no new
        // events should return early with events_processed=0
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Once".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        append_event(&log, &env).unwrap();

        // First compaction sets watermark
        let r1 = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert_eq!(r1.events_processed, 1);

        // Second compaction with no new events should hit the early return
        let r2 = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert_eq!(r2.events_processed, 0);
        assert_eq!(r2.issues_materialized, 0);
    }

    #[test]
    fn test_lock_release_on_nonexistent_lock_entry() {
        // LockReleased where the lock doesn't exist in state should be a no-op
        let mut state = CheckpointState::default();
        let mut changed_issues = HashSet::new();
        let mut changed_locks: HashSet<i64> = HashSet::new();

        let env = make_envelope(
            "agent-1",
            1,
            Event::LockReleased {
                issue_display_id: 999,
            },
        );
        apply(&mut state, &env, &mut changed_issues, &mut changed_locks);
        assert!(state.locks.is_empty());
        assert!(changed_locks.is_empty());
    }

    #[test]
    fn test_compact_skips_non_directory_entries_in_agents() {
        // Files (not directories) inside the agents dir should be skipped
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        // Create a regular file inside agents/ (not a directory)
        std::fs::write(cache_dir.join("agents/stray-file.txt"), "junk").unwrap();

        // Also create a valid agent with an event
        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");
        let env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "Valid".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        append_event(&log, &env).unwrap();

        // Should succeed, skipping the stray file
        let result = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert_eq!(result.events_processed, 1);
        assert_eq!(result.issues_materialized, 1);
    }

    #[test]
    fn test_milestone_unassigned() {
        // Setting milestone_uuid to None should clear the milestone
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let issue_uuid = Uuid::new_v4();
        let milestone_uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: issue_uuid,
                title: "Ms test".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = now - Duration::seconds(10);

        let mut e2 = make_envelope(
            "agent-1",
            2,
            Event::MilestoneAssigned {
                issue_uuid,
                milestone_uuid: Some(milestone_uuid),
            },
        );
        e2.timestamp = now - Duration::seconds(5);

        let e3 = make_envelope(
            "agent-1",
            3,
            Event::MilestoneAssigned {
                issue_uuid,
                milestone_uuid: None,
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();
        append_event(&log, &e3).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(
            state.issues[&issue_uuid].milestone_uuid, None,
            "Milestone should be cleared"
        );
    }

    #[test]
    fn test_parent_changed_to_none() {
        // Clearing the parent should work
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let parent = Uuid::new_v4();
        let child = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: child,
                title: "Child".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: Some(parent),
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = now - Duration::seconds(10);

        let e2 = make_envelope(
            "agent-1",
            2,
            Event::ParentChanged {
                issue_uuid: child,
                new_parent_uuid: None,
            },
        );

        append_event(&log, &e1).unwrap();
        append_event(&log, &e2).unwrap();

        compact(cache_dir, "agent-1", true).unwrap();

        let state = read_checkpoint(cache_dir).unwrap();
        assert_eq!(
            state.issues[&child].parent_uuid, None,
            "Parent should be cleared"
        );
    }

    #[test]
    fn test_clock_skew_past_timestamp_no_warning() {
        // Events with timestamps in the past should NOT trigger skew warnings.
        // Past events are expected during incremental compaction; only
        // future-dated events indicate a skewed clock (#330).
        let mut state = CheckpointState::default();
        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Old".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        // Set timestamp far in the past (well beyond 60s threshold)
        env.timestamp = Utc::now() - Duration::seconds(300);

        detect_clock_skew(&mut state, &env);
        assert_eq!(state.skew_warnings.len(), 0);
    }

    #[test]
    fn test_clock_skew_future_timestamp() {
        // Events with timestamps far in the future indicate a skewed clock.
        let mut state = CheckpointState::default();
        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Future".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        // Set timestamp far in the future (well beyond 60s threshold)
        env.timestamp = Utc::now() + Duration::seconds(300);

        detect_clock_skew(&mut state, &env);
        assert_eq!(state.skew_warnings.len(), 1);
        assert_eq!(state.skew_warnings[0].agent_id, "agent-1");
    }

    #[test]
    fn test_check_unsigned_missing_signature_only() {
        // signed_by present but signature missing should still warn
        let mut state = CheckpointState::default();
        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Half-signed".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        env.signed_by = Some("agent-1".to_string());
        env.signature = None; // Missing signature

        let nonexistent = PathBuf::from("/tmp/nonexistent_trust");
        check_unsigned(&mut state, &env, &nonexistent);
        assert_eq!(
            state.unsigned_event_warnings.len(),
            1,
            "Should warn when signature is None"
        );
    }

    #[test]
    fn test_check_unsigned_missing_signed_by_only() {
        // signature present but signed_by missing should still warn
        let mut state = CheckpointState::default();
        let mut env = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Half-signed".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        env.signed_by = None;
        env.signature = Some("fake-sig".to_string());

        let nonexistent = PathBuf::from("/tmp/nonexistent_trust");
        check_unsigned(&mut state, &env, &nonexistent);
        assert_eq!(
            state.unsigned_event_warnings.len(),
            1,
            "Should warn when signed_by is None"
        );
    }

    // Coverage for lines 186, 188: watermark migration path
    // When state has no embedded watermark but a legacy watermark.json exists,
    // compact() should migrate it into the checkpoint state.
    #[test]
    fn test_compact_migrates_legacy_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let uuid = Uuid::new_v4();
        let log = cache_dir.join("agents/agent-1/events.log");

        // Create and compact a first event to establish state
        let mut e1 = make_envelope(
            "agent-1",
            1,
            Event::IssueCreated {
                uuid,
                title: "First".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
            },
        );
        e1.timestamp = Utc::now() - Duration::seconds(10);
        append_event(&log, &e1).unwrap();
        compact(cache_dir, "agent-1", true).unwrap();

        // Now simulate a legacy migration scenario:
        // Read the current checkpoint, strip the embedded watermark, write it back
        // so the next compaction finds no embedded watermark but does find a legacy file.
        let state = read_checkpoint(cache_dir).unwrap();
        let embedded_watermark = state.watermark.clone().unwrap();

        // Write a legacy watermark.json file
        let checkpoint_dir = cache_dir.join("checkpoint");
        let legacy_wm_path = checkpoint_dir.join("watermark.json");
        let wm_json = serde_json::to_string(&embedded_watermark).unwrap();
        std::fs::write(&legacy_wm_path, &wm_json).unwrap();

        // Strip embedded watermark from checkpoint state to simulate legacy state
        let mut state_no_wm = state.clone();
        state_no_wm.watermark = None;
        write_checkpoint(cache_dir, &state_no_wm).unwrap();

        // Add a second event that is after the watermark
        let e2 = make_envelope(
            "agent-1",
            2,
            Event::LabelAdded {
                issue_uuid: uuid,
                label: "migrated".to_string(),
            },
        );
        append_event(&log, &e2).unwrap();

        // This compaction should:
        // 1. Find no embedded watermark (state.watermark = None)
        // 2. Fall back to legacy watermark.json (lines 186, 188 covered)
        // 3. Process only the new event (incremental)
        let result = compact(cache_dir, "agent-1", true).unwrap().unwrap();
        assert_eq!(
            result.events_processed, 1,
            "Only the new event should be processed"
        );

        // Verify the migration happened and the issue has the label
        let final_state = read_checkpoint(cache_dir).unwrap();
        assert!(
            final_state.issues[&uuid].labels.contains("migrated"),
            "Label should be applied after migration"
        );
        // Embedded watermark should now be set
        assert!(
            final_state.watermark.is_some(),
            "Checkpoint should have embedded watermark after migration"
        );
    }

    // Coverage for line 556: remove_file path in materialize when lock was previously
    // materialized and then released in an incremental compaction.
    #[test]
    fn test_materialize_removes_released_lock_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        let lock_path = cache_dir.join("locks/7.json");
        let log = cache_dir.join("agents/agent-1/events.log");
        let now = Utc::now();

        // Step 1: claim — this materializes the lock file
        let mut e_claim = make_envelope(
            "agent-1",
            1,
            Event::LockClaimed {
                issue_display_id: 7,
                branch: Some("feature/remove-test".to_string()),
            },
        );
        e_claim.timestamp = now - Duration::seconds(5);
        append_event(&log, &e_claim).unwrap();
        compact(cache_dir, "agent-1", true).unwrap();
        assert!(
            lock_path.exists(),
            "Lock file should exist after claim compaction"
        );

        // Step 2: release — this should delete the materialized lock file (line 556)
        let mut e_release = make_envelope(
            "agent-1",
            2,
            Event::LockReleased {
                issue_display_id: 7,
            },
        );
        e_release.timestamp = now - Duration::seconds(2);
        append_event(&log, &e_release).unwrap();
        compact(cache_dir, "agent-1", true).unwrap();

        // The lock file should have been deleted by the materialize function (line 556)
        assert!(
            !lock_path.exists(),
            "Lock file should be removed after release compaction"
        );
    }

    // Coverage for lines 615-619: check_unsigned path where allowed_signers exists
    // but the signature fails verification (invalid signature on a signed event).
    #[test]
    fn test_check_unsigned_with_invalid_signature_and_trust_file() {
        use std::process::Command;
        if Command::new("ssh-keygen").arg("--help").output().is_err() {
            eprintln!("Skipping: ssh-keygen not available");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();
        setup_cache(cache_dir);

        // Create a real key so allowed_signers file is valid
        let keys_dir = dir.path().join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();
        let private_key_path = keys_dir.join("agent_ed25519");
        let public_key_path = keys_dir.join("agent_ed25519.pub");
        let output = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &private_key_path.to_string_lossy(),
                "-N",
                "",
                "-C",
                "check-test@host",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());

        let public_key = std::fs::read_to_string(&public_key_path).unwrap();
        let public_key = public_key.trim();

        // Create an allowed_signers file with the real public key
        let signers_path = cache_dir.join("trust").join("allowed_signers");
        std::fs::create_dir_all(signers_path.parent().unwrap()).unwrap();
        // Use "check-agent@crosslink" as the principal (matching envelope.agent_id + "@crosslink")
        std::fs::write(
            &signers_path,
            format!("check-agent@crosslink {}\n", public_key),
        )
        .unwrap();

        // Create an envelope with garbage signature (not matching real sig)
        let mut env = make_envelope(
            "check-agent",
            1,
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Invalid sig".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "check-agent".to_string(),
            },
        );
        env.signed_by = Some("SHA256:fake".to_string());
        env.signature = Some("aW52YWxpZHNpZw==".to_string()); // base64("invalidsig")

        // check_unsigned with a valid allowed_signers path and a bad sig
        // should call verify_event_signature -> Ok(false) -> push warning (lines 615-619)
        let mut state = CheckpointState::default();
        check_unsigned(&mut state, &env, &signers_path);
        assert_eq!(
            state.unsigned_event_warnings.len(),
            1,
            "Should warn when signature is present but invalid"
        );
        assert_eq!(state.unsigned_event_warnings[0].agent_id, "check-agent");
    }

    #[test]
    fn test_read_lock_info_valid() {
        // read_lock_info should successfully parse a well-formed lock file
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("compaction.lock");

        let now = Utc::now();
        let content = serde_json::json!({
            "agent_id": "agent-test",
            "acquired_at": now.to_rfc3339(),
            "pid": 12345,
        });
        std::fs::write(&lock_path, content.to_string()).unwrap();

        let info = CompactionLockGuard::read_lock_info(&lock_path).unwrap();
        assert_eq!(info.agent_id, "agent-test");
        // Check that the parsed time is close to what we wrote
        let diff = (info.acquired_at - now).num_milliseconds().abs();
        assert!(diff < 1000, "Parsed time should be close to written time");
    }
}
