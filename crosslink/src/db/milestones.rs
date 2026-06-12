use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;

use super::core::Database;
use super::helpers::{issue_from_row, parse_datetime};
use crate::models::Issue;

impl Database {
    // Milestones

    /// Create a new milestone.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn create_milestone(&self, name: &str, description: Option<&str>) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO milestones (name, description, status, created_at) VALUES (?1, ?2, 'open', ?3)",
            params![name, description, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get a milestone by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_milestone(&self, id: i64) -> Result<Option<crate::models::Milestone>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, description, status, created_at, closed_at FROM milestones WHERE id = ?1",
        )?;

        let milestone = stmt
            .query_row([id], |row| {
                Ok(crate::models::Milestone {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    created_at: parse_datetime(&row.get::<_, String>(4)?),
                    closed_at: row.get::<_, Option<String>>(5)?.map(|s| parse_datetime(&s)),
                })
            })
            .ok();

        Ok(milestone)
    }

    /// Look up a milestone's display id by its UUID.
    ///
    /// Used by the v3 create path to read back the reduction-assigned id after
    /// hydration when the in-memory reduced state has not yet frozen it.
    ///
    /// # Errors
    /// Returns an error if no milestone with the given UUID exists.
    pub fn get_milestone_id_by_uuid(&self, uuid: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT id FROM milestones WHERE uuid = ?1",
                rusqlite::params![uuid],
                |row| row.get(0),
            )
            .context("Milestone with given UUID not found")
    }

    /// List milestones, optionally filtered by status.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn list_milestones(&self, status: Option<&str>) -> Result<Vec<crate::models::Milestone>> {
        let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::ToSql>>) = status.map_or_else(
            || {
                let sql = "SELECT id, name, description, status, created_at, closed_at FROM milestones WHERE status = ?1 ORDER BY id DESC";
                let params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new("open".to_string())];
                (sql, params)
            },
            |s| {
                if s == "all" {
                    ("SELECT id, name, description, status, created_at, closed_at FROM milestones ORDER BY id DESC", vec![])
                } else {
                    ("SELECT id, name, description, status, created_at, closed_at FROM milestones WHERE status = ?1 ORDER BY id DESC",
                     vec![Box::new(s.to_string())])
                }
            },
        );

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(std::convert::AsRef::as_ref).collect();
        let mut stmt = self.conn.prepare(sql)?;
        let milestones = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(crate::models::Milestone {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    created_at: parse_datetime(&row.get::<_, String>(4)?),
                    closed_at: row.get::<_, Option<String>>(5)?.map(|s| parse_datetime(&s)),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(milestones)
    }

    /// Add an issue to a milestone.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn add_issue_to_milestone(&self, milestone_id: i64, issue_id: i64) -> Result<bool> {
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO milestone_issues (milestone_id, issue_id) VALUES (?1, ?2)",
            params![milestone_id, issue_id],
        )?;
        Ok(result > 0)
    }

    /// Remove an issue from a milestone.
    ///
    /// # Errors
    ///
    /// Returns an error if the database delete fails.
    pub fn remove_issue_from_milestone(&self, milestone_id: i64, issue_id: i64) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM milestone_issues WHERE milestone_id = ?1 AND issue_id = ?2",
            params![milestone_id, issue_id],
        )?;
        Ok(rows > 0)
    }

    /// Get all issues in a milestone.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_milestone_issues(&self, milestone_id: i64) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            r"
            SELECT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at, i.scheduled_at, i.due_at
            FROM issues i
            JOIN milestone_issues mi ON i.id = mi.issue_id
            WHERE mi.milestone_id = ?1
            ORDER BY i.id
            ",
        )?;

        let issues = stmt
            .query_map([milestone_id], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    /// Close a milestone by setting its status and closed timestamp.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn close_milestone(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE milestones SET status = 'closed', closed_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    /// Delete a milestone by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database delete fails.
    pub fn delete_milestone(&self, id: i64) -> Result<bool> {
        let rows = self
            .conn
            .execute("DELETE FROM milestones WHERE id = ?1", [id])?;
        Ok(rows > 0)
    }

    /// Get the milestone assigned to an issue.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_issue_milestone(&self, issue_id: i64) -> Result<Option<crate::models::Milestone>> {
        let mut stmt = self.conn.prepare(
            r"
            SELECT m.id, m.name, m.description, m.status, m.created_at, m.closed_at
            FROM milestones m
            JOIN milestone_issues mi ON m.id = mi.milestone_id
            WHERE mi.issue_id = ?1
            LIMIT 1
            ",
        )?;

        let milestone = stmt
            .query_row([issue_id], |row| {
                Ok(crate::models::Milestone {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    created_at: parse_datetime(&row.get::<_, String>(4)?),
                    closed_at: row.get::<_, Option<String>>(5)?.map(|s| parse_datetime(&s)),
                })
            })
            .ok();

        Ok(milestone)
    }

    /// Get the total number of milestones.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_milestone_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM milestones", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Get the milestone UUID for an issue, if one is assigned and has a UUID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_milestone_uuid_for_issue(&self, issue_id: i64) -> Result<Option<String>> {
        match self.conn.query_row(
            "SELECT m.uuid FROM milestones m JOIN milestone_issues mi ON m.id = mi.milestone_id WHERE mi.issue_id = ?1 LIMIT 1",
            [issue_id],
            |row| row.get::<_, Option<String>>(0),
        ) {
            Ok(val) => Ok(val),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
