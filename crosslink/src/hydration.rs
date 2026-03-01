//! Hydrate local SQLite from JSON issue files on the coordination branch.
//!
//! On every `crosslink sync`, this module reads all `issues/*.json` files from
//! the coordination branch worktree cache and writes them into the local SQLite
//! database in a single transaction. This keeps SQLite as the universal read
//! path while JSON on the git branch remains the source of truth.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use crate::db::{Database, HydratedIssue, HydratedMilestone};
use crate::issue_file::{
    read_all_issue_files, read_all_milestone_files, read_milestones_file, IssueFile,
};

/// Statistics returned after hydration.
#[derive(Debug, Default)]
pub struct HydrationStats {
    pub issues: usize,
    pub comments: usize,
    pub dependencies: usize,
    pub relations: usize,
    pub milestones: usize,
}

/// Hydrate the local SQLite database from JSON files in the coordination branch cache.
///
/// This function:
/// 1. Reads all `issues/*.json` files from `cache_dir/issues/`
/// 2. Reads `meta/counters.json` and `meta/milestones.json`
/// 3. Clears all shared data from SQLite (issues, comments, labels, deps, etc.)
/// 4. Re-inserts everything from the JSON files in a single transaction
///
/// Sessions are NOT touched — they are machine-local state.
pub fn hydrate_to_sqlite(cache_dir: &Path, db: &Database) -> Result<HydrationStats> {
    let issues_dir = cache_dir.join("issues");
    let issue_files = read_all_issue_files(&issues_dir)?;

    if issue_files.is_empty() {
        return Ok(HydrationStats::default());
    }

    // Try per-file milestones first (new format), fall back to legacy single-file
    let milestones_dir = cache_dir.join("meta").join("milestones");
    let mut milestone_entries = read_all_milestone_files(&milestones_dir)?;
    if milestone_entries.is_empty() {
        let legacy_path = cache_dir.join("meta").join("milestones.json");
        let legacy = read_milestones_file(&legacy_path)?;
        milestone_entries = legacy.milestones.into_values().collect();
    }

    // Build uuid -> display_id lookup for resolving cross-references
    let mut uuid_to_id: HashMap<String, i64> = issue_files
        .iter()
        .filter_map(|f| f.display_id.map(|id| (f.uuid.to_string(), id)))
        .collect();

    // Build milestone uuid -> display_id lookup
    let milestone_uuid_to_id: HashMap<String, i64> = milestone_entries
        .iter()
        .map(|m| (m.uuid.to_string(), m.display_id))
        .collect();

    let mut stats = HydrationStats::default();

    db.transaction(|| {
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
                status: &entry.status,
                created_at: &created_at,
                closed_at: closed_at.as_deref(),
            })?;
            stats.milestones += 1;
        }

        // Sort issues so parents come before children (foreign key constraint)
        let sorted_issues = topo_sort_issues(&issue_files);

        // Insert issues (offline issues get sequential negative IDs)
        let mut next_local_id: i64 = -1;
        for issue in &sorted_issues {
            let display_id = match issue.display_id {
                Some(id) => id,
                None => {
                    let local_id = next_local_id;
                    next_local_id -= 1;
                    // Track in uuid_to_id so cross-references resolve
                    uuid_to_id.insert(issue.uuid.to_string(), local_id);
                    local_id
                }
            };

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
                status: &issue.status,
                priority: &issue.priority,
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

            // Comments
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

        // Hydrate dependencies (single-direction: blockers array on blocked issue)
        hydrate_dependencies(db, &issue_files, &uuid_to_id, &mut stats)?;

        // Hydrate relations (single-direction: related array, insert both directions)
        hydrate_relations(db, &issue_files, &uuid_to_id, &mut stats)?;

        Ok(stats)
    })
}

/// Sort issues so parents appear before children (for foreign key constraints).
/// Issues without parents come first, then children in dependency order.
fn topo_sort_issues(issues: &[IssueFile]) -> Vec<&IssueFile> {
    let uuid_set: std::collections::HashSet<_> = issues.iter().map(|i| i.uuid).collect();
    let mut roots = Vec::new();
    let mut children = Vec::new();

    for issue in issues {
        match issue.parent_uuid {
            Some(parent) if uuid_set.contains(&parent) => children.push(issue),
            _ => roots.push(issue),
        }
    }

    // Simple two-pass: roots first, then children.
    // For deeper nesting, a full topo sort would be needed,
    // but crosslink typically has at most 1-2 levels of nesting.
    let mut sorted = roots;

    // Multi-pass: keep appending children whose parent is already in sorted
    let mut remaining = children;
    for _ in 0..10 {
        if remaining.is_empty() {
            break;
        }
        let sorted_uuids: std::collections::HashSet<_> = sorted.iter().map(|i| i.uuid).collect();
        let (ready, still_remaining): (Vec<_>, Vec<_>) = remaining
            .into_iter()
            .partition(|i| i.parent_uuid.is_none_or(|p| sorted_uuids.contains(&p)));
        sorted.extend(ready);
        remaining = still_remaining;
    }
    // Any remaining (orphaned parents not in the set) go at the end
    sorted.extend(remaining);
    sorted
}

/// Hydrate the dependencies table from `blockers` arrays in issue files.
fn hydrate_dependencies(
    db: &Database,
    issue_files: &[IssueFile],
    uuid_to_id: &HashMap<String, i64>,
    stats: &mut HydrationStats,
) -> Result<()> {
    for issue in issue_files {
        let blocked_id = match issue.display_id {
            Some(id) => id,
            None => continue,
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
    issue_files: &[IssueFile],
    uuid_to_id: &HashMap<String, i64>,
    stats: &mut HydrationStats,
) -> Result<()> {
    for issue in issue_files {
        let issue_id = match issue.display_id {
            Some(id) => id,
            None => continue,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_file::{write_issue_file, CommentEntry, IssueFile, TimeEntry};
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
            status: "open".to_string(),
            priority: "medium".to_string(),
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
        let mut issue_a_with_dep = issue_a.clone();
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

        let mut issue_a_related = issue_a.clone();
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
        write_issues_to_cache(cache.path(), &[issue.clone()]);
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
            status: "open".to_string(),
            created_at: Utc::now(),
            closed_at: None,
        };
        crate::issue_file::write_milestone_file(&ms_dir.join(format!("{}.json", ms_uuid)), &entry)
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
                status: "open".to_string(),
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
}
