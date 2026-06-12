use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use super::core::Database;
use super::helpers::session_from_row;
use crate::models::Session;

impl Database {
    // Sessions

    /// Convenience wrapper for tests -- starts a session with no `agent_id`.
    #[cfg(test)]
    pub fn start_session(&self) -> Result<i64> {
        self.start_session_with_agent(None)
    }

    /// Start a new session, optionally scoped to an agent.
    ///
    /// # Errors
    ///
    /// Returns an error if the database insert fails.
    pub fn start_session_with_agent(&self, agent_id: Option<&str>) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (started_at, agent_id) VALUES (?1, ?2)",
            params![now, agent_id],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// End a session, recording optional handoff notes.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn end_session(&self, id: i64, notes: Option<&str>) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let rows = self.conn.execute(
            "UPDATE sessions SET ended_at = ?1, handoff_notes = ?2 WHERE id = ?3",
            params![now, notes, id],
        )?;
        Ok(rows > 0)
    }

    /// Convenience wrapper for tests -- gets current session without agent scoping.
    #[cfg(test)]
    pub fn get_current_session(&self) -> Result<Option<Session>> {
        self.get_current_session_for_agent(None)
    }

    /// Get the current active session scoped to the given `agent_id`.
    /// If `agent_id` is Some, only returns sessions belonging to that agent.
    /// If `agent_id` is None, returns any active session (backward compat).
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
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

    /// Convenience wrapper for tests -- gets last session without agent scoping.
    #[cfg(test)]
    pub fn get_last_session(&self) -> Result<Option<Session>> {
        self.get_last_session_for_agent(None)
    }

    /// Get the most recent ended session scoped to the given `agent_id`.
    /// If `agent_id` is Some, only returns sessions belonging to that agent.
    /// If `agent_id` is None, returns any ended session (backward compat).
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
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

    /// Set the active issue for a session.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn set_session_issue(&self, session_id: i64, issue_id: i64) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE sessions SET active_issue_id = ?1 WHERE id = ?2",
            params![issue_id, session_id],
        )?;
        Ok(rows > 0)
    }

    /// Clear the active issue for a session.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn clear_session_issue(&self, session_id: i64) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE sessions SET active_issue_id = NULL WHERE id = ?1",
            params![session_id],
        )?;
        Ok(rows > 0)
    }

    /// Record the last action breadcrumb for a session.
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    pub fn set_session_action(&self, session_id: i64, action: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE sessions SET last_action = ?1 WHERE id = ?2",
            params![action, session_id],
        )?;
        Ok(rows > 0)
    }

    /// Update handoff notes for a session.
    ///
    /// Retained as a tested DB primitive; its production caller (the offline
    /// reference-rewrite path) was removed with the v2 write machinery (#754).
    ///
    /// # Errors
    ///
    /// Returns an error if the database update fails.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn update_session_notes(&self, session_id: i64, notes: &str) -> Result<bool> {
        let rows = self.conn.execute(
            "UPDATE sessions SET handoff_notes = ?1 WHERE id = ?2",
            params![notes, session_id],
        )?;
        Ok(rows > 0)
    }

    /// Retrieve all sessions that have handoff notes.
    ///
    /// Retained as a tested DB primitive; its production caller (the offline
    /// reference-rewrite path) was removed with the v2 write machinery (#754).
    ///
    /// # Errors
    ///
    /// Returns an error if the database query fails.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get_all_sessions_with_notes(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, started_at, ended_at, active_issue_id, handoff_notes, last_action, agent_id FROM sessions WHERE handoff_notes IS NOT NULL ORDER BY id",
        )?;
        let sessions = stmt
            .query_map([], session_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }
}
