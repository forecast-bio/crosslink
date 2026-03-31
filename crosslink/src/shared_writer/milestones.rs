//! Milestone operations: create, close, delete, assign, unassign.

use anyhow::Result;
use chrono::Utc;
use std::cell::Cell;
use uuid::Uuid;

use crate::db::Database;
use crate::issue_file::MilestoneEntry;

use super::core::{SharedWriter, WriteSet};

impl SharedWriter {
    /// Create a milestone on the coordination branch.
    ///
    /// Returns the assigned milestone display ID.
    ///
    /// # Errors
    /// Returns an error if writing or pushing to the coordination branch fails.
    pub fn create_milestone(
        &self,
        db: &Database,
        name: &str,
        description: Option<&str>,
    ) -> Result<i64> {
        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let name_owned = name.to_string();
        let desc_owned = description.map(std::string::ToString::to_string);
        let display_id = Cell::new(0i64);

        let _ = self.write_commit_push(
            |writer| {
                let (id, counters) = writer.claim_milestone_id()?;
                display_id.set(id);
                let entry = MilestoneEntry {
                    uuid,
                    display_id: id,
                    name: name_owned.clone(),
                    description: desc_owned.clone(),
                    status: crate::models::IssueStatus::Open,
                    created_at: now,
                    closed_at: None,
                };
                let mut json = Vec::new();
                serde_json::to_writer_pretty(&mut json, &entry)?;
                Ok(WriteSet {
                    files: vec![(format!("meta/milestones/{uuid}.json"), json)],
                    counters: Some(counters),
                    use_git_rm: false,
                })
            },
            &format!("create milestone: {name}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(display_id.get())
    }

    /// Close a milestone on the coordination branch.
    ///
    /// # Errors
    /// Returns an error if the milestone cannot be loaded or the write fails.
    pub fn close_milestone(&self, db: &Database, milestone_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut entry = writer.load_milestone_by_id(milestone_id)?;
                entry.status = crate::models::IssueStatus::Closed;
                entry.closed_at = Some(Utc::now());
                let mut json = Vec::new();
                serde_json::to_writer_pretty(&mut json, &entry)?;
                Ok(WriteSet {
                    files: vec![(format!("meta/milestones/{}.json", entry.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("close milestone #{milestone_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Delete a milestone file from the coordination branch.
    ///
    /// # Errors
    /// Returns an error if the milestone cannot be loaded or the write fails.
    pub fn delete_milestone(&self, db: &Database, milestone_id: i64) -> Result<()> {
        let entry = self.load_milestone_by_id(milestone_id)?;
        let rel_path = format!("meta/milestones/{}.json", entry.uuid);

        let _ = self.write_commit_push(
            |writer| {
                let path = writer
                    .cache_dir
                    .join("meta")
                    .join("milestones")
                    .join(format!("{}.json", entry.uuid));
                if path.exists() {
                    std::fs::remove_file(&path)?;
                }
                Ok(WriteSet {
                    files: vec![(rel_path.clone(), vec![])],
                    counters: None,
                    use_git_rm: true,
                })
            },
            &format!("delete milestone #{milestone_id}"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Set `milestone_uuid` on issue JSON files for the given issue IDs.
    ///
    /// Loads the milestone to get its UUID, then patches each issue file.
    /// Also adds the issues to the `SQLite` `milestone_issues` table via hydration.
    ///
    /// # Errors
    /// Returns an error if the milestone or any issue cannot be loaded, or the write fails.
    pub fn set_milestone_on_issues(
        &self,
        db: &Database,
        milestone_id: i64,
        issue_ids: &[i64],
    ) -> Result<()> {
        let milestone = self.load_milestone_by_id(milestone_id)?;
        let ms_uuid = milestone.uuid;

        let ids: Vec<i64> = issue_ids.to_vec();
        let _ = self.write_commit_push(
            |writer| {
                let mut files = Vec::new();
                for &issue_id in &ids {
                    let mut issue = writer.load_issue_by_id(issue_id, db)?;
                    issue.milestone_uuid = Some(ms_uuid);
                    issue.updated_at = Utc::now();
                    let json = serde_json::to_vec_pretty(&issue)?;
                    let rel_path = writer.issue_rel_path(&issue.uuid);
                    files.push((rel_path, json));
                }
                Ok(WriteSet {
                    files,
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("add {} issue(s) to milestone #{}", ids.len(), milestone_id),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }

    /// Clear `milestone_uuid` on an issue JSON file.
    ///
    /// # Errors
    /// Returns an error if the issue cannot be loaded or the write fails.
    pub fn clear_milestone_on_issue(&self, db: &Database, issue_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                issue.milestone_uuid = None;
                issue.updated_at = Utc::now();
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("remove issue #{issue_id} from milestone"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }
}
