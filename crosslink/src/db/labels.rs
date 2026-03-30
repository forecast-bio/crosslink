use anyhow::Result;
use rusqlite::params;

use super::core::{Database, MAX_LABEL_LEN};

impl Database {
    /// Add a label to an issue.
    ///
    /// # Errors
    /// Returns an error if the label exceeds the maximum length or the database write fails.
    pub fn add_label(&self, issue_id: i64, label: &str) -> Result<bool> {
        let issue_id = self.resolve_id(issue_id);
        if label.len() > MAX_LABEL_LEN {
            anyhow::bail!("Label exceeds maximum length of {MAX_LABEL_LEN} characters");
        }
        let result = self.conn.execute(
            "INSERT OR IGNORE INTO labels (issue_id, label) VALUES (?1, ?2)",
            params![issue_id, label],
        )?;
        Ok(result > 0)
    }

    /// Remove a label from an issue.
    ///
    /// # Errors
    /// Returns an error if the database delete fails.
    pub fn remove_label(&self, issue_id: i64, label: &str) -> Result<bool> {
        let issue_id = self.resolve_id(issue_id);
        let rows = self.conn.execute(
            "DELETE FROM labels WHERE issue_id = ?1 AND label = ?2",
            params![issue_id, label],
        )?;
        Ok(rows > 0)
    }

    /// Get all labels for an issue.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_labels(&self, issue_id: i64) -> Result<Vec<String>> {
        let issue_id = self.resolve_id(issue_id);
        let mut stmt = self
            .conn
            .prepare("SELECT label FROM labels WHERE issue_id = ?1 ORDER BY label")?;
        let labels = stmt
            .query_map([issue_id], |row| row.get(0))?
            .collect::<std::result::Result<Vec<String>, _>>()?;
        Ok(labels)
    }

    /// Fetch labels for all given issue IDs in a single query.
    ///
    /// Returns a map from `issue_id` to its labels. Issues with no labels
    /// are included with an empty Vec.
    ///
    /// # Errors
    /// Returns an error if the database query fails.
    pub fn get_labels_batch(
        &self,
        issue_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Vec<String>>> {
        use std::collections::HashMap;

        let mut result: HashMap<i64, Vec<String>> =
            issue_ids.iter().map(|&id| (id, Vec::new())).collect();
        if issue_ids.is_empty() {
            return Ok(result);
        }

        let placeholders: String = issue_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT issue_id, label FROM labels WHERE issue_id IN ({placeholders}) ORDER BY issue_id, label"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(issue_ids.iter()), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (issue_id, label) = row?;
            result.entry(issue_id).or_default().push(label);
        }
        Ok(result)
    }
}
