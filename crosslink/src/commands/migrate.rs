//! Migration commands for converting between local SQLite and shared JSON.
//!
//! - `migrate-to-shared`: Export all SQLite issues to JSON on the coordination branch.
//! - `migrate-from-shared`: Import JSON issues from the coordination branch into SQLite.

use anyhow::{bail, Result};
use chrono::Utc;
use std::collections::HashMap;
use std::path::Path;
use uuid::Uuid;

use crate::db::Database;
use crate::hydration::hydrate_to_sqlite;
use crate::identity::AgentConfig;
use crate::issue_file::{
    write_counters, write_issue_file, write_milestone_file, CommentEntry, Counters, IssueFile,
    MilestoneEntry,
};
use crate::sync::SyncManager;

/// `crosslink migrate-to-shared` — export local SQLite issues to shared JSON.
///
/// Reads all issues, comments, labels, dependencies, relations, milestones
/// from the local database and writes them as JSON files on the coordination branch.
pub fn to_shared(crosslink_dir: &Path, db: &Database) -> Result<()> {
    let agent = AgentConfig::load(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!("No agent configured. Run 'crosslink agent init <id>' first.")
    })?;

    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    let cache_dir = sync.cache_path().to_path_buf();
    let issues_dir = cache_dir.join("issues");
    let meta_dir = cache_dir.join("meta");
    std::fs::create_dir_all(&issues_dir)?;
    std::fs::create_dir_all(&meta_dir)?;

    // Check if there are already issue files on the coordination branch
    let existing_count = std::fs::read_dir(&issues_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .count();
    if existing_count > 0 {
        bail!(
            "Coordination branch already has {} issue file(s). \
             Migration would overwrite them. Aborting.\n\
             Use 'crosslink migrate-from-shared' to import instead.",
            existing_count
        );
    }

    // Load all issues from SQLite
    let issues = db.list_issues(Some("all"), None, None)?;
    if issues.is_empty() {
        println!("No issues to migrate.");
        return Ok(());
    }

    // Assign UUIDs: display_id → UUID mapping
    let mut id_to_uuid: HashMap<i64, Uuid> = HashMap::new();
    for issue in &issues {
        id_to_uuid.insert(issue.id, Uuid::new_v4());
    }

    // Load milestones and assign UUIDs
    let milestones = db.list_milestones(Some("all"))?;
    let mut milestone_id_to_uuid: HashMap<i64, Uuid> = HashMap::new();
    for ms in &milestones {
        milestone_id_to_uuid.insert(ms.id, Uuid::new_v4());
    }

    let mut max_comment_id: i64 = 0;
    let mut files_written = 0;

    // Convert each issue to an IssueFile and write JSON
    for issue in &issues {
        let uuid = id_to_uuid[&issue.id];

        // Resolve parent UUID
        let parent_uuid = issue
            .parent_id
            .and_then(|pid| id_to_uuid.get(&pid).copied());

        // Load associated data
        let labels = db.get_labels(issue.id)?;
        let comments = db.get_comments(issue.id)?;
        let blockers = db.get_blockers(issue.id)?;
        let related_issues = db.get_related_issues(issue.id)?;
        let milestone = db.get_issue_milestone(issue.id)?;

        // Convert comments
        let comment_entries: Vec<CommentEntry> = comments
            .iter()
            .map(|c| {
                if c.id > max_comment_id {
                    max_comment_id = c.id;
                }
                CommentEntry {
                    id: c.id,
                    author: agent.agent_id.clone(),
                    content: c.content.clone(),
                    created_at: c.created_at,
                    kind: "note".to_string(),
                    trigger_type: None,
                    intervention_context: None,
                    signed_by: None,
                    signature: None,
                }
            })
            .collect();

        // Convert blockers to UUIDs (skip if blocker not in our set)
        let blocker_uuids: Vec<Uuid> = blockers
            .iter()
            .filter_map(|bid| id_to_uuid.get(bid).copied())
            .collect();

        // Convert relations to UUIDs (single direction — only store if related_id > issue_id
        // to avoid duplicates; hydration handles bidirectional insertion)
        let related_uuids: Vec<Uuid> = related_issues
            .iter()
            .filter(|r| r.id > issue.id) // only store one direction
            .filter_map(|r| id_to_uuid.get(&r.id).copied())
            .collect();

        // Resolve milestone UUID
        let milestone_uuid = milestone
            .as_ref()
            .and_then(|m| milestone_id_to_uuid.get(&m.id).copied());

        let issue_file = IssueFile {
            uuid,
            display_id: Some(issue.id),
            title: issue.title.clone(),
            description: issue.description.clone(),
            status: issue.status.clone(),
            priority: issue.priority.clone(),
            parent_uuid,
            created_by: agent.agent_id.clone(),
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            closed_at: issue.closed_at,
            labels,
            comments: comment_entries,
            blockers: blocker_uuids,
            related: related_uuids,
            milestone_uuid,
            time_entries: vec![],
        };

        let path = issues_dir.join(format!("{}.json", uuid));
        write_issue_file(&path, &issue_file)?;
        files_written += 1;
    }

    // Write counters.json
    let max_display_id = issues.iter().map(|i| i.id).max().unwrap_or(0);
    let max_milestone_id = milestones.iter().map(|m| m.id).max().unwrap_or(0);
    let counters = Counters {
        next_display_id: max_display_id + 1,
        next_comment_id: max_comment_id + 1,
        next_milestone_id: max_milestone_id + 1,
    };
    write_counters(&meta_dir.join("counters.json"), &counters)?;

    // Write per-file milestones to meta/milestones/{uuid}.json
    if !milestones.is_empty() {
        let milestones_dir = meta_dir.join("milestones");
        std::fs::create_dir_all(&milestones_dir)?;
        for ms in &milestones {
            let uuid = milestone_id_to_uuid[&ms.id];
            let entry = MilestoneEntry {
                uuid,
                display_id: ms.id,
                name: ms.name.clone(),
                description: ms.description.clone(),
                status: ms.status.clone(),
                created_at: ms.created_at,
                closed_at: ms.closed_at,
            };
            write_milestone_file(&milestones_dir.join(format!("{}.json", uuid)), &entry)?;
        }
    }

    // Commit and push
    git_in_dir(&cache_dir, &["add", "issues/", "meta/"])?;
    let commit_msg = format!(
        "{}: migrate {} issues to shared at {}",
        agent.agent_id,
        files_written,
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
    );
    git_in_dir(&cache_dir, &["commit", "-m", &commit_msg])?;

    // Best-effort push
    match git_in_dir(&cache_dir, &["push", "origin", crate::sync::HUB_BRANCH]) {
        Ok(_) => println!("Pushed to remote."),
        Err(e) => {
            let err = e.to_string();
            if err.contains("Could not resolve host") || err.contains("Could not read from remote")
            {
                println!("Offline — committed locally, will push on next sync.");
            } else {
                eprintln!("Warning: push failed: {}. Committed locally.", err);
            }
        }
    }

    println!(
        "Migrated {} issue(s), {} milestone(s) to shared coordination branch.",
        files_written,
        milestones.len()
    );
    println!(
        "Next display ID: {}, next comment ID: {}",
        counters.next_display_id, counters.next_comment_id
    );

    Ok(())
}

/// `crosslink migrate-from-shared` — import shared JSON issues into local SQLite.
///
/// Fetches the coordination branch and hydrates all issues into the local database.
pub fn from_shared(crosslink_dir: &Path, db: &Database) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    let cache_dir = sync.cache_path().to_path_buf();
    let issues_dir = cache_dir.join("issues");

    // Count issue files
    let issue_count = if issues_dir.exists() {
        std::fs::read_dir(&issues_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .count()
    } else {
        0
    };

    if issue_count == 0 {
        println!("No issue files found on coordination branch.");
        return Ok(());
    }

    // Hydrate into SQLite
    let stats = hydrate_to_sqlite(&cache_dir, db)?;

    println!(
        "Imported from shared: {} issue(s), {} comment(s), {} dep(s), {} relation(s), {} milestone(s).",
        stats.issues, stats.comments, stats.dependencies, stats.relations, stats.milestones
    );

    Ok(())
}

/// `crosslink migrate-rename-branch` — rename crosslink/locks to crosslink/hub.
///
/// Runs the auto-migration and updates the `.crosslink/.gitignore` if needed.
pub fn rename_branch(crosslink_dir: &Path) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    let migrated = sync.migrate_from_locks_branch()?;
    if migrated {
        println!("Migrated crosslink/locks -> crosslink/hub");

        // Update .gitignore
        let gitignore_path = crosslink_dir.join(".gitignore");
        if gitignore_path.exists() {
            let content = std::fs::read_to_string(&gitignore_path)?;
            let updated = content.replace(".locks-cache/", ".hub-cache/");
            if content != updated {
                std::fs::write(&gitignore_path, updated)?;
                println!("Updated .crosslink/.gitignore");
            }
        }

        // Initialize the new cache worktree
        sync.init_cache()?;
        println!("Cache initialized at .crosslink/.hub-cache/");
    } else {
        println!("No migration needed (already using crosslink/hub).");
    }
    Ok(())
}

/// Run a git command in the specified directory.
fn git_in_dir(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run git {:?}", args))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {:?} failed: {}", args, stderr);
    }
    Ok(output)
}

use anyhow::Context;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_to_shared_no_agent() {
        let (db, dir) = setup_test_db();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let result = to_shared(&crosslink_dir, &db);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No agent configured"));
    }

    #[test]
    fn test_from_shared_no_sync() {
        let (db, dir) = setup_test_db();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Without sync manager setup, from_shared should fail gracefully
        let result = from_shared(&crosslink_dir, &db);
        assert!(result.is_err());
    }

    #[test]
    fn test_issue_to_issuefile_conversion() {
        // Test the core conversion logic without git
        let (db, _dir) = setup_test_db();

        let id1 = db
            .create_issue("Bug fix", Some("Fix the bug"), "high")
            .unwrap();
        let id2 = db.create_issue("Feature", None, "medium").unwrap();
        db.add_comment(id1, "First comment", "note").unwrap();
        db.add_label(id1, "bug").unwrap();
        db.add_dependency(id1, id2).unwrap(); // id1 blocked by id2

        let issues = db.list_issues(Some("all"), None, None).unwrap();
        assert_eq!(issues.len(), 2);

        // Simulate UUID assignment
        let mut id_to_uuid: HashMap<i64, Uuid> = HashMap::new();
        for issue in &issues {
            id_to_uuid.insert(issue.id, Uuid::new_v4());
        }

        // Convert issue 1
        let issue = issues.iter().find(|i| i.id == id1).unwrap();
        let labels = db.get_labels(issue.id).unwrap();
        let comments = db.get_comments(issue.id).unwrap();
        let blockers = db.get_blockers(issue.id).unwrap();

        assert_eq!(labels, vec!["bug"]);
        assert_eq!(comments.len(), 1);
        assert_eq!(blockers, vec![id2]);

        let blocker_uuids: Vec<Uuid> = blockers
            .iter()
            .filter_map(|bid| id_to_uuid.get(bid).copied())
            .collect();
        assert_eq!(blocker_uuids.len(), 1);
        assert_eq!(blocker_uuids[0], id_to_uuid[&id2]);

        let issue_file = IssueFile {
            uuid: id_to_uuid[&id1],
            display_id: Some(id1),
            title: issue.title.clone(),
            description: issue.description.clone(),
            status: issue.status.clone(),
            priority: issue.priority.clone(),
            parent_uuid: None,
            created_by: "test-agent".to_string(),
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            closed_at: issue.closed_at,
            labels,
            comments: comments
                .iter()
                .map(|c| CommentEntry {
                    id: c.id,
                    author: "test-agent".to_string(),
                    content: c.content.clone(),
                    created_at: c.created_at,
                    kind: "note".to_string(),
                    trigger_type: None,
                    intervention_context: None,
                    signed_by: None,
                    signature: None,
                })
                .collect(),
            blockers: blocker_uuids,
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };

        // Verify the conversion
        assert_eq!(issue_file.title, "Bug fix");
        assert_eq!(issue_file.description, Some("Fix the bug".to_string()));
        assert_eq!(issue_file.priority, "high");
        assert_eq!(issue_file.labels, vec!["bug"]);
        assert_eq!(issue_file.comments.len(), 1);
        assert_eq!(issue_file.blockers.len(), 1);

        // JSON roundtrip
        let json = serde_json::to_string_pretty(&issue_file).unwrap();
        let parsed: IssueFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.uuid, issue_file.uuid);
        assert_eq!(parsed.display_id, issue_file.display_id);
        assert_eq!(parsed.blockers, issue_file.blockers);
    }

    #[test]
    fn test_write_issue_files_to_dir() {
        let dir = tempdir().unwrap();
        let issues_dir = dir.path().join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();

        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let issue = IssueFile {
            uuid,
            display_id: Some(1),
            title: "Test issue".to_string(),
            description: None,
            status: "open".to_string(),
            priority: "medium".to_string(),
            parent_uuid: None,
            created_by: "agent".to_string(),
            created_at: now,
            updated_at: now,
            closed_at: None,
            labels: vec!["test".to_string()],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };

        let path = issues_dir.join(format!("{}.json", uuid));
        write_issue_file(&path, &issue).unwrap();

        // Verify file exists and is valid
        let loaded = crate::issue_file::read_issue_file(&path).unwrap();
        assert_eq!(loaded.uuid, uuid);
        assert_eq!(loaded.title, "Test issue");
        assert_eq!(loaded.labels, vec!["test"]);
    }

    #[test]
    fn test_counters_from_issues() {
        let (db, _dir) = setup_test_db();

        db.create_issue("Issue 1", None, "medium").unwrap();
        db.create_issue("Issue 2", None, "high").unwrap();
        let id3 = db.create_issue("Issue 3", None, "low").unwrap();
        db.add_comment(id3, "comment A", "note").unwrap();
        let cid = db.add_comment(id3, "comment B", "note").unwrap();

        let issues = db.list_issues(Some("all"), None, None).unwrap();
        let max_display_id = issues.iter().map(|i| i.id).max().unwrap_or(0);

        let mut max_comment_id: i64 = 0;
        for issue in &issues {
            let comments = db.get_comments(issue.id).unwrap();
            for c in &comments {
                if c.id > max_comment_id {
                    max_comment_id = c.id;
                }
            }
        }

        assert_eq!(max_display_id, id3);
        assert_eq!(max_comment_id, cid);

        let counters = Counters {
            next_display_id: max_display_id + 1,
            next_comment_id: max_comment_id + 1,
            next_milestone_id: 1,
        };
        assert_eq!(counters.next_display_id, id3 + 1);
        assert_eq!(counters.next_comment_id, cid + 1);
    }

    #[test]
    fn test_milestone_migration() {
        let (db, _dir) = setup_test_db();

        let ms_id = db.create_milestone("v1.0", Some("First release")).unwrap();
        let issue_id = db.create_issue("Feature A", None, "high").unwrap();
        db.add_issue_to_milestone(ms_id, issue_id).unwrap();

        let milestones = db.list_milestones(Some("all")).unwrap();
        assert_eq!(milestones.len(), 1);
        assert_eq!(milestones[0].name, "v1.0");

        let ms_issues = db.get_milestone_issues(ms_id).unwrap();
        assert_eq!(ms_issues.len(), 1);
        assert_eq!(ms_issues[0].id, issue_id);

        // Convert to MilestoneEntry
        let uuid = Uuid::new_v4();
        let ms = &milestones[0];
        let entry = MilestoneEntry {
            uuid,
            display_id: ms.id,
            name: ms.name.clone(),
            description: ms.description.clone(),
            status: ms.status.clone(),
            created_at: ms.created_at,
            closed_at: ms.closed_at,
        };
        assert_eq!(entry.name, "v1.0");
        assert_eq!(entry.description, Some("First release".to_string()));
    }

    #[test]
    fn test_relation_single_direction() {
        let (db, _dir) = setup_test_db();

        let id1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let id2 = db.create_issue("Issue 2", None, "medium").unwrap();
        let id3 = db.create_issue("Issue 3", None, "medium").unwrap();

        db.add_relation(id1, id2).unwrap();
        db.add_relation(id1, id3).unwrap();

        let mut id_to_uuid: HashMap<i64, Uuid> = HashMap::new();
        id_to_uuid.insert(id1, Uuid::new_v4());
        id_to_uuid.insert(id2, Uuid::new_v4());
        id_to_uuid.insert(id3, Uuid::new_v4());

        // For issue 1, related issues are 2 and 3
        let related = db.get_related_issues(id1).unwrap();
        assert_eq!(related.len(), 2);

        // Only store relations where related_id > issue_id
        let related_uuids: Vec<Uuid> = related
            .iter()
            .filter(|r| r.id > id1)
            .filter_map(|r| id_to_uuid.get(&r.id).copied())
            .collect();
        assert_eq!(related_uuids.len(), 2);

        // For issue 2, related issue is 1 (but 1 < 2 so we skip it)
        let related2 = db.get_related_issues(id2).unwrap();
        let related2_uuids: Vec<Uuid> = related2
            .iter()
            .filter(|r| r.id > id2)
            .filter_map(|r| id_to_uuid.get(&r.id).copied())
            .collect();
        // id1 < id2, so no stored relations from id2's perspective
        // id3 > id2, but id2 isn't directly related to id3
        assert_eq!(related2_uuids.len(), 0);
    }

    #[test]
    fn test_subissue_parent_uuid() {
        let (db, _dir) = setup_test_db();

        let parent = db.create_issue("Parent", None, "medium").unwrap();
        let child = db.create_subissue(parent, "Child", None, "medium").unwrap();

        let mut id_to_uuid: HashMap<i64, Uuid> = HashMap::new();
        id_to_uuid.insert(parent, Uuid::new_v4());
        id_to_uuid.insert(child, Uuid::new_v4());

        let child_issue = db.get_issue(child).unwrap().unwrap();
        assert_eq!(child_issue.parent_id, Some(parent));

        let parent_uuid = child_issue
            .parent_id
            .and_then(|pid| id_to_uuid.get(&pid).copied());
        assert!(parent_uuid.is_some());
        assert_eq!(parent_uuid.unwrap(), id_to_uuid[&parent]);
    }
}
