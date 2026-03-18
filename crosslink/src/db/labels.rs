use anyhow::Result;
use rusqlite::params;

use super::core::{Database, MAX_LABEL_LEN};

impl Database {
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
}
