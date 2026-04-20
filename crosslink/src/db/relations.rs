use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use super::core::Database;
use super::helpers::issue_from_row;
use crate::models::Issue;

impl Database {
    /// Add a dependency between two issues (blocker blocks target).
    ///
    /// # Errors
    /// Returns an error if an issue would block itself, a circular dependency
    /// would be created, or the database operation fails.
    pub fn add_dependency(&self, target_id: i64, blocker_id: i64) -> Result<bool> {
        let target_id = self.resolve_id(target_id);
        let blocker_id = self.resolve_id(blocker_id);
        // Prevent self-blocking
        if target_id == blocker_id {
            anyhow::bail!("An issue cannot block itself");
        }

        // Check for circular dependencies before inserting
        if self.would_create_cycle(target_id, blocker_id)? {
            anyhow::bail!("Adding this dependency would create a circular dependency chain");
        }

        let result = self.conn.execute(
            "INSERT OR IGNORE INTO dependencies (blocker_id, blocked_id) VALUES (?1, ?2)",
            params![blocker_id, target_id],
        )?;
        Ok(result > 0)
    }

    /// Check if adding `blocker_id` -> `target_id` would create a cycle.
    /// A cycle exists if `target_id` can already reach `blocker_id` through existing dependencies.
    fn would_create_cycle(&self, target_id: i64, blocker_id: i64) -> Result<bool> {
        // If target_id can reach blocker_id, then adding blocker_id -> target_id creates a cycle
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![target_id];

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

    /// Remove a dependency between two issues.
    ///
    /// # Errors
    /// Returns an error if the database operation fails.
    pub fn remove_dependency(&self, target_id: i64, blocker_id: i64) -> Result<bool> {
        let resolved_target = self.resolve_id(target_id);
        let resolved_blocker = self.resolve_id(blocker_id);
        let rows = self.conn.execute(
            "DELETE FROM dependencies WHERE blocker_id = ?1 AND blocked_id = ?2",
            params![resolved_blocker, resolved_target],
        )?;
        Ok(rows > 0)
    }

    /// Fetch blocker counts for all given issue IDs in a single query.
    ///
    /// Returns a map from `issue_id` to the number of blockers.
    /// Issues with no blockers are included with count 0.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_blocker_counts_batch(
        &self,
        issue_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, usize>> {
        use std::collections::HashMap;

        let mut result: HashMap<i64, usize> = issue_ids.iter().map(|&id| (id, 0)).collect();
        if issue_ids.is_empty() {
            return Ok(result);
        }

        let placeholders: String = issue_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT blocked_id, COUNT(*) FROM dependencies WHERE blocked_id IN ({placeholders}) GROUP BY blocked_id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(issue_ids.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (issue_id, count) = row?;
            result.insert(issue_id, usize::try_from(count).unwrap_or(0));
        }
        Ok(result)
    }

    /// Get the list of blocker issue IDs for the given issue.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_blockers(&self, issue_id: i64) -> Result<Vec<i64>> {
        let issue_id = self.resolve_id(issue_id);
        let mut stmt = self
            .conn
            .prepare("SELECT blocker_id FROM dependencies WHERE blocked_id = ?1")?;
        let blockers = stmt
            .query_map([issue_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?;
        Ok(blockers)
    }

    /// Get the list of issue IDs that the given issue is blocking.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_blocking(&self, issue_id: i64) -> Result<Vec<i64>> {
        let issue_id = self.resolve_id(issue_id);
        let mut stmt = self
            .conn
            .prepare("SELECT blocked_id FROM dependencies WHERE blocker_id = ?1")?;
        let blocking = stmt
            .query_map([issue_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?;
        Ok(blocking)
    }

    /// List all open issues that have at least one open blocker.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn list_blocked_issues(&self) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            r"
            SELECT DISTINCT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at, i.scheduled_at, i.due_at
            FROM issues i
            JOIN dependencies d ON i.id = d.blocked_id
            JOIN issues blocker ON d.blocker_id = blocker.id
            WHERE i.status = 'open' AND blocker.status = 'open'
            ORDER BY i.id
            ",
        )?;

        let issues = stmt
            .query_map([], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    /// List all open issues that have no open blockers.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn list_ready_issues(&self) -> Result<Vec<Issue>> {
        let mut stmt = self.conn.prepare(
            r"
            SELECT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at, i.scheduled_at, i.due_at
            FROM issues i
            WHERE i.status = 'open'
            AND NOT EXISTS (
                SELECT 1 FROM dependencies d
                JOIN issues blocker ON d.blocker_id = blocker.id
                WHERE d.blocked_id = i.id AND blocker.status = 'open'
            )
            ORDER BY i.id
            ",
        )?;

        let issues = stmt
            .query_map([], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    /// Add a bidirectional relation between two issues.
    ///
    /// # Errors
    /// Returns an error if an issue is related to itself or the database
    /// operation fails.
    pub fn add_relation(&self, issue_id_1: i64, issue_id_2: i64) -> Result<bool> {
        let issue_id_1 = self.resolve_id(issue_id_1);
        let issue_id_2 = self.resolve_id(issue_id_2);
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

    /// Remove a bidirectional relation between two issues.
    ///
    /// # Errors
    /// Returns an error if the database operation fails.
    pub fn remove_relation(&self, issue_id_1: i64, issue_id_2: i64) -> Result<bool> {
        let issue_id_1 = self.resolve_id(issue_id_1);
        let issue_id_2 = self.resolve_id(issue_id_2);
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

    /// Get all issues related to the given issue (both directions).
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_related_issues(&self, issue_id: i64) -> Result<Vec<Issue>> {
        let issue_id = self.resolve_id(issue_id);
        let mut stmt = self.conn.prepare(
            r"
            SELECT i.id, i.title, i.description, i.status, i.priority, i.parent_id, i.created_at, i.updated_at, i.closed_at, i.scheduled_at, i.due_at
            FROM issues i
            WHERE i.id IN (
                SELECT issue_id_2 FROM relations WHERE issue_id_1 = ?1
                UNION
                SELECT issue_id_1 FROM relations WHERE issue_id_2 = ?1
            )
            ORDER BY i.id
            ",
        )?;

        let issues = stmt
            .query_map([issue_id], issue_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(issues)
    }

    /// Get related issue IDs (both directions of the relation).
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_related_issue_ids(&self, issue_id: i64) -> Result<Vec<i64>> {
        let issue_id = self.resolve_id(issue_id);
        let mut stmt = self.conn.prepare(
            "SELECT issue_id_2 FROM relations WHERE issue_id_1 = ?1 UNION SELECT issue_id_1 FROM relations WHERE issue_id_2 = ?1",
        )?;
        let ids = stmt
            .query_map([issue_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<i64>, _>>()?;
        Ok(ids)
    }
}
