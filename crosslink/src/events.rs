//! Core event types, serialization, and log I/O for the event-sourced CRDT system.
//!
//! Events are append-only NDJSON records stored in per-agent log files at
//! `agents/{agent_id}/events.log` on the coordination branch. Each event
//! carries an `EventEnvelope` with ordering metadata and an optional SSH
//! signature.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{Read as _, Seek, SeekFrom, Write};
use std::path::Path;
use uuid::Uuid;

use crate::signing;

/// Total ordering key for events: (timestamp, `agent_id`, `agent_seq`).
///
/// Events are sorted by this key during compaction to produce a deterministic
/// materialized state regardless of which agent reads them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OrderingKey {
    pub timestamp: DateTime<Utc>,
    pub agent_id: String,
    pub agent_seq: u64,
}

impl OrderingKey {
    #[must_use]
    pub fn from_envelope(env: &EventEnvelope) -> Self {
        Self {
            timestamp: env.timestamp,
            agent_id: env.agent_id.clone(),
            agent_seq: env.agent_seq,
        }
    }
}

/// Event envelope — every event carries this metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub agent_id: String,
    pub agent_seq: u64,
    pub timestamp: DateTime<Utc>,
    pub event: Event,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

/// The event variants across T1 (identity-establishing / exclusive) and
/// T2 (causal) tiers.
///
/// # Tiering
///
/// - **T1** events establish or claim identity and resolve first-claim-wins by
///   [`OrderingKey`]: `IssueCreated`, `LockClaimed`, `LockReleased`,
///   `MilestoneCreated`. `MilestoneCreated` is T1-like because it mints a
///   milestone identity (uuid + claimed display id) the same way `IssueCreated`
///   mints an issue identity; its display-id adoption uses the same
///   first-claim-wins rule.
/// - **T2** events are causal: they mutate or reference an already-established
///   identity and are last-writer-wins (or set-union) by `OrderingKey`. This
///   includes `IssueUpdated`, `StatusChanged`, dependency/relation/label/parent
///   edits, `CommentAdded`, `TimeEntryAdded`, `ScheduleChanged`,
///   `MilestoneClosed`, `MilestoneDeleted`, and `IssueDeleted` (a tombstone:
///   causal w.r.t. the issue it removes, and — by design — deletion wins
///   forever, so no later event can resurrect a deleted issue).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    // T1: Exclusive
    IssueCreated {
        uuid: Uuid,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        priority: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        labels: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent_uuid: Option<Uuid>,
        created_by: String,
        /// Display id claimed from `meta/counters.json` by the (future) emitter.
        ///
        /// During the v2/v3 transition, files receive display ids from the
        /// shared counter. If the reducer allocated ids purely from event order
        /// it could disagree with the file-written id and clobber the file at
        /// materialization. Carrying the authoritative claim here lets the
        /// reducer adopt it (first-claim-wins by `OrderingKey`). `None` for
        /// pure-v3 emitters with no counter; the reducer then allocates.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_id: Option<i64>,
        /// When the issue becomes actionable (GH #361), set at creation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_at: Option<DateTime<Utc>>,
        /// Hard deadline (GH #361), set at creation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_at: Option<DateTime<Utc>>,
    },
    LockClaimed {
        issue_display_id: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    LockReleased {
        issue_display_id: i64,
    },

    // T2: Causal
    IssueUpdated {
        uuid: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        priority: Option<String>,
    },
    StatusChanged {
        uuid: Uuid,
        new_status: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        closed_at: Option<DateTime<Utc>>,
    },
    DependencyAdded {
        blocked_uuid: Uuid,
        blocker_uuid: Uuid,
    },
    DependencyRemoved {
        blocked_uuid: Uuid,
        blocker_uuid: Uuid,
    },
    RelationAdded {
        uuid_a: Uuid,
        uuid_b: Uuid,
    },
    RelationRemoved {
        uuid_a: Uuid,
        uuid_b: Uuid,
    },
    MilestoneAssigned {
        issue_uuid: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        milestone_uuid: Option<Uuid>,
    },
    LabelAdded {
        issue_uuid: Uuid,
        label: String,
    },
    LabelRemoved {
        issue_uuid: Uuid,
        label: String,
    },
    ParentChanged {
        issue_uuid: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        new_parent_uuid: Option<Uuid>,
    },

    /// A comment was added to an issue (T2: causal).
    ///
    /// Mirrors the `CommentEntry` / `CommentFile` payload field-for-field so
    /// materialization is lossless once the reducer becomes the sole writer
    /// (#754 cutover). `comment_uuid` is the V2-layout comment identity used as
    /// the idempotency key; `display_id` is the counter-claimed `i64` comment id
    /// (the `id` field of `CommentEntry`), adopted first-claim-wins like issue
    /// display ids. Replaying the same `CommentAdded` leaves exactly one comment.
    CommentAdded {
        issue_uuid: Uuid,
        comment_uuid: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_id: Option<i64>,
        author: String,
        content: String,
        created_at: DateTime<Utc>,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        intervention_context: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        driver_key_fingerprint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signed_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// A time-tracking entry was added to an issue (T2: causal).
    ///
    /// Mirrors `TimeEntry` field-for-field. The file format `TimeEntry` has no
    /// natural unique identity (only a counter-claimed `id`), so `entry_uuid` is
    /// added to the EVENT (not the file) to key idempotent reduction; the file
    /// schema is unchanged. `display_id` is the counter-claimed `id`.
    TimeEntryAdded {
        issue_uuid: Uuid,
        entry_uuid: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_id: Option<i64>,
        started_at: DateTime<Utc>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_at: Option<DateTime<Utc>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_seconds: Option<i64>,
    },

    /// An issue was deleted (T2: causal tombstone).
    ///
    /// Deletion wins forever, by design: once an `IssueDeleted` is applied, the
    /// uuid is tombstoned and ALL later events for it (including a later-ordered
    /// `IssueCreated`) are ignored — no resurrection. See the reducer for the
    /// tombstone-guard placement.
    IssueDeleted {
        uuid: Uuid,
    },

    /// A milestone was created (T1-like: mints milestone identity).
    ///
    /// Mirrors the `MilestoneEntry` file payload (`uuid`, `name`, `description`,
    /// `status`, `created_at`). `display_id` is the counter-claimed milestone id,
    /// adopted first-claim-wins by `OrderingKey` exactly like issue display ids.
    MilestoneCreated {
        uuid: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        display_id: Option<i64>,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        created_at: DateTime<Utc>,
    },

    /// A milestone was closed (T2: causal).
    MilestoneClosed {
        uuid: Uuid,
        closed_at: DateTime<Utc>,
    },

    /// A milestone was deleted (T2: causal).
    ///
    /// Removes the milestone from state and clears `milestone_uuid` on any issue
    /// that referenced it (mirrors the dangling-reference cleanup expected of the
    /// file path at the #754 cutover).
    MilestoneDeleted {
        uuid: Uuid,
    },

    /// The scheduling fields of an issue changed (T2: causal, last-writer-wins).
    ///
    /// Carries the FULL post-change state of both fields: `None` means the field
    /// was cleared, `Some(_)` means set to that value. The later `OrderingKey`
    /// overwrites the earlier — there is no per-field merge, so a `ScheduleChanged`
    /// always fully determines both `scheduled_at` and `due_at`.
    ScheduleChanged {
        issue_uuid: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_at: Option<DateTime<Utc>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        due_at: Option<DateTime<Utc>>,
    },
}

// ── Codec ────────────────────────────────────────────────────────────

/// Trait for encoding/decoding event envelopes.
pub trait EventCodec {
    /// Encode a single event envelope to bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    fn encode(&self, event: &EventEnvelope) -> Result<Vec<u8>>;

    /// Encode a batch of event envelopes to bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization of any event fails.
    fn encode_batch(&self, events: &[EventEnvelope]) -> Result<Vec<u8>>;

    /// Decode all event envelopes from bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if deserialization fails for a non-trailing line.
    fn decode_all(&self, bytes: &[u8]) -> Result<Vec<EventEnvelope>>;
}

/// NDJSON (newline-delimited JSON) codec for event envelopes.
pub struct NdjsonCodec;

impl EventCodec for NdjsonCodec {
    fn encode(&self, event: &EventEnvelope) -> Result<Vec<u8>> {
        let mut line = serde_json::to_vec(event).context("Failed to encode event as JSON")?;
        line.push(b'\n');
        Ok(line)
    }

    fn encode_batch(&self, events: &[EventEnvelope]) -> Result<Vec<u8>> {
        let mut buf = Vec::new();
        for event in events {
            serde_json::to_writer(&mut buf, event).context("Failed to encode event")?;
            buf.push(b'\n');
        }
        Ok(buf)
    }

    fn decode_all(&self, bytes: &[u8]) -> Result<Vec<EventEnvelope>> {
        let lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
        let mut events = Vec::new();
        let total = lines.len();
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<EventEnvelope>(line) {
                Ok(envelope) => events.push(envelope),
                Err(e) => {
                    // Tolerate a corrupt trailing line (crash mid-write) but
                    // treat corruption in the middle of the log as a hard error.
                    if i == total - 1 || (i == total - 2 && lines.last() == Some(&&b""[..])) {
                        tracing::warn!(
                            "truncating incomplete trailing event line ({} bytes): {}",
                            line.len(),
                            e
                        );
                    } else {
                        return Err(e).context("Failed to decode event line");
                    }
                }
            }
        }
        Ok(events)
    }
}

// ── Log I/O ──────────────────────────────────────────────────────────

/// Truncate any incomplete trailing line left by a crash.
///
/// Reads the tail of the file and, if it does not end with `\n`, truncates
/// back to the last newline so the next append starts on a clean line.
///
/// Used by [`append_event`], a v2-era worktree-log helper now exercised only by
/// tests and the v2-read fixtures the migration relies on (#754).
#[cfg_attr(not(test), allow(dead_code))]
fn repair_trailing_line(file: &mut std::fs::File) -> Result<()> {
    let len = file.seek(SeekFrom::End(0))?;
    if len == 0 {
        return Ok(());
    }
    // Read the last byte to check for newline terminator.
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    // File does not end with newline — find the last newline and truncate.
    // Read up to 64 KiB from the tail to find it.
    let tail_size = len.min(65536);
    file.seek(SeekFrom::End(-tail_size.cast_signed()))?;
    let mut buf = vec![0u8; tail_size as usize];
    file.read_exact(&mut buf)?;
    let truncate_to = buf.iter().rposition(|&b| b == b'\n').map_or(
        // No newline found at all — the entire file is one corrupt fragment.
        0,
        |pos| len - tail_size + pos as u64 + 1,
    );
    tracing::warn!(
        "truncating {} bytes of incomplete trailing data from event log",
        len - truncate_to
    );
    file.set_len(truncate_to)?;
    file.seek(SeekFrom::End(0))?;
    Ok(())
}

/// Append an event to an agent's log file (creates file if needed).
///
/// Repairs any incomplete trailing line left by a previous crash before
/// appending, and fsyncs after writing to ensure durability.
///
/// This writes the v2 worktree `events.log`. The v2 write path is gone (#754);
/// the function is retained for tests and the v2-read fixtures the migration
/// relies on, so it is allowed to be unused outside test builds.
///
/// # Errors
///
/// Returns an error if the log file cannot be opened, repaired, or written to.
#[cfg_attr(not(test), allow(dead_code))]
pub fn append_event(log_path: &Path, envelope: &EventEnvelope) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create dir for {}", log_path.display()))?;
    }
    let codec = NdjsonCodec;
    let bytes = codec.encode(envelope)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(log_path)
        .with_context(|| format!("Failed to open event log: {}", log_path.display()))?;
    repair_trailing_line(&mut file)
        .with_context(|| format!("Failed to repair event log: {}", log_path.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("Failed to append to event log: {}", log_path.display()))?;
    file.sync_all()
        .with_context(|| format!("Failed to fsync event log: {}", log_path.display()))?;
    Ok(())
}

/// Read all events from a log file.
///
/// Delegates to [`read_events_from_bytes`] after reading the file.
///
/// # Errors
///
/// Returns an error if the log file cannot be read or contains corrupt data.
pub fn read_events(log_path: &Path) -> Result<Vec<EventEnvelope>> {
    if !log_path.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(log_path)
        .with_context(|| format!("Failed to read event log: {}", log_path.display()))?;
    read_events_from_bytes(&bytes)
        .with_context(|| format!("Failed to parse event log: {}", log_path.display()))
}

/// Read all events from raw bytes (NDJSON).
///
/// This is the byte-level parse core shared by both the file-based path
/// ([`read_events`]) and the git-object-store path ([`ObjectStoreSource`]).
/// Semantics are identical: line-by-line NDJSON, same blank/corrupt-trailing-
/// line handling as the codec's `decode_all`.
///
/// # Errors
///
/// Returns an error if any non-trailing line is corrupt (same as `decode_all`).
pub fn read_events_from_bytes(bytes: &[u8]) -> Result<Vec<EventEnvelope>> {
    let codec = NdjsonCodec;
    codec.decode_all(bytes)
}

/// Read only events with ordering key > watermark.
///
/// Currently deserializes all events and filters in-memory. For very large
/// logs this could be optimized by seeking to an approximate offset based on
/// the watermark timestamp, but the NDJSON format requires scanning for
/// newline boundaries regardless. The current approach is correct and
/// performant for typical log sizes (<100k events). (#333)
///
/// # Errors
///
/// Returns an error if reading or parsing the log file fails.
pub fn read_events_after(log_path: &Path, watermark: &OrderingKey) -> Result<Vec<EventEnvelope>> {
    let all = read_events(log_path)?;
    Ok(all
        .into_iter()
        .filter(|e| OrderingKey::from_envelope(e) > *watermark)
        .collect())
}

// ── Event Signing ────────────────────────────────────────────────────

/// Canonicalize an event envelope for signing.
///
/// Uses the event's JSON representation (without signature fields) as the
/// content to sign, ensuring deterministic canonical form.
fn canonicalize_event(envelope: &EventEnvelope) -> Result<Vec<u8>> {
    let event_json = serde_json::to_string(&envelope.event)?;
    Ok(signing::canonicalize_for_signing(&[
        ("agent_id", &envelope.agent_id),
        ("agent_seq", &envelope.agent_seq.to_string()),
        ("timestamp", &envelope.timestamp.to_rfc3339()),
        ("event", &event_json),
    ]))
}

/// Sign an event envelope using the agent's SSH key.
///
/// # Errors
///
/// Returns an error if canonicalization or SSH signing fails.
pub fn sign_event(
    envelope: &mut EventEnvelope,
    private_key_path: &Path,
    fingerprint: &str,
) -> Result<()> {
    let content = canonicalize_event(envelope)?;
    let sig = signing::sign_content(private_key_path, &content, "crosslink-event")?;
    envelope.signed_by = Some(fingerprint.to_string());
    envelope.signature = Some(sig);
    Ok(())
}

/// Verify an event's signature against the allowed signers store.
///
/// # Errors
///
/// Returns an error if canonicalization or signature verification fails.
pub fn verify_event_signature(
    envelope: &EventEnvelope,
    allowed_signers_path: &Path,
) -> Result<bool> {
    let (Some(signed_by), Some(signature)) = (&envelope.signed_by, &envelope.signature) else {
        return Ok(false);
    };
    let content = canonicalize_event(envelope)?;
    let principal = format!("{}@crosslink", envelope.agent_id);
    signing::verify_content(
        allowed_signers_path,
        &principal,
        "crosslink-event",
        &content,
        signature,
    )
    .with_context(|| format!("Failed to verify event signature for {signed_by}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_envelope(agent_id: &str, seq: u64) -> EventEnvelope {
        EventEnvelope {
            agent_id: agent_id.to_string(),
            agent_seq: seq,
            timestamp: Utc::now(),
            event: Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "Test issue".to_string(),
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

    #[test]
    fn test_ndjson_codec_roundtrip() {
        let codec = NdjsonCodec;
        let envelope = make_envelope("agent-1", 1);
        let bytes = codec.encode(&envelope).unwrap();
        let decoded: EventEnvelope = serde_json::from_slice(bytes.trim_ascii()).unwrap();
        assert_eq!(decoded.agent_id, "agent-1");
        assert_eq!(decoded.agent_seq, 1);
    }

    #[test]
    fn test_ndjson_codec_batch_roundtrip() {
        let codec = NdjsonCodec;
        let envelopes = vec![make_envelope("agent-1", 1), make_envelope("agent-2", 2)];
        let bytes = codec.encode_batch(&envelopes).unwrap();
        let decoded = codec.decode_all(&bytes).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].agent_id, "agent-1");
        assert_eq!(decoded[1].agent_id, "agent-2");
    }

    #[test]
    fn test_ordering_key_comparison() {
        use chrono::Duration;
        let now = Utc::now();
        let k1 = OrderingKey {
            timestamp: now,
            agent_id: "a".to_string(),
            agent_seq: 1,
        };
        let k2 = OrderingKey {
            timestamp: now + Duration::seconds(1),
            agent_id: "a".to_string(),
            agent_seq: 1,
        };
        let k3 = OrderingKey {
            timestamp: now,
            agent_id: "b".to_string(),
            agent_seq: 1,
        };
        let k4 = OrderingKey {
            timestamp: now,
            agent_id: "a".to_string(),
            agent_seq: 2,
        };

        assert!(k1 < k2, "later timestamp should be greater");
        assert!(k1 < k3, "agent_id 'a' < 'b'");
        assert!(k1 < k4, "agent_seq 1 < 2");
    }

    #[test]
    fn test_append_and_read_events() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("agents/agent-1/events.log");

        let e1 = make_envelope("agent-1", 1);
        let e2 = make_envelope("agent-1", 2);

        append_event(&log_path, &e1).unwrap();
        append_event(&log_path, &e2).unwrap();

        let events = read_events(&log_path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].agent_seq, 1);
        assert_eq!(events[1].agent_seq, 2);
    }

    #[test]
    fn test_read_events_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("nonexistent/events.log");
        let events = read_events(&log_path).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_read_events_after_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.log");

        let now = Utc::now();
        let mut e1 = make_envelope("agent-1", 1);
        e1.timestamp = now - chrono::Duration::seconds(10);
        let mut e2 = make_envelope("agent-1", 2);
        e2.timestamp = now;
        let mut e3 = make_envelope("agent-1", 3);
        e3.timestamp = now + chrono::Duration::seconds(10);

        append_event(&log_path, &e1).unwrap();
        append_event(&log_path, &e2).unwrap();
        append_event(&log_path, &e3).unwrap();

        let watermark = OrderingKey::from_envelope(&e1);
        let filtered = read_events_after(&log_path, &watermark).unwrap();
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].agent_seq, 2);
        assert_eq!(filtered[1].agent_seq, 3);
    }

    #[test]
    fn test_event_serde_all_variants() {
        let variants = vec![
            Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "test".to_string(),
                description: Some("desc".to_string()),
                priority: "high".to_string(),
                labels: vec!["bug".to_string()],
                parent_uuid: None,
                created_by: "agent-1".to_string(),
                display_id: Some(7),
                scheduled_at: Some(Utc::now()),
                due_at: Some(Utc::now()),
            },
            Event::LockClaimed {
                issue_display_id: 1,
                branch: Some("feature/x".to_string()),
            },
            Event::LockReleased {
                issue_display_id: 1,
            },
            Event::IssueUpdated {
                uuid: Uuid::new_v4(),
                title: Some("new title".to_string()),
                description: None,
                priority: None,
            },
            Event::StatusChanged {
                uuid: Uuid::new_v4(),
                new_status: "closed".to_string(),
                closed_at: Some(Utc::now()),
            },
            Event::DependencyAdded {
                blocked_uuid: Uuid::new_v4(),
                blocker_uuid: Uuid::new_v4(),
            },
            Event::DependencyRemoved {
                blocked_uuid: Uuid::new_v4(),
                blocker_uuid: Uuid::new_v4(),
            },
            Event::RelationAdded {
                uuid_a: Uuid::new_v4(),
                uuid_b: Uuid::new_v4(),
            },
            Event::RelationRemoved {
                uuid_a: Uuid::new_v4(),
                uuid_b: Uuid::new_v4(),
            },
            Event::MilestoneAssigned {
                issue_uuid: Uuid::new_v4(),
                milestone_uuid: Some(Uuid::new_v4()),
            },
            Event::LabelAdded {
                issue_uuid: Uuid::new_v4(),
                label: "bug".to_string(),
            },
            Event::LabelRemoved {
                issue_uuid: Uuid::new_v4(),
                label: "bug".to_string(),
            },
            Event::ParentChanged {
                issue_uuid: Uuid::new_v4(),
                new_parent_uuid: None,
            },
            Event::CommentAdded {
                issue_uuid: Uuid::new_v4(),
                comment_uuid: Uuid::new_v4(),
                display_id: Some(3),
                author: "agent-1".to_string(),
                content: "a comment".to_string(),
                created_at: Utc::now(),
                kind: "note".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None,
                signature: None,
            },
            Event::TimeEntryAdded {
                issue_uuid: Uuid::new_v4(),
                entry_uuid: Uuid::new_v4(),
                display_id: Some(1),
                started_at: Utc::now(),
                ended_at: Some(Utc::now()),
                duration_seconds: Some(3600),
            },
            Event::IssueDeleted {
                uuid: Uuid::new_v4(),
            },
            Event::MilestoneCreated {
                uuid: Uuid::new_v4(),
                display_id: Some(1),
                name: "v1.0".to_string(),
                description: Some("first release".to_string()),
                created_at: Utc::now(),
            },
            Event::MilestoneClosed {
                uuid: Uuid::new_v4(),
                closed_at: Utc::now(),
            },
            Event::MilestoneDeleted {
                uuid: Uuid::new_v4(),
            },
            Event::ScheduleChanged {
                issue_uuid: Uuid::new_v4(),
                scheduled_at: Some(Utc::now()),
                due_at: None,
            },
        ];

        for event in variants {
            let json = serde_json::to_string(&event).unwrap();
            let parsed: Event = serde_json::from_str(&json).unwrap();
            // Verify the tag roundtrips
            let json2 = serde_json::to_string(&parsed).unwrap();
            let reparsed: Event = serde_json::from_str(&json2).unwrap();
            assert_eq!(
                serde_json::to_string(&parsed).unwrap(),
                serde_json::to_string(&reparsed).unwrap()
            );
        }
    }

    #[test]
    fn test_old_format_issue_created_parses() {
        // An IssueCreated log line written before PR3.5 (no display_id,
        // scheduled_at, due_at fields) must still parse via #[serde(default)].
        let json = r#"{"type":"IssueCreated","uuid":"00000000-0000-0000-0000-000000000001","title":"old","priority":"medium","created_by":"agent-1"}"#;
        let parsed: Event = serde_json::from_str(json).unwrap();
        if let Event::IssueCreated {
            display_id,
            scheduled_at,
            due_at,
            title,
            ..
        } = parsed
        {
            assert_eq!(title, "old");
            assert!(display_id.is_none());
            assert!(scheduled_at.is_none());
            assert!(due_at.is_none());
        } else {
            panic!("Expected IssueCreated variant");
        }
    }

    #[test]
    fn test_old_format_issue_created_in_envelope_parses() {
        // Full old-format envelope line must still decode.
        let json = r#"{"agent_id":"agent-1","agent_seq":1,"timestamp":"2025-01-01T00:00:00Z","event":{"type":"IssueCreated","uuid":"00000000-0000-0000-0000-000000000002","title":"legacy","priority":"high","labels":["bug"],"created_by":"agent-1"}}"#;
        let env: EventEnvelope = serde_json::from_str(json).unwrap();
        assert_eq!(env.agent_seq, 1);
        assert!(matches!(env.event, Event::IssueCreated { .. }));
    }

    #[test]
    fn test_event_envelope_serde_roundtrip() {
        let envelope = EventEnvelope {
            agent_id: "agent-1".to_string(),
            agent_seq: 42,
            timestamp: Utc::now(),
            event: Event::LabelAdded {
                issue_uuid: Uuid::new_v4(),
                label: "feature".to_string(),
            },
            signed_by: Some("SHA256:abc".to_string()),
            signature: Some("sig123".to_string()),
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let parsed: EventEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_id, "agent-1");
        assert_eq!(parsed.agent_seq, 42);
        assert_eq!(parsed.signed_by, Some("SHA256:abc".to_string()));
    }

    #[test]
    fn test_event_envelope_optional_fields_omitted() {
        let envelope = make_envelope("agent-1", 1);
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(!json.contains("signed_by"));
        assert!(!json.contains("signature"));
    }

    #[test]
    fn test_canonicalize_event_deterministic() {
        let envelope = make_envelope("agent-1", 1);
        let c1 = canonicalize_event(&envelope).unwrap();
        let c2 = canonicalize_event(&envelope).unwrap();
        assert_eq!(c1, c2);
    }

    #[test]
    fn test_ordering_key_from_envelope() {
        let envelope = make_envelope("agent-1", 5);
        let key = OrderingKey::from_envelope(&envelope);
        assert_eq!(key.agent_id, "agent-1");
        assert_eq!(key.agent_seq, 5);
        assert_eq!(key.timestamp, envelope.timestamp);
    }

    #[test]
    fn test_decode_all_truncates_incomplete_trailing_line() {
        let codec = NdjsonCodec;
        let envelope = make_envelope("agent-1", 1);
        let mut bytes = codec.encode(&envelope).unwrap();
        // Append a partial/corrupt trailing fragment (simulates crash mid-write)
        bytes.extend_from_slice(b"{\"agent_id\":\"agent-1\",\"age");
        let events = codec.decode_all(&bytes).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].agent_seq, 1);
    }

    #[test]
    fn test_decode_all_errors_on_corrupt_middle_line() {
        let codec = NdjsonCodec;
        let e1 = make_envelope("agent-1", 1);
        let e2 = make_envelope("agent-1", 2);
        let mut bytes = codec.encode(&e1).unwrap();
        bytes.extend_from_slice(b"CORRUPT_LINE\n");
        bytes.extend_from_slice(&codec.encode(&e2).unwrap());
        let result = codec.decode_all(&bytes);
        assert!(result.is_err(), "corruption in middle should be an error");
    }

    #[test]
    fn test_append_repairs_incomplete_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.log");

        // Write a valid event
        let e1 = make_envelope("agent-1", 1);
        append_event(&log_path, &e1).unwrap();

        // Simulate crash: append partial data without newline
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"{\"agent_id\":\"partial").unwrap();
        }

        // Next append should repair the file and succeed
        let e2 = make_envelope("agent-1", 2);
        append_event(&log_path, &e2).unwrap();

        let events = read_events(&log_path).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].agent_seq, 1);
        assert_eq!(events[1].agent_seq, 2);
    }

    #[test]
    fn test_append_repairs_empty_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.log");

        // Simulate crash on very first write: partial data, no newline
        std::fs::write(&log_path, b"{\"agent_id\":\"partial").unwrap();

        // Next append should truncate the corrupt data and write cleanly
        let e1 = make_envelope("agent-1", 1);
        append_event(&log_path, &e1).unwrap();

        let events = read_events(&log_path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].agent_seq, 1);
    }

    #[test]
    fn test_verify_event_signature_returns_false_when_unsigned() {
        let dir = tempfile::tempdir().unwrap();
        let signers_path = dir.path().join("allowed_signers");
        std::fs::write(&signers_path, "").unwrap();

        let envelope = make_envelope("agent-1", 1);
        let result = verify_event_signature(&envelope, &signers_path).unwrap();
        assert!(!result, "Unsigned event should return false");
    }

    #[test]
    fn test_verify_event_signature_returns_false_when_only_signed_by() {
        let dir = tempfile::tempdir().unwrap();
        let signers_path = dir.path().join("allowed_signers");
        std::fs::write(&signers_path, "").unwrap();

        let mut envelope = make_envelope("agent-1", 1);
        envelope.signed_by = Some("SHA256:abc".to_string());
        let result = verify_event_signature(&envelope, &signers_path).unwrap();
        assert!(!result, "Event with only signed_by should return false");
    }

    #[test]
    fn test_verify_event_signature_returns_false_when_only_signature() {
        let dir = tempfile::tempdir().unwrap();
        let signers_path = dir.path().join("allowed_signers");
        std::fs::write(&signers_path, "").unwrap();

        let mut envelope = make_envelope("agent-1", 1);
        envelope.signature = Some("sig123".to_string());
        let result = verify_event_signature(&envelope, &signers_path).unwrap();
        assert!(!result, "Event with only signature should return false");
    }

    #[test]
    fn test_read_events_after_watermark_returns_empty_when_all_before() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.log");

        let now = Utc::now();
        let mut e1 = make_envelope("agent-1", 1);
        e1.timestamp = now - chrono::Duration::seconds(20);
        let mut e2 = make_envelope("agent-1", 2);
        e2.timestamp = now - chrono::Duration::seconds(10);

        append_event(&log_path, &e1).unwrap();
        append_event(&log_path, &e2).unwrap();

        let watermark = OrderingKey {
            timestamp: now,
            agent_id: "agent-1".to_string(),
            agent_seq: 999,
        };
        let filtered = read_events_after(&log_path, &watermark).unwrap();
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_read_events_after_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("nonexistent.log");

        let watermark = OrderingKey {
            timestamp: Utc::now(),
            agent_id: "a".to_string(),
            agent_seq: 0,
        };
        let events = read_events_after(&log_path, &watermark).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_canonicalize_event_different_events_differ() {
        let e1 = make_envelope("agent-1", 1);
        let mut e2 = make_envelope("agent-1", 2);
        e2.timestamp = e1.timestamp;

        let c1 = canonicalize_event(&e1).unwrap();
        let c2 = canonicalize_event(&e2).unwrap();
        assert_ne!(
            c1, c2,
            "Different agent_seq should produce different canonical forms"
        );
    }

    #[test]
    fn test_canonicalize_event_ignores_signature_fields() {
        let mut e1 = make_envelope("agent-1", 1);
        let c_before = canonicalize_event(&e1).unwrap();

        e1.signed_by = Some("SHA256:abc".to_string());
        e1.signature = Some("sig123".to_string());
        let c_after = canonicalize_event(&e1).unwrap();
        assert_eq!(c_before, c_after);
    }

    #[test]
    fn test_ndjson_codec_decode_all_empty_input() {
        let codec = NdjsonCodec;
        let events = codec.decode_all(b"").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_ndjson_codec_decode_all_only_newlines() {
        let codec = NdjsonCodec;
        let events = codec.decode_all(b"\n\n\n").unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_ordering_key_equality() {
        let now = Utc::now();
        let k1 = OrderingKey {
            timestamp: now,
            agent_id: "a".to_string(),
            agent_seq: 1,
        };
        let k2 = OrderingKey {
            timestamp: now,
            agent_id: "a".to_string(),
            agent_seq: 1,
        };
        assert_eq!(k1, k2);
    }

    #[test]
    fn test_ordering_key_serde_roundtrip() {
        let key = OrderingKey {
            timestamp: Utc::now(),
            agent_id: "test-agent".to_string(),
            agent_seq: 42,
        };
        let json = serde_json::to_string(&key).unwrap();
        let parsed: OrderingKey = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, key);
    }

    #[test]
    fn test_event_issue_created_with_labels_and_parent() {
        let parent_uuid = Uuid::new_v4();
        let event = Event::IssueCreated {
            uuid: Uuid::new_v4(),
            title: "child issue".to_string(),
            description: Some("desc".to_string()),
            priority: "high".to_string(),
            labels: vec!["bug".to_string(), "urgent".to_string()],
            parent_uuid: Some(parent_uuid),
            created_by: "agent-1".to_string(),
            display_id: None,
            scheduled_at: None,
            due_at: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("bug"));
        assert!(json.contains("urgent"));
        assert!(json.contains(&parent_uuid.to_string()));

        let parsed: Event = serde_json::from_str(&json).unwrap();
        if let Event::IssueCreated {
            labels,
            parent_uuid: p,
            ..
        } = parsed
        {
            assert_eq!(labels, vec!["bug", "urgent"]);
            assert_eq!(p, Some(parent_uuid));
        } else {
            panic!("Expected IssueCreated variant");
        }
    }

    #[test]
    fn test_event_lock_claimed_without_branch() {
        let event = Event::LockClaimed {
            issue_display_id: 5,
            branch: None,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(!json.contains("branch"));

        let parsed: Event = serde_json::from_str(&json).unwrap();
        if let Event::LockClaimed {
            issue_display_id,
            branch,
        } = parsed
        {
            assert_eq!(issue_display_id, 5);
            assert!(branch.is_none());
        } else {
            panic!("Expected LockClaimed variant");
        }
    }

    #[test]
    fn test_append_event_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("deep/nested/dir/events.log");

        let e = make_envelope("agent-1", 1);
        append_event(&log_path, &e).unwrap();

        let events = read_events(&log_path).unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_ndjson_codec_encode_ends_with_newline() {
        let codec = NdjsonCodec;
        let e = make_envelope("agent-1", 1);
        let bytes = codec.encode(&e).unwrap();
        assert_eq!(*bytes.last().unwrap(), b'\n');
    }

    #[test]
    fn test_ndjson_codec_batch_encode_ends_with_newline() {
        let codec = NdjsonCodec;
        let events = vec![make_envelope("a", 1), make_envelope("b", 2)];
        let bytes = codec.encode_batch(&events).unwrap();
        assert_eq!(*bytes.last().unwrap(), b'\n');
    }

    // Coverage for sign_event and verify_event_signature with actual SSH keys
    #[test]
    fn test_sign_and_verify_event_roundtrip() {
        use std::process::Command;
        if Command::new("ssh-keygen").arg("--help").output().is_err() {
            eprintln!("Skipping: ssh-keygen not available");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let keys_dir = dir.path().join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();

        // Generate a test key pair
        let private_key_path = keys_dir.join("test_ed25519");
        let public_key_path = keys_dir.join("test_ed25519.pub");
        let output = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &private_key_path.to_string_lossy(),
                "-N",
                "",
                "-C",
                "test-agent@test",
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "ssh-keygen failed");

        // Get fingerprint
        let fp_output = Command::new("ssh-keygen")
            .args(["-l", "-f", &public_key_path.to_string_lossy()])
            .output()
            .unwrap();
        let fp_str = String::from_utf8_lossy(&fp_output.stdout);
        let fingerprint = fp_str.split_whitespace().nth(1).unwrap().to_string();

        // Sign the event
        let mut envelope = make_envelope("test-agent", 1);
        sign_event(&mut envelope, &private_key_path, &fingerprint).unwrap();

        assert_eq!(envelope.signed_by, Some(fingerprint.clone()));
        assert!(envelope.signature.is_some());

        // Set up allowed_signers file
        let public_key = std::fs::read_to_string(&public_key_path).unwrap();
        let public_key = public_key.trim();
        let signers_path = dir.path().join("allowed_signers");
        let principal = "test-agent@crosslink".to_string();
        std::fs::write(&signers_path, format!("{principal} {public_key}\n")).unwrap();

        // Verify the signature
        let verified = verify_event_signature(&envelope, &signers_path).unwrap();
        assert!(verified, "Valid event signature should verify successfully");
    }

    #[test]
    fn test_verify_event_signature_invalid_signature() {
        use std::process::Command;
        if Command::new("ssh-keygen").arg("--help").output().is_err() {
            eprintln!("Skipping: ssh-keygen not available");
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let keys_dir = dir.path().join("keys");
        std::fs::create_dir_all(&keys_dir).unwrap();

        // Generate a key to have a valid allowed_signers entry
        let private_key_path = keys_dir.join("test_ed25519");
        let public_key_path = keys_dir.join("test_ed25519.pub");
        let output = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                &private_key_path.to_string_lossy(),
                "-N",
                "",
                "-C",
                "test-agent@test",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());

        let public_key = std::fs::read_to_string(&public_key_path).unwrap();
        let public_key = public_key.trim();
        let signers_path = dir.path().join("allowed_signers");
        std::fs::write(
            &signers_path,
            format!("test-agent@crosslink {public_key}\n"),
        )
        .unwrap();

        // Create an envelope with a tampered/garbage signature
        let mut envelope = make_envelope("test-agent", 1);
        envelope.signed_by = Some("SHA256:fake".to_string());
        envelope.signature = Some("aW52YWxpZHNpZ25hdHVyZQ==".to_string()); // base64("invalidsignature")

        // Verification should return false (not an error) for invalid signatures
        let result = verify_event_signature(&envelope, &signers_path);
        // Either Ok(false) or an Err — either way the signature is not valid
        match result {
            Ok(false) | Err(_) => {} // expected — invalid sig or ssh-keygen error
            Ok(true) => panic!("Should not verify a garbage signature"),
        }
    }

    // Coverage for EventCodec trait object usage (line 132)
    #[test]
    fn test_event_codec_trait_object() {
        let codec: &dyn EventCodec = &NdjsonCodec;
        let envelope = make_envelope("agent-1", 42);
        let bytes = codec.encode(&envelope).unwrap();
        let decoded = codec.decode_all(&bytes).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].agent_seq, 42);
    }

    // Coverage for the `continue` path in decode_all when line is empty (line 163)
    #[test]
    fn test_decode_all_skips_empty_lines_between_events() {
        let codec = NdjsonCodec;
        let e1 = make_envelope("agent-1", 1);
        let e2 = make_envelope("agent-1", 2);
        let mut bytes = codec.encode(&e1).unwrap();
        // Insert an extra blank line between the two events
        bytes.extend_from_slice(b"\n");
        bytes.extend_from_slice(&codec.encode(&e2).unwrap());
        let events = codec.decode_all(&bytes).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].agent_seq, 1);
        assert_eq!(events[1].agent_seq, 2);
    }
}
