//! Milestone operations: create, close, delete, assign, unassign.

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use crate::db::Database;

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

        let _ = self.write_commit_push(
            |_writer| {
                let event = crate::events::Event::MilestoneCreated {
                    uuid,
                    display_id: None,
                    name: name_owned.clone(),
                    description: desc_owned.clone(),
                    created_at: now,
                };
                Ok(WriteSet {
                    events: vec![event],
                })
            },
            &format!("create milestone: {name}"),
        )?;

        self.hydrate_with_retry(db);
        // The milestone id is reduction-assigned (REQ-4); read it from the
        // cached reduced state, falling back to a SQLite lookup when provisional.
        if let Some(id) = self.v3_assigned_milestone_id(&uuid) {
            return Ok(id);
        }
        db.get_milestone_id_by_uuid(&uuid.to_string())
    }

    /// Close a milestone on the coordination branch.
    ///
    /// # Errors
    /// Returns an error if the milestone cannot be loaded or the write fails.
    pub fn close_milestone(&self, db: &Database, milestone_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let entry = writer.load_milestone_by_id(milestone_id)?;
                let closed_at = Utc::now();
                let event = crate::events::Event::MilestoneClosed {
                    uuid: entry.uuid,
                    closed_at,
                };
                Ok(WriteSet {
                    events: vec![event],
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

        let _ = self.write_commit_push(
            |_writer| {
                let event = crate::events::Event::MilestoneDeleted { uuid: entry.uuid };
                Ok(WriteSet {
                    events: vec![event],
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
                let mut events = Vec::new();
                for &issue_id in &ids {
                    let issue = writer.load_issue_by_id(issue_id, db)?;
                    events.push(crate::events::Event::MilestoneAssigned {
                        issue_uuid: issue.uuid,
                        milestone_uuid: Some(ms_uuid),
                    });
                }
                Ok(WriteSet { events })
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
                let issue = writer.load_issue_by_id(issue_id, db)?;
                let event = crate::events::Event::MilestoneAssigned {
                    issue_uuid: issue.uuid,
                    milestone_uuid: None,
                };
                Ok(WriteSet {
                    events: vec![event],
                })
            },
            &format!("remove issue #{issue_id} from milestone"),
        )?;

        self.hydrate_with_retry(db);
        Ok(())
    }
}
