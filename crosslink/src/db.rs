use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::Path;

use crate::models::{Comment, Issue, Session};

pub const SCHEMA_VERSION: i32 = 14;

/// Valid values for issue priority.
pub const VALID_PRIORITIES: &[&str] = &["low", "medium", "high", "critical"];

/// Valid values for issue status (used for future validation).
#[allow(dead_code)]
pub const VALID_STATUSES: &[&str] = &["open", "closed", "archived"];

/// Maximum lengths for string inputs.
pub const MAX_TITLE_LEN: usize = 512;
pub const MAX_LABEL_LEN: usize = 128;
pub const MAX_DESCRIPTION_LEN: usize = 64 * 1024; // 64KB
pub const MAX_COMMENT_LEN: usize = 1024 * 1024; // 1MB

/// Validate that a priority value is known, returning an error if not.
pub fn validate_priority(priority: &str) -> Result<()> {
    if VALID_PRIORITIES.contains(&priority) {
        Ok(())
    } else {
        anyhow::bail!(
            "Invalid priority '{}'. Valid values: {}",
            priority,
            VALID_PRIORITIES.join(", ")
        )
    }
}

/// Row from `get_comments_with_author`: (id, author, content, created_at, kind, trigger_type, intervention_context).
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

/// Row from `get_time_entries_for_issue`: (id, started_at, ended_at, duration_seconds).
pub type TimeEntryRow = (i64, DateTime<Utc>, Option<DateTime<Utc>>, Option<i64>);

pub struct Database {
    conn: Connection,
}

/// Parameters for inserting a hydrated issue from JSON into SQLite.
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

/// Parameters for inserting a hydrated milestone from JSON into SQLite.
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
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open database")?;
        let db = Database { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Execute a closure within a database transaction.
    /// If the closure returns Ok, the transaction is committed.
    /// If the closure returns Err, the transaction is rolled back.
    pub fn transaction<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        self.conn.execute("BEGIN TRANSACTION", [])?;
        match f() {
            Ok(result) => {
                self.conn.execute("COMMIT", [])?;
                Ok(result)
            }
            Err(e) => {
                if let Err(rollback_err) = self.conn.execute("ROLLBACK", []) {
                    eprintln!("warning: ROLLBACK failed: {}", rollback_err);
                }
                Err(e)
            }
        }
    }

    /// Run a migration statement, logging unexpected errors.
    /// Expected errors (duplicate column, table already exists) are silently ignored.
    fn migrate(&self, sql: &str) {
        if let Err(e) = self.conn.execute(sql, []) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("already exists") {
                eprintln!("warning: migration error ({}): {}", sql.trim(), msg);
            }
        }
    }

    /// Run a batch migration statement, logging unexpected errors.
    fn migrate_batch(&self, sql: &str) {
        if let Err(e) = self.conn.execute_batch(sql) {
            let msg = e.to_string();
            if !msg.contains("duplicate column") && !msg.contains("already exists") {
                eprintln!("warning: migration batch error: {}", msg);
            }
        }
    }

    fn init_schema(&self) -> Result<()> {
        // Check if we need to initialize
        let version: i32 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(user_version), 0) FROM pragma_user_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        if version < SCHEMA_VERSION {
            self.conn.execute_batch(
                r#"
                -- Core issues table
                CREATE TABLE IF NOT EXISTS issues (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    title TEXT NOT NULL,
                    description TEXT,
                    status TEXT NOT NULL DEFAULT 'open',
                    priority TEXT NOT NULL DEFAULT 'medium',
                    parent_id INTEGER,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    closed_at TEXT,
                    FOREIGN KEY (parent_id) REFERENCES issues(id) ON DELETE CASCADE
                );

                -- Labels (many-to-many)
                CREATE TABLE IF NOT EXISTS labels (
                    issue_id INTEGER NOT NULL,
                    label TEXT NOT NULL,
                    PRIMARY KEY (issue_id, label),
                    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
                );

                -- Dependencies (blocker blocks blocked)
                CREATE TABLE IF NOT EXISTS dependencies (
                    blocker_id INTEGER NOT NULL,
                    blocked_id INTEGER NOT NULL,
                    PRIMARY KEY (blocker_id, blocked_id),
                    FOREIGN KEY (blocker_id) REFERENCES issues(id) ON DELETE CASCADE,
                    FOREIGN KEY (blocked_id) REFERENCES issues(id) ON DELETE CASCADE
                );

                -- Comments
                CREATE TABLE IF NOT EXISTS comments (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    issue_id INTEGER NOT NULL,
                    content TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
                );

                -- Sessions (for context preservation)
                CREATE TABLE IF NOT EXISTS sessions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    active_issue_id INTEGER,
                    handoff_notes TEXT,
                    FOREIGN KEY (active_issue_id) REFERENCES issues(id) ON DELETE SET NULL
                );

                -- Time tracking
                CREATE TABLE IF NOT EXISTS time_entries (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    issue_id INTEGER NOT NULL,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    duration_seconds INTEGER,
                    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
                );

                -- Relations (related issues, bidirectional)
                CREATE TABLE IF NOT EXISTS relations (
                    issue_id_1 INTEGER NOT NULL,
                    issue_id_2 INTEGER NOT NULL,
                    created_at TEXT NOT NULL,
                    PRIMARY KEY (issue_id_1, issue_id_2),
                    FOREIGN KEY (issue_id_1) REFERENCES issues(id) ON DELETE CASCADE,
                    FOREIGN KEY (issue_id_2) REFERENCES issues(id) ON DELETE CASCADE
                );

                -- Milestones
                CREATE TABLE IF NOT EXISTS milestones (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL,
                    description TEXT,
                    status TEXT NOT NULL DEFAULT 'open',
                    created_at TEXT NOT NULL,
                    closed_at TEXT
                );

                -- Milestone-Issue relationship (many-to-many)
                CREATE TABLE IF NOT EXISTS milestone_issues (
                    milestone_id INTEGER NOT NULL,
                    issue_id INTEGER NOT NULL,
                    PRIMARY KEY (milestone_id, issue_id),
                    FOREIGN KEY (milestone_id) REFERENCES milestones(id) ON DELETE CASCADE,
                    FOREIGN KEY (issue_id) REFERENCES issues(id) ON DELETE CASCADE
                );

                -- Indexes
                CREATE INDEX IF NOT EXISTS idx_issues_status ON issues(status);
                CREATE INDEX IF NOT EXISTS idx_issues_priority ON issues(priority);
                CREATE INDEX IF NOT EXISTS idx_labels_issue ON labels(issue_id);
                CREATE INDEX IF NOT EXISTS idx_comments_issue ON comments(issue_id);
                CREATE INDEX IF NOT EXISTS idx_deps_blocker ON dependencies(blocker_id);
                CREATE INDEX IF NOT EXISTS idx_deps_blocked ON dependencies(blocked_id);
                CREATE INDEX IF NOT EXISTS idx_issues_parent ON issues(parent_id);
                CREATE INDEX IF NOT EXISTS idx_time_entries_issue ON time_entries(issue_id);
                CREATE INDEX IF NOT EXISTS idx_relations_1 ON relations(issue_id_1);
                CREATE INDEX IF NOT EXISTS idx_relations_2 ON relations(issue_id_2);
                CREATE INDEX IF NOT EXISTS idx_milestone_issues_m ON milestone_issues(milestone_id);
                CREATE INDEX IF NOT EXISTS idx_milestone_issues_i ON milestone_issues(issue_id);
                "#,
            )?;

            // Migration: add parent_id column if upgrading from v1
            self.migrate(
                "ALTER TABLE issues ADD COLUMN parent_id INTEGER REFERENCES issues(id) ON DELETE CASCADE",
            );

            // Migration v7: Recreate sessions table with ON DELETE SET NULL for active_issue_id
            // This ensures deleting an issue clears the session reference instead of failing
            if version < 7 {
                self.migrate_batch(
                    r#"
                    DROP TABLE IF EXISTS sessions_new;
                    CREATE TABLE sessions_new (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        started_at TEXT NOT NULL,
                        ended_at TEXT,
                        active_issue_id INTEGER,
                        handoff_notes TEXT,
                        FOREIGN KEY (active_issue_id) REFERENCES issues(id) ON DELETE SET NULL
                    );
                    INSERT OR IGNORE INTO sessions_new (id, started_at, ended_at, active_issue_id, handoff_notes)
                        SELECT id, started_at, ended_at, active_issue_id, handoff_notes FROM sessions;
                    DROP TABLE IF EXISTS sessions;
                    ALTER TABLE sessions_new RENAME TO sessions;
                    "#,
                );
            }

            // Migration v8: Add last_action column to sessions table
            if version < 8 {
                self.migrate("ALTER TABLE sessions ADD COLUMN last_action TEXT");
            }

            // Migration v9: Add agent_id column to sessions table
            if version < 9 {
                self.migrate("ALTER TABLE sessions ADD COLUMN agent_id TEXT");
            }

            // Migration v10: Add uuid columns for shared issue coordination
            if version < 10 {
                self.migrate("ALTER TABLE issues ADD COLUMN uuid TEXT");
                self.migrate("CREATE UNIQUE INDEX IF NOT EXISTS idx_issues_uuid ON issues(uuid)");
                self.migrate("ALTER TABLE issues ADD COLUMN created_by TEXT");
                self.migrate("ALTER TABLE comments ADD COLUMN uuid TEXT");
                self.migrate("ALTER TABLE comments ADD COLUMN author TEXT");
                self.migrate("ALTER TABLE milestones ADD COLUMN uuid TEXT");
                self.migrate(
                    "CREATE UNIQUE INDEX IF NOT EXISTS idx_milestones_uuid ON milestones(uuid)",
                );
            }

            // Migration v11: Add kind column to comments for typed audit trail
            if version < 11 {
                self.migrate("ALTER TABLE comments ADD COLUMN kind TEXT DEFAULT 'note'");
            }

            // Migration v12: Add trigger_type and intervention_context for driver intervention tracking
            if version < 12 {
                self.migrate("ALTER TABLE comments ADD COLUMN trigger_type TEXT");
                self.migrate("ALTER TABLE comments ADD COLUMN intervention_context TEXT");
            }

            // Migration v13: Add driver_key_fingerprint to comments for audit trail
            if version < 13 {
                let _ = self.conn.execute(
                    "ALTER TABLE comments ADD COLUMN driver_key_fingerprint TEXT",
                    [],
                );
            }

            // Migration v14: Drop leftover sessions_new table from a bug where
            // user_version was always read as 0 (wrong column name in the query),
            // causing the v7 migration to re-run on every open and leave behind
            // a stale sessions_new table.
            if version < 14 {
                self.migrate("DROP TABLE IF EXISTS sessions_new");
            }

            self.conn
                .execute(&format!("PRAGMA user_version = {}", SCHEMA_VERSION), [])?;
        }

        // Enable foreign keys
        self.conn.execute("PRAGMA foreign_keys = ON", [])?;

        Ok(())
    }

    // Issue CRUD
    pub fn create_issue(
        &self,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
        self.create_issue_with_parent(title, description, priority, None)
    }

    pub fn create_subissue(
        &self,
        parent_id: i64,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
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
            anyhow::bail!(
                "Title exceeds maximum length of {} characters",
                MAX_TITLE_LEN
            );
        }
        if let Some(d) = description {
            if d.len() > MAX_DESCRIPTION_LEN {
                anyhow::bail!(
                    "Description exceeds maximum length of {} bytes",
                    MAX_DESCRIPTION_LEN
                );
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

    pub fn get_subissues(&self, parent_id: i64) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, parent_id, created_at, updated_at, closed_at FROM issues WHERE parent_id = ?1 ORDER BY id",
        )?;

        let issues = stmt
            .query_map([parent_id], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    pub fn get_issue(&self, id: i64) -> Result<Option<Issue>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, parent_id, created_at, updated_at, closed_at FROM issues WHERE id = ?1",
        )?;

        let issue = stmt.query_row([id], issue_from_row).ok();

        Ok(issue)
    }

    /// Look up an issue's display ID by its UUID.
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
    pub fn get_issue_uuid_by_id(&self, id: i64) -> Result<String> {
        self.conn
            .query_row(
                "SELECT uuid FROM issues WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .with_context(|| format!("Issue with id {} not found", id))
    }

    /// Get an issue by ID, returning an error if not found.
    /// Use this instead of get_issue when you need the issue to exist.
    pub fn require_issue(&self, id: i64) -> Result<Issue> {
        self.get_issue(id)?
            .ok_or_else(|| anyhow::anyhow!("Issue #{} not found", id))
    }

    pub fn list_issues(
        &self,
        status_filter: Option<&str>,
        label_filter: Option<&str>,
        priority_filter: Option<&str>,
    ) -> Result<Vec<Issue>> {
        let mut sql = String::from(
            "SELECT DISTINCT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at FROM issues i",
        );
        let mut conditions = Vec::new();
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if label_filter.is_some() {
            sql.push_str(" JOIN labels l ON i.id = l.issue_id");
        }

        if let Some(status) = status_filter {
            if status != "all" {
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
            params_vec.iter().map(|p| p.as_ref()).collect();

        let issues = stmt
            .query_map(params_refs.as_slice(), issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    pub fn update_issue(
        &self,
        id: i64,
        title: Option<&str>,
        description: Option<&str>,
        priority: Option<&str>,
    ) -> Result<bool> {
        if let Some(t) = title {
            if t.len() > MAX_TITLE_LEN {
                anyhow::bail!(
                    "Title exceeds maximum length of {} characters",
                    MAX_TITLE_LEN
                );
            }
        }
        if let Some(d) = description {
            if d.len() > MAX_DESCRIPTION_LEN {
                anyhow::bail!(
                    "Description exceeds maximum length of {} bytes",
                    MAX_DESCRIPTION_LEN
                );
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
            params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = self.conn.execute(&sql, params_refs.as_slice())?;
        Ok(rows > 0)
    }

    pub fn close_issue(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET status = 'closed', closed_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    pub fn reopen_issue(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET status = 'open', closed_at = NULL, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    pub fn delete_issue(&self, id: i64) -> Result<bool> {
        let rows = self
            .conn
            .execute("DELETE FROM issues WHERE id = ?1", [id])?;
        Ok(rows > 0)
    }

    // Labels
    pub fn add_label(&self, issue_id: i64, label: &str) -> Result<bool> {
        if label.len() > MAX_LABEL_LEN {
            anyhow::bail!(
                "Label exceeds maximum length of {} characters",
                MAX_LABEL_LEN
            );
        }
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO labels (issue_id, label) VALUES (?1, ?2)",
            params![issue_id, label],
        )?;
        Ok(result > 0)
    }

    pub fn remove_label(&self, issue_id: i64, label: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM labels WHERE issue_id = ?1 AND label = ?2",
            params![issue_id, label],
        )?;
        Ok(rows > 0)
    }

    pub fn get_labels(&self, issue_id: i64) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT label FROM labels WHERE issue_id = ?1 ORDER BY label")?;
        let labels = stmt
            .query_map([issue_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(labels)
    }

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

    // Dependencies
    pub fn add_dependency(&self, blocked_id: i64, blocker_id: i64) -> Result<bool> {
        // Prevent self-blocking
        if blocked_id == blocker_id {
            anyhow::bail!("An issue cannot block itself");
        }

        // Check for circular dependencies before inserting
        if self.would_create_cycle(blocked_id, blocker_id)? {
            anyhow::bail!("Adding this dependency would create a circular dependency chain");
        }

        let result = self.conn.execute(
            "INSERT OR IGNORE INTO dependencies (blocker_id, blocked_id) VALUES (?1, ?2)",
            params![blocker_id, blocked_id],
        )?;
        Ok(result > 0)
    }

    /// Check if adding blocker_id -> blocked_id would create a cycle.
    /// A cycle exists if blocked_id can already reach blocker_id through existing dependencies.
    fn would_create_cycle(&self, blocked_id: i64, blocker_id: i64) -> Result<bool> {
        // If blocked_id can reach blocker_id, then adding blocker_id -> blocked_id creates a cycle
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![blocked_id];

        while let Some(current) = stack.pop() {
            if current == blocker_id {
                return Ok(true); // Found a path from blocked_id to blocker_id
            }

            if visited.insert(current) {
                // Get all issues that 'current' blocks (issues where current is the blocker)
                let blocking = self.get_blocking(current)?;
                for next in blocking {
                    if !visited.contains(&next) {
                        stack.push(next);
                    }
                }
            }
        }

        Ok(false)
    }

    pub fn remove_dependency(&self, blocked_id: i64, blocker_id: i64) -> Result<bool> {
        let rows = self.conn.execute(
            "DELETE FROM dependencies WHERE blocker_id = ?1 AND blocked_id = ?2",
            params![blocker_id, blocked_id],
        )?;
        Ok(rows > 0)
    }

    pub fn get_blockers(&self, issue_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT blocker_id FROM dependencies WHERE blocked_id = ?1")?;
        let blockers = stmt
            .query_map([issue_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?;
        Ok(blockers)
    }

    pub fn get_blocking(&self, issue_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT blocked_id FROM dependencies WHERE blocker_id = ?1")?;
        let blocking = stmt
            .query_map([issue_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?;
        Ok(blocking)
    }

    /// Get the uuid and created_by metadata for an issue (columns added in migration v10).
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

    /// Get time entries for an issue.
    pub fn get_time_entries_for_issue(&self, issue_id: i64) -> Result<Vec<TimeEntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, duration_seconds FROM time_entries WHERE issue_id = ?1 ORDER BY id",
        )?;
        let entries = stmt
            .query_map([issue_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    parse_datetime(row.get::<_, String>(1)?),
                    row.get::<_, Option<String>>(2)?.map(parse_datetime),
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
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

    /// Get related issue IDs (both directions of the relation).
    pub fn get_related_issue_ids(&self, issue_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT issue_id_2 FROM relations WHERE issue_id_1 = ?1 UNION SELECT issue_id_1 FROM relations WHERE issue_id_2 = ?1",
        )?;
        let ids = stmt
            .query_map([issue_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?;
        Ok(ids)
    }

    pub fn list_blocked_issues(&self) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at
            FROM issues i
            JOIN dependencies d ON i.id = d.blocked_id
            JOIN issues blocker ON d.blocker_id = blocker.id
            WHERE i.status = 'open' AND blocker.status = 'open'
            ORDER BY i.id
            "#,
        )?;

        let issues = stmt
            .query_map([], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    pub fn list_ready_issues(&self) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at
            FROM issues i
            WHERE i.status = 'open'
            AND NOT EXISTS (
                SELECT 1 FROM dependencies d
                JOIN issues blocker ON d.blocker_id = blocker.id
                WHERE d.blocked_id = i.id AND blocker.status = 'open'
            )
            ORDER BY i.id
            "#,
        )?;

        let issues = stmt
            .query_map([], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    // Sessions

    /// Convenience wrapper for tests — starts a session with no agent_id.
    #[cfg(test)]
    pub fn start_session(&self) -> Result<i64> {
        self.start_session_with_agent(None)
    }

    pub fn start_session_with_agent(&self, agent_id: Option<&str>) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (started_at, agent_id) VALUES (?1, ?2)",
            params![now, agent_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn end_session(&self, id: i64, notes: Option<&str>) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE sessions SET ended_at = ?1, handoff_notes = ?2 WHERE id = ?3",
            params![now, notes, id],
        )?;
        Ok(rows > 0)
    }

    /// Convenience wrapper for tests — gets current session without agent scoping.
    #[cfg(test)]
    pub fn get_current_session(&self) -> Result<Option<Session>> {
        self.get_current_session_for_agent(None)
    }

    /// Get the current active session scoped to the given agent_id.
    /// If agent_id is Some, only returns sessions belonging to that agent.
    /// If agent_id is None, returns any active session (backward compat).
    pub fn get_current_session_for_agent(&self, agent_id: Option<&str>) -> Result<Option<Session>> {
        if let Some(aid) = agent_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, started_at, ended_at, active_issue_id, handoff_notes, last_action, agent_id FROM sessions WHERE ended_at IS NULL AND agent_id = ?1 ORDER BY id DESC LIMIT 1",
            )?;
            Ok(stmt.query_row(params![aid], session_from_row).ok())
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, started_at, ended_at, active_issue_id, handoff_notes, last_action, agent_id FROM sessions WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1",
            )?;
            Ok(stmt.query_row([], session_from_row).ok())
        }
    }

    /// Convenience wrapper for tests — gets last session without agent scoping.
    #[cfg(test)]
    pub fn get_last_session(&self) -> Result<Option<Session>> {
        self.get_last_session_for_agent(None)
    }

    /// Get the most recent ended session scoped to the given agent_id.
    /// If agent_id is Some, only returns sessions belonging to that agent.
    /// If agent_id is None, returns any ended session (backward compat).
    pub fn get_last_session_for_agent(&self, agent_id: Option<&str>) -> Result<Option<Session>> {
        if let Some(aid) = agent_id {
            let mut stmt = self.conn.prepare(
                "SELECT id, started_at, ended_at, active_issue_id, handoff_notes, last_action, agent_id FROM sessions WHERE ended_at IS NOT NULL AND agent_id = ?1 ORDER BY id DESC LIMIT 1",
            )?;
            Ok(stmt.query_row(params![aid], session_from_row).ok())
        } else {
            let mut stmt = self.conn.prepare(
                "SELECT id, started_at, ended_at, active_issue_id, handoff_notes, last_action, agent_id FROM sessions WHERE ended_at IS NOT NULL ORDER BY id DESC LIMIT 1",
            )?;
            Ok(stmt.query_row([], session_from_row).ok())
        }
    }

    pub fn set_session_issue(&self, session_id: i64, issue_id: i64) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE sessions SET active_issue_id = ?1 WHERE id = ?2",
            params![issue_id, session_id],
        )?;
        Ok(rows > 0)
    }

    pub fn set_session_action(&self, session_id: i64, action: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE sessions SET last_action = ?1 WHERE id = ?2",
            params![action, session_id],
        )?;
        Ok(rows > 0)
    }

    pub fn update_session_notes(&self, session_id: i64, notes: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE sessions SET handoff_notes = ?1 WHERE id = ?2",
            params![notes, session_id],
        )?;
        Ok(rows > 0)
    }

    pub fn get_all_sessions_with_notes(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, active_issue_id, handoff_notes, last_action, agent_id FROM sessions WHERE handoff_notes IS NOT NULL ORDER BY id",
        )?;
        let sessions = stmt
            .query_map([], session_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    // Time tracking
    pub fn start_timer(&self, issue_id: i64) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO time_entries (issue_id, started_at) VALUES (?1, ?2)",
            params![issue_id, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn stop_timer(&self, issue_id: i64) -> Result<bool> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        // Get the active entry
        let started_at: Option<String> = self
            .conn
            .query_row(
                "SELECT started_at FROM time_entries WHERE issue_id = ?1 AND ended_at IS NULL",
                [issue_id],
                |row| row.get(0),
            )
            .ok();

        if let Some(started) = started_at {
            let start_dt = DateTime::parse_from_rfc3339(&started)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or(now);
            let duration = now.signed_duration_since(start_dt).num_seconds();

            let rows = self.conn.execute(
                "UPDATE time_entries SET ended_at = ?1, duration_seconds = ?2 WHERE issue_id = ?3 AND ended_at IS NULL",
                params![now_str, duration, issue_id],
            )?;
            Ok(rows > 0)
        } else {
            Ok(false)
        }
    }

    pub fn get_active_timer(&self) -> Result<Option<(i64, DateTime<Utc>)>> {
        let result: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT issue_id, started_at FROM time_entries WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        Ok(result.map(|(id, started)| (id, parse_datetime(started))))
    }

    pub fn get_total_time(&self, issue_id: i64) -> Result<i64> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(duration_seconds), 0) FROM time_entries WHERE issue_id = ?1 AND duration_seconds IS NOT NULL",
                [issue_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(total)
    }

    /// Search issues by query string across titles, descriptions, and comments
    pub fn search_issues(&self, query: &str) -> Result<Vec<Issue>> {
        // Escape SQL LIKE wildcards to prevent unintended pattern matching
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{}%", escaped);
        let mut stmt = self.conn.prepare(
            r#"
            SELECT DISTINCT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at
            FROM issues i
            LEFT JOIN comments c ON i.id = c.issue_id
            WHERE i.title LIKE ?1 ESCAPE '\' COLLATE NOCASE
               OR i.description LIKE ?1 ESCAPE '\' COLLATE NOCASE
               OR c.content LIKE ?1 ESCAPE '\' COLLATE NOCASE
            ORDER BY i.id DESC
            "#,
        )?;

        let issues = stmt
            .query_map([&pattern], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    // Relations (bidirectional)
    pub fn add_relation(&self, issue_id_1: i64, issue_id_2: i64) -> Result<bool> {
        if issue_id_1 == issue_id_2 {
            anyhow::bail!("Cannot relate an issue to itself");
        }
        // Store with smaller ID first for consistency
        let (a, b) = if issue_id_1 < issue_id_2 {
            (issue_id_1, issue_id_2)
        } else {
            (issue_id_2, issue_id_1)
        };
        let now = Utc::now().to_rfc3339();
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO relations (issue_id_1, issue_id_2, created_at) VALUES (?1, ?2, ?3)",
            params![a, b, now],
        )?;
        Ok(result > 0)
    }

    pub fn remove_relation(&self, issue_id_1: i64, issue_id_2: i64) -> Result<bool> {
        let (a, b) = if issue_id_1 < issue_id_2 {
            (issue_id_1, issue_id_2)
        } else {
            (issue_id_2, issue_id_1)
        };
        let rows = self.conn.execute(
            "DELETE FROM relations WHERE issue_id_1 = ?1 AND issue_id_2 = ?2",
            params![a, b],
        )?;
        Ok(rows > 0)
    }

    pub fn update_parent(&self, id: i64, parent_id: Option<i64>) -> Result<bool> {
        let now = chrono::Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET parent_id = ?1, updated_at = ?2 WHERE id = ?3",
            params![parent_id, now, id],
        )?;
        Ok(rows > 0)
    }

    pub fn get_related_issues(&self, issue_id: i64) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at
            FROM issues i
            WHERE i.id IN (
                SELECT issue_id_2 FROM relations WHERE issue_id_1 = ?1
                UNION
                SELECT issue_id_1 FROM relations WHERE issue_id_2 = ?1
            )
            ORDER BY i.id
            "#,
        )?;

        let issues = stmt
            .query_map([issue_id], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

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

    // Archiving
    pub fn archive_issue(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET status = 'archived', updated_at = ?1 WHERE id = ?2 AND status = 'closed'",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    pub fn unarchive_issue(&self, id: i64) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE issues SET status = 'closed', updated_at = ?1 WHERE id = ?2 AND status = 'archived'",
            params![now, id],
        )?;
        Ok(rows > 0)
    }

    pub fn list_archived_issues(&self) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, description, status, priority, parent_id, created_at, updated_at, closed_at FROM issues WHERE status = 'archived' ORDER BY id DESC",
        )?;

        let issues = stmt
            .query_map([], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    pub fn archive_older_than(&self, days: i64) -> Result<i32> {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        let cutoff_str = cutoff.to_rfc3339();
        let now = Utc::now().to_rfc3339();

        let rows = self.conn.execute(
            "UPDATE issues SET status = 'archived', updated_at = ?1 WHERE status = 'closed' AND closed_at < ?2",
            params![now, cutoff_str],
        )?;

        Ok(rows as i32)
    }

    // === Integrity check helpers ===

    /// Get the maximum issue display ID in the database, or 0 if empty.
    pub fn get_max_display_id(&self) -> Result<i64> {
        let max: i64 =
            self.conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM issues", [], |row| {
                    row.get(0)
                })?;
        Ok(max)
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

    /// Get the current schema version (PRAGMA user_version).
    pub fn get_schema_version(&self) -> Result<i32> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok(version)
    }

    /// Get the count of issues in the database.
    pub fn get_issue_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))?;
        Ok(count)
    }

    pub fn get_milestone_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM milestones", [], |row| row.get(0))?;
        Ok(count)
    }

    // === Hydration helpers (for shared issue coordination) ===

    /// Delete all shared data tables in preparation for re-hydration from JSON.
    /// Sessions are NOT cleared — they are machine-local state.
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

    /// Insert a hydrated issue from a JSON IssueFile.
    /// Uses the display_id as the SQLite `id` column.
    /// For offline issues (display_id=None), uses negative temp IDs.
    pub fn insert_hydrated_issue(&self, h: &HydratedIssue<'_>) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO issues (id, uuid, title, description, status, priority, parent_id, created_by, created_at, updated_at, closed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![h.id, h.uuid, h.title, h.description, h.status, h.priority, h.parent_id, h.created_by, h.created_at, h.updated_at, h.closed_at],
        )?;
        Ok(())
    }

    /// Insert a label for a hydrated issue.
    pub fn insert_hydrated_label(&self, issue_id: i64, label: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO labels (issue_id, label) VALUES (?1, ?2)",
            params![issue_id, label],
        )?;
        Ok(())
    }

    /// Insert a comment for a hydrated issue.
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
    pub fn insert_dependency_raw(&self, blocker_id: i64, blocked_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO dependencies (blocker_id, blocked_id) VALUES (?1, ?2)",
            params![blocker_id, blocked_id],
        )?;
        Ok(())
    }

    /// Insert a raw relation row (used during hydration).
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
    pub fn insert_hydrated_milestone_issue(&self, milestone_id: i64, issue_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO milestone_issues (milestone_id, issue_id) VALUES (?1, ?2)",
            params![milestone_id, issue_id],
        )?;
        Ok(())
    }
}

fn parse_datetime(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|e| {
            eprintln!(
                "warning: failed to parse datetime '{}': {}, using current time",
                s, e
            );
            chrono::Utc::now()
        })
}

/// Maps a database row to an Issue struct.
/// Expects columns in order: id, title, description, status, priority, parent_id, created_at, updated_at, closed_at
fn session_from_row(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        started_at: parse_datetime(row.get::<_, String>(1)?),
        ended_at: row.get::<_, Option<String>>(2)?.map(parse_datetime),
        active_issue_id: row.get(3)?,
        handoff_notes: row.get(4)?,
        last_action: row.get(5)?,
        agent_id: row.get(6)?,
    })
}

fn issue_from_row(row: &rusqlite::Row) -> rusqlite::Result<Issue> {
    Ok(Issue {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        parent_id: row.get(5)?,
        created_at: parse_datetime(row.get::<_, String>(6)?),
        updated_at: parse_datetime(row.get::<_, String>(7)?),
        closed_at: row.get::<_, Option<String>>(8)?.map(parse_datetime),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    // ==================== Issue CRUD Tests ====================

    #[test]
    fn test_create_and_get_issue() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();
        assert!(id > 0);

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.id, id);
        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.description, None);
        assert_eq!(issue.status, "open");
        assert_eq!(issue.priority, "medium");
        assert_eq!(issue.parent_id, None);
        assert!(issue.closed_at.is_none());
    }

    #[test]
    fn test_create_issue_with_description() {
        let (db, _dir) = setup_test_db();

        let id = db
            .create_issue("Test issue", Some("Detailed description"), "high")
            .unwrap();
        let issue = db.get_issue(id).unwrap().unwrap();

        assert_eq!(issue.title, "Test issue");
        assert_eq!(issue.description, Some("Detailed description".to_string()));
        assert_eq!(issue.priority, "high");
    }

    #[test]
    fn test_create_subissue() {
        let (db, _dir) = setup_test_db();

        let parent_id = db.create_issue("Parent issue", None, "high").unwrap();
        let child_id = db
            .create_subissue(parent_id, "Child issue", None, "medium")
            .unwrap();

        let child = db.get_issue(child_id).unwrap().unwrap();
        assert_eq!(child.parent_id, Some(parent_id));

        let subissues = db.get_subissues(parent_id).unwrap();
        assert_eq!(subissues.len(), 1);
        assert_eq!(subissues[0].id, child_id);
    }

    #[test]
    fn test_get_nonexistent_issue() {
        let (db, _dir) = setup_test_db();
        let issue = db.get_issue(99999).unwrap();
        assert!(issue.is_none());
    }

    #[test]
    fn test_list_issues() {
        let (db, _dir) = setup_test_db();

        db.create_issue("Issue 1", None, "low").unwrap();
        db.create_issue("Issue 2", None, "medium").unwrap();
        db.create_issue("Issue 3", None, "high").unwrap();

        let issues = db.list_issues(None, None, None).unwrap();
        assert_eq!(issues.len(), 3);
    }

    #[test]
    fn test_list_issues_filter_by_status() {
        let (db, _dir) = setup_test_db();

        let id1 = db.create_issue("Open issue", None, "low").unwrap();
        let id2 = db.create_issue("To be closed", None, "medium").unwrap();
        db.close_issue(id2).unwrap();

        let open_issues = db.list_issues(Some("open"), None, None).unwrap();
        assert_eq!(open_issues.len(), 1);
        assert_eq!(open_issues[0].id, id1);

        let closed_issues = db.list_issues(Some("closed"), None, None).unwrap();
        assert_eq!(closed_issues.len(), 1);
        assert_eq!(closed_issues[0].id, id2);

        let all_issues = db.list_issues(Some("all"), None, None).unwrap();
        assert_eq!(all_issues.len(), 2);
    }

    #[test]
    fn test_list_issues_filter_by_priority() {
        let (db, _dir) = setup_test_db();

        db.create_issue("Low priority", None, "low").unwrap();
        db.create_issue("High priority", None, "high").unwrap();

        let high_issues = db.list_issues(None, None, Some("high")).unwrap();
        assert_eq!(high_issues.len(), 1);
        assert_eq!(high_issues[0].priority, "high");
    }

    #[test]
    fn test_update_issue() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Original title", None, "low").unwrap();

        let updated = db
            .update_issue(
                id,
                Some("Updated title"),
                Some("New description"),
                Some("critical"),
            )
            .unwrap();
        assert!(updated);

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title, "Updated title");
        assert_eq!(issue.description, Some("New description".to_string()));
        assert_eq!(issue.priority, "critical");
    }

    #[test]
    fn test_update_issue_partial() {
        let (db, _dir) = setup_test_db();

        let id = db
            .create_issue("Original title", Some("Original desc"), "low")
            .unwrap();

        db.update_issue(id, Some("New title"), None, None).unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title, "New title");
        assert_eq!(issue.description, Some("Original desc".to_string()));
        assert_eq!(issue.priority, "low");
    }

    #[test]
    fn test_close_and_reopen_issue() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();

        let closed = db.close_issue(id).unwrap();
        assert!(closed);

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.status, "closed");
        assert!(issue.closed_at.is_some());

        let reopened = db.reopen_issue(id).unwrap();
        assert!(reopened);

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.status, "open");
        assert!(issue.closed_at.is_none());
    }

    #[test]
    fn test_close_nonexistent_issue_returns_false() {
        let (db, _dir) = setup_test_db();

        // Closing an issue that doesn't exist should return false
        let closed = db.close_issue(99999).unwrap();
        assert!(
            !closed,
            "close_issue should return false for nonexistent issue"
        );
    }

    #[test]
    fn test_reopen_nonexistent_issue_returns_false() {
        let (db, _dir) = setup_test_db();

        // Reopening an issue that doesn't exist should return false
        let reopened = db.reopen_issue(99999).unwrap();
        assert!(
            !reopened,
            "reopen_issue should return false for nonexistent issue"
        );
    }

    #[test]
    fn test_delete_issue() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("To delete", None, "low").unwrap();
        assert!(db.get_issue(id).unwrap().is_some());

        let deleted = db.delete_issue(id).unwrap();
        assert!(deleted);
        assert!(db.get_issue(id).unwrap().is_none());
    }

    #[test]
    fn test_delete_nonexistent_issue() {
        let (db, _dir) = setup_test_db();
        let deleted = db.delete_issue(99999).unwrap();
        assert!(!deleted);
    }

    // ==================== Labels Tests ====================

    #[test]
    fn test_add_and_get_labels() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();

        db.add_label(id, "bug").unwrap();
        db.add_label(id, "urgent").unwrap();

        let labels = db.get_labels(id).unwrap();
        assert_eq!(labels.len(), 2);
        assert!(labels.contains(&"bug".to_string()));
        assert!(labels.contains(&"urgent".to_string()));
    }

    #[test]
    fn test_add_duplicate_label_returns_false() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();

        // First add should return true (label was added)
        let first = db.add_label(id, "bug").unwrap();
        assert!(first, "First add_label should return true");

        // Second add should return false (duplicate, nothing inserted)
        let second = db.add_label(id, "bug").unwrap();
        assert!(!second, "Duplicate add_label should return false");

        let labels = db.get_labels(id).unwrap();
        assert_eq!(labels.len(), 1);
    }

    #[test]
    fn test_remove_label() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();

        db.add_label(id, "bug").unwrap();
        db.add_label(id, "urgent").unwrap();

        let removed = db.remove_label(id, "bug").unwrap();
        assert!(removed);

        let labels = db.get_labels(id).unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0], "urgent");
    }

    #[test]
    fn test_remove_nonexistent_label_returns_false() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();
        db.add_label(id, "bug").unwrap();

        // Removing a label that doesn't exist should return false
        let removed = db.remove_label(id, "nonexistent").unwrap();
        assert!(
            !removed,
            "remove_label should return false for nonexistent label"
        );
    }

    #[test]
    fn test_list_issues_filter_by_label() {
        let (db, _dir) = setup_test_db();

        let id1 = db.create_issue("Bug issue", None, "high").unwrap();
        let id2 = db.create_issue("Feature issue", None, "medium").unwrap();

        db.add_label(id1, "bug").unwrap();
        db.add_label(id2, "feature").unwrap();

        let bug_issues = db.list_issues(None, Some("bug"), None).unwrap();
        assert_eq!(bug_issues.len(), 1);
        assert_eq!(bug_issues[0].id, id1);
    }

    // ==================== Comments Tests ====================

    #[test]
    fn test_add_and_get_comments() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();

        let comment_id = db.add_comment(id, "First comment", "note").unwrap();
        assert!(comment_id > 0);

        db.add_comment(id, "Second comment", "note").unwrap();

        let comments = db.get_comments(id).unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].content, "First comment");
        assert_eq!(comments[1].content, "Second comment");
    }

    // ==================== Dependencies Tests ====================

    #[test]
    fn test_add_and_get_dependencies() {
        let (db, _dir) = setup_test_db();

        let blocker = db.create_issue("Blocker issue", None, "high").unwrap();
        let blocked = db.create_issue("Blocked issue", None, "medium").unwrap();

        db.add_dependency(blocked, blocker).unwrap();

        let blockers = db.get_blockers(blocked).unwrap();
        assert_eq!(blockers.len(), 1);
        assert_eq!(blockers[0], blocker);

        let blocking = db.get_blocking(blocker).unwrap();
        assert_eq!(blocking.len(), 1);
        assert_eq!(blocking[0], blocked);
    }

    #[test]
    fn test_remove_dependency() {
        let (db, _dir) = setup_test_db();

        let blocker = db.create_issue("Blocker", None, "high").unwrap();
        let blocked = db.create_issue("Blocked", None, "medium").unwrap();

        db.add_dependency(blocked, blocker).unwrap();
        let removed = db.remove_dependency(blocked, blocker).unwrap();
        assert!(removed);

        let blockers = db.get_blockers(blocked).unwrap();
        assert!(blockers.is_empty());
    }

    #[test]
    fn test_list_blocked_issues() {
        let (db, _dir) = setup_test_db();

        let blocker = db.create_issue("Blocker", None, "high").unwrap();
        let blocked = db.create_issue("Blocked", None, "medium").unwrap();
        let unblocked = db.create_issue("Unblocked", None, "low").unwrap();

        db.add_dependency(blocked, blocker).unwrap();

        let blocked_issues = db.list_blocked_issues().unwrap();
        assert_eq!(blocked_issues.len(), 1);
        assert_eq!(blocked_issues[0].id, blocked);

        // Unblocked issue should not appear
        assert!(!blocked_issues.iter().any(|i| i.id == unblocked));
    }

    #[test]
    fn test_list_ready_issues() {
        let (db, _dir) = setup_test_db();

        let blocker = db.create_issue("Blocker", None, "high").unwrap();
        let blocked = db.create_issue("Blocked", None, "medium").unwrap();
        let ready = db.create_issue("Ready", None, "low").unwrap();

        db.add_dependency(blocked, blocker).unwrap();

        let ready_issues = db.list_ready_issues().unwrap();

        // Blocker and ready should be in ready list (not blocked by anything)
        let ready_ids: Vec<i64> = ready_issues.iter().map(|i| i.id).collect();
        assert!(ready_ids.contains(&blocker));
        assert!(ready_ids.contains(&ready));
        assert!(!ready_ids.contains(&blocked));
    }

    #[test]
    fn test_blocked_becomes_ready_when_blocker_closed() {
        let (db, _dir) = setup_test_db();

        let blocker = db.create_issue("Blocker", None, "high").unwrap();
        let blocked = db.create_issue("Blocked", None, "medium").unwrap();

        db.add_dependency(blocked, blocker).unwrap();

        // Initially blocked
        let blocked_issues = db.list_blocked_issues().unwrap();
        assert_eq!(blocked_issues.len(), 1);

        // Close blocker
        db.close_issue(blocker).unwrap();

        // Now should be ready
        let blocked_issues = db.list_blocked_issues().unwrap();
        assert!(blocked_issues.is_empty());

        let ready_issues = db.list_ready_issues().unwrap();
        assert!(ready_issues.iter().any(|i| i.id == blocked));
    }

    // ==================== Sessions Tests ====================

    #[test]
    fn test_start_and_get_session() {
        let (db, _dir) = setup_test_db();

        let id = db.start_session().unwrap();
        assert!(id > 0);

        let session = db.get_current_session().unwrap().unwrap();
        assert_eq!(session.id, id);
        assert!(session.ended_at.is_none());
        assert!(session.active_issue_id.is_none());
    }

    #[test]
    fn test_end_session() {
        let (db, _dir) = setup_test_db();

        let id = db.start_session().unwrap();
        db.end_session(id, Some("Handoff notes")).unwrap();

        let current = db.get_current_session().unwrap();
        assert!(current.is_none());

        let last = db.get_last_session().unwrap().unwrap();
        assert_eq!(last.id, id);
        assert!(last.ended_at.is_some());
        assert_eq!(last.handoff_notes, Some("Handoff notes".to_string()));
    }

    #[test]
    fn test_update_comment_content() {
        let (db, _dir) = setup_test_db();
        let issue_id = db.create_issue("Test", None, "medium").unwrap();
        let comment_id = db
            .add_comment(issue_id, "See L1 for details", "note")
            .unwrap();

        let updated = db
            .update_comment_content(comment_id, "See #5 for details")
            .unwrap();
        assert!(updated);

        let comments = db.get_comments(issue_id).unwrap();
        assert_eq!(comments[0].content, "See #5 for details");
    }

    #[test]
    fn test_update_comment_content_nonexistent() {
        let (db, _dir) = setup_test_db();
        let updated = db.update_comment_content(99999, "new content").unwrap();
        assert!(!updated);
    }

    #[test]
    fn test_update_session_notes() {
        let (db, _dir) = setup_test_db();
        let session_id = db.start_session().unwrap();
        db.end_session(session_id, Some("Working on L1")).unwrap();

        let updated = db
            .update_session_notes(session_id, "Working on #5")
            .unwrap();
        assert!(updated);

        let session = db.get_last_session().unwrap().unwrap();
        assert_eq!(session.handoff_notes, Some("Working on #5".to_string()));
    }

    #[test]
    fn test_get_all_sessions_with_notes() {
        let (db, _dir) = setup_test_db();

        // Session without notes
        let s1 = db.start_session().unwrap();
        db.end_session(s1, None).unwrap();

        // Session with notes
        let s2 = db.start_session().unwrap();
        db.end_session(s2, Some("Handoff for L1")).unwrap();

        // Another with notes
        let s3 = db.start_session().unwrap();
        db.end_session(s3, Some("Continuing L2 work")).unwrap();

        let sessions = db.get_all_sessions_with_notes().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(
            sessions[0].handoff_notes,
            Some("Handoff for L1".to_string())
        );
        assert_eq!(
            sessions[1].handoff_notes,
            Some("Continuing L2 work".to_string())
        );
    }

    #[test]
    fn test_set_session_issue() {
        let (db, _dir) = setup_test_db();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        let session_id = db.start_session().unwrap();

        db.set_session_issue(session_id, issue_id).unwrap();

        let session = db.get_current_session().unwrap().unwrap();
        assert_eq!(session.active_issue_id, Some(issue_id));
    }

    // ==================== Time Tracking Tests ====================

    #[test]
    fn test_start_and_stop_timer() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();

        let timer_id = db.start_timer(id).unwrap();
        assert!(timer_id > 0);

        let active = db.get_active_timer().unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().0, id);

        std::thread::sleep(std::time::Duration::from_millis(100));

        db.stop_timer(id).unwrap();

        let active = db.get_active_timer().unwrap();
        assert!(active.is_none());
    }

    #[test]
    fn test_get_total_time() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test issue", None, "medium").unwrap();

        // No time tracked yet
        let total = db.get_total_time(id).unwrap();
        assert_eq!(total, 0);
    }

    // ==================== Search Tests ====================

    #[test]
    fn test_search_issues_by_title() {
        let (db, _dir) = setup_test_db();

        db.create_issue("Fix authentication bug", None, "high")
            .unwrap();
        db.create_issue("Add dark mode", None, "medium").unwrap();
        db.create_issue("Auth improvements", None, "low").unwrap();

        let results = db.search_issues("auth").unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_issues_by_description() {
        let (db, _dir) = setup_test_db();

        db.create_issue(
            "Feature A",
            Some("This relates to authentication"),
            "medium",
        )
        .unwrap();
        db.create_issue("Feature B", Some("Something else"), "medium")
            .unwrap();

        let results = db.search_issues("authentication").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_issues_by_comment() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Some issue", None, "medium").unwrap();
        db.add_comment(id, "Found the root cause in authentication module", "note")
            .unwrap();

        let results = db.search_issues("authentication").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, id);
    }

    // ==================== Relations Tests ====================

    #[test]
    fn test_add_and_get_relations() {
        let (db, _dir) = setup_test_db();

        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

        db.add_relation(id1, id2).unwrap();

        let related = db.get_related_issues(id1).unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].id, id2);

        // Bidirectional
        let related = db.get_related_issues(id2).unwrap();
        assert_eq!(related.len(), 1);
        assert_eq!(related[0].id, id1);
    }

    #[test]
    fn test_relation_to_self_fails() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Issue", None, "medium").unwrap();

        let result = db.add_relation(id, id);
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_relation() {
        let (db, _dir) = setup_test_db();

        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();

        db.add_relation(id1, id2).unwrap();
        db.remove_relation(id1, id2).unwrap();

        let related = db.get_related_issues(id1).unwrap();
        assert!(related.is_empty());
    }

    // ==================== Milestones Tests ====================

    #[test]
    fn test_create_and_get_milestone() {
        let (db, _dir) = setup_test_db();

        let id = db.create_milestone("v1.0", Some("First release")).unwrap();
        assert!(id > 0);

        let milestone = db.get_milestone(id).unwrap().unwrap();
        assert_eq!(milestone.name, "v1.0");
        assert_eq!(milestone.description, Some("First release".to_string()));
        assert_eq!(milestone.status, "open");
    }

    #[test]
    fn test_list_milestones() {
        let (db, _dir) = setup_test_db();

        db.create_milestone("v1.0", None).unwrap();
        db.create_milestone("v2.0", None).unwrap();

        let milestones = db.list_milestones(None).unwrap();
        assert_eq!(milestones.len(), 2);
    }

    #[test]
    fn test_add_issue_to_milestone() {
        let (db, _dir) = setup_test_db();

        let milestone_id = db.create_milestone("v1.0", None).unwrap();
        let issue_id = db.create_issue("Feature", None, "medium").unwrap();

        db.add_issue_to_milestone(milestone_id, issue_id).unwrap();

        let issues = db.get_milestone_issues(milestone_id).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, issue_id);

        let milestone = db.get_issue_milestone(issue_id).unwrap().unwrap();
        assert_eq!(milestone.id, milestone_id);
    }

    #[test]
    fn test_close_milestone() {
        let (db, _dir) = setup_test_db();

        let id = db.create_milestone("v1.0", None).unwrap();
        db.close_milestone(id).unwrap();

        let milestone = db.get_milestone(id).unwrap().unwrap();
        assert_eq!(milestone.status, "closed");
        assert!(milestone.closed_at.is_some());
    }

    // ==================== Archive Tests ====================

    #[test]
    fn test_archive_closed_issue() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test", None, "medium").unwrap();
        db.close_issue(id).unwrap();

        let archived = db.archive_issue(id).unwrap();
        assert!(archived);

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.status, "archived");
    }

    #[test]
    fn test_archive_open_issue_fails() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test", None, "medium").unwrap();

        let archived = db.archive_issue(id).unwrap();
        assert!(!archived);

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.status, "open");
    }

    #[test]
    fn test_unarchive_issue() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test", None, "medium").unwrap();
        db.close_issue(id).unwrap();
        db.archive_issue(id).unwrap();

        let unarchived = db.unarchive_issue(id).unwrap();
        assert!(unarchived);

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.status, "closed");
    }

    #[test]
    fn test_list_archived_issues() {
        let (db, _dir) = setup_test_db();

        let id1 = db.create_issue("Archived", None, "medium").unwrap();
        let _id2 = db.create_issue("Open", None, "medium").unwrap();

        db.close_issue(id1).unwrap();
        db.archive_issue(id1).unwrap();

        let archived = db.list_archived_issues().unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].id, id1);
    }

    // ==================== Security Tests ====================

    #[test]
    fn test_sql_injection_in_title() {
        let (db, _dir) = setup_test_db();

        // Attempt SQL injection via title
        let malicious = "'; DROP TABLE issues; --";
        let id = db.create_issue(malicious, None, "medium").unwrap();

        // Should have created issue with literal string, not executed SQL
        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title, malicious);

        // Database should still be intact
        let issues = db.list_issues(None, None, None).unwrap();
        assert!(!issues.is_empty());
    }

    #[test]
    fn test_sql_injection_in_description() {
        let (db, _dir) = setup_test_db();

        let malicious = "test'); DELETE FROM issues; --";
        let id = db
            .create_issue("Normal title", Some(malicious), "medium")
            .unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.description, Some(malicious.to_string()));
    }

    #[test]
    fn test_sql_injection_in_label() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test", None, "medium").unwrap();
        let malicious = "bug'; DROP TABLE labels; --";

        db.add_label(id, malicious).unwrap();

        let labels = db.get_labels(id).unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0], malicious);
    }

    #[test]
    fn test_sql_injection_in_search() {
        let (db, _dir) = setup_test_db();

        db.create_issue("Normal issue", None, "medium").unwrap();

        // Attempt injection in search
        let malicious = "%'; DROP TABLE issues; --";
        let results = db.search_issues(malicious).unwrap();

        // Should return empty results, not crash
        assert!(results.is_empty());

        // Database should still be intact
        let issues = db.list_issues(None, None, None).unwrap();
        assert!(!issues.is_empty());
    }

    #[test]
    fn test_sql_injection_in_comment() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test", None, "medium").unwrap();
        let malicious = "comment'); DELETE FROM comments; --";

        db.add_comment(id, malicious, "note").unwrap();

        let comments = db.get_comments(id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, malicious);
    }

    #[test]
    fn test_unicode_in_fields() {
        let (db, _dir) = setup_test_db();

        let title = "测试问题 🐛 αβγ";
        let description = "Description with émojis 🎉 and ñ";

        let id = db.create_issue(title, Some(description), "medium").unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title, title);
        assert_eq!(issue.description, Some(description.to_string()));
    }

    #[test]
    fn test_very_long_strings() {
        let (db, _dir) = setup_test_db();

        // Within limits: should succeed
        let long_title = "a".repeat(MAX_TITLE_LEN);
        let long_desc = "b".repeat(MAX_DESCRIPTION_LEN);

        let id = db
            .create_issue(&long_title, Some(&long_desc), "medium")
            .unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title.len(), MAX_TITLE_LEN);
        assert_eq!(issue.description.unwrap().len(), MAX_DESCRIPTION_LEN);

        // Exceeding limits: should fail
        let too_long_title = "a".repeat(MAX_TITLE_LEN + 1);
        assert!(db.create_issue(&too_long_title, None, "medium").is_err());

        let too_long_desc = "b".repeat(MAX_DESCRIPTION_LEN + 1);
        assert!(db
            .create_issue("ok", Some(&too_long_desc), "medium")
            .is_err());
    }

    #[test]
    fn test_null_bytes_in_strings() {
        let (db, _dir) = setup_test_db();

        let title = "test\0null\0bytes";
        let id = db.create_issue(title, None, "medium").unwrap();

        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title, title);
    }

    // ==================== Cascade Delete Tests ====================

    #[test]
    fn test_delete_issue_cascades_labels() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test", None, "medium").unwrap();
        db.add_label(id, "bug").unwrap();
        db.add_label(id, "urgent").unwrap();

        db.delete_issue(id).unwrap();

        // Labels should be gone (via CASCADE)
        let labels = db.get_labels(id).unwrap();
        assert!(labels.is_empty());
    }

    #[test]
    fn test_delete_issue_cascades_comments() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("Test", None, "medium").unwrap();
        db.add_comment(id, "Comment 1", "note").unwrap();
        db.add_comment(id, "Comment 2", "note").unwrap();

        db.delete_issue(id).unwrap();

        let comments = db.get_comments(id).unwrap();
        assert!(comments.is_empty());
    }

    #[test]
    fn test_delete_parent_cascades_subissues() {
        let (db, _dir) = setup_test_db();

        let parent_id = db.create_issue("Parent", None, "high").unwrap();
        let child_id = db
            .create_subissue(parent_id, "Child", None, "medium")
            .unwrap();

        db.delete_issue(parent_id).unwrap();

        // Child should be deleted too
        assert!(db.get_issue(child_id).unwrap().is_none());
    }

    // ==================== Edge Cases ====================

    #[test]
    fn test_empty_title() {
        let (db, _dir) = setup_test_db();

        let id = db.create_issue("", None, "medium").unwrap();
        let issue = db.get_issue(id).unwrap().unwrap();
        assert_eq!(issue.title, "");
    }

    #[test]
    fn test_update_parent() {
        let (db, _dir) = setup_test_db();

        let parent1 = db.create_issue("Parent 1", None, "high").unwrap();
        let parent2 = db.create_issue("Parent 2", None, "high").unwrap();
        let child = db.create_issue("Child", None, "medium").unwrap();

        db.update_parent(child, Some(parent1)).unwrap();
        let issue = db.get_issue(child).unwrap().unwrap();
        assert_eq!(issue.parent_id, Some(parent1));

        db.update_parent(child, Some(parent2)).unwrap();
        let issue = db.get_issue(child).unwrap().unwrap();
        assert_eq!(issue.parent_id, Some(parent2));

        db.update_parent(child, None).unwrap();
        let issue = db.get_issue(child).unwrap().unwrap();
        assert_eq!(issue.parent_id, None);
    }

    // ==================== Database Corruption Recovery ====================

    #[test]
    fn test_corrupted_db_file_empty() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("issues.db");

        // Create an empty file (corrupted)
        std::fs::write(&db_path, b"").unwrap();

        // SQLite treats empty files as new databases, so this should succeed
        // and the database should be usable afterward
        let result = Database::open(&db_path);
        assert!(
            result.is_ok(),
            "Empty file should be treated as new DB: {:?}",
            result.err()
        );
        let db = result.unwrap();
        let id = db
            .create_issue("Test after recovery", None, "medium")
            .unwrap();
        assert!(id > 0);
    }

    #[test]
    fn test_corrupted_db_file_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("issues.db");

        // Write garbage data
        std::fs::write(&db_path, b"not a sqlite database at all!").unwrap();

        // Should fail gracefully with an error, not panic
        let result = Database::open(&db_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_corrupted_db_file_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("issues.db");

        // Create valid DB first
        {
            let db = Database::open(&db_path).unwrap();
            db.create_issue("Test", None, "medium").unwrap();
        }

        // Truncate it (simulate crash during write)
        let content = std::fs::read(&db_path).unwrap();
        std::fs::write(&db_path, &content[..content.len() / 2]).unwrap();

        // Truncated DB should fail to open -- SQLite detects corruption
        let result = Database::open(&db_path);
        match result {
            Err(e) => {
                let err_msg = format!("{}", e);
                assert!(
                    err_msg.contains("not a database")
                        || err_msg.contains("malformed")
                        || err_msg.contains("corrupt")
                        || err_msg.contains("disk image"),
                    "Error should indicate corruption, got: {}",
                    err_msg
                );
            }
            Ok(db) => {
                // If SQLite somehow recovers, verify the original data is gone
                let issues = db.list_issues(Some("all"), None, None).unwrap();
                assert!(
                    issues.is_empty(),
                    "Truncated DB should not retain original data"
                );
            }
        }
    }

    #[test]
    fn test_db_readonly_location() {
        // This test only works on Unix-like systems
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let dir = tempfile::tempdir().unwrap();
            let db_path = dir.path().join("issues.db");

            // Create the file first
            std::fs::write(&db_path, b"").unwrap();

            // Make it read-only
            let mut perms = std::fs::metadata(&db_path).unwrap().permissions();
            perms.set_mode(0o444);
            std::fs::set_permissions(&db_path, perms).unwrap();

            // Should fail gracefully
            let result = Database::open(&db_path);
            assert!(result.is_err());
        }
    }
}

// ==================== Property-Based Tests ====================

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("issues.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    // Generate valid priority strings
    fn valid_priority() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("low".to_string()),
            Just("medium".to_string()),
            Just("high".to_string()),
            Just("critical".to_string()),
        ]
    }

    // Generate arbitrary (but safe) strings for titles
    fn safe_string() -> impl Strategy<Value = String> {
        // Avoid null bytes; limit to MAX_TITLE_LEN so strings are valid as titles
        "[a-zA-Z0-9 _\\-\\.!?]{0,512}".prop_map(|s| s)
    }

    proptest! {
        /// Any valid title should be storable and retrievable unchanged
        #[test]
        fn prop_title_roundtrip(title in safe_string()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue(&title, None, "medium").unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.title, title);
        }

        /// Any valid description should be storable and retrievable unchanged
        #[test]
        fn prop_description_roundtrip(desc in safe_string()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue("Test", Some(&desc), "medium").unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.description, Some(desc));
        }

        /// All valid priorities should work
        #[test]
        fn prop_priority_valid(priority in valid_priority()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue("Test", None, &priority).unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.priority, priority);
        }

        /// Labels should be storable and retrievable
        #[test]
        fn prop_label_roundtrip(label in "[a-zA-Z0-9_\\-]{1,50}") {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue("Test", None, "medium").unwrap();
            db.add_label(id, &label).unwrap();
            let labels = db.get_labels(id).unwrap();
            prop_assert!(labels.contains(&label));
        }

        /// Comments should be storable and retrievable
        #[test]
        fn prop_comment_roundtrip(content in safe_string()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue("Test", None, "medium").unwrap();
            db.add_comment(id, &content, "note").unwrap();
            let comments = db.get_comments(id).unwrap();
            prop_assert_eq!(comments.len(), 1);
            prop_assert_eq!(&comments[0].content, &content);
        }

        /// Creating multiple issues should always increase count
        #[test]
        fn prop_create_increases_count(count in 1usize..20) {
            let (db, _dir) = setup_test_db();
            for i in 0..count {
                db.create_issue(&format!("Issue {}", i), None, "medium").unwrap();
            }
            let issues = db.list_issues(None, None, None).unwrap();
            prop_assert_eq!(issues.len(), count);
        }

        /// Close then reopen should leave issue open
        #[test]
        fn prop_close_reopen_idempotent(title in safe_string()) {
            let (db, _dir) = setup_test_db();
            let id = db.create_issue(&title, None, "medium").unwrap();

            db.close_issue(id).unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.status, "closed");

            db.reopen_issue(id).unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            prop_assert_eq!(issue.status, "open");
        }

        /// Blocking should be reflected in blocked list
        #[test]
        fn prop_blocking_relationship(a in 1i64..100, b in 1i64..100) {
            if a == b {
                return Ok(()); // Skip self-blocking
            }
            let (db, _dir) = setup_test_db();

            // Create both issues
            for i in 1..=std::cmp::max(a, b) {
                db.create_issue(&format!("Issue {}", i), None, "medium").unwrap();
            }

            db.add_dependency(a, b).unwrap();
            let blockers = db.get_blockers(a).unwrap();
            prop_assert!(blockers.contains(&b));
        }

        /// Search should find issues with matching titles
        #[test]
        fn prop_search_finds_title(
            prefix in "[a-zA-Z]{3,10}",
            suffix in "[a-zA-Z]{3,10}"
        ) {
            let (db, _dir) = setup_test_db();
            let title = format!("{} unique marker {}", prefix, suffix);
            db.create_issue(&title, None, "medium").unwrap();

            // Search for the unique marker
            let results = db.search_issues("unique marker").unwrap();
            prop_assert!(!results.is_empty());
            prop_assert!(results.iter().any(|i| i.title.contains("unique marker")));
        }

        /// Circular dependencies should be prevented
        #[test]
        fn prop_no_circular_deps(chain_len in 2usize..6) {
            let (db, _dir) = setup_test_db();

            // Create a chain of issues
            let mut ids = Vec::new();
            for i in 0..chain_len {
                let id = db.create_issue(&format!("Issue {}", i), None, "medium").unwrap();
                ids.push(id);
            }

            // Create a linear dependency chain: 0 <- 1 <- 2 <- ... <- n-1
            for i in 0..chain_len - 1 {
                db.add_dependency(ids[i], ids[i + 1]).unwrap();
            }

            // Trying to close the cycle (n-1 <- 0) should fail
            let result = db.add_dependency(ids[chain_len - 1], ids[0]);
            prop_assert!(result.is_err(), "Circular dependency should be rejected");
        }

        /// Deleting a parent should cascade to all children
        #[test]
        fn prop_cascade_deletes_children(child_count in 1usize..5) {
            let (db, _dir) = setup_test_db();

            // Create parent
            let parent_id = db.create_issue("Parent", None, "medium").unwrap();

            // Create children
            let mut child_ids = Vec::new();
            for i in 0..child_count {
                let id = db.create_subissue(parent_id, &format!("Child {}", i), None, "low").unwrap();
                child_ids.push(id);
            }

            // Verify children exist
            let issues_before = db.list_issues(None, None, None).unwrap();
            prop_assert_eq!(issues_before.len(), child_count + 1);

            // Delete parent
            db.delete_issue(parent_id).unwrap();

            // All children should be gone too
            let issues_after = db.list_issues(None, None, None).unwrap();
            prop_assert_eq!(issues_after.len(), 0);

            // Verify each child is gone
            for child_id in child_ids {
                let child = db.get_issue(child_id).unwrap();
                prop_assert!(child.is_none(), "Child should be deleted");
            }
        }

        /// Ready list should never contain issues with open blockers
        #[test]
        fn prop_ready_list_correctness(issue_count in 2usize..8) {
            let (db, _dir) = setup_test_db();

            // Create issues
            let mut ids = Vec::new();
            for i in 0..issue_count {
                let id = db.create_issue(&format!("Issue {}", i), None, "medium").unwrap();
                ids.push(id);
            }

            // Create some dependencies (each issue blocked by next, except last)
            for i in 0..issue_count - 1 {
                let _ = db.add_dependency(ids[i], ids[i + 1]);
            }

            // Get ready issues
            let ready = db.list_ready_issues().unwrap();

            // Verify: no ready issue should have open blockers
            for issue in &ready {
                let blockers = db.get_blockers(issue.id).unwrap();
                for blocker_id in blockers {
                    if let Some(blocker) = db.get_issue(blocker_id).unwrap() {
                        prop_assert_ne!(
                            blocker.status, "open",
                            "Ready issue {} has open blocker {}",
                            issue.id, blocker_id
                        );
                    }
                }
            }
        }

        /// Session active_issue_id should be set to NULL when issue is deleted
        #[test]
        fn prop_session_issue_delete_cascade(title in safe_string()) {
            let (db, _dir) = setup_test_db();

            // Create issue and session
            let issue_id = db.create_issue(&title, None, "medium").unwrap();
            let session_id = db.start_session().unwrap();
            db.set_session_issue(session_id, issue_id).unwrap();

            // Verify session has issue
            let session = db.get_current_session().unwrap().unwrap();
            prop_assert_eq!(session.active_issue_id, Some(issue_id));

            // Delete the issue
            db.delete_issue(issue_id).unwrap();

            // Session should still exist but with NULL active_issue_id
            let session_after = db.get_current_session().unwrap().unwrap();
            prop_assert_eq!(session_after.id, session_id);
            prop_assert_eq!(session_after.active_issue_id, None, "Session active_issue_id should be NULL after issue deletion");
        }

        /// Search wildcards should be escaped properly
        #[test]
        fn prop_search_wildcards_escaped(
            prefix in "[a-zA-Z]{3,5}",
            suffix in "[a-zA-Z]{3,5}"
        ) {
            let (db, _dir) = setup_test_db();

            // Create an issue with % and _ in title
            let special_title = format!("{}%test_marker{}", prefix, suffix);
            db.create_issue(&special_title, None, "medium").unwrap();

            // Create another issue that would match if wildcards weren't escaped
            db.create_issue("other content here", None, "medium").unwrap();

            // Search for the special characters literally
            let results = db.search_issues("%test_").unwrap();

            // Should find only the issue with literal % and _
            prop_assert!(results.iter().all(|i| i.title.contains("%test_")));
        }
    }
}
