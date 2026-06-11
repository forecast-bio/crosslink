//! Issue mutation operations: create, update, close, reopen, delete,
//! comments, labels, blockers, and relations.
//!
//! # Multi-store consistency (re: #604)
//!
//! Each public mutation here layers three persistence steps —
//! `JSON` write to the hub cache, git commit (+push), then `SQLite`
//! re-hydration. These three are intentionally NOT transactional in
//! the ACID-across-stores sense. The relationships are:
//!
//! - **git commits are the canonical event log.** The hub branch is
//!   the source of truth that other agents synchronize against.
//! - **The per-issue `JSON` files are a current-state view written
//!   inside each commit.** They live in the same commit as their
//!   own update, so they can't drift from the log: any reader that
//!   trusts `git log + git show` sees the same state as any reader
//!   that opens the working tree files.
//! - **`SQLite` is a derived cache** rebuilt by `hydrate_to_sqlite`
//!   from the `JSON` view after every successful mutation.
//!
//! The "failure modes table" in #604 enumerates five interleavings
//! of partial failure across these three steps. Four of them
//! (commit-fails, push-fails, hydrate-fails, post-commit-`SQLite`-fails)
//! self-heal on the next successful mutation — the next `git add`
//! picks up any straggling `JSON` modification, the next `hydrate`
//! retries, and the next sync push catches up. Mode #4 alone (a
//! process crash mid-write leaving a truncated `JSON` file on disk)
//! is genuinely non-recoverable without manual `git checkout`, and
//! is fixed in `apply_write_set` by switching to
//! [`crate::utils::atomic_write`] (temp file + POSIX rename).
//!
//! We considered the issue's "events as source of truth, derived
//! stores rebuilt by replay" suggestion but concluded it would
//! over-couple stores that are designed to be eventually
//! consistent. The hub branch is *already* the canonical event log;
//! the per-issue `JSON` is part of the commit, not a separate store
//! that can drift from it. The actual gap was crash atomicity, and
//! the fix is local to the write step.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::cell::Cell;
use uuid::Uuid;

use crate::db::Database;
use crate::issue_file::{CommentEntry, CommentFile, IssueFile};

use super::core::{PushOutcome, SharedWriter, WriteSet};

/// Represents an update to a description field with three possible states:
/// unchanged, cleared, or set to a new value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DescriptionUpdate<'a> {
    /// Do not modify the description.
    #[default]
    Unchanged,
    /// Clear the description (set to `None`).
    Clear,
    /// Set the description to the given value.
    Set(&'a str),
}

impl<'a> From<Option<Option<&'a str>>> for DescriptionUpdate<'a> {
    fn from(opt: Option<Option<&'a str>>) -> Self {
        match opt {
            None => Self::Unchanged,
            Some(None) => Self::Clear,
            Some(Some(s)) => Self::Set(s),
        }
    }
}

/// Generic three-valued update for optional fields (GH #361). Use for any
/// setter that needs to distinguish "leave alone" from "set to `None`" from
/// "set to `Some(value)`".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FieldUpdate<T> {
    /// Do not modify the field.
    #[default]
    Unchanged,
    /// Clear the field (set to `None`).
    Clear,
    /// Set the field to the given value.
    Set(T),
}

impl<T> From<Option<Option<T>>> for FieldUpdate<T> {
    fn from(opt: Option<Option<T>>) -> Self {
        match opt {
            None => Self::Unchanged,
            Some(None) => Self::Clear,
            Some(Some(v)) => Self::Set(v),
        }
    }
}

/// Field-level update for an existing issue. Every field defaults to
/// "leave unchanged," so callers touch only what they want to change:
///
/// ```ignore
/// writer.update_issue(&db, id, IssueUpdate {
///     title: Some("renamed"),
///     scheduled_at: FieldUpdate::Clear,
///     ..Default::default()
/// })?;
/// ```
///
/// Replaces the previous 8-argument positional signature that was
/// trivial to misuse at the call site (two adjacent `Option<&str>`
/// parameters for status and priority were indistinguishable).
#[derive(Debug, Clone, Copy, Default)]
pub struct IssueUpdate<'a> {
    pub title: Option<&'a str>,
    pub description: DescriptionUpdate<'a>,
    pub status: Option<&'a str>,
    pub priority: Option<&'a str>,
    pub scheduled_at: FieldUpdate<chrono::DateTime<chrono::Utc>>,
    pub due_at: FieldUpdate<chrono::DateTime<chrono::Utc>>,
}

/// Internal shape of a new-issue creation request, used to keep
/// `create_issue_inner`'s signature narrow. The public `create_issue` /
/// `create_subissue` entry points keep their positional-argument shape
/// for backward compatibility with callers throughout the crate; this
/// struct exists purely so the shared inner helper doesn't have to
/// carry 8 positional parameters.
#[derive(Debug, Clone, Copy)]
struct IssueCreate<'a> {
    title: &'a str,
    description: Option<&'a str>,
    priority: &'a str,
    parent_uuid: Option<Uuid>,
    scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    due_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Internal parameters for creating a comment (shared by `add_comment`
/// and `add_intervention_comment` to avoid duplicating V1/V2 dispatch).
#[derive(Clone)]
struct CommentParams {
    content: String,
    kind: String,
    trigger_type: Option<String>,
    intervention_context: Option<String>,
    driver_key_fingerprint: Option<String>,
}

impl SharedWriter {
    /// Internal helper: create an issue (optionally as a subissue).
    ///
    /// Shared by `create_issue` and `create_subissue` to avoid duplicating
    /// the UUID generation, ID claiming, V2 directory setup, offline
    /// rewrite, and hydration logic.
    fn create_issue_inner(
        &self,
        db: &Database,
        create: IssueCreate<'_>,
        commit_msg: &str,
    ) -> Result<i64> {
        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let title_owned = create.title.to_string();
        let desc_owned = create.description.map(std::string::ToString::to_string);
        let priority_parsed: crate::models::Priority = create.priority.parse()?;
        let agent_id = self.agent.agent_id.clone();
        let display_id = Cell::new(0i64);
        let parent_uuid = create.parent_uuid;
        let scheduled_at = create.scheduled_at;
        let due_at = create.due_at;

        let outcome = self.write_commit_push(
            |writer| {
                let (id, counters) = writer.claim_display_id(1)?;
                display_id.set(id);
                let is_v2 = writer.layout_version() >= 2;
                let issue = IssueFile {
                    uuid,
                    display_id: Some(id),
                    title: title_owned.clone(),
                    description: desc_owned.clone(),
                    status: crate::models::IssueStatus::Open,
                    priority: priority_parsed,
                    parent_uuid,
                    created_by: agent_id.clone(),
                    created_at: now,
                    updated_at: now,
                    closed_at: None,
                    scheduled_at,
                    due_at,
                    labels: vec![],
                    comments: vec![],
                    blockers: vec![],
                    related: vec![],
                    milestone_uuid: None,
                    time_entries: vec![],
                };
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&uuid);
                if is_v2 {
                    let comments_dir = writer
                        .cache_dir
                        .join("issues")
                        .join(uuid.to_string())
                        .join("comments");
                    std::fs::create_dir_all(&comments_dir)
                        .context("Failed to create v2 comments directory")?;
                }
                let event = crate::events::Event::IssueCreated {
                    uuid,
                    title: title_owned.clone(),
                    description: desc_owned.clone(),
                    priority: priority_parsed.to_string(),
                    labels: vec![],
                    parent_uuid,
                    created_by: agent_id.clone(),
                    display_id: Some(id),
                    scheduled_at,
                    due_at,
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: Some(counters),
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            commit_msg,
        )?;

        if outcome == PushOutcome::LocalOnly {
            self.rewrite_as_offline(uuid)?;
            self.hydrate_with_retry(db);
            return db.get_issue_id_by_uuid(&uuid.to_string());
        }

        self.hydrate_with_retry(db);
        Ok(display_id.get())
    }

    /// Create a new issue: generate UUID, claim display ID, write JSON, push, hydrate.
    ///
    /// Returns the assigned display ID. `scheduled_at` / `due_at` are
    /// optional scheduling dates (GH #361); pass `None` for neither to
    /// create a dateless issue.
    ///
    /// # Errors
    ///
    /// Returns an error if UUID generation, counter claiming, JSON serialization,
    /// git operations, or hydration fail.
    pub fn create_issue(
        &self,
        db: &Database,
        title: &str,
        description: Option<&str>,
        priority: &str,
        scheduled_at: Option<DateTime<Utc>>,
        due_at: Option<DateTime<Utc>>,
    ) -> Result<i64> {
        self.create_issue_inner(
            db,
            IssueCreate {
                title,
                description,
                priority,
                parent_uuid: None,
                scheduled_at,
                due_at,
            },
            &format!("create issue: {title}"),
        )
    }

    /// Create a subissue under a parent.
    ///
    /// Returns the assigned display ID for the child. Subissues never carry
    /// scheduling dates — those are a property of the parent deliverable
    /// (GH #361, REQ-12). The CLI layer rejects `--scheduled`/`--due`
    /// when `--parent` is present; this function does not accept them.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent issue cannot be resolved, or if creation fails.
    pub fn create_subissue(
        &self,
        db: &Database,
        parent_id: i64,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
        let parent_uuid = self.resolve_uuid(parent_id, db)?;
        self.create_issue_inner(
            db,
            IssueCreate {
                title,
                description,
                priority,
                parent_uuid: Some(parent_uuid),
                scheduled_at: None,
                due_at: None,
            },
            &format!("create subissue under #{parent_id}: {title}"),
        )
    }

    /// Update an issue's title, description, status, priority, or scheduling.
    ///
    /// Unspecified fields of `update` are left unchanged. See [`IssueUpdate`]
    /// for the field-level semantics (Unchanged / Clear / Set).
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded, status/priority parsing
    /// fails, or git operations fail.
    pub fn update_issue(
        &self,
        db: &Database,
        display_id: i64,
        update: IssueUpdate<'_>,
    ) -> Result<()> {
        let title_owned = update.title.map(std::string::ToString::to_string);
        let desc_update = update.description;
        let status_parsed = update
            .status
            .map(str::parse::<crate::models::IssueStatus>)
            .transpose()?;
        let priority_parsed = update
            .priority
            .map(str::parse::<crate::models::Priority>)
            .transpose()?;
        let scheduled_at = update.scheduled_at;
        let due_at = update.due_at;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if let Some(ref t) = title_owned {
                    issue.title.clone_from(t);
                }
                match &desc_update {
                    DescriptionUpdate::Unchanged => {}
                    DescriptionUpdate::Clear => issue.description = None,
                    DescriptionUpdate::Set(s) => issue.description = Some((*s).to_string()),
                }
                if let Some(s) = status_parsed {
                    issue.status = s;
                }
                if let Some(p) = priority_parsed {
                    issue.priority = p;
                }
                let schedule_changed = !matches!(scheduled_at, FieldUpdate::Unchanged)
                    || !matches!(due_at, FieldUpdate::Unchanged);
                match scheduled_at {
                    FieldUpdate::Unchanged => {}
                    FieldUpdate::Clear => issue.scheduled_at = None,
                    FieldUpdate::Set(dt) => issue.scheduled_at = Some(dt),
                }
                match due_at {
                    FieldUpdate::Unchanged => {}
                    FieldUpdate::Clear => issue.due_at = None,
                    FieldUpdate::Set(dt) => issue.due_at = Some(dt),
                }
                issue.updated_at = Utc::now();

                // Build events (#756). Title/description/priority deltas → an
                // IssueUpdated. The reducer's IssueUpdated only carries Set (not
                // Clear) for description; a Clear is expressed by the file write,
                // which stays the source of truth until #754. Schedule edits → a
                // ScheduleChanged carrying the FULL post-change state of both
                // fields (last-writer-wins, no per-field merge).
                let mut events = Vec::new();
                let upd_description = match &desc_update {
                    DescriptionUpdate::Set(s) => Some((*s).to_string()),
                    DescriptionUpdate::Unchanged | DescriptionUpdate::Clear => None,
                };
                if title_owned.is_some() || upd_description.is_some() || priority_parsed.is_some() {
                    events.push(crate::events::Event::IssueUpdated {
                        uuid: issue.uuid,
                        title: title_owned.clone(),
                        description: upd_description,
                        priority: priority_parsed.map(|p| p.to_string()),
                    });
                }
                if schedule_changed {
                    events.push(crate::events::Event::ScheduleChanged {
                        issue_uuid: issue.uuid,
                        scheduled_at: issue.scheduled_at,
                        due_at: issue.due_at,
                    });
                }
                // A direct status change through update_issue (distinct from the
                // close_issue/reopen_issue paths) must also reach the event log
                // so it agrees with the file.
                if status_parsed.is_some() {
                    events.push(crate::events::Event::StatusChanged {
                        uuid: issue.uuid,
                        new_status: issue.status.to_string(),
                        closed_at: issue.closed_at,
                    });
                }

                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events,
                })
            },
            &format!("update issue #{display_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Close an issue (set status to "closed" and record `closed_at`).
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded or git operations fail.
    pub fn close_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                let now = Utc::now();
                issue.status = crate::models::IssueStatus::Closed;
                issue.closed_at = Some(now);
                issue.updated_at = now;
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                let event = crate::events::Event::StatusChanged {
                    uuid: issue.uuid,
                    new_status: "closed".to_string(),
                    closed_at: Some(now),
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            &format!("close issue #{display_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Reopen an issue (set status to "open", clear `closed_at`).
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded or git operations fail.
    pub fn reopen_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                issue.status = crate::models::IssueStatus::Open;
                issue.closed_at = None;
                issue.updated_at = Utc::now();
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                let event = crate::events::Event::StatusChanged {
                    uuid: issue.uuid,
                    new_status: "open".to_string(),
                    closed_at: None,
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            &format!("reopen issue #{display_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Delete an issue JSON file from the coordination branch.
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be found or git operations fail.
    pub fn delete_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let issue = self.load_issue_by_id(display_id, db)?;
        let uuid = issue.uuid;

        let _ = self.write_commit_push(
            |writer| {
                // Don't delete files here — let `git rm -r` in the staging
                // loop handle both index and disk removal so the commit
                // failure path can restore from HEAD (#427).
                let event = crate::events::Event::IssueDeleted { uuid };
                if writer.layout_version() >= 2 {
                    // V2: pass the directory path so git rm -r removes
                    // issue.json + comments/ recursively (#460)
                    Ok(WriteSet {
                        files: vec![(format!("issues/{uuid}"), vec![])],
                        counters: None,
                        use_git_rm: true,
                        events: vec![event],
                    })
                } else {
                    // V1: pass the flat file path
                    Ok(WriteSet {
                        files: vec![(format!("issues/{uuid}.json"), vec![])],
                        counters: None,
                        use_git_rm: true,
                        events: vec![event],
                    })
                }
            },
            &format!("delete issue #{display_id}"),
        )?;

        // Post-commit cleanup: remove any untracked remnants (e.g. comment
        // files created between commits that git rm didn't know about). Safe
        // to do now because the commit already succeeded (#460).
        let issue_dir = self.cache_dir.join("issues").join(uuid.to_string());
        if issue_dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&issue_dir) {
                tracing::debug!(
                    "post-delete cleanup of {} failed: {}",
                    issue_dir.display(),
                    e
                );
            }
        }
        let v1_path = self.cache_dir.join(format!("issues/{uuid}.json"));
        if v1_path.exists() {
            if let Err(e) = std::fs::remove_file(&v1_path) {
                tracing::debug!("post-delete cleanup of {} failed: {}", v1_path.display(), e);
            }
        }

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Internal helper: add a comment to an issue with the given parameters.
    ///
    /// Handles counter claiming, signing, and V1/V2 layout dispatch.
    fn add_comment_inner(
        &self,
        db: &Database,
        display_id: i64,
        params: &CommentParams,
        commit_msg: &str,
    ) -> Result<i64> {
        let agent_id = self.agent.agent_id.clone();
        let comment_id = Cell::new(0i64);

        let _ = self.write_commit_push(
            |writer| {
                let mut counters = writer.read_counters()?;
                let id = counters.next_comment_id;
                counters.next_comment_id += 1;
                comment_id.set(id);

                let (signed_by, signature) = writer.sign_comment(&params.content, &agent_id, id);

                // One timestamp + one comment uuid shared by the file write and
                // the CommentAdded event so they match exactly (#756). In V1 the
                // uuid is not stored in the file (CommentEntry has only `id`); it
                // exists solely as the event's idempotency key.
                let created_at = Utc::now();
                let comment_uuid = Uuid::new_v4();
                // Clone signing material for the event up-front so the file
                // structs below can move the originals.
                let ev_signed_by = signed_by.clone();
                let ev_signature = signature.clone();
                let make_event = |issue_uuid: Uuid| crate::events::Event::CommentAdded {
                    issue_uuid,
                    comment_uuid,
                    display_id: Some(id),
                    author: agent_id.clone(),
                    content: params.content.clone(),
                    created_at,
                    kind: params.kind.clone(),
                    trigger_type: params.trigger_type.clone(),
                    intervention_context: params.intervention_context.clone(),
                    driver_key_fingerprint: params.driver_key_fingerprint.clone(),
                    signed_by: ev_signed_by.clone(),
                    signature: ev_signature.clone(),
                };

                if writer.layout_version() >= 2 {
                    let issue = writer.load_issue_by_id(display_id, db)?;
                    let comment_file = CommentFile {
                        uuid: comment_uuid,
                        issue_uuid: issue.uuid,
                        author: agent_id.clone(),
                        content: params.content.clone(),
                        created_at,
                        kind: params.kind.clone(),
                        trigger_type: params.trigger_type.clone(),
                        intervention_context: params.intervention_context.clone(),
                        driver_key_fingerprint: params.driver_key_fingerprint.clone(),
                        signed_by,
                        signature,
                    };
                    let json = serde_json::to_vec_pretty(&comment_file)?;
                    let rel_path = Self::comment_rel_path(&issue.uuid, &comment_uuid);
                    let event = make_event(issue.uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        use_git_rm: false,
                        events: vec![event],
                    })
                } else {
                    let mut issue = writer.load_issue_by_id(display_id, db)?;
                    issue.comments.push(CommentEntry {
                        id,
                        author: agent_id.clone(),
                        content: params.content.clone(),
                        created_at,
                        kind: params.kind.clone(),
                        trigger_type: params.trigger_type.clone(),
                        intervention_context: params.intervention_context.clone(),
                        driver_key_fingerprint: params.driver_key_fingerprint.clone(),
                        signed_by,
                        signature,
                    });
                    issue.updated_at = Utc::now();

                    let json = serde_json::to_vec_pretty(&issue)?;
                    let rel_path = writer.issue_rel_path(&issue.uuid);
                    let event = make_event(issue.uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        events: vec![event],
                        use_git_rm: false,
                    })
                }
            },
            commit_msg,
        )?;

        self.hydrate_with_retry(db);
        Ok(comment_id.get())
    }

    /// Add a comment to an issue.
    ///
    /// Returns the comment ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded or git operations fail.
    pub fn add_comment(
        &self,
        db: &Database,
        display_id: i64,
        content: &str,
        kind: &str,
    ) -> Result<i64> {
        self.add_comment_inner(
            db,
            display_id,
            &CommentParams {
                content: content.to_string(),
                kind: kind.to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
            },
            &format!("comment on issue #{display_id}"),
        )
    }

    /// Add a driver intervention comment to an issue (kind = "intervention").
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded or git operations fail.
    pub fn add_intervention_comment(
        &self,
        db: &Database,
        display_id: i64,
        content: &str,
        trigger_type: &str,
        intervention_context: Option<&str>,
        driver_key_fingerprint: Option<&str>,
    ) -> Result<i64> {
        self.add_comment_inner(
            db,
            display_id,
            &CommentParams {
                content: content.to_string(),
                kind: super::core::KIND_INTERVENTION.to_string(),
                trigger_type: Some(trigger_type.to_string()),
                intervention_context: intervention_context.map(std::string::ToString::to_string),
                driver_key_fingerprint: driver_key_fingerprint
                    .map(std::string::ToString::to_string),
            },
            &format!("intervention on issue #{display_id}"),
        )
    }

    /// Add a label to an issue.
    ///
    /// Returns `Ok(true)` if the label was newly added, `Ok(false)` if the
    /// issue already carried the label (no-op short-circuit).
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded or git operations fail.
    pub fn add_label(&self, db: &Database, display_id: i64, label: &str) -> Result<bool> {
        let label_owned = label.to_string();

        // Idempotency short-circuit (#600): if the label is already present,
        // serializing the unchanged issue would hand `write_commit_push` an
        // identical file, which git rejects with "nothing to commit". Skip
        // git entirely and report no-op via the boolean return.
        let current = self.load_issue_by_id(display_id, db)?;
        if current.labels.contains(&label_owned) {
            return Ok(false);
        }

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if !issue.labels.contains(&label_owned) {
                    issue.labels.push(label_owned.clone());
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                let event = crate::events::Event::LabelAdded {
                    issue_uuid: issue.uuid,
                    label: label_owned.clone(),
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            &format!("label issue #{display_id} with {label}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(true)
    }

    /// Remove a label from an issue.
    ///
    /// Returns `Ok(true)` if the label was removed, `Ok(false)` if the issue
    /// did not carry the label (no-op short-circuit).
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded or git operations fail.
    pub fn remove_label(&self, db: &Database, display_id: i64, label: &str) -> Result<bool> {
        let label_owned = label.to_string();

        // Idempotency short-circuit (#600): if the label is absent, skip the
        // write entirely to avoid an empty git commit.
        let current = self.load_issue_by_id(display_id, db)?;
        if !current.labels.contains(&label_owned) {
            return Ok(false);
        }

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if let Some(pos) = issue.labels.iter().position(|l| l == &label_owned) {
                    issue.labels.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                let event = crate::events::Event::LabelRemoved {
                    issue_uuid: issue.uuid,
                    label: label_owned.clone(),
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            &format!("unlabel {label} from issue #{display_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(true)
    }

    /// Add a blocker dependency: `issue_id` is blocked by `blocking_issue_id`.
    ///
    /// Only modifies the blocked issue's file (single-direction storage).
    ///
    /// Returns `Ok(true)` if the blocker was newly added, `Ok(false)` if it
    /// was already recorded (no-op short-circuit).
    ///
    /// # Errors
    ///
    /// Returns an error if either issue cannot be resolved or git operations fail.
    pub fn add_blocker(
        &self,
        db: &Database,
        issue_id: i64,
        blocking_issue_id: i64,
    ) -> Result<bool> {
        let blocker_uuid = self.resolve_uuid(blocking_issue_id, db)?;

        // Idempotency short-circuit (#600): if the blocker is already
        // recorded, the closure would serialize an identical issue file and
        // `git commit` would fail with "nothing to commit".
        let current = self.load_issue_by_id(issue_id, db)?;
        if current.blockers.contains(&blocker_uuid) {
            return Ok(false);
        }

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if !issue.blockers.contains(&blocker_uuid) {
                    issue.blockers.push(blocker_uuid);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                // Reducer convention (apply_graph_event): DependencyAdded inserts
                // `blocker_uuid` into `blocked_uuid`'s blockers. Here `issue` is
                // the blocked issue and `blocker_uuid` the blocking one.
                let event = crate::events::Event::DependencyAdded {
                    blocked_uuid: issue.uuid,
                    blocker_uuid,
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            &format!("block issue #{issue_id} on #{blocking_issue_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(true)
    }

    /// Remove a blocker dependency.
    ///
    /// Returns `Ok(true)` if the blocker was removed, `Ok(false)` if the
    /// blocker was not present (no-op short-circuit).
    ///
    /// # Errors
    ///
    /// Returns an error if either issue cannot be resolved or git operations fail.
    pub fn remove_blocker(
        &self,
        db: &Database,
        issue_id: i64,
        blocking_issue_id: i64,
    ) -> Result<bool> {
        let blocker_uuid = self.resolve_uuid(blocking_issue_id, db)?;

        // Idempotency short-circuit (#600): if the blocker is absent, skip
        // the write entirely to avoid an empty git commit.
        let current = self.load_issue_by_id(issue_id, db)?;
        if !current.blockers.contains(&blocker_uuid) {
            return Ok(false);
        }

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if let Some(pos) = issue.blockers.iter().position(|u| u == &blocker_uuid) {
                    issue.blockers.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                let event = crate::events::Event::DependencyRemoved {
                    blocked_uuid: issue.uuid,
                    blocker_uuid,
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            &format!("unblock issue #{issue_id} from #{blocking_issue_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(true)
    }

    /// Add a relation between two issues (single-direction storage).
    ///
    /// Returns `Ok(true)` if the relation was newly added, `Ok(false)` if
    /// it was already recorded (no-op short-circuit).
    ///
    /// # Errors
    ///
    /// Returns an error if either issue cannot be resolved or git operations fail.
    pub fn add_relation(&self, db: &Database, issue_id: i64, related_id: i64) -> Result<bool> {
        let related_uuid = self.resolve_uuid(related_id, db)?;

        // Idempotency short-circuit (#600): if the relation is already
        // recorded, skip the write entirely to avoid an empty git commit.
        let current = self.load_issue_by_id(issue_id, db)?;
        if current.related.contains(&related_uuid) {
            return Ok(false);
        }

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if !issue.related.contains(&related_uuid) {
                    issue.related.push(related_uuid);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                let event = crate::events::Event::RelationAdded {
                    uuid_a: issue.uuid,
                    uuid_b: related_uuid,
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            &format!("relate issue #{issue_id} to #{related_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(true)
    }

    /// Remove a relation between two issues.
    ///
    /// Returns `Ok(true)` if the relation was removed, `Ok(false)` if no
    /// such relation existed (no-op short-circuit).
    ///
    /// # Errors
    ///
    /// Returns an error if either issue cannot be resolved or git operations fail.
    pub fn remove_relation(&self, db: &Database, issue_id: i64, related_id: i64) -> Result<bool> {
        let related_uuid = self.resolve_uuid(related_id, db)?;

        // Idempotency short-circuit (#600): if the relation is absent, skip
        // the write entirely to avoid an empty git commit.
        let current = self.load_issue_by_id(issue_id, db)?;
        if !current.related.contains(&related_uuid) {
            return Ok(false);
        }

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if let Some(pos) = issue.related.iter().position(|u| u == &related_uuid) {
                    issue.related.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                let event = crate::events::Event::RelationRemoved {
                    uuid_a: issue.uuid,
                    uuid_b: related_uuid,
                };
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                    events: vec![event],
                })
            },
            &format!("unrelate issue #{issue_id} from #{related_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(true)
    }

    /// Rewrite a just-committed issue to set `display_id: null` and revert the
    /// counter bump. Used when a push failed (offline/exhausted retries) so the
    /// locally-claimed display ID doesn't collide with remote state.
    ///
    /// # Event log consistency (#756)
    ///
    /// The same local commit also carries an `IssueCreated` event line claiming
    /// `display_id: Some(claimed)` (emitted by `write_commit_push` in the same
    /// commit). The display-id claim was revoked here, so the committed event
    /// line is now wrong: if it survived, the reducer would adopt the claimed id
    /// (first-claim-wins) while the file says `None`, violating the invariant
    /// "the event log's `display_id` claims match what the files claim". The
    /// local commit's working-tree log line survives the `LocalOnly` path (no
    /// reset happened — push merely failed), so we REWRITE that trailing line in
    /// place to `display_id: None` and fold it into the same `--amend`. The `hub_v3`
    /// shadow ref is best-effort and reconciles on the next successful compaction;
    /// the authoritative v2 log is corrected here.
    pub(super) fn rewrite_as_offline(&self, uuid: Uuid) -> Result<()> {
        // Serialize access to the hub cache (#373)
        let _lock_guard = self.sync.acquire_lock()?;

        let path = self.issue_path(&uuid);
        let mut issue = crate::issue_file::read_issue_file(&path)?;
        issue.display_id = None;
        let json = serde_json::to_string_pretty(&issue)?;
        // Use atomic_write to avoid the open-truncate-write race (#604, #750).
        // std::fs::write would truncate the file before writing; a crash mid-write
        // leaves a zero-byte or partial JSON that breaks all subsequent reads.
        crate::utils::atomic_write(&path, json.as_bytes())?;

        // Revoke the display-id claim in the trailing IssueCreated event line so
        // the event log agrees with the file (#756).
        let log_rewritten = self.set_issue_created_claim_in_log(uuid, None)?;

        // Revert the counter bump (the remote never saw it)
        let mut counters = self.read_counters()?;
        if counters.next_display_id > 1 {
            counters.next_display_id -= 1;
        }
        self.write_counters_to_cache(&counters)?;

        // Amend the local commit with the reverted files
        let rel_path = self.issue_rel_path(&uuid);
        self.git_in_cache(&["add", &rel_path])?;
        self.git_in_cache(&["add", "meta/counters.json"])?;
        if log_rewritten {
            let rel_log = format!("agents/{}/events.log", self.agent.agent_id);
            self.git_in_cache(&["add", &rel_log])?;
        }
        self.git_commit_in_cache_with_args(&["--amend", "--no-edit"])?;
        Ok(())
    }

    /// Rewrite the most recent `IssueCreated` event line for `uuid` in this
    /// agent's event log to carry `display_id: new_claim`. Returns `true` if a
    /// line was rewritten, `false` if no matching line was found (e.g. the event
    /// was already pruned by compaction).
    ///
    /// Used to keep the event log's display-id claim consistent with the file
    /// across the offline lifecycle (#756): `rewrite_as_offline` passes `None`
    /// (the claim was revoked), and `promote_offline_issues` passes the
    /// newly-claimed `Some(id)` so the reduced display id matches the promoted
    /// file. Matches the trailing line so re-runs in a retry loop are idempotent.
    pub(super) fn set_issue_created_claim_in_log(
        &self,
        uuid: Uuid,
        new_claim: Option<i64>,
    ) -> Result<bool> {
        let log_path = self.event_log_path();
        let mut envelopes = crate::events::read_events(&log_path)?;

        // Find the last IssueCreated for this uuid and set its display_id.
        let mut rewrote = false;
        for env in envelopes.iter_mut().rev() {
            let is_target = matches!(
                &env.event,
                crate::events::Event::IssueCreated { uuid: ev_uuid, .. } if *ev_uuid == uuid
            );
            if !is_target {
                continue;
            }
            if let crate::events::Event::IssueCreated { display_id, .. } = &mut env.event {
                if *display_id != new_claim {
                    *display_id = new_claim;
                    rewrote = true;

                    // The payload changed, so any existing signature is now
                    // invalid — compaction would flag the event as tampered.
                    // Clear it, then re-sign with our own key when configured
                    // (this is our event; re-signing is legitimate). If no key
                    // is available the event becomes honestly unsigned.
                    env.signed_by = None;
                    env.signature = None;
                    if let (Some(key_path), Some(fingerprint)) = (
                        self.resolve_ssh_key_path(),
                        self.agent.ssh_fingerprint.as_ref(),
                    ) {
                        if let Err(e) = crate::events::sign_event(env, &key_path, fingerprint) {
                            tracing::warn!(
                                "re-signing rewritten IssueCreated claim failed \
                                 (event left unsigned): {e}"
                            );
                        }
                    }
                }
            }
            break;
        }

        if rewrote {
            use crate::events::EventCodec;
            let codec = crate::events::NdjsonCodec;
            let bytes = codec.encode_batch(&envelopes)?;
            crate::utils::atomic_write(&log_path, &bytes)?;

            // Keep the hub v3 shadow ref at byte parity: the original claim
            // line was already mirrored at append time, so the rewritten log
            // must be committed onto the ref too (as a child commit — pushes
            // stay fast-forward). Best-effort, same policy as the mirror.
            if self.hub_v3_dual_write {
                if let Err(e) = crate::hub_v3::commit_log_bytes(
                    &self.cache_dir,
                    &self.agent.agent_id,
                    &bytes,
                    "crosslink claim rewrite",
                ) {
                    tracing::warn!("hub v3 shadow claim-rewrite failed: {e}");
                }
            }
        }
        Ok(rewrote)
    }
}
