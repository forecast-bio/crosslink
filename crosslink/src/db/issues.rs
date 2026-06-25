use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;

use super::core::{
    validate_priority, validate_status, Database, MAX_DESCRIPTION_LEN, MAX_TITLE_LEN,
};
use super::helpers::issue_from_row;
use crate::models::Issue;

impl Database {
    // Issue CRUD

    /// Create a new issue with the given title, optional description, and priority.
    ///
    /// # Errors
    /// Returns an error if the priority is invalid, the title or description
    /// exceeds maximum length, or the database insert fails.
    pub fn create_issue(
        &self,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
        self.create_issue_with_parent(title, description, priority, None)
    }

    /// Create a new subissue under the given parent issue.
    ///
    /// # Errors
    /// Returns an error if the priority is invalid, the title or description
    /// exceeds maximum length, or the database insert fails.
    pub fn create_subissue(
        &self,
        parent_id: i64,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
        let parent_id = self.resolve_id(parent_id);
        self.create_issue_with_parent(title, description, priority, Some(parent_id))
    }

    fn create_issue_with_parent(
        &self,
        title: &str,
        description: Option<&str>,
        priority: &str,
        parent_id: Option<i64>,
    ) -> Result<i64> {
        validate_priority(priority)?;
        if title.len() > MAX_TITLE_LEN {
            anyhow::bail!("Title exceeds maximum length of {MAX_TITLE_LEN} characters");
        }
        if let Some(d) = description {
            if d.len() > MAX_DESCRIPTION_LEN {
                anyhow::bail!("Description exceeds maximum length of {MAX_DESCRIPTION_LEN} bytes");
            }
        }
        let now = Utc::now().to_rfc3339();
        let uuid = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO issues (title, description, priority, parent_id, status, created_at, updated_at, uuid) VALUES (?1, ?2, ?3, ?4, 'open', ?5, ?5, ?6)",
            params![title, description, priority, parent_id, now, uuid],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get all subissues of the given parent issue.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_subissues(&self, parent_id: i64) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, parent_id, created_at, updated_at, closed_at, scheduled_at, due_at FROM issues WHERE parent_id = ?1 ORDER BY id",
        )?;

        let issues = stmt
            .query_map([parent_id], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    /// Resolve an issue ID, trying the local equivalent if a positive ID
    /// isn't found. Users type "1" meaning "the first issue", regardless
    /// of whether it's stored as #1 (hub) or L1 (local, id=-1 in `SQLite`).
    pub fn resolve_id(&self, id: i64) -> i64 {
        if id > 0 {
            let exists: bool = self
                .conn
                .query_row("SELECT 1 FROM issues WHERE id = ?1", [id], |_| Ok(true))
                .unwrap_or(false);
            if !exists {
                let local_exists: bool = self
                    .conn
                    .query_row("SELECT 1 FROM issues WHERE id = ?1", [-id], |_| Ok(true))
                    .unwrap_or(false);
                if local_exists {
                    return -id;
                }
            }
        }
        id
    }

    /// Get an issue by its display ID, returning `None` if not found.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_issue(&self, id: i64) -> Result<Option<Issue>> {
        let id = self.resolve_id(id);
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, parent_id, created_at, updated_at, closed_at, scheduled_at, due_at FROM issues WHERE id = ?1",
        )?;

        Ok(stmt.query_row([id], issue_from_row).ok())
    }

    /// Look up an issue's display ID by its UUID.
    ///
    /// # Errors
    /// Returns an error if no issue with the given UUID exists.
    pub fn get_issue_id_by_uuid(&self, uuid: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT id FROM issues WHERE uuid = ?1",
                params![uuid],
                |row| row.get(0),
            )
            .context("Issue with given UUID not found")
    }

    /// Look up an issue's UUID by its display ID (supports negative local IDs).
    ///
    /// # Errors
    /// Returns an error if no issue with the given ID exists.
    pub fn get_issue_uuid_by_id(&self, id: i64) -> Result<String> {
        self.conn
            .query_row(
                "SELECT uuid FROM issues WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .with_context(|| format!("Issue with id {id} not found"))
    }

    /// Get an issue by ID, returning an error if not found.
    /// Use this instead of `get_issue` when you need the issue to exist.
    ///
    /// # Errors
    /// Returns an error if no issue with the given ID exists or the query fails.
    pub fn require_issue(&self, id: i64) -> Result<Issue> {
        let id = self.resolve_id(id);
        self.get_issue(id)?
            .ok_or_else(|| anyhow::anyhow!("Issue {} not found", crate::utils::format_issue_id(id)))
    }

    /// List issues with optional status, label, and priority filters.
    ///
    /// # Errors
    /// Returns an error if a filter value is invalid or the database query fails.
    pub fn list_issues(
        &self,
        status_filter: Option<&str>,
        label_filter: Option<&str>,
        priority_filter: Option<&str>,
    ) -> Result<Vec<Issue>> {
        let mut sql = String::from(
            "SELECT DISTINCT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at, i.scheduled_at, i.due_at FROM issues i",
        );
        let mut conditions = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if label_filter.is_some() {
            sql.push_str(" JOIN labels l ON i.id = l.issue_id");
        }

        if let Some(status) = status_filter {
            if status != "all" {
                validate_status(status)?;
                conditions.push("i.status = ?".to_string());
                params_vec.push(Box::new(status.to_string()));
            }
        }

        if let Some(label) = label_filter {
            conditions.push("l.label = ?".to_string());
            params_vec.push(Box::new(label.to_string()));
        }

        if let Some(priority) = priority_filter {
            conditions.push("i.priority = ?".to_string());
            params_vec.push(Box::new(priority.to_string()));
        }

        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }

        sql.push_str(" ORDER BY i.id DESC");

        let mut stmt = self.conn.prepare(&sql)?;
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(std::convert::AsRef::as_ref).collect();

        let issues = stmt
            .query_map(params_refs.as_slice(), issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    /// Update an issue's title, description, and/or priority.
    ///
    /// # Errors
    /// Returns an error if the title or description exceeds maximum length,
    /// the priority is invalid, or the database update fails.
    pub fn update_issue(
        &self,
        id: i64,
        title: Option<&str>,
        description: Option<&str>,
        priority: Option<&str>,
    ) -> Result<bool> {
        let id = self.resolve_id(id);
        if let Some(t) = title {
            if t.len() > MAX_TITLE_LEN {
                anyhow::bail!("Title exceeds maximum length of {MAX_TITLE_LEN} characters");
            }
        }
        if let Some(d) = description {
            if d.len() > MAX_DESCRIPTION_LEN {
                anyhow::bail!("Description exceeds maximum length of {MAX_DESCRIPTION_LEN} bytes");
            }
        }
        let now = Utc::now().to_rfc3339();
        let mut updates = vec!["updated_at = ?1".to_string()];
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(now)];

        if let Some(t) = title {
            updates.push(format!("title = ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(t.to_string()));
        }

        if let Some(d) = description {
            updates.push(format!("description = ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(d.to_string()));
        }

        if let Some(p) = priority {
            validate_priority(p)?;
            updates.push(format!("priority = ?{}", params_vec.len() + 1));
            params_vec.push(Box::new(p.to_string()));
        }

        params_vec.push(Box::new(id));
        let sql = format!(
            "UPDATE issues SET {} WHERE id = ?{}",
            updates.join(", "),
            params_vec.len()
        );

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(std::convert::AsRef::as_ref).collect();
        let rows = self.conn.execute(&sql, params_refs.as_slice())?;
        Ok(rows > 0)
    }

    /// Close an issue by setting its status to `closed`.
    ///
    /// # Errors
    /// Returns an error if the database update fails.
    pub fn close_issue(&self, id: i64) -> Result<bool> {
        let id = self.resolve_id(id);
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET status = 'closed', closed_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    /// Reopen a closed issue by setting its status back to `open`.
    ///
    /// # Errors
    /// Returns an error if the database update fails.
    pub fn reopen_issue(&self, id: i64) -> Result<bool> {
        let id = self.resolve_id(id);
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET status = 'open', closed_at = NULL, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    /// Delete an issue by its display ID.
    ///
    /// # Errors
    /// Returns an error if the database delete fails.
    pub fn delete_issue(&self, id: i64) -> Result<bool> {
        let id = self.resolve_id(id);
        let rows = self
            .conn
            .execute("DELETE FROM issues WHERE id = ?1", [id])?;
        Ok(rows > 0)
    }

    /// Search issues by query string across titles, descriptions, and comments.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn search_issues(&self, query: &str) -> Result<Vec<Issue>> {
        // Escape SQL LIKE wildcards to prevent unintended pattern matching
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let mut stmt = self.conn.prepare(
            r"
            SELECT DISTINCT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at, i.scheduled_at, i.due_at
            FROM issues i
            LEFT JOIN comments c ON i.id = c.issue_id
            WHERE i.title LIKE ?1 ESCAPE '\' COLLATE NOCASE
               OR i.description LIKE ?1 ESCAPE '\' COLLATE NOCASE
               OR c.content LIKE ?1 ESCAPE '\' COLLATE NOCASE
            ORDER BY i.id DESC
            ",
        )?;

        let issues = stmt
            .query_map([&pattern], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    /// Archive a closed issue.
    ///
    /// # Errors
    /// Returns an error if the database update fails.
    pub fn archive_issue(&self, id: i64) -> Result<bool> {
        let id = self.resolve_id(id);
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET status = 'archived', updated_at = ?1 WHERE id = ?2 AND status = 'closed'",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    /// Unarchive an issue, setting its status back to `closed`.
    ///
    /// # Errors
    /// Returns an error if the database update fails.
    pub fn unarchive_issue(&self, id: i64) -> Result<bool> {
        let id = self.resolve_id(id);
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET status = 'closed', updated_at = ?1 WHERE id = ?2 AND status = 'archived'",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    /// List all archived issues.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn list_archived_issues(&self) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, parent_id, created_at, updated_at, closed_at, scheduled_at, due_at FROM issues WHERE status = 'archived' ORDER BY id DESC",
        )?;

        let issues = stmt
            .query_map([], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    /// Archive all issues closed more than the given number of days ago.
    ///
    /// # Errors
    /// Returns an error if the database update fails.
    pub fn archive_older_than(&self, days: i64) -> Result<i32> {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        let cutoff_str = cutoff.to_rfc3339();
        let now = Utc::now().to_rfc3339();

        let rows = self.conn.execute(
            "UPDATE issues SET status = 'archived', updated_at = ?1 WHERE status = 'closed' AND closed_at < ?2",
            params![now, cutoff_str],
        )?;

        Ok(i32::try_from(rows).unwrap_or(i32::MAX))
    }

    /// Update an issue's parent, making it a subissue or a top-level issue.
    ///
    /// # Errors
    /// Returns an error if the database update fails.
    pub fn update_parent(&self, id: i64, parent_id: Option<i64>) -> Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET parent_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![parent_id, now, id],
        )?;
        Ok(rows > 0)
    }

    // === Integrity check helpers ===

    /// Get the maximum issue display ID in the database, or 0 if empty.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_max_display_id(&self) -> Result<i64> {
        let max: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM issues", [], |row| {
                    row.get(0)
                })?;
        Ok(max)
    }

    /// Get the count of issues in the database.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_issue_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Count issues created since a given timestamp.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn count_issues_since(&self, since: &str) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM issues WHERE created_at >= ?1",
            params![since],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Count comments created since a given timestamp.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn count_comments_since(&self, since: &str) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM comments WHERE created_at >= ?1",
            params![since],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Get the uuid and `created_by` metadata for an issue (columns added in migration v10).
    ///
    /// # Errors
    /// Returns an error if the issue is not found or the database query fails.
    pub fn get_issue_export_metadata(
        &self,
        issue_id: i64,
    ) -> Result<(Option<String>, Option<String>)> {
        self.conn
            .query_row(
                "SELECT uuid, created_by FROM issues WHERE id = ?1",
                params![issue_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .context("Failed to get issue export metadata")
    }
}
