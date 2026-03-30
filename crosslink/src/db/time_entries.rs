use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;

use super::core::Database;
use super::helpers::parse_datetime;

/// Row from `get_time_entries_for_issue`: (id, `started_at`, `ended_at`, `duration_seconds`).
pub type TimeEntryRow = (i64, DateTime<Utc>, Option<DateTime<Utc>>, Option<i64>);

impl Database {
    /// Start a timer for the given issue.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn start_timer(&self, issue_id: i64) -> Result<i64> {
        let issue_id = self.resolve_id(issue_id);
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO time_entries (issue_id, started_at) VALUES (?1, ?2)",
            params![issue_id, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Stop the active timer for the given issue.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn stop_timer(&self, issue_id: i64) -> Result<bool> {
        let issue_id = self.resolve_id(issue_id);
        let now_str = Utc::now().to_rfc3339();

        let rows = self.conn.execute(
            "UPDATE time_entries SET ended_at = ?1, duration_seconds = CAST((julianday(?1) - julianday(started_at)) * 86400 AS INTEGER) WHERE issue_id = ?2 AND ended_at IS NULL",
            params![now_str, issue_id],
        )?;
        Ok(rows > 0)
    }

    /// Get the currently active (unfinished) timer, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_active_timer(&self) -> Result<Option<(i64, DateTime<Utc>)>> {
        let result: Result<(i64, String), _> = self
            .conn
            .query_row(
                "SELECT issue_id, started_at FROM time_entries WHERE ended_at IS NULL ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            );

        match result {
            Ok((id, started)) => Ok(Some((id, parse_datetime(&started)))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get the total tracked time for an issue in seconds.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_total_time(&self, issue_id: i64) -> Result<i64> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(duration_seconds), 0) FROM time_entries WHERE issue_id = ?1 AND duration_seconds IS NOT NULL",
                [issue_id],
                |row| row.get(0),
            )?;
        Ok(total)
    }

    /// Get time entries for an issue.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_time_entries_for_issue(&self, issue_id: i64) -> Result<Vec<TimeEntryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, duration_seconds FROM time_entries WHERE issue_id = ?1 ORDER BY id",
        )?;
        let entries = stmt
            .query_map([issue_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    parse_datetime(&row.get::<_, String>(1)?),
                    row.get::<_, Option<String>>(2)?.map(|s| parse_datetime(&s)),
                    row.get::<_, Option<i64>>(3)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(entries)
    }
}
