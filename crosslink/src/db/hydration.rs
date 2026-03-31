use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use super::core::Database;

/// Parameters for inserting a hydrated issue from JSON into `SQLite`.
pub struct HydratedIssue<'a> {
    pub id: i64,
    pub uuid: &'a str,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub status: &'a str,
    pub priority: &'a str,
    pub parent_id: Option<i64>,
    pub created_by: Option<&'a str>,
    pub created_at: &'a str,
    pub updated_at: &'a str,
    pub closed_at: Option<&'a str>,
}

/// Parameters for inserting a hydrated milestone from JSON into `SQLite`.
pub struct HydratedMilestone<'a> {
    pub id: i64,
    pub uuid: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub status: &'a str,
    pub created_at: &'a str,
    pub closed_at: Option<&'a str>,
}

impl Database {
    // === Hydration helpers (for shared issue coordination) ===

    /// Delete all shared data tables in preparation for re-hydration from JSON.
    /// Sessions are NOT cleared -- they are machine-local state.
    ///
    /// # Errors
    ///
    /// Returns an error if the database batch execution fails.
    pub fn clear_shared_data(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM milestone_issues;
             DELETE FROM time_entries;
             DELETE FROM relations;
             DELETE FROM dependencies;
             DELETE FROM labels;
             DELETE FROM comments;
             DELETE FROM milestones;
             DELETE FROM issues;",
        )?;
        Ok(())
    }

    /// Insert a hydrated issue from a JSON `IssueFile`.
    /// Uses the `display_id` as the `SQLite` `id` column.
    /// For offline issues (`display_id=None`), uses negative temp IDs.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn insert_hydrated_issue(&self, h: &HydratedIssue<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO issues (id, uuid, title, description, status, priority, parent_id, created_by, created_at, updated_at, closed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![h.id, h.uuid, h.title, h.description, h.status, h.priority, h.parent_id, h.created_by, h.created_at, h.updated_at, h.closed_at],
        )?;
        Ok(())
    }

    /// Insert a label for a hydrated issue.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn insert_hydrated_label(&self, issue_id: i64, label: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO labels (issue_id, label) VALUES (?1, ?2)",
            params![issue_id, label],
        )?;
        Ok(())
    }

    /// Insert a comment for a hydrated issue.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_hydrated_comment(
        &self,
        id: i64,
        issue_id: i64,
        uuid: Option<&str>,
        author: Option<&str>,
        content: &str,
        created_at: &str,
        kind: &str,
        trigger_type: Option<&str>,
        intervention_context: Option<&str>,
        driver_key_fingerprint: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO comments (id, issue_id, uuid, author, content, created_at, kind, trigger_type, intervention_context, driver_key_fingerprint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![id, issue_id, uuid, author, content, created_at, kind, trigger_type, intervention_context, driver_key_fingerprint],
        )?;
        Ok(())
    }

    /// Insert a raw dependency row (used during hydration).
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn insert_dependency_raw(&self, blocker_id: i64, depends_on_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO dependencies (blocker_id, blocked_id) VALUES (?1, ?2)",
            params![blocker_id, depends_on_id],
        )?;
        Ok(())
    }

    /// Insert a raw relation row (used during hydration).
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn insert_relation_raw(&self, issue_id_1: i64, issue_id_2: i64) -> Result<()> {
        let (a, b) = if issue_id_1 <= issue_id_2 {
            (issue_id_1, issue_id_2)
        } else {
            (issue_id_2, issue_id_1)
        };
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO relations (issue_id_1, issue_id_2, created_at) VALUES (?1, ?2, ?3)",
            params![a, b, now],
        )?;
        Ok(())
    }

    /// Insert a hydrated time entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn insert_hydrated_time_entry(
        &self,
        id: i64,
        issue_id: i64,
        started_at: &str,
        ended_at: Option<&str>,
        duration_seconds: Option<i64>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO time_entries (id, issue_id, started_at, ended_at, duration_seconds)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, issue_id, started_at, ended_at, duration_seconds],
        )?;
        Ok(())
    }

    /// Insert a hydrated milestone.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn insert_hydrated_milestone(&self, h: &HydratedMilestone<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO milestones (id, uuid, name, description, status, created_at, closed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                h.id,
                h.uuid,
                h.name,
                h.description,
                h.status,
                h.created_at,
                h.closed_at
            ],
        )?;
        Ok(())
    }

    /// Insert a milestone-issue association.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn insert_hydrated_milestone_issue(&self, milestone_id: i64, issue_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO milestone_issues (milestone_id, issue_id) VALUES (?1, ?2)",
            params![milestone_id, issue_id],
        )?;
        Ok(())
    }
}
