//! Per-user dashboard `SQLite` index at `~/.crosslink/dashboard.db`.
//!
//! Schema mirrors `DESIGN-CROSSLINK-DASHBOARD.md` §6. Each user's local
//! dashboard has its own index — projects, materialised aggregate state,
//! alerts, running PTY sessions, audit log, and activity feed.
//!
//! This is distinct from the main crosslink `db` module ([`crate::db`])
//! which lives inside each project and tracks that project's issues.

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// Current schema version. Increment and add a migration block to
/// [`DashboardDb::open`] when the schema changes.
pub const SCHEMA_VERSION: i32 = 1;

/// Handle on the dashboard's `SQLite` index.
pub struct DashboardDb {
    pub conn: Connection,
}

impl DashboardDb {
    /// Open (and create if missing) the dashboard DB at the given path.
    /// Applies the schema and any pending migrations.
    ///
    /// # Errors
    /// Returns an error if the parent directory cannot be created, the
    /// connection cannot be opened, or a migration fails.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create dashboard state dir {}", parent.display())
            })?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open dashboard DB at {}", path.display()))?;

        let this = Self { conn };
        this.create_tables()?;
        let current_version = this.get_schema_version()?;
        if current_version < SCHEMA_VERSION {
            // Version-gated migration blocks go here as the schema
            // evolves. v1 is the initial schema — no migrations needed
            // to reach it beyond the idempotent `create_tables` above.
            this.conn
                .pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(this)
    }

    /// Default dashboard DB location: `~/.crosslink/dashboard.db`.
    ///
    /// # Errors
    /// Returns an error if the user's home directory cannot be determined.
    pub fn default_path() -> Result<PathBuf> {
        let home =
            resolve_home_dir().context("Could not determine home directory for dashboard state")?;
        Ok(home.join(".crosslink").join("dashboard.db"))
    }

    /// Current schema version from `PRAGMA user_version`.
    fn get_schema_version(&self) -> Result<i32> {
        let v: i32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok(v)
    }

    fn create_tables(&self) -> Result<()> {
        // See DESIGN-CROSSLINK-DASHBOARD.md §6 for the authoritative
        // schema description and column semantics.
        self.conn.execute_batch(
            r"
                -- Tracked repositories
                CREATE TABLE IF NOT EXISTS projects (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    slug TEXT NOT NULL UNIQUE,
                    clone_path TEXT NOT NULL,
                    default_branch TEXT NOT NULL,
                    hub_sha TEXT,
                    hub_fetched_at TEXT,
                    status TEXT NOT NULL DEFAULT 'active',
                    added_at TEXT NOT NULL,
                    last_activity_at TEXT,
                    pinned INTEGER NOT NULL DEFAULT 0
                );

                -- Materialised aggregate state per project (tile rendering)
                CREATE TABLE IF NOT EXISTS project_state (
                    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                    open_issues INTEGER NOT NULL DEFAULT 0,
                    overdue_issues INTEGER NOT NULL DEFAULT 0,
                    due_soon_issues INTEGER NOT NULL DEFAULT 0,
                    blocked_issues INTEGER NOT NULL DEFAULT 0,
                    active_agents INTEGER NOT NULL DEFAULT 0,
                    stale_locks INTEGER NOT NULL DEFAULT 0,
                    ci_status TEXT,
                    updated_at TEXT NOT NULL
                );

                -- Active alerts (per-project, derived; ACK is local-only)
                CREATE TABLE IF NOT EXISTS alerts (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                    kind TEXT NOT NULL,
                    severity TEXT NOT NULL,
                    subject_ref TEXT,
                    detail TEXT,
                    opened_at TEXT NOT NULL,
                    resolved_at TEXT,
                    acknowledged_at TEXT,
                    acknowledged_by TEXT
                );

                -- Running xterm.js PTY sessions
                CREATE TABLE IF NOT EXISTS pty_sessions (
                    id TEXT PRIMARY KEY,
                    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
                    command TEXT NOT NULL,
                    started_at TEXT NOT NULL,
                    ended_at TEXT,
                    exit_code INTEGER,
                    pid INTEGER
                );

                -- Write-action audit log (what the dashboard did, for whom)
                CREATE TABLE IF NOT EXISTS actions (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    project_id INTEGER REFERENCES projects(id),
                    actor TEXT NOT NULL,
                    verb TEXT NOT NULL,
                    subject TEXT,
                    payload_json TEXT,
                    requested_at TEXT NOT NULL,
                    completed_at TEXT,
                    outcome TEXT,
                    error TEXT
                );

                -- Per-project event stream (for drill-down activity feeds)
                CREATE TABLE IF NOT EXISTS activity (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                    at TEXT NOT NULL,
                    author TEXT,
                    kind TEXT NOT NULL,
                    subject_ref TEXT,
                    summary TEXT
                );

                -- Free-form config k/v persisted across restarts
                CREATE TABLE IF NOT EXISTS config (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_alerts_project_open
                    ON alerts(project_id) WHERE resolved_at IS NULL;
                CREATE INDEX IF NOT EXISTS idx_activity_project_at
                    ON activity(project_id, at DESC);
                CREATE INDEX IF NOT EXISTS idx_actions_project_at
                    ON actions(project_id, requested_at DESC);
            ",
        )?;
        Ok(())
    }
}

/// Resolve the user's home directory from environment variables,
/// preferring `$HOME` on Unix and `$USERPROFILE` on Windows. Matches the
/// resolution approach in `crosslink/src/sync/trust.rs::home_dir`.
fn resolve_home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_open_creates_file_and_parent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("dashboard.db");
        let db = DashboardDb::open(&path).unwrap();
        assert!(path.exists(), "dashboard DB file should exist after open");
        assert_eq!(db.get_schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn test_open_idempotent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dashboard.db");
        {
            let _db = DashboardDb::open(&path).unwrap();
        }
        // Re-opening must not error or clobber.
        let db = DashboardDb::open(&path).unwrap();
        assert_eq!(db.get_schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn test_open_creates_all_tables() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dashboard.db");
        let db = DashboardDb::open(&path).unwrap();

        // Verify each table exists via sqlite_master.
        for expected in &[
            "projects",
            "project_state",
            "alerts",
            "pty_sessions",
            "actions",
            "activity",
            "config",
        ] {
            let count: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    [expected],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "expected table '{expected}' to exist");
        }
    }

    #[test]
    fn test_open_creates_expected_indexes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dashboard.db");
        let db = DashboardDb::open(&path).unwrap();
        for expected in &[
            "idx_alerts_project_open",
            "idx_activity_project_at",
            "idx_actions_project_at",
        ] {
            let count: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    [expected],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "expected index '{expected}' to exist");
        }
    }

    #[test]
    fn test_default_path_ends_in_dashboard_db() {
        std::env::set_var("HOME", "/tmp/test-home-crosslink-ops");
        let p = DashboardDb::default_path().unwrap();
        assert!(p.ends_with(".crosslink/dashboard.db"));
    }

    #[test]
    fn test_projects_table_basic_insert() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("dashboard.db");
        let db = DashboardDb::open(&path).unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, added_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    "forecast-bio/crosslink",
                    "/tmp/x",
                    "main",
                    "2026-04-20T00:00:00Z"
                ],
            )
            .unwrap();
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}
