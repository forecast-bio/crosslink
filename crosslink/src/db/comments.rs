use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::core::{Database, MAX_COMMENT_LEN};
use super::helpers::parse_datetime;
use crate::models::Comment;

/// Row from `get_comments_with_author`: (id, author, content, created_at, kind, trigger_type, intervention_context, driver_key_fingerprint).
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
    // Comments
    pub fn add_comment(&self, issue_id: i64, content: &str, kind: &str) -> Result<i64> {
        if content.len() > MAX_COMMENT_LEN {
            anyhow::bail!(
                "Comment exceeds maximum length of {} bytes",
                MAX_COMMENT_LEN
            );
        }
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO comments (issue_id, content, created_at, kind) VALUES (?1, ?2, ?3, ?4)",
            params![issue_id, content, now, kind],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn add_intervention_comment(
        &self,
        issue_id: i64,
        content: &str,
        trigger_type: &str,
        intervention_context: Option<&str>,
        driver_key_fingerprint: Option<&str>,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO comments (issue_id, content, created_at, kind, trigger_type, intervention_context, driver_key_fingerprint)
             VALUES (?1, ?2, ?3, 'intervention', ?4, ?5, ?6)",
            params![issue_id, content, now, trigger_type, intervention_context, driver_key_fingerprint],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_comments(&self, issue_id: i64) -> Result<Vec<Comment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, issue_id, content, created_at, COALESCE(kind, 'note'), trigger_type, intervention_context, driver_key_fingerprint FROM comments WHERE issue_id = ?1 ORDER BY created_at, id",
        )?;
        let comments = stmt
            .query_map([issue_id], |row| {
                Ok(Comment {
                    id: row.get(0)?,
                    issue_id: row.get(1)?,
                    content: row.get(2)?,
                    created_at: parse_datetime(row.get::<_, String>(3)?),
                    kind: row.get(4)?,
                    trigger_type: row.get(5)?,
                    intervention_context: row.get(6)?,
                    driver_key_fingerprint: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(comments)
    }

    pub fn update_comment_content(&self, comment_id: i64, content: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE comments SET content = ?1 WHERE id = ?2",
            params![content, comment_id],
        )?;
        Ok(rows > 0)
    }

    /// Get comments with author field for an issue (author added in migration v10).
    pub fn get_comments_with_author(&self, issue_id: i64) -> Result<Vec<CommentAuthorRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, author, content, created_at, COALESCE(kind, 'note'), trigger_type, intervention_context, driver_key_fingerprint FROM comments WHERE issue_id = ?1 ORDER BY created_at, id",
        )?;
        let comments = stmt
            .query_map([issue_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    parse_datetime(row.get::<_, String>(3)?),
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(comments)
    }

    /// Get the maximum comment ID in the database, or 0 if empty.
    pub fn get_max_comment_id(&self) -> Result<i64> {
        let max: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM comments", [], |row| {
                    row.get(0)
                })?;
        Ok(max)
    }
}
