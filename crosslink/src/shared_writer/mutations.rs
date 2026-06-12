//! Issue mutation operations: create, update, close, reopen, delete,
//! comments, labels, blockers, and relations.
//!
//! # Event-only write model (hub v3, #754)
//!
//! Each mutation builds a [`WriteSet`] of events and routes it through
//! [`crate::shared_writer::SharedWriter::write_commit_push`], which appends the
//! events to the agent's own ref ([`crate::hub_v3`]) and pushes it
//! fast-forward. There are no worktree `JSON` files: display ids are assigned by
//! the deterministic reduction (REQ-4) and read back from the reduced
//! [`crate::checkpoint::CheckpointState`], and `SQLite` is a derived cache
//! re-hydrated from that state after every successful mutation. Mutations on a
//! legacy v2 hub are refused with a migrate prompt (the v2 write path is gone).

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::cell::Cell;
use uuid::Uuid;

use crate::db::Database;

use super::core::{SharedWriter, WriteSet};

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
    /// Shared by `create_issue` and `create_subissue`. The display id is
    /// reduction-assigned (REQ-4): the emitted `IssueCreated` carries
    /// `display_id: None` and the id is read back from the reduced state (or
    /// freshly-hydrated `SQLite` when the id is still provisional).
    fn create_issue_inner(
        &self,
        db: &Database,
        create: IssueCreate<'_>,
        commit_msg: &str,
    ) -> Result<i64> {
        let uuid = Uuid::new_v4();
        let title_owned = create.title.to_string();
        let desc_owned = create.description.map(std::string::ToString::to_string);
        let priority_parsed: crate::models::Priority = create.priority.parse()?;
        let agent_id = self.agent.agent_id.clone();
        let parent_uuid = create.parent_uuid;
        let scheduled_at = create.scheduled_at;
        let due_at = create.due_at;

        self.write_commit_push(
            |_writer| {
                let event = crate::events::Event::IssueCreated {
                    uuid,
                    title: title_owned.clone(),
                    description: desc_owned.clone(),
                    priority: priority_parsed.to_string(),
                    labels: vec![],
                    parent_uuid,
                    created_by: agent_id.clone(),
                    display_id: None,
                    scheduled_at,
                    due_at,
                };
                Ok(WriteSet {
                    events: vec![event],
                })
            },
            commit_msg,
        )?;

        self.hydrate_with_retry(db);
        if let Some(id) = self.v3_assigned_display_id(&uuid) {
            return Ok(id);
        }
        db.get_issue_id_by_uuid(&uuid.to_string())
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

                // Build events (REQ-4). Title/description/priority deltas → an
                // IssueUpdated. The reducer's IssueUpdated carries only Set (not
                // Clear) for description.
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
                // close_issue/reopen_issue paths) must also reach the event log.
                if status_parsed.is_some() {
                    events.push(crate::events::Event::StatusChanged {
                        uuid: issue.uuid,
                        new_status: issue.status.to_string(),
                        closed_at: issue.closed_at,
                    });
                }

                Ok(WriteSet { events })
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
                let issue = writer.load_issue_by_id(display_id, db)?;
                let now = Utc::now();
                let event = crate::events::Event::StatusChanged {
                    uuid: issue.uuid,
                    new_status: "closed".to_string(),
                    closed_at: Some(now),
                };
                Ok(WriteSet {
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
                let issue = writer.load_issue_by_id(display_id, db)?;
                let event = crate::events::Event::StatusChanged {
                    uuid: issue.uuid,
                    new_status: "open".to_string(),
                    closed_at: None,
                };
                Ok(WriteSet {
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
            |_writer| {
                let event = crate::events::Event::IssueDeleted { uuid };
                Ok(WriteSet {
                    events: vec![event],
                })
            },
            &format!("delete issue #{display_id}"),
        )?;

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
        // Capture the comment uuid so the id can be resolved from the reduced
        // state after hydration (the event-only path mints no counter id).
        let comment_uuid_cell: Cell<Option<Uuid>> = Cell::new(None);

        let _ = self.write_commit_push(
            |writer| {
                let issue = writer.load_issue_by_id(display_id, db)?;

                // The comment uuid is the event's idempotency key; the display
                // id is reduction-assigned (REQ-4), so the event carries `None`.
                let created_at = Utc::now();
                let comment_uuid = Uuid::new_v4();
                comment_uuid_cell.set(Some(comment_uuid));

                // Sign over a provisional id of 0: the reduction assigns the
                // authoritative comment id, and the signature attests the
                // content/author rather than the id.
                let (signed_by, signature) = writer.sign_comment(&params.content, &agent_id, 0);

                let event = crate::events::Event::CommentAdded {
                    issue_uuid: issue.uuid,
                    comment_uuid,
                    display_id: None,
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
                Ok(WriteSet {
                    events: vec![event],
                })
            },
            commit_msg,
        )?;

        self.hydrate_with_retry(db);
        // The comment id is reduction-assigned (REQ-4). Resolve it from the
        // reduced state via the captured comment uuid; fall back to a SQLite
        // lookup when reduction has not yet frozen an id (provisional).
        if let Some(cuuid) = comment_uuid_cell.get() {
            if let Some(id) = self.v3_assigned_comment_id(display_id, &cuuid) {
                return Ok(id);
            }
            return db.get_comment_id_by_uuid(&cuuid.to_string());
        }
        // Unreachable in practice: the closure always sets the uuid. Surface a
        // diagnostic rather than a misleading id if the invariant ever breaks.
        anyhow::bail!("comment uuid was not captured during write")
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
                let issue = writer.load_issue_by_id(display_id, db)?;
                let event = crate::events::Event::LabelAdded {
                    issue_uuid: issue.uuid,
                    label: label_owned.clone(),
                };
                Ok(WriteSet {
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
                let issue = writer.load_issue_by_id(display_id, db)?;
                let event = crate::events::Event::LabelRemoved {
                    issue_uuid: issue.uuid,
                    label: label_owned.clone(),
                };
                Ok(WriteSet {
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
                let issue = writer.load_issue_by_id(issue_id, db)?;
                // Reducer convention (apply_graph_event): DependencyAdded inserts
                // `blocker_uuid` into `blocked_uuid`'s blockers. Here `issue` is
                // the blocked issue and `blocker_uuid` the blocking one.
                let event = crate::events::Event::DependencyAdded {
                    blocked_uuid: issue.uuid,
                    blocker_uuid,
                };
                Ok(WriteSet {
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
                let issue = writer.load_issue_by_id(issue_id, db)?;
                let event = crate::events::Event::DependencyRemoved {
                    blocked_uuid: issue.uuid,
                    blocker_uuid,
                };
                Ok(WriteSet {
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
                let issue = writer.load_issue_by_id(issue_id, db)?;
                let event = crate::events::Event::RelationAdded {
                    uuid_a: issue.uuid,
                    uuid_b: related_uuid,
                };
                Ok(WriteSet {
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
                let issue = writer.load_issue_by_id(issue_id, db)?;
                let event = crate::events::Event::RelationRemoved {
                    uuid_a: issue.uuid,
                    uuid_b: related_uuid,
                };
                Ok(WriteSet {
                    events: vec![event],
                })
            },
            &format!("unrelate issue #{issue_id} from #{related_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(true)
    }
}
