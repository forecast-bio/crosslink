use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use super::core::Database;

/// A row from the `sentinel_runs` table.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SentinelRun {
    pub id: i64,
    pub run_id: String,
    pub started_at: String,
    pub completed_at: Option<String>,
    pub mode: String,
    pub signals_found: i64,
    pub dispatched: i64,
    pub collected: i64,
    pub triaged: i64,
    pub skipped: i64,
    pub deferred: i64,
}

/// A row from the `sentinel_dispatches` table.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SentinelDispatch {
    pub id: i64,
    pub run_id: String,
    pub signal_ref: String,
    pub signal_title: String,
    pub source: String,
    pub disposition: String,
    pub agent_id: Option<String>,
    pub crosslink_issue_id: Option<i64>,
    pub gh_issue_number: Option<i64>,
    pub label: String,
    pub attempt_number: i32,
    pub model_used: Option<String>,
    pub outcome: String,
    pub outcome_detail: Option<String>,
    pub created_at: String,
    pub completed_at: Option<String>,
}

/// Aggregated dispatch metrics grouped by model and label.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DispatchMetric {
    pub model: String,
    pub label: String,
    pub total: i64,
    pub successes: i64,
    pub failures: i64,
    pub exhausted: i64,
    pub pending: i64,
    pub orphaned: i64,
    pub success_rate: f64,
}

/// Counter columns for completing a sentinel run.
#[derive(Debug, Clone, Default)]
pub struct RunCounters {
    pub signals_found: i64,
    pub dispatched: i64,
    pub collected: i64,
    pub triaged: i64,
    pub skipped: i64,
    pub deferred: i64,
}

/// Parameters for inserting a new sentinel dispatch record.
pub struct NewDispatch<'a> {
    pub run_id: &'a str,
    pub signal_ref: &'a str,
    pub signal_title: &'a str,
    pub source: &'a str,
    pub disposition: &'a str,
    pub agent_id: Option<&'a str>,
    pub crosslink_issue_id: Option<i64>,
    pub gh_issue_number: Option<i64>,
    pub label: &'a str,
    pub attempt_number: i32,
    pub model_used: Option<&'a str>,
}

impl Database {
    // === Sentinel runs ===

    /// Insert a new sentinel run record. Returns the auto-generated row ID.
    pub fn insert_sentinel_run(&self, run_id: &str, mode: &str) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sentinel_runs (run_id, started_at, mode) VALUES (?1, ?2, ?3)",
            params![run_id, now, mode],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a sentinel run with final statistics.
    pub fn complete_sentinel_run(&self, run_id: &str, counters: &RunCounters) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sentinel_runs
             SET completed_at = ?1, signals_found = ?2, dispatched = ?3,
                 collected = ?4, triaged = ?5, skipped = ?6, deferred = ?7
             WHERE run_id = ?8",
            params![
                now,
                counters.signals_found,
                counters.dispatched,
                counters.collected,
                counters.triaged,
                counters.skipped,
                counters.deferred,
                run_id,
            ],
        )?;
        Ok(())
    }

    /// List recent sentinel runs, most recent first.
    pub fn list_sentinel_runs(&self, limit: usize) -> Result<Vec<SentinelRun>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, started_at, completed_at, mode,
                    signals_found, dispatched, collected, triaged, skipped, deferred
             FROM sentinel_runs ORDER BY started_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit as i64], |row| {
                Ok(SentinelRun {
                    id: row.get(0)?,
                    run_id: row.get(1)?,
                    started_at: row.get(2)?,
                    completed_at: row.get(3)?,
                    mode: row.get(4)?,
                    signals_found: row.get(5)?,
                    dispatched: row.get(6)?,
                    collected: row.get(7)?,
                    triaged: row.get(8)?,
                    skipped: row.get(9)?,
                    deferred: row.get(10)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    // === Sentinel dispatches ===

    /// Insert a new sentinel dispatch record. Returns the auto-generated row ID.
    pub fn insert_sentinel_dispatch(&self, d: &NewDispatch<'_>) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sentinel_dispatches
             (run_id, signal_ref, signal_title, source, disposition, agent_id,
              crosslink_issue_id, gh_issue_number, label, attempt_number, model_used, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                d.run_id,
                d.signal_ref,
                d.signal_title,
                d.source,
                d.disposition,
                d.agent_id,
                d.crosslink_issue_id,
                d.gh_issue_number,
                d.label,
                d.attempt_number,
                d.model_used,
                now,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a dispatch record with its outcome.
    pub fn update_dispatch_outcome(
        &self,
        dispatch_id: i64,
        outcome: &str,
        outcome_detail: &str,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sentinel_dispatches
             SET outcome = ?1, outcome_detail = ?2, completed_at = ?3
             WHERE id = ?4",
            params![outcome, outcome_detail, now, dispatch_id],
        )?;
        Ok(())
    }

    /// Get all dispatches with outcome = 'pending'.
    pub fn get_pending_dispatches(&self) -> Result<Vec<SentinelDispatch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, signal_ref, signal_title, source, disposition,
                    agent_id, crosslink_issue_id, gh_issue_number, label,
                    attempt_number, model_used, outcome, outcome_detail,
                    created_at, completed_at
             FROM sentinel_dispatches WHERE outcome = 'pending'",
        )?;
        let rows = stmt
            .query_map([], Self::map_dispatch_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Count dispatches with outcome = 'pending'.
    pub fn count_pending_dispatches(&self) -> Result<i64> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sentinel_dispatches WHERE outcome = 'pending'",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Get the most recent dispatch for a given `(gh_issue_number, label)` pair.
    /// Used for the authoritative dedup check (Layer 3).
    pub fn get_latest_dispatch_for_signal(
        &self,
        gh_issue_number: i64,
        label: &str,
    ) -> Result<Option<SentinelDispatch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, signal_ref, signal_title, source, disposition,
                    agent_id, crosslink_issue_id, gh_issue_number, label,
                    attempt_number, model_used, outcome, outcome_detail,
                    created_at, completed_at
             FROM sentinel_dispatches
             WHERE gh_issue_number = ?1 AND label = ?2
             ORDER BY created_at DESC LIMIT 1",
        )?;
        let mut rows = stmt
            .query_map(params![gh_issue_number, label], Self::map_dispatch_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.pop())
    }

    /// Load all dispatches for `SeenSet` construction (most recent per `signal_ref`).
    pub fn load_dispatch_seen_set(&self) -> Result<Vec<SentinelDispatch>> {
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.run_id, d.signal_ref, d.signal_title, d.source, d.disposition,
                    d.agent_id, d.crosslink_issue_id, d.gh_issue_number, d.label,
                    d.attempt_number, d.model_used, d.outcome, d.outcome_detail,
                    d.created_at, d.completed_at
             FROM sentinel_dispatches d
             INNER JOIN (
                 SELECT signal_ref, MAX(created_at) as max_created
                 FROM sentinel_dispatches
                 GROUP BY signal_ref
             ) latest ON d.signal_ref = latest.signal_ref AND d.created_at = latest.max_created",
        )?;
        let rows = stmt
            .query_map([], Self::map_dispatch_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// List all dispatches for a given sentinel run, ordered by creation time.
    pub fn list_dispatches_for_run(&self, run_id: &str) -> Result<Vec<SentinelDispatch>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, run_id, signal_ref, signal_title, source, disposition,
                    agent_id, crosslink_issue_id, gh_issue_number, label,
                    attempt_number, model_used, outcome, outcome_detail,
                    created_at, completed_at
             FROM sentinel_dispatches WHERE run_id = ?1
             ORDER BY created_at",
        )?;
        let rows = stmt
            .query_map(params![run_id], Self::map_dispatch_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get success rate metrics grouped by model and label.
    pub fn get_dispatch_metrics(&self) -> Result<Vec<DispatchMetric>> {
        let mut stmt = self.conn.prepare(
            "SELECT
                COALESCE(model_used, 'unknown') as model,
                label,
                COUNT(*) as total,
                SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) as successes,
                SUM(CASE WHEN outcome = 'failure' THEN 1 ELSE 0 END) as failures,
                SUM(CASE WHEN outcome = 'exhausted' THEN 1 ELSE 0 END) as exhausted,
                SUM(CASE WHEN outcome = 'pending' THEN 1 ELSE 0 END) as pending,
                SUM(CASE WHEN outcome = 'orphaned' THEN 1 ELSE 0 END) as orphaned
             FROM sentinel_dispatches
             WHERE disposition = 'dispatch'
             GROUP BY model_used, label
             ORDER BY label, model_used",
        )?;
        let rows = stmt
            .query_map([], |row| {
                let total: i64 = row.get(2)?;
                let successes: i64 = row.get(3)?;
                let completed = total - row.get::<_, i64>(6)?; // total - pending
                let success_rate = if completed > 0 {
                    (successes as f64 / completed as f64) * 100.0
                } else {
                    0.0
                };
                Ok(DispatchMetric {
                    model: row.get(0)?,
                    label: row.get(1)?,
                    total,
                    successes,
                    failures: row.get(4)?,
                    exhausted: row.get(5)?,
                    pending: row.get(6)?,
                    orphaned: row.get(7)?,
                    success_rate,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Shared row mapper for `sentinel_dispatches` queries.
    fn map_dispatch_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SentinelDispatch> {
        Ok(SentinelDispatch {
            id: row.get(0)?,
            run_id: row.get(1)?,
            signal_ref: row.get(2)?,
            signal_title: row.get(3)?,
            source: row.get(4)?,
            disposition: row.get(5)?,
            agent_id: row.get(6)?,
            crosslink_issue_id: row.get(7)?,
            gh_issue_number: row.get(8)?,
            label: row.get(9)?,
            attempt_number: row.get(10)?,
            model_used: row.get(11)?,
            outcome: row.get(12)?,
            outcome_detail: row.get(13)?,
            created_at: row.get(14)?,
            completed_at: row.get(15)?,
        })
    }
}
