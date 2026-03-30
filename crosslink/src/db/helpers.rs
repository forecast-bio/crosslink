use chrono::{DateTime, Utc};

use crate::models::{Issue, Session};

/// Parse an RFC3339 datetime string, falling back to the current time on error.
pub fn parse_datetime(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).map_or_else(
        |e| {
            tracing::warn!(
                "failed to parse datetime '{}': {}, using current time",
                s,
                e
            );
            chrono::Utc::now()
        },
        |dt| dt.with_timezone(&Utc),
    )
}

/// Maps a database row to a Session struct.
/// Expects columns in order: id, `started_at`, `ended_at`, `active_issue_id`, `handoff_notes`, `last_action`, `agent_id`
pub fn session_from_row(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        started_at: parse_datetime(&row.get::<_, String>(1)?),
        ended_at: row.get::<_, Option<String>>(2)?.map(|s| parse_datetime(&s)),
        active_issue_id: row.get(3)?,
        handoff_notes: row.get(4)?,
        last_action: row.get(5)?,
        agent_id: row.get(6)?,
    })
}

/// Maps a database row to an Issue struct.
/// Expects columns in order: id, title, description, status, priority, `parent_id`, `created_at`, `updated_at`, `closed_at`
pub fn issue_from_row(row: &rusqlite::Row) -> rusqlite::Result<Issue> {
    Ok(Issue {
        id: row.get(0)?,
        title: row.get(1)?,
        description: row.get(2)?,
        status: row.get(3)?,
        priority: row.get(4)?,
        parent_id: row.get(5)?,
        created_at: parse_datetime(&row.get::<_, String>(6)?),
        updated_at: parse_datetime(&row.get::<_, String>(7)?),
        closed_at: row.get::<_, Option<String>>(8)?.map(|s| parse_datetime(&s)),
    })
}
