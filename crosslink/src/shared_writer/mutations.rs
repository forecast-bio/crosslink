//! Issue mutation operations: create, update, close, reopen, delete,
//! comments, labels, blockers, and relations.

use anyhow::{Context, Result};
use chrono::Utc;
use std::cell::Cell;
use uuid::Uuid;

use crate::db::Database;
use crate::issue_file::{CommentEntry, CommentFile, IssueFile};

use super::core::{PushOutcome, SharedWriter, WriteSet};

impl SharedWriter {
    /// Create a new issue: generate UUID, claim display ID, write JSON, push, hydrate.
    ///
    /// Returns the assigned display ID.
    pub fn create_issue(
        &self,
        db: &Database,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let title_owned = title.to_string();
        let desc_owned = description.map(|s| s.to_string());
        let priority_owned = priority.to_string();
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
                    status: "open".to_string(),
                    priority: priority_owned.clone(),
                    parent_uuid: None,
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
                    // Ensure the comments subdirectory exists for v2 layout
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
            &format!("create issue: {}", title),
        )?;

        if outcome == PushOutcome::LocalOnly {
            self.rewrite_as_offline(uuid)?;
            self.hydrate_with_retry(db)?;
            return db.get_issue_id_by_uuid(&uuid.to_string());
        }

        self.hydrate_with_retry(db)?;
        Ok(display_id.get())
    }

    /// Create a subissue under a parent.
    ///
    /// Returns the assigned display ID for the child.
    pub fn create_subissue(
        &self,
        db: &Database,
        parent_id: i64,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
        let parent_uuid = self.resolve_uuid(parent_id, db)?;
        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let title_owned = title.to_string();
        let desc_owned = description.map(|s| s.to_string());
        let priority_owned = priority.to_string();
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
                    status: "open".to_string(),
                    priority: priority_owned.clone(),
                    parent_uuid: Some(parent_uuid),
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
            &format!("create subissue under #{}: {}", parent_id, title),
        )?;

        if outcome == PushOutcome::LocalOnly {
            self.rewrite_as_offline(uuid)?;
            self.hydrate_with_retry(db)?;
            return db.get_issue_id_by_uuid(&uuid.to_string());
        }

        self.hydrate_with_retry(db)?;
        Ok(display_id.get())
    }

    /// Update an issue's title, description, status, or priority.
    pub fn update_issue(
        &self,
        db: &Database,
        display_id: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        status: Option<&str>,
        priority: Option<&str>,
    ) -> Result<()> {
        let title_owned = title.map(|s| s.to_string());
        let desc_owned = description.map(|d| d.map(|s| s.to_string()));
        let status_owned = status.map(|s| s.to_string());
        let priority_owned = priority.map(|s| s.to_string());

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if let Some(ref t) = title_owned {
                    issue.title = t.clone();
                }
                if let Some(ref d) = desc_owned {
                    issue.description = d.clone();
                }
                if let Some(ref s) = status_owned {
                    issue.status = s.clone();
                }
                if let Some(ref p) = priority_owned {
                    issue.priority = p.clone();
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
            &format!("update issue #{}", display_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Close an issue (set status to "closed" and record closed_at).
    pub fn close_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                let now = Utc::now();
                issue.status = "closed".to_string();
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
            &format!("close issue #{}", display_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Reopen an issue (set status to "open", clear closed_at).
    pub fn reopen_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                issue.status = "open".to_string();
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
            &format!("reopen issue #{}", display_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Delete an issue JSON file from the coordination branch.
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
                        files: vec![(format!("issues/{}", uuid), vec![])],
                        counters: None,
                        use_git_rm: true,
                    })
                } else {
                    // V1: pass the flat file path
                    Ok(WriteSet {
                        files: vec![(format!("issues/{}.json", uuid), vec![])],
                        counters: None,
                        use_git_rm: true,
                    })
                }
            },
            &format!("delete issue #{}", display_id),
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
        let v1_path = self.cache_dir.join(format!("issues/{}.json", uuid));
        if v1_path.exists() {
            if let Err(e) = std::fs::remove_file(&v1_path) {
                tracing::debug!("post-delete cleanup of {} failed: {}", v1_path.display(), e);
            }
        }

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Add a comment to an issue.
    ///
    /// Returns the comment ID.
    pub fn add_comment(
        &self,
        db: &Database,
        display_id: i64,
        content: &str,
        kind: &str,
    ) -> Result<i64> {
        let content_owned = content.to_string();
        let kind_owned = kind.to_string();
        let agent_id = self.agent.agent_id.clone();
        let comment_id = Cell::new(0i64);

        let _ = self.write_commit_push(
            |writer| {
                let mut counters = writer.read_counters()?;
                let id = counters.next_comment_id;
                counters.next_comment_id += 1;
                comment_id.set(id);

                let (signed_by, signature) = writer.sign_comment(&content_owned, &agent_id, id);

                if writer.layout_version() >= 2 {
                    // V2: write a standalone comment file, don't modify the issue file
                    let issue = writer.load_issue_by_id(display_id, db)?;
                    let comment_uuid = Uuid::new_v4();
                    let comment_file = CommentFile {
                        uuid: comment_uuid,
                        issue_uuid: issue.uuid,
                        author: agent_id.clone(),
                        content: content_owned.clone(),
                        created_at: Utc::now(),
                        kind: kind_owned.clone(),
                        trigger_type: None,
                        intervention_context: None,
                        driver_key_fingerprint: None,
                        signed_by,
                        signature,
                    };
                    let json = serde_json::to_vec_pretty(&comment_file)?;
                    let rel_path = writer.comment_rel_path(&issue.uuid, &comment_uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        use_git_rm: false,
                    })
                } else {
                    // V1: append comment inline to the issue file
                    let mut issue = writer.load_issue_by_id(display_id, db)?;
                    issue.comments.push(CommentEntry {
                        id,
                        author: agent_id.clone(),
                        content: content_owned.clone(),
                        created_at: Utc::now(),
                        kind: kind_owned.clone(),
                        trigger_type: None,
                        intervention_context: None,
                        driver_key_fingerprint: None,
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
            &format!("comment on issue #{}", display_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(comment_id.get())
    }

    /// Add a driver intervention comment to an issue (kind = "intervention").
    pub fn add_intervention_comment(
        &self,
        db: &Database,
        display_id: i64,
        content: &str,
        trigger_type: &str,
        intervention_context: Option<&str>,
        driver_key_fingerprint: Option<&str>,
    ) -> Result<i64> {
        let content_owned = content.to_string();
        let trigger_owned = trigger_type.to_string();
        let context_owned = intervention_context.map(|s| s.to_string());
        let driver_fp_owned = driver_key_fingerprint.map(|s| s.to_string());
        let agent_id = self.agent.agent_id.clone();
        let comment_id = Cell::new(0i64);

        let _ = self.write_commit_push(
            |writer| {
                let mut counters = writer.read_counters()?;
                let id = counters.next_comment_id;
                counters.next_comment_id += 1;
                comment_id.set(id);

                let (signed_by, signature) = writer.sign_comment(&content_owned, &agent_id, id);

                if writer.layout_version() >= 2 {
                    // V2: write a standalone comment file
                    let issue = writer.load_issue_by_id(display_id, db)?;
                    let comment_uuid = Uuid::new_v4();
                    let comment_file = CommentFile {
                        uuid: comment_uuid,
                        issue_uuid: issue.uuid,
                        author: agent_id.clone(),
                        content: content_owned.clone(),
                        created_at: Utc::now(),
                        kind: "intervention".to_string(),
                        trigger_type: Some(trigger_owned.clone()),
                        intervention_context: context_owned.clone(),
                        driver_key_fingerprint: driver_fp_owned.clone(),
                        signed_by,
                        signature,
                    };
                    let json = serde_json::to_vec_pretty(&comment_file)?;
                    let rel_path = writer.comment_rel_path(&issue.uuid, &comment_uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        use_git_rm: false,
                    })
                } else {
                    // V1: append comment inline to the issue file
                    let mut issue = writer.load_issue_by_id(display_id, db)?;
                    issue.comments.push(CommentEntry {
                        id,
                        author: agent_id.clone(),
                        content: content_owned.clone(),
                        created_at: Utc::now(),
                        kind: "intervention".to_string(),
                        trigger_type: Some(trigger_owned.clone()),
                        intervention_context: context_owned.clone(),
                        driver_key_fingerprint: driver_fp_owned.clone(),
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
            &format!("intervention on issue #{}", display_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(comment_id.get())
    }

    /// Add a label to an issue.
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
            &format!("label issue #{} with {}", display_id, label),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Remove a label from an issue.
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
            &format!("unlabel {} from issue #{}", label, display_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Add a blocker dependency: `blocked_id` is blocked by `blocker_id`.
    ///
    /// Only modifies the blocked issue's file (single-direction storage).
    pub fn add_blocker(&self, db: &Database, blocked_id: i64, blocker_id: i64) -> Result<()> {
        let blocker_uuid = self.resolve_uuid(blocker_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(blocked_id, db)?;
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
            &format!("block issue #{} on #{}", blocked_id, blocker_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Remove a blocker dependency.
    pub fn remove_blocker(&self, db: &Database, blocked_id: i64, blocker_id: i64) -> Result<()> {
        let blocker_uuid = self.resolve_uuid(blocker_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(blocked_id, db)?;
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
            &format!("unblock issue #{} from #{}", blocked_id, blocker_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Add a relation between two issues (single-direction storage).
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
            &format!("relate issue #{} to #{}", issue_id, related_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Remove a relation between two issues.
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
            &format!("unrelate issue #{} from #{}", issue_id, related_id),
        )?;

        self.hydrate_with_retry(db)?;
        Ok(())
    }

    /// Rewrite a just-committed issue to set `display_id: null` and revert the
    /// counter bump. Used when a push failed (offline/exhausted retries) so the
    /// locally-claimed display ID doesn't collide with remote state.
    pub(super) fn rewrite_as_offline(&self, uuid: Uuid) -> Result<()> {
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
