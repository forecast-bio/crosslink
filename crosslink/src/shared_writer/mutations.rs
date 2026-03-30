//! Issue mutation operations: create, update, close, reopen, delete,
//! comments, labels, blockers, and relations.

use anyhow::{Context, Result};
use chrono::Utc;
use std::cell::Cell;
use uuid::Uuid;

use crate::db::Database;
use crate::issue_file::{CommentEntry, CommentFile, IssueFile};

use super::core::{PushOutcome, SharedWriter, WriteSet};

/// Represents an update to a description field with three possible states:
/// unchanged, cleared, or set to a new value.
pub enum DescriptionUpdate<'a> {
    /// Do not modify the description.
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
        title: &str,
        description: Option<&str>,
        priority: &str,
        parent_uuid: Option<Uuid>,
        commit_msg: &str,
    ) -> Result<i64> {
        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let title_owned = title.to_string();
        let desc_owned = description.map(std::string::ToString::to_string);
        let priority_parsed: crate::models::Priority = priority.parse()?;
        let agent_id = self.agent.agent_id.clone();
        let display_id = Cell::new(0i64);

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
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: Some(counters),
                    use_git_rm: false,
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
    /// Returns the assigned display ID.
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
    ) -> Result<i64> {
        self.create_issue_inner(
            db,
            title,
            description,
            priority,
            None,
            &format!("create issue: {title}"),
        )
    }

    /// Create a subissue under a parent.
    ///
    /// Returns the assigned display ID for the child.
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
            title,
            description,
            priority,
            Some(parent_uuid),
            &format!("create subissue under #{parent_id}: {title}"),
        )
    }

    /// Update an issue's title, description, status, or priority.
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded, status/priority parsing
    /// fails, or git operations fail.
    pub fn update_issue(
        &self,
        db: &Database,
        display_id: i64,
        title: Option<&str>,
        description: DescriptionUpdate<'_>,
        status: Option<&str>,
        priority: Option<&str>,
    ) -> Result<()> {
        let title_owned = title.map(std::string::ToString::to_string);
        let desc_update = description;
        let status_parsed = status
            .map(str::parse::<crate::models::IssueStatus>)
            .transpose()?;
        let priority_parsed = priority
            .map(str::parse::<crate::models::Priority>)
            .transpose()?;

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
                issue.updated_at = Utc::now();
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
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
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
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
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
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
                if writer.layout_version() >= 2 {
                    // V2: pass the directory path so git rm -r removes
                    // issue.json + comments/ recursively (#460)
                    Ok(WriteSet {
                        files: vec![(format!("issues/{uuid}"), vec![])],
                        counters: None,
                        use_git_rm: true,
                    })
                } else {
                    // V1: pass the flat file path
                    Ok(WriteSet {
                        files: vec![(format!("issues/{uuid}.json"), vec![])],
                        counters: None,
                        use_git_rm: true,
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

                if writer.layout_version() >= 2 {
                    let issue = writer.load_issue_by_id(display_id, db)?;
                    let comment_uuid = Uuid::new_v4();
                    let comment_file = CommentFile {
                        uuid: comment_uuid,
                        issue_uuid: issue.uuid,
                        author: agent_id.clone(),
                        content: params.content.clone(),
                        created_at: Utc::now(),
                        kind: params.kind.clone(),
                        trigger_type: params.trigger_type.clone(),
                        intervention_context: params.intervention_context.clone(),
                        driver_key_fingerprint: params.driver_key_fingerprint.clone(),
                        signed_by,
                        signature,
                    };
                    let json = serde_json::to_vec_pretty(&comment_file)?;
                    let rel_path = Self::comment_rel_path(&issue.uuid, &comment_uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        use_git_rm: false,
                    })
                } else {
                    let mut issue = writer.load_issue_by_id(display_id, db)?;
                    issue.comments.push(CommentEntry {
                        id,
                        author: agent_id.clone(),
                        content: params.content.clone(),
                        created_at: Utc::now(),
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
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
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
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded or git operations fail.
    pub fn add_label(&self, db: &Database, display_id: i64, label: &str) -> Result<()> {
        let label_owned = label.to_string();

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if !issue.labels.contains(&label_owned) {
                    issue.labels.push(label_owned.clone());
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("label issue #{display_id} with {label}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Remove a label from an issue.
    ///
    /// # Errors
    ///
    /// Returns an error if the issue cannot be loaded or git operations fail.
    pub fn remove_label(&self, db: &Database, display_id: i64, label: &str) -> Result<()> {
        let label_owned = label.to_string();

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if let Some(pos) = issue.labels.iter().position(|l| l == &label_owned) {
                    issue.labels.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("unlabel {label} from issue #{display_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Add a blocker dependency: `issue_id` is blocked by `blocking_issue_id`.
    ///
    /// Only modifies the blocked issue's file (single-direction storage).
    ///
    /// # Errors
    ///
    /// Returns an error if either issue cannot be resolved or git operations fail.
    pub fn add_blocker(&self, db: &Database, issue_id: i64, blocking_issue_id: i64) -> Result<()> {
        let blocker_uuid = self.resolve_uuid(blocking_issue_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if !issue.blockers.contains(&blocker_uuid) {
                    issue.blockers.push(blocker_uuid);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("block issue #{issue_id} on #{blocking_issue_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Remove a blocker dependency.
    ///
    /// # Errors
    ///
    /// Returns an error if either issue cannot be resolved or git operations fail.
    pub fn remove_blocker(
        &self,
        db: &Database,
        issue_id: i64,
        blocking_issue_id: i64,
    ) -> Result<()> {
        let blocker_uuid = self.resolve_uuid(blocking_issue_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if let Some(pos) = issue.blockers.iter().position(|u| u == &blocker_uuid) {
                    issue.blockers.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("unblock issue #{issue_id} from #{blocking_issue_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Add a relation between two issues (single-direction storage).
    ///
    /// # Errors
    ///
    /// Returns an error if either issue cannot be resolved or git operations fail.
    pub fn add_relation(&self, db: &Database, issue_id: i64, related_id: i64) -> Result<()> {
        let related_uuid = self.resolve_uuid(related_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if !issue.related.contains(&related_uuid) {
                    issue.related.push(related_uuid);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("relate issue #{issue_id} to #{related_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Remove a relation between two issues.
    ///
    /// # Errors
    ///
    /// Returns an error if either issue cannot be resolved or git operations fail.
    pub fn remove_relation(&self, db: &Database, issue_id: i64, related_id: i64) -> Result<()> {
        let related_uuid = self.resolve_uuid(related_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if let Some(pos) = issue.related.iter().position(|u| u == &related_uuid) {
                    issue.related.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("unrelate issue #{issue_id} from #{related_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Rewrite a just-committed issue to set `display_id: null` and revert the
    /// counter bump. Used when a push failed (offline/exhausted retries) so the
    /// locally-claimed display ID doesn't collide with remote state.
    pub(super) fn rewrite_as_offline(&self, uuid: Uuid) -> Result<()> {
        // Serialize access to the hub cache (#373)
        let _lock_guard = self.sync.acquire_lock()?;

        let path = self.issue_path(&uuid);
        let mut issue = crate::issue_file::read_issue_file(&path)?;
        issue.display_id = None;
        let json = serde_json::to_string_pretty(&issue)?;
        std::fs::write(&path, json)?;

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
        self.git_in_cache(&["commit", "--amend", "--no-edit"])?;
        Ok(())
    }
}
