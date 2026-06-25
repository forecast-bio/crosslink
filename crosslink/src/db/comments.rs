use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::core::{Database, MAX_COMMENT_LEN};
use super::helpers::parse_datetime;
use crate::models::Comment;

/// Row from `get_comments_with_author`: (id, author, content, `created_at`, kind, `trigger_type`, `intervention_context`, `driver_key_fingerprint`).
pub type CommentAuthorRow = (
    i64,
    Option<String>,
    String,
    DateTime<Utc>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

impl Database {
    /// Add a comment to an issue.
    ///
    /// # Errors
    /// Returns an error if the comment exceeds the maximum length or the database write fails.
    pub fn add_comment(&self, issue_id: i64, content: &str, kind: &str) -> Result<i64> {
        let issue_id = self.resolve_id(issue_id);
        if content.len() > MAX_COMMENT_LEN {
            anyhow::bail!("Comment exceeds maximum length of {MAX_COMMENT_LEN} bytes");
        }
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO comments (issue_id, content, created_at, kind) VALUES (?1, ?2, ?3, ?4)",
            params![issue_id, content, now, kind],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Add an intervention comment to an issue.
    ///
    /// # Errors
    /// Returns an error if the database write fails.
    pub fn add_intervention_comment(
        &self,
        issue_id: i64,
        content: &str,
        trigger_type: &str,
        intervention_context: Option<&str>,
        driver_key_fingerprint: Option<&str>,
    ) -> Result<i64> {
        let issue_id = self.resolve_id(issue_id);
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO comments (issue_id, content, created_at, kind, trigger_type, intervention_context, driver_key_fingerprint)
             VALUES (?1, ?2, ?3, 'intervention', ?4, ?5, ?6)",
            params![issue_id, content, now, trigger_type, intervention_context, driver_key_fingerprint],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Look up a comment's display id by its UUID.
    ///
    /// Used by the v3 comment path to read back the reduction-assigned id after
    /// hydration when the in-memory reduced state has not yet frozen it.
    ///
    /// # Errors
    /// Returns an error if no comment with the given UUID exists.
    pub fn get_comment_id_by_uuid(&self, uuid: &str) -> Result<i64> {
        self.conn
            .query_row(
                "SELECT id FROM comments WHERE uuid = ?1",
                params![uuid],
                |row| row.get(0),
            )
            .context("Comment with given UUID not found")
    }

    /// Get all comments for an issue.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_comments(&self, issue_id: i64) -> Result<Vec<Comment>> {
        let issue_id = self.resolve_id(issue_id);
        let mut stmt = self.conn.prepare(
            "SELECT id, issue_id, content, created_at, COALESCE(kind, 'note'), trigger_type, intervention_context, driver_key_fingerprint FROM comments WHERE issue_id = ?1 ORDER BY created_at, id",
        )?;
        let comments = stmt
            .query_map([issue_id], |row| {
                Ok(Comment {
                    id: row.get(0)?,
                    issue_id: row.get(1)?,
                    content: row.get(2)?,
                    created_at: parse_datetime(&row.get::<_, String>(3)?),
                    kind: row.get(4)?,
                    trigger_type: row.get(5)?,
                    intervention_context: row.get(6)?,
                    driver_key_fingerprint: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(comments)
    }

    /// Update the content of a comment.
    ///
    /// Retained as a tested DB primitive; its production caller (the offline
    /// reference-rewrite path) was removed with the v2 write machinery (#754).
    ///
    /// # Errors
    /// Returns an error if the database update fails.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn update_comment_content(&self, comment_id: i64, content: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE comments SET content = ?1 WHERE id = ?2",
            params![content, comment_id],
        )?;
        Ok(rows > 0)
    }

    /// Get comments with author field for an issue (author added in migration v10).
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_comments_with_author(&self, issue_id: i64) -> Result<Vec<CommentAuthorRow>> {
        let issue_id = self.resolve_id(issue_id);
        let mut stmt = self.conn.prepare(
            "SELECT id, author, content, created_at, COALESCE(kind, 'note'), trigger_type, intervention_context, driver_key_fingerprint FROM comments WHERE issue_id = ?1 ORDER BY created_at, id",
        )?;
        let comments = stmt
            .query_map([issue_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    parse_datetime(&row.get::<_, String>(3)?),
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(comments)
    }

    /// Search all comments for a query string (case-insensitive LIKE).
    /// Returns matching comments with their parent issue title.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn search_comments(&self, query: &str) -> Result<Vec<(Comment, i64, String)>> {
        let pattern = format!("%{query}%");
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.issue_id, c.content, c.created_at, COALESCE(c.kind, 'note'), \
             c.trigger_type, c.intervention_context, c.driver_key_fingerprint, \
             i.id, i.title \
             FROM comments c JOIN issues i ON c.issue_id = i.id \
             WHERE c.content LIKE ?1 COLLATE NOCASE \
             ORDER BY c.created_at DESC",
        )?;
        let rows = stmt
            .query_map(params![pattern], |row| {
                let comment = Comment {
                    id: row.get(0)?,
                    issue_id: row.get(1)?,
                    content: row.get(2)?,
                    created_at: parse_datetime(&row.get::<_, String>(3)?),
                    kind: row.get(4)?,
                    trigger_type: row.get(5)?,
                    intervention_context: row.get(6)?,
                    driver_key_fingerprint: row.get(7)?,
                };
                let issue_id: i64 = row.get(8)?;
                let issue_title: String = row.get(9)?;
                Ok((comment, issue_id, issue_title))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get the maximum comment ID in the database, or 0 if empty.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_max_comment_id(&self) -> Result<i64> {
        let max: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM comments", [], |row| {
                    row.get(0)
                })?;
        Ok(max)
    }
}
