use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use super::core::Database;
use super::helpers::{issue_from_row, parse_datetime};
use crate::models::Issue;

impl Database {
    // Milestones
    pub fn create_milestone(&self, name: &str, description: Option<&str>) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO milestones (name, description, status, created_at) VALUES (?1, ?2, 'open', ?3)",
            params![name, description, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

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
                    created_at: parse_datetime(row.get::<_, String>(4)?),
                    closed_at: row.get::<_, Option<String>>(5)?.map(parse_datetime),
                })
            })
            .ok();

        Ok(milestone)
    }

    pub fn list_milestones(&self, status: Option<&str>) -> Result<Vec<crate::models::Milestone>> {
        let (sql, params_vec): (&str, Vec<Box<dyn rusqlite::ToSql>>) = if let Some(s) = status {
            if s == "all" {
                ("SELECT id, name, description, status, created_at, closed_at FROM milestones ORDER BY id DESC", vec![])
            } else {
                ("SELECT id, name, description, status, created_at, closed_at FROM milestones WHERE status = ?1 ORDER BY id DESC",
                 vec![Box::new(s.to_string())])
            }
        } else {
            ("SELECT id, name, description, status, created_at, closed_at FROM milestones WHERE status = ?1 ORDER BY id DESC",
             vec![Box::new("open".to_string())])
        };

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();
        let mut stmt = self.conn.prepare(sql)?;
        let milestones = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(crate::models::Milestone {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    created_at: parse_datetime(row.get::<_, String>(4)?),
                    closed_at: row.get::<_, Option<String>>(5)?.map(parse_datetime),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(milestones)
    }

    pub fn add_issue_to_milestone(&self, milestone_id: i64, issue_id: i64) -> Result<bool> {
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO milestone_issues (milestone_id, issue_id) VALUES (?1, ?2)",
            params![milestone_id, issue_id],
        )?;
        Ok(result > 0)
    }

    pub fn remove_issue_from_milestone(&self, milestone_id: i64, issue_id: i64) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM milestone_issues WHERE milestone_id = ?1 AND issue_id = ?2",
            params![milestone_id, issue_id],
        )?;
        Ok(rows > 0)
    }

    pub fn get_milestone_issues(&self, milestone_id: i64) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at
            FROM issues i
            JOIN milestone_issues mi ON i.id = mi.issue_id
            WHERE mi.milestone_id = ?1
            ORDER BY i.id
            "#,
        )?;

        let issues = stmt
            .query_map([milestone_id], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    pub fn close_milestone(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE milestones SET status = 'closed', closed_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    pub fn delete_milestone(&self, id: i64) -> Result<bool> {
        let rows = self
            .conn
            .execute("DELETE FROM milestones WHERE id = ?1", [id])?;
        Ok(rows > 0)
    }

    pub fn get_issue_milestone(&self, issue_id: i64) -> Result<Option<crate::models::Milestone>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT m.id, m.name, m.description, m.status, m.created_at, m.closed_at
            FROM milestones m
            JOIN milestone_issues mi ON m.id = mi.milestone_id
            WHERE mi.issue_id = ?1
            LIMIT 1
            "#,
        )?;

        let milestone = stmt
            .query_row([issue_id], |row| {
                Ok(crate::models::Milestone {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    status: row.get(3)?,
                    created_at: parse_datetime(row.get::<_, String>(4)?),
                    closed_at: row.get::<_, Option<String>>(5)?.map(parse_datetime),
                })
            })
            .ok();

        Ok(milestone)
    }

    pub fn get_milestone_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM milestones", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Get the milestone UUID for an issue, if one is assigned and has a UUID.
    pub fn get_milestone_uuid_for_issue(&self, issue_id: i64) -> Result<Option<String>> {
        let result = self
            .conn
            .query_row(
                "SELECT m.uuid FROM milestones m JOIN milestone_issues mi ON m.id = mi.milestone_id WHERE mi.issue_id = ?1 LIMIT 1",
                [issue_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten();
        Ok(result)
    }
}
