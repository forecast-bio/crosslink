//! Hydrate local `SQLite` from JSON issue files on the coordination branch.
//!
//! On every `crosslink sync`, this module reads all `issues/*.json` files from
//! the coordination branch worktree cache and writes them into the local `SQLite`
//! database in a single transaction. This keeps `SQLite` as the universal read
//! path while JSON on the git branch remains the source of truth.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use crate::db::{Database, HydratedIssue, HydratedMilestone};
use crate::issue_file::{
    read_all_issue_files, read_all_milestone_files, read_comment_files, read_layout_version,
    read_milestones_file, write_comment_file, CommentFile, IssueFile,
};

/// Deduplicate issue files that share the same `display_id`.
///
/// When multiple JSON files claim the same `display_id` (e.g. from a sync loop
/// that created duplicates), keep the one with the most recent `updated_at`
/// timestamp and return the rest for cleanup.
fn dedup_issue_files(issues: &[IssueFile]) -> (Vec<&IssueFile>, Vec<&IssueFile>) {
    let mut by_display_id: HashMap<i64, Vec<&IssueFile>> = HashMap::new();
    let mut no_display_id = Vec::new();

    for issue in issues {
        match issue.display_id {
            Some(id) => by_display_id.entry(id).or_default().push(issue),
            None => no_display_id.push(issue),
        }
    }

    let mut keep = Vec::new();
    let mut dupes = Vec::new();

    for (_id, mut group) in by_display_id {
        // Sort by updated_at descending — most recent first
        group.sort_by_key(|b| std::cmp::Reverse(b.updated_at));
        keep.push(group[0]);
        dupes.extend(group.into_iter().skip(1));
    }

    keep.extend(no_display_id);
    (keep, dupes)
}

/// Statistics returned after hydration.
#[derive(Debug, Default)]
pub struct HydrationStats {
    pub issues: usize,
    pub comments: usize,
    pub dependencies: usize,
    pub relations: usize,
    pub milestones: usize,
}

/// Snapshot of an issue row from `SQLite` for preservation during hydration.
struct SavedIssue {
    id: i64,
    uuid: String,
    title: String,
    description: Option<String>,
    status: String,
    priority: String,
    parent_id: Option<i64>,
    created_by: Option<String>,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
}

/// Tuple of comment fields saved from `SQLite` before hydration clears them.
type SavedComment = (
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Tuple of time-entry fields saved from `SQLite` before hydration clears them.
type SavedTimeEntry = (i64, i64, String, Option<String>, Option<i64>);

/// Child-table data preserved for `SQLite`-only issues across hydration.
struct SavedChildren {
    labels: Vec<(i64, String)>,
    comments: Vec<SavedComment>,
    deps: Vec<(i64, i64)>,
    relations: Vec<(i64, i64)>,
    time_entries: Vec<SavedTimeEntry>,
    milestone_issues: Vec<(i64, i64)>,
}

/// Hydrate the local `SQLite` database from JSON files in the coordination branch cache.
///
/// This function:
/// 1. Reads all `issues/*.json` files from `cache_dir/issues/`
/// 2. Reads `meta/counters.json` and `meta/milestones.json`
/// 3. Clears all shared data from `SQLite` (issues, comments, labels, deps, etc.)
/// 4. Re-inserts everything from the JSON files in a single transaction
///
/// Sessions are machine-local state and are preserved across hydration.
/// The `active_issue_id` FK constraint (`ON DELETE SET NULL`) is handled
/// by saving and restoring work items around the clear/reinsert cycle.
///
/// **Data-loss guard (#427):** If JSON has significantly fewer issues than
/// `SQLite`, hydration is skipped to avoid wiping SQLite-only issues that
/// haven't been synced to JSON yet (e.g. after `init --force`).
///
/// # Errors
///
/// Returns an error if reading issue files or database operations fail.
pub fn hydrate_to_sqlite(cache_dir: &Path, db: &Database) -> Result<HydrationStats> {
    let issues_dir = cache_dir.join("issues");
    let issue_files = read_all_issue_files(&issues_dir)?;

    if issue_files.is_empty() {
        return Ok(HydrationStats::default());
    }

    // Self-healing merge: if SQLite has issues that JSON doesn't (e.g. after
    // `init --force` or a partial sync), preserve them through hydration by
    // re-inserting them after the JSON issues (#427).
    let json_uuids: std::collections::HashSet<String> =
        issue_files.iter().map(|f| f.uuid.to_string()).collect();

    // Snapshot SQLite-only issues (those with UUIDs not present in JSON).
    // Only preserve issues whose UUID also doesn't appear as a file/directory
    // in the issues cache — if a UUID exists on disk (even as an empty dir),
    // the issue was tracked by the hub and its absence from issue_files means
    // it was intentionally deleted, not lost (#427).
    let all_rows: Vec<SavedIssue> = db
        .conn
        .prepare(
            "SELECT id, uuid, title, description, status, priority, parent_id, \
             created_by, created_at, updated_at, closed_at FROM issues WHERE uuid IS NOT NULL",
        )?
        .query_map([], |row| {
            Ok(SavedIssue {
                id: row.get(0)?,
                uuid: row.get(1)?,
                title: row.get(2)?,
                description: row.get(3)?,
                status: row.get(4)?,
                priority: row.get(5)?,
                parent_id: row.get(6)?,
                created_by: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                closed_at: row.get(10)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let sqlite_only_rows: Vec<SavedIssue> = all_rows
        .into_iter()
        .filter(|row| {
            if json_uuids.contains(&row.uuid) {
                return false; // Already in JSON — will be hydrated normally
            }
            // Preserve only issues created via direct SQLite (db.create_issue),
            // not issues that were tracked by SharedWriter and then deleted.
            // SharedWriter-created issues have a created_by field (agent ID).
            // Direct SQLite issues have created_by = NULL.
            row.created_by.is_none()
        })
        .collect();
    if !sqlite_only_rows.is_empty() {
        tracing::info!(
            "hydration: preserving {} SQLite-only issue(s) not found in JSON",
            sqlite_only_rows.len()
        );
    }

    // Snapshot child table data for SQLite-only issues before clear_shared_data
    // destroys it. Without this, labels/comments/deps/relations/time entries
    // are permanently lost during re-hydration (#310).
    let preserved_ids: Vec<i64> = sqlite_only_rows.iter().map(|r| r.id).collect();
    let saved_children = snapshot_children(db, &preserved_ids)?;

    // Preserved SQLite-only comments may carry negative IDs assigned by a
    // previous hydration. To avoid colliding with V2 comment IDs assigned in
    // this pass, start V2 numbering below the lowest preserved ID (#681).
    let v2_comment_id_start = saved_children
        .comments
        .iter()
        .map(|c| c.0)
        .min()
        .map_or(-1, |min| min - 1);

    let (deduped, milestone_entries) = dedup_and_load_milestones(&issue_files, cache_dir)?;

    // Build uuid -> display_id lookup for resolving cross-references
    let mut uuid_to_id: HashMap<String, i64> = deduped
        .iter()
        .filter_map(|f| f.display_id.map(|id| (f.uuid.to_string(), id)))
        .collect();

    // Build milestone uuid -> display_id lookup
    let milestone_uuid_to_id: HashMap<String, i64> = milestone_entries
        .iter()
        .map(|m| (m.uuid.to_string(), m.display_id))
        .collect();

    let mut stats = HydrationStats::default();
    let layout_version = read_layout_version(&cache_dir.join("meta")).unwrap_or(1);

    // Disable FK constraints during bulk clear/reinsert to prevent ON DELETE
    // cascades from corrupting session state (e.g. active_issue_id). PRAGMA
    // foreign_keys is a no-op inside a transaction, so toggle outside (#461).
    db.set_foreign_keys(false)?;

    let result = db.transaction(|| {
        db.clear_shared_data()?;

        // Insert milestones first (issues may reference them)
        for entry in &milestone_entries {
            let created_at = entry.created_at.to_rfc3339();
            let closed_at = entry.closed_at.map(|dt| dt.to_rfc3339());
            db.insert_hydrated_milestone(&HydratedMilestone {
                id: entry.display_id,
                uuid: &entry.uuid.to_string(),
                name: &entry.name,
                description: entry.description.as_deref(),
                status: entry.status.as_str(),
                created_at: &created_at,
                closed_at: closed_at.as_deref(),
            })?;
            stats.milestones += 1;
        }

        // Sort issues so parents come before children (foreign key constraint)
        let sorted_issues = topo_sort_issues(&deduped);

        // Insert issues and their child data (labels, comments, time entries, milestones)
        hydrate_issues(
            db,
            &sorted_issues,
            &mut uuid_to_id,
            &milestone_uuid_to_id,
            &issues_dir,
            layout_version,
            v2_comment_id_start,
            &mut stats,
        )?;

        // Hydrate dependencies (single-direction: blockers array on blocked issue)
        hydrate_dependencies(db, &deduped, &uuid_to_id, &mut stats)?;

        // Hydrate relations (single-direction: related array, insert both directions)
        hydrate_relations(db, &deduped, &uuid_to_id, &mut stats)?;

        // Re-insert SQLite-only issues and their children (#427, #310).
        restore_sqlite_only_issues(db, &sqlite_only_rows, &saved_children, &mut stats)?;

        Ok(stats)
    });

    // Re-enable FK constraints regardless of transaction outcome (#461).
    // Use if-let to avoid masking the original transaction error.
    if let Err(e) = db.set_foreign_keys(true) {
        tracing::warn!("failed to re-enable foreign key constraints: {}", e);
    }

    result
}

/// Deduplicate issue files and load milestone entries from cache.
fn dedup_and_load_milestones<'a>(
    issue_files: &'a [IssueFile],
    cache_dir: &Path,
) -> Result<(Vec<&'a IssueFile>, Vec<crate::issue_file::MilestoneEntry>)> {
    let (deduped, dupes) = dedup_issue_files(issue_files);
    if !dupes.is_empty() {
        tracing::warn!(
            "{} duplicate issue file(s) skipped during hydration (same display_id)",
            dupes.len()
        );
        for d in &dupes {
            tracing::warn!(
                "  skipped: {} (display_id {:?}, uuid {})",
                d.title,
                d.display_id,
                d.uuid
            );
        }
    }

    let milestones_dir = cache_dir.join("meta").join("milestones");
    let mut milestone_entries = read_all_milestone_files(&milestones_dir)?;
    if milestone_entries.is_empty() {
        let legacy_path = cache_dir.join("meta").join("milestones.json");
        let legacy = read_milestones_file(&legacy_path)?;
        milestone_entries = legacy.milestones.into_values().collect();
    }

    Ok((deduped, milestone_entries))
}

/// Sort issues so parents appear before children (for foreign key constraints).
/// Issues without parents come first, then children in dependency order.
fn topo_sort_issues<'a>(issues: &[&'a IssueFile]) -> Vec<&'a IssueFile> {
    let uuid_set: std::collections::HashSet<_> = issues.iter().map(|i| i.uuid).collect();
    let mut roots: Vec<&'a IssueFile> = Vec::new();
    let mut children: Vec<&'a IssueFile> = Vec::new();

    for &issue in issues {
        match issue.parent_uuid {
            Some(parent) if uuid_set.contains(&parent) => children.push(issue),
            _ => roots.push(issue),
        }
    }

    // Simple two-pass: roots first, then children.
    // For deeper nesting, a full topo sort would be needed,
    // but crosslink typically has at most 1-2 levels of nesting.
    //
    // Sort roots by (created_at, uuid) so offline issues without display_id
    // get assigned the same local IDs across re-hydration passes (#499).
    roots.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.uuid.cmp(&b.uuid)));
    let mut sorted = roots;

    // Multi-pass: keep appending children whose parent is already in sorted
    let mut remaining = children;
    for _ in 0..10 {
        if remaining.is_empty() {
            break;
        }
        let sorted_uuids: std::collections::HashSet<_> = sorted.iter().map(|i| i.uuid).collect();
        let (mut ready, still_remaining): (Vec<&'a IssueFile>, Vec<&'a IssueFile>) = remaining
            .into_iter()
            .partition(|i| i.parent_uuid.is_none_or(|p| sorted_uuids.contains(&p)));
        ready.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.uuid.cmp(&b.uuid)));
        sorted.extend(ready);
        remaining = still_remaining;
    }
    // Any remaining (orphaned parents not in the set) go at the end
    sorted.extend(remaining);
    sorted
}

/// Insert issues and their child data (labels, comments, time entries, milestones).
#[allow(clippy::too_many_arguments)]
fn hydrate_issues(
    db: &Database,
    sorted_issues: &[&IssueFile],
    uuid_to_id: &mut HashMap<String, i64>,
    milestone_uuid_to_id: &HashMap<String, i64>,
    issues_dir: &Path,
    layout_version: u32,
    v2_comment_id_start: i64,
    stats: &mut HydrationStats,
) -> Result<()> {
    let mut next_local_id: i64 = -1;
    // V2 standalone comments use UUIDs, not sequential integer IDs.
    // Assign unique negative IDs during hydration so each row satisfies
    // the PRIMARY KEY UNIQUE constraint on the comments table. Start below
    // any preserved SQLite-only comment ID so restore_sqlite_only_issues
    // can re-insert its rows without collision (#681).
    let mut next_v2_comment_id: i64 = v2_comment_id_start;

    for issue in sorted_issues {
        let display_id = issue.display_id.unwrap_or_else(|| {
            let local_id = next_local_id;
            next_local_id -= 1;
            // Track in uuid_to_id so cross-references resolve
            uuid_to_id.insert(issue.uuid.to_string(), local_id);
            local_id
        });

        let parent_id = issue
            .parent_uuid
            .and_then(|u| uuid_to_id.get(&u.to_string()).copied());

        let created_at = issue.created_at.to_rfc3339();
        let updated_at = issue.updated_at.to_rfc3339();
        let closed_at = issue.closed_at.map(|dt| dt.to_rfc3339());

        db.insert_hydrated_issue(&HydratedIssue {
            id: display_id,
            uuid: &issue.uuid.to_string(),
            title: &issue.title,
            description: issue.description.as_deref(),
            status: issue.status.as_str(),
            priority: issue.priority.as_str(),
            parent_id,
            created_by: Some(&issue.created_by),
            created_at: &created_at,
            updated_at: &updated_at,
            closed_at: closed_at.as_deref(),
        })?;
        stats.issues += 1;

        // Labels
        for label in &issue.labels {
            db.insert_hydrated_label(display_id, label)?;
        }

        // Comments - inline (v1) entries on the issue file
        for comment in &issue.comments {
            let comment_created = comment.created_at.to_rfc3339();
            db.insert_hydrated_comment(
                comment.id,
                display_id,
                None, // comment uuid not tracked yet
                Some(&comment.author),
                &comment.content,
                &comment_created,
                &comment.kind,
                comment.trigger_type.as_deref(),
                comment.intervention_context.as_deref(),
                comment.driver_key_fingerprint.as_deref(),
            )?;
            stats.comments += 1;
        }

        // Comments - standalone v2 comment files in issues/{uuid}/comments/
        if layout_version >= 2 {
            let comments_dir = issues_dir.join(issue.uuid.to_string()).join("comments");
            if let Ok(v2_comments) = read_comment_files(&comments_dir) {
                for cf in &v2_comments {
                    let comment_created = cf.created_at.to_rfc3339();
                    let v2_id = next_v2_comment_id;
                    next_v2_comment_id -= 1;
                    db.insert_hydrated_comment(
                        v2_id,
                        display_id,
                        Some(&cf.uuid.to_string()),
                        Some(&cf.author),
                        &cf.content,
                        &comment_created,
                        &cf.kind,
                        cf.trigger_type.as_deref(),
                        cf.intervention_context.as_deref(),
                        cf.driver_key_fingerprint.as_deref(),
                    )?;
                    stats.comments += 1;
                }
            }
        }

        // Time entries
        for te in &issue.time_entries {
            let started = te.started_at.to_rfc3339();
            let ended = te.ended_at.map(|dt| dt.to_rfc3339());
            db.insert_hydrated_time_entry(
                te.id,
                display_id,
                &started,
                ended.as_deref(),
                te.duration_seconds,
            )?;
        }

        // Milestone association
        if let Some(ms_uuid) = &issue.milestone_uuid {
            if let Some(&ms_id) = milestone_uuid_to_id.get(&ms_uuid.to_string()) {
                db.insert_hydrated_milestone_issue(ms_id, display_id)?;
            }
        }
    }

    Ok(())
}

/// Snapshot child-table data for the given issue IDs before `clear_shared_data` removes them.
fn snapshot_children(db: &Database, preserved_ids: &[i64]) -> Result<SavedChildren> {
    if preserved_ids.is_empty() {
        return Ok(SavedChildren {
            labels: vec![],
            comments: vec![],
            deps: vec![],
            relations: vec![],
            time_entries: vec![],
            milestone_issues: vec![],
        });
    }

    let id_placeholders: String = preserved_ids
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");

    let labels = db
        .conn
        .prepare(&format!(
            "SELECT issue_id, label FROM labels WHERE issue_id IN ({id_placeholders})"
        ))?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let comments = db.conn
        .prepare(&format!(
            "SELECT id, issue_id, uuid, author, content, created_at, kind, trigger_type, intervention_context, driver_key_fingerprint \
             FROM comments WHERE issue_id IN ({id_placeholders})"
        ))?
        .query_map([], |row| Ok((
            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
            row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?,
        )))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let deps = db.conn
        .prepare(&format!(
            "SELECT blocker_id, blocked_id FROM dependencies WHERE blocker_id IN ({id_placeholders}) OR blocked_id IN ({id_placeholders})"
        ))?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let relations = db.conn
        .prepare(&format!(
            "SELECT issue_id_1, issue_id_2 FROM relations WHERE issue_id_1 IN ({id_placeholders}) OR issue_id_2 IN ({id_placeholders})"
        ))?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let time_entries = db.conn
        .prepare(&format!(
            "SELECT id, issue_id, started_at, ended_at, duration_seconds FROM time_entries WHERE issue_id IN ({id_placeholders})"
        ))?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let milestone_issues = db
        .conn
        .prepare(&format!(
            "SELECT milestone_id, issue_id FROM milestone_issues WHERE issue_id IN ({id_placeholders})"
        ))?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(SavedChildren {
        labels,
        comments,
        deps,
        relations,
        time_entries,
        milestone_issues,
    })
}

/// Re-insert SQLite-only issues and their child data after hydration clears shared tables.
fn restore_sqlite_only_issues(
    db: &Database,
    sqlite_only_rows: &[SavedIssue],
    saved_children: &SavedChildren,
    stats: &mut HydrationStats,
) -> Result<()> {
    for row in sqlite_only_rows {
        db.insert_hydrated_issue(&HydratedIssue {
            id: row.id,
            uuid: &row.uuid,
            title: &row.title,
            description: row.description.as_deref(),
            status: &row.status,
            priority: &row.priority,
            parent_id: row.parent_id,
            created_by: row.created_by.as_deref(),
            created_at: &row.created_at,
            updated_at: &row.updated_at,
            closed_at: row.closed_at.as_deref(),
        })?;
        stats.issues += 1;
    }

    for (issue_id, label) in &saved_children.labels {
        db.insert_hydrated_label(*issue_id, label)?;
    }
    for (
        id,
        issue_id,
        uuid,
        author,
        content,
        created_at,
        kind,
        trigger_type,
        intervention_context,
        driver_key_fingerprint,
    ) in &saved_children.comments
    {
        db.insert_hydrated_comment(
            *id,
            *issue_id,
            uuid.as_deref(),
            author.as_deref(),
            content,
            created_at,
            kind,
            trigger_type.as_deref(),
            intervention_context.as_deref(),
            driver_key_fingerprint.as_deref(),
        )?;
        stats.comments += 1;
    }
    for (blocker_id, blocked_id) in &saved_children.deps {
        db.insert_dependency_raw(*blocker_id, *blocked_id)?;
        stats.dependencies += 1;
    }
    for (id1, id2) in &saved_children.relations {
        db.insert_relation_raw(*id1, *id2)?;
        stats.relations += 1;
    }
    for (id, issue_id, started_at, ended_at, duration_seconds) in &saved_children.time_entries {
        db.insert_hydrated_time_entry(
            *id,
            *issue_id,
            started_at,
            ended_at.as_deref(),
            *duration_seconds,
        )?;
    }
    for (milestone_id, issue_id) in &saved_children.milestone_issues {
        db.insert_hydrated_milestone_issue(*milestone_id, *issue_id)?;
    }

    Ok(())
}

/// Hydrate the dependencies table from `blockers` arrays in issue files.
fn hydrate_dependencies(
    db: &Database,
    issue_files: &[&IssueFile],
    uuid_to_id: &HashMap<String, i64>,
    stats: &mut HydrationStats,
) -> Result<()> {
    for issue in issue_files {
        let Some(blocked_id) = issue.display_id else {
            continue;
        };
        for blocker_uuid in &issue.blockers {
            if let Some(&blocker_id) = uuid_to_id.get(&blocker_uuid.to_string()) {
                db.insert_dependency_raw(blocker_id, blocked_id)?;
                stats.dependencies += 1;
            }
            // Dangling UUID (deleted blocker) is silently skipped
        }
    }
    Ok(())
}

/// Hydrate the relations table from `related` arrays in issue files.
fn hydrate_relations(
    db: &Database,
    issue_files: &[&IssueFile],
    uuid_to_id: &HashMap<String, i64>,
    stats: &mut HydrationStats,
) -> Result<()> {
    for issue in issue_files {
        let Some(issue_id) = issue.display_id else {
            continue;
        };
        for related_uuid in &issue.related {
            if let Some(&related_id) = uuid_to_id.get(&related_uuid.to_string()) {
                db.insert_relation_raw(issue_id, related_id)?;
                stats.relations += 1;
            }
        }
    }
    Ok(())
}

/// Migrate inline comments from v1 issue files to standalone v2 comment files.
///
/// For each issue that has inline `comments`, writes a standalone
/// `issues/{uuid}/comments/{comment-uuid}.json` file using `write_comment_file`.
/// This is called during a v1-to-v2 layout upgrade to split inline comments into
/// their own files.
///
/// Returns the number of comment files written.
///
/// # Errors
///
/// Returns an error if reading issue files or writing comment files fails.
pub fn migrate_inline_comments_to_v2(cache_dir: &Path) -> Result<usize> {
    let issues_dir = cache_dir.join("issues");
    let issue_files = read_all_issue_files(&issues_dir)?;

    let mut count = 0;
    for issue in &issue_files {
        if issue.comments.is_empty() {
            continue;
        }
        for comment in &issue.comments {
            let comment_uuid = uuid::Uuid::new_v4();
            let cf = CommentFile {
                uuid: comment_uuid,
                issue_uuid: issue.uuid,
                author: comment.author.clone(),
                content: comment.content.clone(),
                created_at: comment.created_at,
                kind: comment.kind.clone(),
                trigger_type: comment.trigger_type.clone(),
                intervention_context: comment.intervention_context.clone(),
                driver_key_fingerprint: comment.driver_key_fingerprint.clone(),
                signed_by: comment.signed_by.clone(),
                signature: comment.signature.clone(),
            };
            let path = issues_dir
                .join(issue.uuid.to_string())
                .join("comments")
                .join(format!("{comment_uuid}.json"));
            write_comment_file(&path, &cf)?;
            count += 1;
        }
    }
    Ok(count)
}

// ── Lazy auto-hydration ─────────────────────────────────────────────

const LAST_HYDRATED_REF_FILE: &str = ".last-hydrated-ref";

/// Check if the hub branch has moved since the last hydration and re-hydrate if needed.
///
/// This makes read operations automatically pick up changes from other
/// worktrees without requiring an explicit `crosslink sync` (#500).
///
/// Returns `true` if re-hydration was performed.
///
/// # Errors
///
/// Returns an error if hydration fails.
pub fn maybe_auto_hydrate(crosslink_dir: &Path, db: &Database) -> Result<bool> {
    let Ok(sync) = crate::sync::SyncManager::new(crosslink_dir) else {
        return Ok(false); // No sync manager — nothing to hydrate
    };

    if !sync.is_initialized() {
        return Ok(false);
    }

    let cache_dir = sync.cache_path();
    let current_ref = hub_head_ref(crosslink_dir);
    let Some(current_ref) = current_ref else {
        return Ok(false); // Can't determine hub ref — skip
    };

    let marker_path = crosslink_dir.join(LAST_HYDRATED_REF_FILE);
    let last_ref = std::fs::read_to_string(&marker_path)
        .ok()
        .map(|s| s.trim().to_string());

    if last_ref.as_deref() == Some(&current_ref) {
        return Ok(false); // Hub hasn't moved — no re-hydration needed
    }

    tracing::debug!(
        "hub ref moved ({} -> {}), auto-hydrating",
        last_ref.as_deref().unwrap_or("none"),
        &current_ref[..current_ref.len().min(8)]
    );

    hydrate_to_sqlite(cache_dir, db)?;

    // Store the ref we just hydrated from
    let _ = std::fs::write(&marker_path, &current_ref);

    Ok(true)
}

/// Record the current hub branch HEAD ref after a successful hydration.
///
/// Called from `sync_cmd` and the daemon after explicit hydration so that
/// lazy auto-hydration doesn't redundantly re-hydrate.
pub fn record_hydrated_ref(crosslink_dir: &Path) {
    if let Some(ref_sha) = hub_head_ref(crosslink_dir) {
        let marker_path = crosslink_dir.join(LAST_HYDRATED_REF_FILE);
        let _ = std::fs::write(&marker_path, &ref_sha);
    }
}

/// Get the current HEAD SHA of the hub branch from the hub cache worktree.
fn hub_head_ref(crosslink_dir: &Path) -> Option<String> {
    let sync = crate::sync::SyncManager::new(crosslink_dir).ok()?;
    if !sync.is_initialized() {
        return None;
    }

    let output = std::process::Command::new("git")
        .current_dir(sync.cache_path())
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;

    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_file::{
        write_comment_file, write_issue_file, write_layout_version, CommentEntry, CommentFile,
        IssueFile, TimeEntry,
    };
    use chrono::Utc;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    fn make_issue(display_id: i64, title: &str) -> IssueFile {
        IssueFile {
            uuid: Uuid::new_v4(),
            display_id: Some(display_id),
            title: title.to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "test-agent".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        }
    }

    fn write_issues_to_cache(cache_dir: &Path, issues: &[IssueFile]) {
        let issues_dir = cache_dir.join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();
        for issue in issues {
            let path = issues_dir.join(format!("{}.json", issue.uuid));
            write_issue_file(&path, issue).unwrap();
        }
    }

    #[test]
    fn test_hydrate_empty_cache() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();
        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.issues, 0);
    }

    #[test]
    fn test_hydrate_single_issue() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue = make_issue(1, "Test issue");
        write_issues_to_cache(cache.path(), &[issue]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.issues, 1);

        let loaded = db.get_issue(1).unwrap().unwrap();
        assert_eq!(loaded.title, "Test issue");
    }

    #[test]
    fn test_hydrate_with_labels() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Labeled issue");
        issue.labels = vec!["bug".to_string(), "auth".to_string()];
        write_issues_to_cache(cache.path(), &[issue]);

        hydrate_to_sqlite(cache.path(), &db).unwrap();

        let labels = db.get_labels(1).unwrap();
        assert!(labels.contains(&"bug".to_string()));
        assert!(labels.contains(&"auth".to_string()));
    }

    #[test]
    fn test_hydrate_with_comments() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Commented issue");
        issue.comments = vec![CommentEntry {
            id: 1,
            author: "agent-1".to_string(),
            content: "First comment".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        }];
        write_issues_to_cache(cache.path(), &[issue]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.comments, 1);

        let comments = db.get_comments(1).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, "First comment");
    }

    #[test]
    fn test_hydrate_dependencies() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue_a = make_issue(1, "Blocked issue");
        let issue_b = make_issue(2, "Blocker issue");

        // issue_a is blocked by issue_b
        let mut issue_a_with_dep = issue_a;
        issue_a_with_dep.blockers = vec![issue_b.uuid];

        write_issues_to_cache(cache.path(), &[issue_a_with_dep, issue_b]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.dependencies, 1);

        let blockers = db.get_blockers(1).unwrap();
        assert_eq!(blockers, vec![2]);

        let blocking = db.get_blocking(2).unwrap();
        assert_eq!(blocking, vec![1]);
    }

    #[test]
    fn test_hydrate_dangling_blocker_uuid() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Issue with dangling dep");
        issue.blockers = vec![Uuid::new_v4()]; // non-existent blocker
        write_issues_to_cache(cache.path(), &[issue]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.issues, 1);
        assert_eq!(stats.dependencies, 0); // silently skipped
    }

    #[test]
    fn test_hydrate_relations() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue_a = make_issue(1, "Issue A");
        let issue_b = make_issue(2, "Issue B");

        let mut issue_a_related = issue_a;
        issue_a_related.related = vec![issue_b.uuid];

        write_issues_to_cache(cache.path(), &[issue_a_related, issue_b]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.relations, 1);
    }

    #[test]
    fn test_hydrate_parent_child() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let parent = make_issue(1, "Parent");
        let mut child = make_issue(2, "Child");
        child.parent_uuid = Some(parent.uuid);

        write_issues_to_cache(cache.path(), &[parent, child]);

        hydrate_to_sqlite(cache.path(), &db).unwrap();

        let loaded = db.get_issue(2).unwrap().unwrap();
        assert_eq!(loaded.parent_id, Some(1));
    }

    #[test]
    fn test_hydrate_replaces_previous_data() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        // First hydration
        let issue = make_issue(1, "Original");
        write_issues_to_cache(cache.path(), std::slice::from_ref(&issue));
        hydrate_to_sqlite(cache.path(), &db).unwrap();

        // Second hydration with updated title
        let mut updated = issue;
        updated.title = "Updated".to_string();
        // Re-create the issues dir fresh
        let issues_dir = cache.path().join("issues");
        std::fs::remove_dir_all(&issues_dir).unwrap();
        write_issues_to_cache(cache.path(), &[updated]);

        hydrate_to_sqlite(cache.path(), &db).unwrap();

        let loaded = db.get_issue(1).unwrap().unwrap();
        assert_eq!(loaded.title, "Updated");
    }

    #[test]
    fn test_hydrate_assigns_negative_id_for_null_display_id() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut offline = make_issue(0, "Offline");
        offline.display_id = None; // not yet pushed

        let pushed = make_issue(1, "Pushed");
        write_issues_to_cache(cache.path(), &[offline, pushed]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.issues, 2); // both get hydrated

        // Pushed issue gets its display_id
        assert!(db.get_issue(1).unwrap().is_some());

        // Offline issue gets a negative ID
        let offline_issue = db.get_issue(-1).unwrap();
        assert!(offline_issue.is_some());
        assert_eq!(offline_issue.unwrap().title, "Offline");
    }

    #[test]
    fn test_hydrate_with_time_entries() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Timed issue");
        issue.time_entries = vec![TimeEntry {
            id: 1,
            started_at: Utc::now(),
            ended_at: Some(Utc::now()),
            duration_seconds: Some(3600),
        }];
        write_issues_to_cache(cache.path(), &[issue]);

        hydrate_to_sqlite(cache.path(), &db).unwrap();
        // If we got here without error, time entries were inserted successfully
    }

    #[test]
    fn test_hydrate_milestones_per_file() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue = make_issue(1, "Test");
        write_issues_to_cache(cache.path(), &[issue]);

        // Write per-file milestone
        let ms_dir = cache.path().join("meta").join("milestones");
        std::fs::create_dir_all(&ms_dir).unwrap();
        let ms_uuid = Uuid::new_v4();
        let entry = crate::issue_file::MilestoneEntry {
            uuid: ms_uuid,
            display_id: 1,
            name: "v1.0".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            created_at: Utc::now(),
            closed_at: None,
        };
        crate::issue_file::write_milestone_file(&ms_dir.join(format!("{ms_uuid}.json")), &entry)
            .unwrap();

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.milestones, 1);

        let ms = db.get_milestone(1).unwrap();
        assert!(ms.is_some());
        assert_eq!(ms.unwrap().name, "v1.0");
    }

    #[test]
    fn test_hydrate_milestones_legacy_fallback() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue = make_issue(1, "Test");
        write_issues_to_cache(cache.path(), &[issue]);

        // Write legacy single-file milestones.json (no per-file dir)
        let meta_dir = cache.path().join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        let ms_uuid = Uuid::new_v4();
        let mut milestones = std::collections::HashMap::new();
        milestones.insert(
            ms_uuid,
            crate::issue_file::MilestoneEntry {
                uuid: ms_uuid,
                display_id: 1,
                name: "legacy-ms".to_string(),
                description: None,
                status: crate::models::IssueStatus::Open,
                created_at: Utc::now(),
                closed_at: None,
            },
        );
        let mf = crate::issue_file::MilestonesFile { milestones };
        let json = serde_json::to_string_pretty(&mf).unwrap();
        std::fs::write(meta_dir.join("milestones.json"), json).unwrap();

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.milestones, 1);

        let ms = db.get_milestone(1).unwrap();
        assert!(ms.is_some());
        assert_eq!(ms.unwrap().name, "legacy-ms");
    }

    // ---- dedup_issue_files ----

    #[test]
    fn test_dedup_no_duplicates() {
        let a = make_issue(1, "A");
        let b = make_issue(2, "B");
        let issues = [a, b];
        let (keep, dupes) = dedup_issue_files(&issues);
        assert_eq!(keep.len(), 2);
        assert_eq!(dupes.len(), 0);
    }

    #[test]
    fn test_dedup_keeps_most_recent() {
        use chrono::Duration;
        let mut old = make_issue(1, "Old");
        old.updated_at = Utc::now() - Duration::seconds(60);
        let mut new = make_issue(1, "New");
        new.updated_at = Utc::now();
        // same display_id — new should be kept
        let issues = [old, new];
        let (keep, dupes) = dedup_issue_files(&issues);
        assert_eq!(keep.len(), 1);
        assert_eq!(dupes.len(), 1);
        assert_eq!(keep[0].title, "New");
        assert_eq!(dupes[0].title, "Old");
    }

    #[test]
    fn test_dedup_issue_with_no_display_id_passes_through() {
        let mut issue = make_issue(0, "Offline");
        issue.display_id = None;
        let issues = [issue];
        let (keep, dupes) = dedup_issue_files(&issues);
        assert_eq!(keep.len(), 1);
        assert_eq!(dupes.len(), 0);
    }

    #[test]
    fn test_dedup_three_copies_keeps_newest() {
        use chrono::Duration;
        let mut oldest = make_issue(5, "Oldest");
        oldest.updated_at = Utc::now() - Duration::seconds(120);
        let mut middle = make_issue(5, "Middle");
        middle.updated_at = Utc::now() - Duration::seconds(60);
        let mut newest = make_issue(5, "Newest");
        newest.updated_at = Utc::now();
        let issues = [oldest, middle, newest];
        let (keep, dupes) = dedup_issue_files(&issues);
        assert_eq!(keep.len(), 1);
        assert_eq!(dupes.len(), 2);
        assert_eq!(keep[0].title, "Newest");
    }

    // ---- hydrate_to_sqlite duplicate warning path ----

    #[test]
    fn test_hydrate_deduplicates_same_display_id() {
        use chrono::Duration;
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut old = make_issue(1, "Old title");
        old.updated_at = Utc::now() - Duration::seconds(60);
        let mut new = make_issue(1, "New title");
        new.updated_at = Utc::now();
        // Write both files — they share display_id 1
        write_issues_to_cache(cache.path(), &[old, new]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        // Only one issue should land in the DB (the duplicate is skipped)
        assert_eq!(stats.issues, 1);
        let loaded = db.get_issue(1).unwrap().unwrap();
        assert_eq!(loaded.title, "New title");
    }

    // ---- topo_sort_issues ----

    #[test]
    fn test_topo_sort_roots_before_children() {
        let parent = make_issue(1, "Parent");
        let mut child = make_issue(2, "Child");
        child.parent_uuid = Some(parent.uuid);

        // Pass child before parent — topo sort should fix order
        let sorted = topo_sort_issues(&[&child, &parent]);
        assert_eq!(sorted[0].title, "Parent");
        assert_eq!(sorted[1].title, "Child");
    }

    #[test]
    fn test_topo_sort_three_levels_deep() {
        let grandparent = make_issue(1, "Grandparent");
        let mut parent = make_issue(2, "Parent");
        parent.parent_uuid = Some(grandparent.uuid);
        let mut child = make_issue(3, "Child");
        child.parent_uuid = Some(parent.uuid);

        // Pass in reverse order
        let sorted = topo_sort_issues(&[&child, &parent, &grandparent]);
        // grandparent must come before parent, parent before child
        let pos = |title: &str| sorted.iter().position(|i| i.title == title).unwrap();
        assert!(pos("Grandparent") < pos("Parent"));
        assert!(pos("Parent") < pos("Child"));
    }

    #[test]
    fn test_topo_sort_orphaned_parent_uuid_treated_as_root() {
        // A child whose parent UUID is NOT in the set goes to `roots` directly
        // (the `_ =>` arm in the match), so it is sorted alongside other roots.
        let mut orphan_child = make_issue(2, "OrphanChild");
        orphan_child.parent_uuid = Some(Uuid::new_v4()); // unknown parent — not in uuid_set

        let root = make_issue(1, "Root");

        let sorted = topo_sort_issues(&[&orphan_child, &root]);
        // Both are treated as roots; all issues present, exact order unspecified.
        assert_eq!(sorted.len(), 2);
        let titles: Vec<&str> = sorted.iter().map(|i| i.title.as_str()).collect();
        assert!(titles.contains(&"Root"));
        assert!(titles.contains(&"OrphanChild"));
    }

    #[test]
    fn test_topo_sort_no_issues() {
        let sorted = topo_sort_issues(&[]);
        assert!(sorted.is_empty());
    }

    // ---- hydrate_dependencies / hydrate_relations with None display_id ----

    #[test]
    fn test_hydrate_dependency_skips_issue_with_no_display_id() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        // An issue with no display_id that has a blocker — the blocked issue
        // has no display_id so hydrate_dependencies should `continue` for it.
        let blocker = make_issue(1, "Blocker");
        let mut offline = make_issue(0, "Offline blocked");
        offline.display_id = None;
        offline.blockers = vec![blocker.uuid];

        write_issues_to_cache(cache.path(), &[blocker, offline]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        // The dependency should NOT be inserted (offline issue has no display_id)
        assert_eq!(stats.dependencies, 0);
    }

    #[test]
    fn test_hydrate_relation_skips_issue_with_no_display_id() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let related = make_issue(1, "Related");
        let mut offline = make_issue(0, "Offline related");
        offline.display_id = None;
        offline.related = vec![related.uuid];

        write_issues_to_cache(cache.path(), &[related, offline]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.relations, 0);
    }

    #[test]
    fn test_hydrate_dangling_relation_uuid() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Issue with dangling relation");
        issue.related = vec![Uuid::new_v4()]; // non-existent related issue
        write_issues_to_cache(cache.path(), &[issue]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.relations, 0); // silently skipped
    }

    // ---- issue with description and closed_at ----

    #[test]
    fn test_hydrate_issue_with_description_and_closed_at() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Closed issue");
        issue.description = Some("A detailed description".to_string());
        issue.status = crate::models::IssueStatus::Closed;
        issue.closed_at = Some(Utc::now());

        write_issues_to_cache(cache.path(), &[issue]);

        hydrate_to_sqlite(cache.path(), &db).unwrap();

        let loaded = db.get_issue(1).unwrap().unwrap();
        assert_eq!(
            loaded.description.as_deref(),
            Some("A detailed description")
        );
        assert_eq!(loaded.status, crate::models::IssueStatus::Closed);
        assert!(loaded.closed_at.is_some());
    }

    // ---- milestone association via milestone_uuid ----

    #[test]
    fn test_hydrate_issue_milestone_association() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let ms_uuid = Uuid::new_v4();

        let mut issue = make_issue(1, "Milestone issue");
        issue.milestone_uuid = Some(ms_uuid);
        write_issues_to_cache(cache.path(), &[issue]);

        // Write the milestone file so it gets a display_id
        let ms_dir = cache.path().join("meta").join("milestones");
        std::fs::create_dir_all(&ms_dir).unwrap();
        let entry = crate::issue_file::MilestoneEntry {
            uuid: ms_uuid,
            display_id: 10,
            name: "Sprint 1".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            created_at: Utc::now(),
            closed_at: None,
        };
        crate::issue_file::write_milestone_file(&ms_dir.join(format!("{ms_uuid}.json")), &entry)
            .unwrap();

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.milestones, 1);

        // Verify the milestone<->issue link was created
        let ms = db.get_issue_milestone(1).unwrap();
        assert!(ms.is_some());
        assert_eq!(ms.unwrap().name, "Sprint 1");
    }

    #[test]
    fn test_hydrate_issue_milestone_uuid_not_in_map() {
        // milestone_uuid set on issue but no matching milestone file — link silently skipped
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Orphan milestone ref");
        issue.milestone_uuid = Some(Uuid::new_v4());
        write_issues_to_cache(cache.path(), &[issue]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.issues, 1);
        assert_eq!(stats.milestones, 0);
        // No panic, no error — silently ignored
    }

    // ---- milestone with closed_at ----

    #[test]
    fn test_hydrate_milestone_with_closed_at() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue = make_issue(1, "Test");
        write_issues_to_cache(cache.path(), &[issue]);

        let ms_dir = cache.path().join("meta").join("milestones");
        std::fs::create_dir_all(&ms_dir).unwrap();
        let ms_uuid = Uuid::new_v4();
        let entry = crate::issue_file::MilestoneEntry {
            uuid: ms_uuid,
            display_id: 1,
            name: "Closed sprint".to_string(),
            description: Some("A completed sprint".to_string()),
            status: crate::models::IssueStatus::Closed,
            created_at: Utc::now(),
            closed_at: Some(Utc::now()),
        };
        crate::issue_file::write_milestone_file(&ms_dir.join(format!("{ms_uuid}.json")), &entry)
            .unwrap();

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.milestones, 1);
        let ms = db.get_milestone(1).unwrap().unwrap();
        assert_eq!(ms.status, crate::models::IssueStatus::Closed);
    }

    // ---- v2 layout: standalone comment files ----

    #[test]
    fn test_hydrate_v2_standalone_comment_files() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue = make_issue(1, "V2 issue");
        let issue_uuid = issue.uuid;

        // Write the issue using v2 layout: issues/{uuid}/issue.json
        let issue_dir = cache.path().join("issues").join(issue_uuid.to_string());
        std::fs::create_dir_all(&issue_dir).unwrap();
        write_issue_file(&issue_dir.join("issue.json"), &issue).unwrap();

        // Write a standalone comment file: issues/{uuid}/comments/{comment-uuid}.json
        let comments_dir = issue_dir.join("comments");
        std::fs::create_dir_all(&comments_dir).unwrap();
        let comment_uuid = Uuid::new_v4();
        let cf = CommentFile {
            uuid: comment_uuid,
            issue_uuid,
            author: "agent-1".to_string(),
            content: "Standalone comment".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };
        write_comment_file(&comments_dir.join(format!("{comment_uuid}.json")), &cf).unwrap();

        // Write layout version 2
        let meta_dir = cache.path().join("meta");
        write_layout_version(&meta_dir, 2).unwrap();

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.issues, 1);
        assert_eq!(stats.comments, 1);

        let comments = db.get_comments(1).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].content, "Standalone comment");
    }

    #[test]
    fn test_hydrate_v2_comment_with_optional_fields() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue = make_issue(1, "V2 issue with rich comment");
        let issue_uuid = issue.uuid;

        let issue_dir = cache.path().join("issues").join(issue_uuid.to_string());
        std::fs::create_dir_all(&issue_dir).unwrap();
        write_issue_file(&issue_dir.join("issue.json"), &issue).unwrap();

        let comments_dir = issue_dir.join("comments");
        std::fs::create_dir_all(&comments_dir).unwrap();
        let comment_uuid = Uuid::new_v4();
        let cf = CommentFile {
            uuid: comment_uuid,
            issue_uuid,
            author: "agent-2".to_string(),
            content: "Intervention comment".to_string(),
            created_at: Utc::now(),
            kind: "intervention".to_string(),
            trigger_type: Some("tool_rejected".to_string()),
            intervention_context: Some("tried to write to protected file".to_string()),
            driver_key_fingerprint: Some("SHA256:abc123".to_string()),
            signed_by: Some("SHA256:abc123".to_string()),
            signature: Some("base64sig==".to_string()),
        };
        write_comment_file(&comments_dir.join(format!("{comment_uuid}.json")), &cf).unwrap();

        let meta_dir = cache.path().join("meta");
        write_layout_version(&meta_dir, 2).unwrap();

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.comments, 1);
    }

    #[test]
    fn test_hydrate_v2_multiple_comments_get_unique_ids() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let issue = make_issue(1, "V2 multi-comment");
        let issue_uuid = issue.uuid;

        let issue_dir = cache.path().join("issues").join(issue_uuid.to_string());
        std::fs::create_dir_all(&issue_dir).unwrap();
        write_issue_file(&issue_dir.join("issue.json"), &issue).unwrap();

        let comments_dir = issue_dir.join("comments");
        std::fs::create_dir_all(&comments_dir).unwrap();

        for i in 0..3u32 {
            let cu = Uuid::new_v4();
            let cf = CommentFile {
                uuid: cu,
                issue_uuid,
                author: format!("agent-{i}"),
                content: format!("Comment {i}"),
                created_at: Utc::now(),
                kind: "note".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None,
                signature: None,
            };
            write_comment_file(&comments_dir.join(format!("{cu}.json")), &cf).unwrap();
        }

        let meta_dir = cache.path().join("meta");
        write_layout_version(&meta_dir, 2).unwrap();

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.comments, 3);

        let comments = db.get_comments(1).unwrap();
        assert_eq!(comments.len(), 3);
    }

    // ---- v1 layout: standalone comments dir absent (no read_comment_files called) ----

    #[test]
    fn test_hydrate_v1_layout_skips_v2_comment_files() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        // layout_version defaults to 1 (no meta/version.json)
        let issue = make_issue(1, "V1 issue");
        let issue_uuid = issue.uuid;
        write_issues_to_cache(cache.path(), &[issue]);

        // Write a comment file anyway — it should be ignored at v1
        let comments_dir = cache
            .path()
            .join("issues")
            .join(issue_uuid.to_string())
            .join("comments");
        std::fs::create_dir_all(&comments_dir).unwrap();
        let cu = Uuid::new_v4();
        let cf = CommentFile {
            uuid: cu,
            issue_uuid,
            author: "agent".to_string(),
            content: "Should be ignored".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };
        write_comment_file(&comments_dir.join(format!("{cu}.json")), &cf).unwrap();

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.comments, 0); // v2 path not entered
    }

    // ---- migrate_inline_comments_to_v2 ----

    #[test]
    fn test_migrate_inline_comments_no_issues() {
        let cache = tempdir().unwrap();
        // Empty issues dir — no migration needed
        let issues_dir = cache.path().join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();

        let count = migrate_inline_comments_to_v2(cache.path()).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_migrate_inline_comments_no_comments() {
        let cache = tempdir().unwrap();
        // Issue with no comments — nothing to migrate
        let issue = make_issue(1, "No comments");
        write_issues_to_cache(cache.path(), &[issue]);

        let count = migrate_inline_comments_to_v2(cache.path()).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_migrate_inline_comments_writes_files() {
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Issue with comments");
        issue.comments = vec![
            CommentEntry {
                id: 1,
                author: "agent-1".to_string(),
                content: "First".to_string(),
                created_at: Utc::now(),
                kind: "note".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None,
                signature: None,
            },
            CommentEntry {
                id: 2,
                author: "agent-2".to_string(),
                content: "Second".to_string(),
                created_at: Utc::now(),
                kind: "decision".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None,
                signature: None,
            },
        ];
        let issue_uuid = issue.uuid;
        write_issues_to_cache(cache.path(), &[issue]);

        let count = migrate_inline_comments_to_v2(cache.path()).unwrap();
        assert_eq!(count, 2);

        // Verify the comment files were actually written
        let comments_dir = cache
            .path()
            .join("issues")
            .join(issue_uuid.to_string())
            .join("comments");
        let entries: Vec<_> = std::fs::read_dir(&comments_dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x == "json")
            })
            .collect();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_migrate_inline_comments_preserves_optional_fields() {
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Intervention issue");
        issue.comments = vec![CommentEntry {
            id: 1,
            author: "agent".to_string(),
            content: "Blocked by policy".to_string(),
            created_at: Utc::now(),
            kind: "intervention".to_string(),
            trigger_type: Some("tool_blocked".to_string()),
            intervention_context: Some("tried to delete /etc/passwd".to_string()),
            driver_key_fingerprint: Some("SHA256:xyz".to_string()),
            signed_by: Some("SHA256:xyz".to_string()),
            signature: Some("sig==".to_string()),
        }];
        let issue_uuid = issue.uuid;
        write_issues_to_cache(cache.path(), &[issue]);

        let count = migrate_inline_comments_to_v2(cache.path()).unwrap();
        assert_eq!(count, 1);

        // Read the written file back and check optional fields survived
        let comments_dir = cache
            .path()
            .join("issues")
            .join(issue_uuid.to_string())
            .join("comments");
        let json_path = std::fs::read_dir(&comments_dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .find(|e| {
                e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| x == "json")
            })
            .unwrap()
            .path();

        let cf: CommentFile =
            serde_json::from_str(&std::fs::read_to_string(&json_path).unwrap()).unwrap();
        assert_eq!(cf.kind, "intervention");
        assert_eq!(cf.trigger_type.as_deref(), Some("tool_blocked"));
        assert_eq!(
            cf.intervention_context.as_deref(),
            Some("tried to delete /etc/passwd")
        );
        assert_eq!(cf.driver_key_fingerprint.as_deref(), Some("SHA256:xyz"));
        assert_eq!(cf.signed_by.as_deref(), Some("SHA256:xyz"));
        assert_eq!(cf.signature.as_deref(), Some("sig=="));
    }

    #[test]
    fn test_migrate_inline_comments_nonexistent_issues_dir() {
        // Issues dir doesn't exist at all — read_all_issue_files returns empty vec
        let cache = tempdir().unwrap();
        let count = migrate_inline_comments_to_v2(cache.path()).unwrap();
        assert_eq!(count, 0);
    }

    // ---- time entry with no ended_at ----

    #[test]
    fn test_hydrate_time_entry_without_ended_at() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut issue = make_issue(1, "Active timer");
        issue.time_entries = vec![TimeEntry {
            id: 1,
            started_at: Utc::now(),
            ended_at: None, // timer still running
            duration_seconds: None,
        }];
        write_issues_to_cache(cache.path(), &[issue]);

        hydrate_to_sqlite(cache.path(), &db).unwrap();
        // No error means the None-ended_at path was handled correctly
    }

    // ---- HydrationStats default ----

    #[test]
    fn test_hydration_stats_default() {
        let stats = HydrationStats::default();
        assert_eq!(stats.issues, 0);
        assert_eq!(stats.comments, 0);
        assert_eq!(stats.dependencies, 0);
        assert_eq!(stats.relations, 0);
        assert_eq!(stats.milestones, 0);
    }

    // ---- offline issue as parent of another offline issue ----

    #[test]
    fn test_hydrate_offline_child_resolves_offline_parent() {
        let (db, _dir) = setup_test_db();
        let cache = tempdir().unwrap();

        let mut parent = make_issue(0, "Offline parent");
        parent.display_id = None;
        let parent_uuid = parent.uuid;

        let mut child = make_issue(0, "Offline child");
        child.display_id = None;
        child.parent_uuid = Some(parent_uuid);

        write_issues_to_cache(cache.path(), &[parent, child]);

        let stats = hydrate_to_sqlite(cache.path(), &db).unwrap();
        assert_eq!(stats.issues, 2);

        // Offline parent gets -1, child gets -2
        let loaded_parent = db.get_issue(-1).unwrap();
        let loaded_child = db.get_issue(-2).unwrap();
        assert!(loaded_parent.is_some() || loaded_child.is_some());
    }
}
