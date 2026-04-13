use anyhow::Result;
use chrono::Utc;

use super::{Signal, SignalKind, Source, SourceKind};

/// Configuration for the internal hygiene source.
pub struct InternalHygieneConfig {
    pub stale_threshold_days: i64,
}

impl Default for InternalHygieneConfig {
    fn default() -> Self {
        Self {
            stale_threshold_days: 30,
        }
    }
}

/// Checks crosslink issues for staleness, orphaned subissues, and missing labels.
pub struct InternalHygieneSource {
    config: InternalHygieneConfig,
    db_path: std::path::PathBuf,
}

impl InternalHygieneSource {
    pub fn new(crosslink_dir: &std::path::Path, config: InternalHygieneConfig) -> Self {
        Self {
            config,
            db_path: crosslink_dir.join("issues.db"),
        }
    }

    /// Find open issues with no activity for more than `stale_threshold_days`.
    fn find_stale_issues(&self) -> Result<Vec<Signal>> {
        let db = crate::db::Database::open(&self.db_path)?;
        let threshold = Utc::now() - chrono::Duration::days(self.config.stale_threshold_days);
        let threshold_str = threshold.to_rfc3339();

        let mut stmt = db.conn.prepare(
            "SELECT id, title, updated_at FROM issues
             WHERE status = 'open' AND updated_at < ?1
             ORDER BY updated_at ASC LIMIT 20",
        )?;

        let now = Utc::now();
        let signals = stmt
            .query_map([&threshold_str], |row| {
                let id: i64 = row.get(0)?;
                let title: String = row.get(1)?;
                let updated_at: String = row.get(2)?;
                Ok((id, title, updated_at))
            })?
            .filter_map(std::result::Result::ok)
            .map(|(id, title, updated_at)| Signal {
                source: SourceKind::Internal,
                kind: SignalKind::StaleIssue,
                reference: format!("CL#{id}:stale"),
                title: format!("Stale issue: {title}"),
                body: format!("Issue #{id} has not been updated since {updated_at}."),
                metadata: serde_json::json!({
                    "issue_id": id,
                    "last_updated": updated_at,
                    "stale_days": self.config.stale_threshold_days,
                }),
                detected_at: now,
            })
            .collect();

        Ok(signals)
    }

    /// Find subissues whose parent has been closed (orphaned).
    fn find_orphaned_subissues(&self) -> Result<Vec<Signal>> {
        let db = crate::db::Database::open(&self.db_path)?;

        let mut stmt = db.conn.prepare(
            "SELECT c.id, c.title, c.parent_id, p.status
             FROM issues c
             JOIN issues p ON c.parent_id = p.id
             WHERE c.status = 'open' AND p.status = 'closed'
             LIMIT 20",
        )?;

        let now = Utc::now();
        let signals = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let title: String = row.get(1)?;
                let parent_id: i64 = row.get(2)?;
                Ok((id, title, parent_id))
            })?
            .filter_map(std::result::Result::ok)
            .map(|(id, title, parent_id)| Signal {
                source: SourceKind::Internal,
                kind: SignalKind::StaleIssue,
                reference: format!("CL#{id}:orphan"),
                title: format!("Orphaned subissue: {title}"),
                body: format!("Issue #{id} is open but its parent #{parent_id} is closed."),
                metadata: serde_json::json!({
                    "issue_id": id,
                    "parent_id": parent_id,
                }),
                detected_at: now,
            })
            .collect();

        Ok(signals)
    }

    /// Find open issues with no labels.
    fn find_unlabeled_issues(&self) -> Result<Vec<Signal>> {
        let db = crate::db::Database::open(&self.db_path)?;

        let mut stmt = db.conn.prepare(
            "SELECT i.id, i.title FROM issues i
             WHERE i.status = 'open'
             AND NOT EXISTS (SELECT 1 FROM labels l WHERE l.issue_id = i.id)
             LIMIT 20",
        )?;

        let now = Utc::now();
        let signals = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                let title: String = row.get(1)?;
                Ok((id, title))
            })?
            .filter_map(std::result::Result::ok)
            .map(|(id, title)| Signal {
                source: SourceKind::Internal,
                kind: SignalKind::StaleIssue,
                reference: format!("CL#{id}:unlabeled"),
                title: format!("Unlabeled issue: {title}"),
                body: format!("Issue #{id} has no labels."),
                metadata: serde_json::json!({
                    "issue_id": id,
                }),
                detected_at: now,
            })
            .collect();

        Ok(signals)
    }
}

impl Source for InternalHygieneSource {
    fn name(&self) -> &'static str {
        "internal-hygiene"
    }

    fn poll(&mut self) -> Result<Vec<Signal>> {
        let mut signals = Vec::new();

        match self.find_stale_issues() {
            Ok(s) => signals.extend(s),
            Err(e) => tracing::warn!("stale issue scan failed: {e}"),
        }
        match self.find_orphaned_subissues() {
            Ok(s) => signals.extend(s),
            Err(e) => tracing::warn!("orphan scan failed: {e}"),
        }
        match self.find_unlabeled_issues() {
            Ok(s) => signals.extend(s),
            Err(e) => tracing::warn!("unlabeled scan failed: {e}"),
        }

        Ok(signals)
    }
}
