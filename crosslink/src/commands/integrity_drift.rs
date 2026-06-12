//! Content-level drift detection between `SQLite` and `JSON`, plus re-emit
//! paths that close the gap by writing `SQLite`-only rows back to the
//! `JSON` event log via `SharedWriter` (#602).
//!
//! The existing count-based check in `integrity_cmd::check_hydration`
//! only catches divergence at the issue/milestone-count level. It misses
//! cases where the two sides have the same row counts but different
//! contents — most importantly: `SQLite` has a row (a label, a blocker,
//! a relation) that no `JSON` file represents. The repair path used to
//! silently delete those rows during the clear-then-rehydrate cycle.
//!
//! This module provides the structural primitives:
//!
//! - [`detect`] — diffs every shared table between `SQLite` and a fresh
//!   hydration from `JSON`, returning a [`HydrationDriftReport`].
//! - [`re_emit`] — for each re-emittable category, writes the
//!   `SQLite`-only rows back to the `JSON` / git event log via
//!   `SharedWriter`.
//!
//! Some categories (comments, time entries) have no `JSON` representation
//! and cannot be re-emitted; they are reported but require the snapshot
//! (`db::snapshot`) for recovery.

use anyhow::{Context, Result};
use std::path::Path;

use crate::db::Database;
use crate::hydration::hydrate_to_sqlite;

/// Categorized record of every `SQLite` row that is not represented in
/// the hydrated-from-`JSON` view of state.
///
/// The two checks that operate on this report are:
///
/// - [`HydrationDriftReport::is_empty`] — anything diverges?
/// - [`HydrationDriftReport::has_unrecoverable_loss`] — would a clear /
///   re-hydrate destroy state that re-emit cannot put back?
#[derive(Debug, Default, Clone)]
pub struct HydrationDriftReport {
    /// Issues (by display id) whose UUID is not present in any JSON file.
    /// The existing #427 self-heal logic in `hydrate_to_sqlite` already
    /// preserves these (along with their child rows) for `created_by IS
    /// NULL` issues; the field is populated for reporting only.
    pub sqlite_only_issues: Vec<i64>,

    /// `(issue_display_id, label)` pairs present in `SQLite` but not in
    /// `JSON`, restricted to issues that DO appear in `JSON`.
    /// Re-emittable via `SharedWriter::add_label`.
    pub sqlite_only_labels: Vec<(i64, String)>,

    /// `(blocker_display_id, blocked_display_id)` — blocker
    /// dependencies in `SQLite` but not in `JSON`, restricted to
    /// `JSON`-known issues on both sides. Re-emittable via
    /// `SharedWriter::add_blocker`.
    pub sqlite_only_dependencies: Vec<(i64, i64)>,

    /// `(issue_a_display_id, issue_b_display_id)` — relations in
    /// `SQLite` but not in `JSON`, canonicalized as `(min, max)`
    /// because `SQLite` stores both directions while `JSON` stores one.
    /// Re-emittable via `SharedWriter::add_relation`.
    pub sqlite_only_relations: Vec<(i64, i64)>,

    /// `(milestone_display_id, issue_display_id)` — milestone
    /// assignments in `SQLite` that don't appear as `milestone_uuid` on
    /// the `JSON` issue. Re-emittable via
    /// `SharedWriter::set_milestone_on_issues`.
    pub sqlite_only_milestone_issues: Vec<(i64, i64)>,

    /// `SQLite` comment ids whose UUIDs are not present in any `JSON`
    /// comment file or embedded `issue.comments` array. NOT re-emittable
    /// — re-emit would create a new comment with a fresh UUID and a
    /// new event, losing the original identity. Recovery relies on the
    /// snapshot file.
    pub sqlite_only_comments: Vec<i64>,

    /// Time-entry ids in `SQLite` (on `JSON`-known issues) that would
    /// be destroyed by `clear_shared_data`. Time entries have no `JSON`
    /// representation, so they cannot be re-emitted; recovery relies on
    /// the snapshot file.
    pub sqlite_only_time_entries: Vec<i64>,
}

impl HydrationDriftReport {
    /// True when `SQLite` and the `JSON`-derived view agree on every row
    /// of every shared table.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.sqlite_only_issues.is_empty()
            && self.sqlite_only_labels.is_empty()
            && self.sqlite_only_dependencies.is_empty()
            && self.sqlite_only_relations.is_empty()
            && self.sqlite_only_milestone_issues.is_empty()
            && self.sqlite_only_comments.is_empty()
            && self.sqlite_only_time_entries.is_empty()
    }

    /// True when running `clear_shared_data` would destroy `SQLite`-only
    /// state that [`re_emit`] cannot represent in `JSON`.
    ///
    /// Currently: comments and time entries on `JSON`-known issues.
    /// Issue rows with `created_by = NULL` are preserved by the existing
    /// `hydrate_to_sqlite` self-heal path; they are not counted as
    /// "unrecoverable" here.
    #[must_use]
    pub const fn has_unrecoverable_loss(&self) -> bool {
        !self.sqlite_only_comments.is_empty() || !self.sqlite_only_time_entries.is_empty()
    }

    /// True when every divergent row falls in a category that [`re_emit`]
    /// can write back to `JSON` (labels, deps, relations, milestone
    /// assignments). Used to decide whether `--repair` can proceed
    /// without `--accept-data-loss`.
    #[allow(dead_code)] // Exposed for callers reasoning about drift outside check_hydration.
    #[must_use]
    pub const fn is_fully_re_emittable(&self) -> bool {
        !self.is_empty()
            && self.sqlite_only_comments.is_empty()
            && self.sqlite_only_time_entries.is_empty()
            && self.sqlite_only_issues.is_empty()
    }

    /// Human-readable summary suitable for the integrity check status
    /// line. Empty drift produces an empty string.
    #[must_use]
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        let push = |parts: &mut Vec<String>, label: &str, n: usize| {
            if n > 0 {
                parts.push(format!("{n} sqlite-only {label}"));
            }
        };
        push(&mut parts, "issue(s)", self.sqlite_only_issues.len());
        push(&mut parts, "label(s)", self.sqlite_only_labels.len());
        push(
            &mut parts,
            "dependency(ies)",
            self.sqlite_only_dependencies.len(),
        );
        push(&mut parts, "relation(s)", self.sqlite_only_relations.len());
        push(
            &mut parts,
            "milestone assignment(s)",
            self.sqlite_only_milestone_issues.len(),
        );
        push(&mut parts, "comment(s)", self.sqlite_only_comments.len());
        push(
            &mut parts,
            "time entry(ies)",
            self.sqlite_only_time_entries.len(),
        );
        parts.join(", ")
    }
}

/// Diff every shared `SQLite` table against the `JSON`-derived view of
/// the same state. Returns a categorized record of every row that exists
/// in `SQLite` but not in `JSON`.
///
/// The `JSON`-derived view is built by hydrating into an isolated temp
/// `SQLite` file (reusing the production `hydrate_to_sqlite` path), then
/// `ATTACH`-ing that file to `main_db`'s connection so the diff is a
/// set of cross-database SQL queries — no manual `JSON` walking, no
/// duplicate parsing logic.
///
/// # Errors
///
/// Returns an error if the temp database cannot be created, hydration
/// from JSON fails, ATTACH fails, or any diff query fails.
pub fn detect(cache_dir: &Path, main_db: &Database) -> Result<HydrationDriftReport> {
    // 1. Build the JSON-derived view in an isolated temp database.
    let temp_dir = tempfile::tempdir().context("create temp dir for drift detection")?;
    let temp_db_path = temp_dir.path().join("hydrated-view.sqlite");
    {
        let temp_db =
            Database::open(&temp_db_path).context("open temp drift-detection database")?;
        hydrate_to_sqlite(cache_dir, &temp_db)
            .context("hydrate JSON into temp database for drift detection")?;
        // Explicit drop so the connection releases the file before ATTACH.
    }

    // 2. ATTACH the temp file to the main connection.
    let escaped = temp_db_path.to_string_lossy().replace('\'', "''");
    main_db
        .conn
        .execute(&format!("ATTACH DATABASE '{escaped}' AS json_view"), [])
        .context("attach JSON-view database")?;

    // Run the diff inside a closure so we can ALWAYS detach, even on
    // error from any individual query.
    let result = run_diff_queries(main_db);

    // 3. DETACH — best-effort. If detach fails the connection is still
    // usable for subsequent commands; tracing makes the failure visible.
    if let Err(e) = main_db.conn.execute("DETACH DATABASE json_view", []) {
        tracing::warn!("detach json_view database failed: {e}");
    }

    result
}

/// Execute the per-table diff queries. Separated so the caller can wrap
/// it in a DETACH-guard.
fn run_diff_queries(main_db: &Database) -> Result<HydrationDriftReport> {
    let mut report = HydrationDriftReport::default();

    // --- Issues (sqlite-only by uuid) ---
    {
        let mut stmt = main_db.conn.prepare(
            "SELECT id FROM main.issues \
             WHERE uuid IS NOT NULL \
               AND uuid NOT IN (SELECT uuid FROM json_view.issues WHERE uuid IS NOT NULL) \
             ORDER BY id",
        )?;
        report.sqlite_only_issues = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }

    // --- Labels (sqlite-only on JSON-known issues) ---
    {
        let mut stmt = main_db.conn.prepare(
            "SELECT issue_id, label FROM main.labels \
             WHERE issue_id IN (SELECT id FROM json_view.issues) \
               AND (issue_id, label) NOT IN \
                   (SELECT issue_id, label FROM json_view.labels) \
             ORDER BY issue_id, label",
        )?;
        report.sqlite_only_labels = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }

    // --- Dependencies (sqlite-only on JSON-known issues, both sides) ---
    {
        let mut stmt = main_db.conn.prepare(
            "SELECT blocker_id, blocked_id FROM main.dependencies \
             WHERE blocker_id IN (SELECT id FROM json_view.issues) \
               AND blocked_id IN (SELECT id FROM json_view.issues) \
               AND (blocker_id, blocked_id) NOT IN \
                   (SELECT blocker_id, blocked_id FROM json_view.dependencies) \
             ORDER BY blocker_id, blocked_id",
        )?;
        report.sqlite_only_dependencies = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }

    // --- Relations (canonical (min, max); both directions in SQLite) ---
    // The relations table stores both (a, b) and (b, a). Canonicalize
    // to (min, max) on both sides so the comparison sees one row per
    // logical relation. Then return canonicalized SQLite-only pairs.
    //
    // Note: SQLite column names are `issue_id_1` / `issue_id_2`.
    {
        let mut stmt = main_db.conn.prepare(
            "SELECT DISTINCT \
                 MIN(issue_id_1, issue_id_2) AS lo, \
                 MAX(issue_id_1, issue_id_2) AS hi \
             FROM main.relations \
             WHERE issue_id_1 IN (SELECT id FROM json_view.issues) \
               AND issue_id_2 IN (SELECT id FROM json_view.issues) \
               AND (MIN(issue_id_1, issue_id_2), MAX(issue_id_1, issue_id_2)) NOT IN \
                   (SELECT MIN(issue_id_1, issue_id_2), MAX(issue_id_1, issue_id_2) \
                    FROM json_view.relations) \
             ORDER BY lo, hi",
        )?;
        report.sqlite_only_relations = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }

    // --- Milestone assignments (sqlite-only on JSON-known issues) ---
    {
        let mut stmt = main_db.conn.prepare(
            "SELECT milestone_id, issue_id FROM main.milestone_issues \
             WHERE issue_id IN (SELECT id FROM json_view.issues) \
               AND (milestone_id, issue_id) NOT IN \
                   (SELECT milestone_id, issue_id FROM json_view.milestone_issues) \
             ORDER BY milestone_id, issue_id",
        )?;
        report.sqlite_only_milestone_issues = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }

    // --- Comments (sqlite-only by UUID, on JSON-known issues) ---
    // Comments without UUIDs (legacy or transient) are excluded — there
    // is no stable identity to diff on, and re-emit cannot bring them
    // back regardless.
    {
        let mut stmt = main_db.conn.prepare(
            "SELECT id FROM main.comments \
             WHERE issue_id IN (SELECT id FROM json_view.issues) \
               AND uuid IS NOT NULL \
               AND uuid NOT IN \
                   (SELECT uuid FROM json_view.comments WHERE uuid IS NOT NULL) \
             ORDER BY id",
        )?;
        report.sqlite_only_comments = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }

    // --- Time entries (sqlite-only on JSON-known issues) ---
    // Time entries have no JSON representation, so EVERY time entry on
    // a JSON-known issue would be destroyed by clear_shared_data.
    {
        let mut stmt = main_db.conn.prepare(
            "SELECT id FROM main.time_entries \
             WHERE issue_id IN (SELECT id FROM json_view.issues) \
             ORDER BY id",
        )?;
        report.sqlite_only_time_entries = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
    }

    Ok(report)
}

/// Summary of how many SQLite-only rows were successfully written back
/// to the JSON event log via `SharedWriter`. Returned by [`re_emit`].
#[derive(Debug, Default, Clone)]
pub struct ReEmitStats {
    pub labels: usize,
    pub dependencies: usize,
    pub relations: usize,
    pub milestone_issues: usize,
}

impl ReEmitStats {
    #[must_use]
    pub const fn total(&self) -> usize {
        self.labels + self.dependencies + self.relations + self.milestone_issues
    }
}

/// Write every re-emittable SQLite-only row in `drift` back to the JSON
/// event log via `writer`. Each `add_*` call short-circuits if the row
/// is already present (per #600), so this is safe to invoke even if the
/// JSON side raced ahead between detection and re-emit.
///
/// Categories without a JSON representation (`sqlite_only_comments`,
/// `sqlite_only_time_entries`, `sqlite_only_issues`) are NOT touched —
/// the caller is responsible for either accepting their loss or
/// recovering them from a snapshot.
///
/// # Errors
///
/// Returns an error from the first failing `SharedWriter` mutation.
/// Partial progress IS persisted: each mutation is its own git commit,
/// so any rows that succeeded before the failure remain in the JSON
/// event log and are no longer drift.
pub fn re_emit(
    drift: &HydrationDriftReport,
    writer: &crate::shared_writer::SharedWriter,
    db: &Database,
) -> Result<ReEmitStats> {
    let mut stats = ReEmitStats::default();

    for (issue_id, label) in &drift.sqlite_only_labels {
        if writer.add_label(db, *issue_id, label)? {
            stats.labels += 1;
        }
    }

    for (blocker_id, blocked_id) in &drift.sqlite_only_dependencies {
        if writer.add_blocker(db, *blocked_id, *blocker_id)? {
            stats.dependencies += 1;
        }
    }

    for (a, b) in &drift.sqlite_only_relations {
        if writer.add_relation(db, *a, *b)? {
            stats.relations += 1;
        }
    }

    // Group milestone_issues by milestone_id so set_milestone_on_issues
    // can write a single batch per milestone (matches its existing
    // call shape).
    if !drift.sqlite_only_milestone_issues.is_empty() {
        use std::collections::BTreeMap;
        let mut by_milestone: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
        for (m_id, i_id) in &drift.sqlite_only_milestone_issues {
            by_milestone.entry(*m_id).or_default().push(*i_id);
        }
        for (m_id, issue_ids) in by_milestone {
            writer.set_milestone_on_issues(db, m_id, &issue_ids)?;
            stats.milestone_issues += issue_ids.len();
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::HUB_CACHE_DIR;

    /// Minimal `cache_dir` setup: an empty `issues/` directory under
    /// `crosslink_dir/.hub-cache/`. Enough to satisfy
    /// `hydrate_to_sqlite`'s "no JSON files" early-return.
    fn setup_empty_cache(crosslink_dir: &std::path::Path) {
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
        std::fs::create_dir_all(cache_dir.join("issues")).unwrap();
        std::fs::create_dir_all(cache_dir.join("meta").join("milestones")).unwrap();
    }

    #[test]
    fn test_drift_report_summary_empty() {
        let report = HydrationDriftReport::default();
        assert!(report.is_empty());
        assert!(!report.has_unrecoverable_loss());
        assert!(!report.is_fully_re_emittable());
        assert_eq!(report.summary(), "");
    }

    #[test]
    fn test_drift_report_summary_with_labels() {
        let report = HydrationDriftReport {
            sqlite_only_labels: vec![(1, "bug".to_string())],
            ..Default::default()
        };
        assert!(!report.is_empty());
        assert!(!report.has_unrecoverable_loss());
        assert!(report.is_fully_re_emittable());
        assert_eq!(report.summary(), "1 sqlite-only label(s)");
    }

    #[test]
    fn test_drift_report_unrecoverable_when_comments_present() {
        let report = HydrationDriftReport {
            sqlite_only_comments: vec![42],
            ..Default::default()
        };
        assert!(!report.is_empty());
        assert!(report.has_unrecoverable_loss());
        assert!(!report.is_fully_re_emittable());
    }

    #[test]
    fn test_detect_no_drift_on_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path();
        setup_empty_cache(crosslink_dir);
        let db = Database::open(&dir.path().join("test.db")).unwrap();

        let report = detect(&crosslink_dir.join(HUB_CACHE_DIR), &db).unwrap();
        assert!(
            report.is_empty(),
            "empty SQLite + empty JSON should report no drift, got: {report:?}"
        );
    }

    fn make_issue(display_id: i64, title: &str) -> crate::issue_file::IssueFile {
        crate::issue_file::IssueFile {
            uuid: uuid::Uuid::new_v4(),
            display_id: Some(display_id),
            title: title.to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "test-agent".to_string(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            closed_at: None,
            scheduled_at: None,
            due_at: None,
            labels: Vec::new(),
            comments: Vec::new(),
            blockers: Vec::new(),
            related: Vec::new(),
            milestone_uuid: None,
            time_entries: Vec::new(),
        }
    }

    #[test]
    fn test_detect_sqlite_only_dependency_on_json_known_issues() {
        // The bug-report reproducer: two issues exist in JSON, but a
        // dependency row exists only in SQLite.
        use crate::issue_file::write_issue_file;

        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path();
        setup_empty_cache(crosslink_dir);
        let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);

        let issue_a = make_issue(1, "first");
        let issue_b = make_issue(2, "second");
        write_issue_file(
            &cache_dir
                .join("issues")
                .join(format!("{}.json", issue_a.uuid)),
            &issue_a,
        )
        .unwrap();
        write_issue_file(
            &cache_dir
                .join("issues")
                .join(format!("{}.json", issue_b.uuid)),
            &issue_b,
        )
        .unwrap();

        // Hydrate the JSON view into the real db, then add a SQLite-only
        // dependency row that was never written through SharedWriter.
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        hydrate_to_sqlite(&cache_dir, &db).unwrap();
        db.add_dependency(2, 1).unwrap();

        let report = detect(&cache_dir, &db).unwrap();

        assert_eq!(
            report.sqlite_only_dependencies,
            vec![(1, 2)],
            "the SQLite-only dependency must surface as drift"
        );
        assert!(
            report.is_fully_re_emittable(),
            "dependency-only drift must be re-emittable"
        );
        assert!(!report.has_unrecoverable_loss());
    }
}
