use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

pub const SCHEMA_VERSION: i32 = 15;

/// Valid values for issue priority.
pub const VALID_PRIORITIES: &[&str] = &["low", "medium", "high", "critical"];

/// Valid values for issue status.
pub const VALID_STATUSES: &[&str] = &["open", "closed", "archived"];

/// Maximum lengths for string inputs.
pub const MAX_TITLE_LEN: usize = 512;
pub const MAX_LABEL_LEN: usize = 128;
pub const MAX_DESCRIPTION_LEN: usize = 64 * 1024; // 64KB
pub const MAX_COMMENT_LEN: usize = 1024 * 1024; // 1MB

/// Validate that a status value is known, returning an error if not.
///
/// # Errors
///
/// Returns an error if the status is not one of the valid values.
pub fn validate_status(status: &str) -> Result<()> {
    if VALID_STATUSES.contains(&status) {
        Ok(())
    } else {
        anyhow::bail!(
            "Invalid status '{}'. Valid values: {}",
            status,
            VALID_STATUSES.join(", ")
        )
    }
}

/// Validate that a priority value is known, returning an error if not.
///
/// # Errors
///
/// Returns an error if the priority is not one of the valid values.
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

pub struct Database {
    pub(crate) conn: Connection,
}

impl Database {
    /// Open a database at the given path, initializing the schema if needed.
    ///
    /// # Errors
    /// Returns an error if the database cannot be opened or schema initialization fails.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).context("Failed to open database")?;
        let db = Self { conn };
        db.init_schema()?;
        Ok(db)
    }

    /// Execute a closure within a database transaction.
    /// If the closure returns Ok, the transaction is committed.
    /// If the closure returns Err or the closure panics, the transaction is
    /// rolled back automatically via rusqlite's RAII `Transaction` type.
    ///
    /// # Errors
    /// Returns an error if the transaction cannot be started, committed, or if the closure fails.
    pub fn transaction<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce() -> Result<T>,
    {
        let tx = self.conn.unchecked_transaction()?;
        let result = f()?;
        tx.commit()?;
        Ok(result)
    }

    /// Toggle `SQLite` foreign key enforcement.
    ///
    /// Must be called outside a transaction (`PRAGMA foreign_keys` is a
    /// no-op inside one). Used by hydration to prevent `ON DELETE` cascades
    /// during bulk clear/reinsert (#461).
    ///
    /// # Errors
    /// Returns an error if the pragma execution fails.
    pub fn set_foreign_keys(&self, enabled: bool) -> Result<()> {
        let value = if enabled { "ON" } else { "OFF" };
        self.conn
            .execute_batch(&format!("PRAGMA foreign_keys = {value};"))?;
        Ok(())
    }

    /// Run a migration statement, logging unexpected errors.
    /// Expected errors (duplicate column, table already exists) are logged at debug level.
    fn migrate(&self, sql: &str) {
        if let Err(e) = self.conn.execute(sql, []) {
            let msg = e.to_string();
            if msg.contains("duplicate column") || msg.contains("already exists") {
                tracing::debug!(
                    "migration skipped (already applied): {}: {}",
                    sql.trim(),
                    msg
                );
            } else {
                tracing::warn!("migration error ({}): {}", sql.trim(), msg);
            }
        }
    }

    /// Run a batch migration statement, logging unexpected errors.
    /// Expected errors (duplicate column, table already exists) are logged at debug level.
    fn migrate_batch(&self, sql: &str) {
        if let Err(e) = self.conn.execute_batch(sql) {
            let msg = e.to_string();
            if msg.contains("duplicate column") || msg.contains("already exists") {
                tracing::debug!("migration batch skipped (already applied): {}", msg);
            } else {
                tracing::warn!("migration batch error: {}", msg);
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
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "failed to read schema version (PRAGMA user_version): {e}, defaulting to 0"
                );
                0
            });

        if version < SCHEMA_VERSION {
            self.create_tables()?;
            self.run_migrations(version);

            self.conn
                .execute(&format!("PRAGMA user_version = {SCHEMA_VERSION}"), [])?;
        }

        // Enable foreign keys
        self.conn.execute("PRAGMA foreign_keys = ON", [])?;

        Ok(())
    }

    fn create_tables(&self) -> Result<()> {
        self.conn.execute_batch(
            r"
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
                ",
        )?;
        Ok(())
    }

    fn run_migrations(&self, version: i32) {
        // Migration: add parent_id column if upgrading from v1
        self.migrate(
            "ALTER TABLE issues ADD COLUMN parent_id INTEGER REFERENCES issues(id) ON DELETE CASCADE",
        );

        // Migration v7: Recreate sessions table with ON DELETE SET NULL for active_issue_id
        // This ensures deleting an issue clears the session reference instead of failing
        if version < 7 {
            self.migrate_batch(
                r"
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
                    ",
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

        // Migration v15: Token usage tracking table for web dashboard
        if version < 15 {
            self.migrate_batch(
                r"
                    CREATE TABLE IF NOT EXISTS token_usage (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        agent_id TEXT NOT NULL,
                        session_id INTEGER,
                        timestamp TEXT NOT NULL,
                        input_tokens INTEGER NOT NULL DEFAULT 0,
                        output_tokens INTEGER NOT NULL DEFAULT 0,
                        cache_read_tokens INTEGER,
                        cache_creation_tokens INTEGER,
                        model TEXT NOT NULL DEFAULT 'unknown',
                        cost_estimate REAL,
                        FOREIGN KEY (session_id) REFERENCES sessions(id) ON DELETE SET NULL
                    );
                    CREATE INDEX IF NOT EXISTS idx_token_usage_agent ON token_usage(agent_id);
                    CREATE INDEX IF NOT EXISTS idx_token_usage_session ON token_usage(session_id);
                    CREATE INDEX IF NOT EXISTS idx_token_usage_timestamp ON token_usage(timestamp);
                    ",
            );
        }
    }

    /// Get the current schema version (PRAGMA `user_version`).
    ///
    /// # Errors
    /// Returns an error if the pragma query fails.
    pub fn get_schema_version(&self) -> Result<i32> {
        let version: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok(version)
    }
}
