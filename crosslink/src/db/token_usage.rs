use std::fmt::Write;

use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use super::core::Database;
use super::helpers::parse_datetime;
use crate::models::TokenUsage;

/// Aggregated token usage grouped by agent and model.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageSummaryRow {
    pub agent_id: String,
    pub model: String,
    pub request_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cache_read_tokens: i64,
    pub total_cache_creation_tokens: i64,
    pub total_cost: f64,
}

impl Database {
    // === Token usage tracking ===

    /// Record a token usage entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    #[allow(clippy::too_many_arguments)]
    pub fn create_token_usage(
        &self,
        agent_id: &str,
        session_id: Option<i64>,
        input_tokens: i64,
        output_tokens: i64,
        cache_read_tokens: Option<i64>,
        cache_creation_tokens: Option<i64>,
        model: &str,
        cost_estimate: Option<f64>,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO token_usage (agent_id, session_id, timestamp, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, model, cost_estimate)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                agent_id,
                session_id,
                now,
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                model,
                cost_estimate,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get a single token usage record by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_token_usage(&self, id: i64) -> Result<Option<TokenUsage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, agent_id, session_id, timestamp, input_tokens, output_tokens,
                    cache_read_tokens, cache_creation_tokens, model, cost_estimate
             FROM token_usage WHERE id = ?1",
        )?;
        let mut rows = stmt
            .query_map(params![id], |row| {
                Ok(TokenUsage {
                    id: row.get(0)?,
                    agent_id: row.get(1)?,
                    session_id: row.get(2)?,
                    timestamp: parse_datetime(&row.get::<_, String>(3)?),
                    input_tokens: row.get(4)?,
                    output_tokens: row.get(5)?,
                    cache_read_tokens: row.get(6)?,
                    cache_creation_tokens: row.get(7)?,
                    model: row.get(8)?,
                    cost_estimate: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.pop())
    }

    /// List token usage records with optional filters.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn list_token_usage(
        &self,
        agent_id: Option<&str>,
        session_id: Option<i64>,
        model: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
        limit: Option<i64>,
    ) -> Result<Vec<TokenUsage>> {
        let mut sql = String::from(
            "SELECT id, agent_id, session_id, timestamp, input_tokens, output_tokens,
                    cache_read_tokens, cache_creation_tokens, model, cost_estimate
             FROM token_usage WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(aid) = agent_id {
            param_values.push(Box::new(aid.to_string()));
            let _ = write!(sql, " AND agent_id = ?{}", param_values.len());
        }
        if let Some(sid) = session_id {
            param_values.push(Box::new(sid));
            let _ = write!(sql, " AND session_id = ?{}", param_values.len());
        }
        if let Some(m) = model {
            param_values.push(Box::new(m.to_string()));
            let _ = write!(sql, " AND model = ?{}", param_values.len());
        }
        if let Some(f) = from {
            param_values.push(Box::new(f.to_string()));
            let _ = write!(sql, " AND timestamp >= ?{}", param_values.len());
        }
        if let Some(t) = to {
            param_values.push(Box::new(t.to_string()));
            let _ = write!(sql, " AND timestamp <= ?{}", param_values.len());
        }

        sql.push_str(" ORDER BY timestamp DESC");

        if let Some(lim) = limit {
            param_values.push(Box::new(lim));
            let _ = write!(sql, " LIMIT ?{}", param_values.len());
        }

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(TokenUsage {
                    id: row.get(0)?,
                    agent_id: row.get(1)?,
                    session_id: row.get(2)?,
                    timestamp: parse_datetime(&row.get::<_, String>(3)?),
                    input_tokens: row.get(4)?,
                    output_tokens: row.get(5)?,
                    cache_read_tokens: row.get(6)?,
                    cache_creation_tokens: row.get(7)?,
                    model: row.get(8)?,
                    cost_estimate: row.get(9)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get aggregated usage summary, optionally filtered by agent and time range.
    /// Groups by `agent_id` and model.
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    pub fn get_usage_summary(
        &self,
        agent_id: Option<&str>,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<UsageSummaryRow>> {
        let mut sql = String::from(
            "SELECT agent_id, model,
                    COUNT(*) as request_count,
                    SUM(input_tokens) as total_input_tokens,
                    SUM(output_tokens) as total_output_tokens,
                    COALESCE(SUM(cache_read_tokens), 0) as total_cache_read_tokens,
                    COALESCE(SUM(cache_creation_tokens), 0) as total_cache_creation_tokens,
                    COALESCE(SUM(cost_estimate), 0.0) as total_cost
             FROM token_usage WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(aid) = agent_id {
            param_values.push(Box::new(aid.to_string()));
            let _ = write!(sql, " AND agent_id = ?{}", param_values.len());
        }
        if let Some(f) = from {
            param_values.push(Box::new(f.to_string()));
            let _ = write!(sql, " AND timestamp >= ?{}", param_values.len());
        }
        if let Some(t) = to {
            param_values.push(Box::new(t.to_string()));
            let _ = write!(sql, " AND timestamp <= ?{}", param_values.len());
        }

        sql.push_str(" GROUP BY agent_id, model ORDER BY total_cost DESC");

        let param_refs: Vec<&dyn rusqlite::types::ToSql> = param_values
            .iter()
            .map(std::convert::AsRef::as_ref)
            .collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(UsageSummaryRow {
                    agent_id: row.get(0)?,
                    model: row.get(1)?,
                    request_count: row.get(2)?,
                    total_input_tokens: row.get(3)?,
                    total_output_tokens: row.get(4)?,
                    total_cache_read_tokens: row.get(5)?,
                    total_cache_creation_tokens: row.get(6)?,
                    total_cost: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
