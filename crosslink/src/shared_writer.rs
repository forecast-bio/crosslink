//! Write-side operations for multi-agent shared issue tracking.
//!
//! `SharedWriter` wraps a `SyncManager` and `AgentConfig` to provide
//! write operations that persist issue data as JSON on the coordination
//! branch and then update local SQLite. In single-agent mode (no
//! `agent.json`), `SharedWriter::new()` returns `None` and all commands
//! fall back to direct `Database` writes.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::db::Database;
use crate::hydration::hydrate_to_sqlite;
use crate::identity::AgentConfig;
use crate::issue_file::{
    read_counters, read_issue_file, read_milestone_file, write_counters, CommentEntry, Counters,
    IssueFile, MilestoneEntry,
};
use crate::sync::SyncManager;

/// Stats from rewriting local issue references after promotion.
#[derive(Debug, Default)]
pub struct RewriteStats {
    pub comments_updated: usize,
    pub descriptions_updated: usize,
    pub sessions_updated: usize,
}

impl RewriteStats {
    pub fn total(&self) -> usize {
        self.comments_updated + self.descriptions_updated + self.sessions_updated
    }
}

/// Replace `Lx` tokens in text with their promoted `#N` equivalents.
///
/// Only replaces at word boundaries to avoid false positives (e.g. "FILE1" is not rewritten).
/// Returns `Some(new_text)` if any replacements were made, `None` otherwise.
fn replace_local_refs(text: &str, replacements: &[(String, String)]) -> Option<String> {
    let mut result = text.to_string();
    let mut changed = false;
    for (old, new) in replacements {
        let mut i = 0;
        while let Some(pos) = result[i..].find(old.as_str()) {
            let abs_pos = i + pos;
            let end_pos = abs_pos + old.len();

            // Check word boundary before: must be start of string or non-alphanumeric
            let before_ok = abs_pos == 0 || !result.as_bytes()[abs_pos - 1].is_ascii_alphanumeric();

            // Check word boundary after: must be end of string or non-alphanumeric
            let after_ok =
                end_pos >= result.len() || !result.as_bytes()[end_pos].is_ascii_alphanumeric();

            if before_ok && after_ok {
                result = format!("{}{}{}", &result[..abs_pos], new, &result[end_pos..]);
                changed = true;
                i = abs_pos + new.len();
            } else {
                i = end_pos;
            }
        }
    }
    if changed {
        Some(result)
    } else {
        None
    }
}

/// Content to write in a single atomic commit-push operation.
struct WriteSet {
    /// Files to write: (relative path in cache, serialized content).
    files: Vec<(String, Vec<u8>)>,
    /// Updated counters, if any.
    counters: Option<Counters>,
    /// If true, stage removals (`git rm`) instead of additions (`git add`).
    use_git_rm: bool,
}

/// Maximum number of push retries on conflict before giving up.
const MAX_RETRIES: usize = 3;

/// Outcome of a write_commit_push operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushOutcome {
    /// Commit was pushed to remote successfully.
    Pushed,
    /// Commit was saved locally but push failed (offline or all retries exhausted).
    LocalOnly,
}

/// Write-side coordinator for multi-agent shared issue tracking.
///
/// Handles: generate UUID → claim display ID → write JSON → commit →
/// push (with rebase-retry) → update local SQLite.
pub struct SharedWriter {
    #[allow(dead_code)] // used in Phase 3 for fetch integration
    sync: SyncManager,
    agent: AgentConfig,
    cache_dir: PathBuf,
}

impl SharedWriter {
    /// Create a SharedWriter if multi-agent mode is configured.
    ///
    /// Returns `None` if there is no `agent.json` (single-agent mode).
    pub fn new(crosslink_dir: &Path) -> Result<Option<Self>> {
        let agent = match AgentConfig::load(crosslink_dir)? {
            Some(a) => a,
            None => return Ok(None),
        };
        let sync = SyncManager::new(crosslink_dir)?;
        if !sync.is_initialized() {
            bail!("Sync cache not initialized. Run `crosslink sync` first.");
        }
        let cache_dir = sync.cache_path().to_path_buf();

        // Ensure directory structure exists
        std::fs::create_dir_all(cache_dir.join("issues"))?;
        std::fs::create_dir_all(cache_dir.join("meta").join("milestones"))?;

        Ok(Some(SharedWriter {
            sync,
            agent,
            cache_dir,
        }))
    }

    pub fn agent_id(&self) -> &str {
        &self.agent.agent_id
    }

    /// Create a new issue: generate UUID, claim display ID, write JSON, push, hydrate.
    ///
    /// Returns the assigned display ID.
    pub fn create_issue(
        &self,
        db: &Database,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let title_owned = title.to_string();
        let desc_owned = description.map(|s| s.to_string());
        let priority_owned = priority.to_string();
        let agent_id = self.agent.agent_id.clone();
        let display_id = Cell::new(0i64);

        let outcome = self.write_commit_push(
            |writer| {
                let (id, counters) = writer.claim_display_id(1)?;
                display_id.set(id);
                let issue = IssueFile {
                    uuid,
                    display_id: Some(id),
                    title: title_owned.clone(),
                    description: desc_owned.clone(),
                    status: "open".to_string(),
                    priority: priority_owned.clone(),
                    parent_uuid: None,
                    created_by: agent_id.clone(),
                    created_at: now,
                    updated_at: now,
                    closed_at: None,
                    labels: vec![],
                    comments: vec![],
                    blockers: vec![],
                    related: vec![],
                    milestone_uuid: None,
                    time_entries: vec![],
                };
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", uuid), json)],
                    counters: Some(counters),
                    use_git_rm: false,
                })
            },
            &format!("create issue: {}", title),
        )?;

        if outcome == PushOutcome::LocalOnly {
            self.rewrite_as_offline(uuid)?;
            hydrate_to_sqlite(&self.cache_dir, db)?;
            return db.get_issue_id_by_uuid(&uuid.to_string());
        }

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(display_id.get())
    }

    /// Create a subissue under a parent.
    ///
    /// Returns the assigned display ID for the child.
    pub fn create_subissue(
        &self,
        db: &Database,
        parent_id: i64,
        title: &str,
        description: Option<&str>,
        priority: &str,
    ) -> Result<i64> {
        let parent_uuid = self.resolve_uuid(parent_id, db)?;
        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let title_owned = title.to_string();
        let desc_owned = description.map(|s| s.to_string());
        let priority_owned = priority.to_string();
        let agent_id = self.agent.agent_id.clone();
        let display_id = Cell::new(0i64);

        let outcome = self.write_commit_push(
            |writer| {
                let (id, counters) = writer.claim_display_id(1)?;
                display_id.set(id);
                let issue = IssueFile {
                    uuid,
                    display_id: Some(id),
                    title: title_owned.clone(),
                    description: desc_owned.clone(),
                    status: "open".to_string(),
                    priority: priority_owned.clone(),
                    parent_uuid: Some(parent_uuid),
                    created_by: agent_id.clone(),
                    created_at: now,
                    updated_at: now,
                    closed_at: None,
                    labels: vec![],
                    comments: vec![],
                    blockers: vec![],
                    related: vec![],
                    milestone_uuid: None,
                    time_entries: vec![],
                };
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", uuid), json)],
                    counters: Some(counters),
                    use_git_rm: false,
                })
            },
            &format!("create subissue under #{}: {}", parent_id, title),
        )?;

        if outcome == PushOutcome::LocalOnly {
            self.rewrite_as_offline(uuid)?;
            hydrate_to_sqlite(&self.cache_dir, db)?;
            return db.get_issue_id_by_uuid(&uuid.to_string());
        }

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(display_id.get())
    }

    /// Update an issue's title, description, status, or priority.
    pub fn update_issue(
        &self,
        db: &Database,
        display_id: i64,
        title: Option<&str>,
        description: Option<Option<&str>>,
        status: Option<&str>,
        priority: Option<&str>,
    ) -> Result<()> {
        let title_owned = title.map(|s| s.to_string());
        let desc_owned = description.map(|d| d.map(|s| s.to_string()));
        let status_owned = status.map(|s| s.to_string());
        let priority_owned = priority.map(|s| s.to_string());

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if let Some(ref t) = title_owned {
                    issue.title = t.clone();
                }
                if let Some(ref d) = desc_owned {
                    issue.description = d.clone();
                }
                if let Some(ref s) = status_owned {
                    issue.status = s.clone();
                }
                if let Some(ref p) = priority_owned {
                    issue.priority = p.clone();
                }
                issue.updated_at = Utc::now();
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("update issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Close an issue (set status to "closed" and record closed_at).
    pub fn close_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                let now = Utc::now();
                issue.status = "closed".to_string();
                issue.closed_at = Some(now);
                issue.updated_at = now;
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("close issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Reopen an issue (set status to "open", clear closed_at).
    pub fn reopen_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                issue.status = "open".to_string();
                issue.closed_at = None;
                issue.updated_at = Utc::now();
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("reopen issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Delete an issue JSON file from the coordination branch.
    pub fn delete_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let issue = self.load_issue_by_id(display_id, db)?;
        let uuid = issue.uuid;
        let rel_path = format!("issues/{}.json", uuid);

        let _ = self.write_commit_push(
            |writer| {
                // Remove the file from disk (may already be gone on retry)
                let path = writer.issue_path(&uuid);
                if path.exists() {
                    std::fs::remove_file(&path)?;
                }
                // Include the path so the staging loop can `git rm` it
                Ok(WriteSet {
                    files: vec![(rel_path.clone(), vec![])],
                    counters: None,
                    use_git_rm: true,
                })
            },
            &format!("delete issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Add a comment to an issue.
    ///
    /// Returns the comment ID.
    pub fn add_comment(&self, db: &Database, display_id: i64, content: &str) -> Result<i64> {
        let content_owned = content.to_string();
        let agent_id = self.agent.agent_id.clone();
        let comment_id = Cell::new(0i64);

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                let mut counters = writer.read_counters()?;
                let id = counters.next_comment_id;
                counters.next_comment_id += 1;
                comment_id.set(id);

                issue.comments.push(CommentEntry {
                    id,
                    author: agent_id.clone(),
                    content: content_owned.clone(),
                    created_at: Utc::now(),
                });
                issue.updated_at = Utc::now();

                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: Some(counters),
                    use_git_rm: false,
                })
            },
            &format!("comment on issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(comment_id.get())
    }

    /// Add a label to an issue.
    pub fn add_label(&self, db: &Database, display_id: i64, label: &str) -> Result<()> {
        let label_owned = label.to_string();

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if !issue.labels.contains(&label_owned) {
                    issue.labels.push(label_owned.clone());
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("label issue #{} with {}", display_id, label),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Remove a label from an issue.
    pub fn remove_label(&self, db: &Database, display_id: i64, label: &str) -> Result<()> {
        let label_owned = label.to_string();

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(display_id, db)?;
                if let Some(pos) = issue.labels.iter().position(|l| l == &label_owned) {
                    issue.labels.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("unlabel {} from issue #{}", label, display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Add a blocker dependency: `blocked_id` is blocked by `blocker_id`.
    ///
    /// Only modifies the blocked issue's file (single-direction storage).
    pub fn add_blocker(&self, db: &Database, blocked_id: i64, blocker_id: i64) -> Result<()> {
        let blocker_uuid = self.resolve_uuid(blocker_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(blocked_id, db)?;
                if !issue.blockers.contains(&blocker_uuid) {
                    issue.blockers.push(blocker_uuid);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("block issue #{} on #{}", blocked_id, blocker_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Remove a blocker dependency.
    pub fn remove_blocker(&self, db: &Database, blocked_id: i64, blocker_id: i64) -> Result<()> {
        let blocker_uuid = self.resolve_uuid(blocker_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(blocked_id, db)?;
                if let Some(pos) = issue.blockers.iter().position(|u| u == &blocker_uuid) {
                    issue.blockers.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("unblock issue #{} from #{}", blocked_id, blocker_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Add a relation between two issues (single-direction storage).
    pub fn add_relation(&self, db: &Database, issue_id: i64, related_id: i64) -> Result<()> {
        let related_uuid = self.resolve_uuid(related_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if !issue.related.contains(&related_uuid) {
                    issue.related.push(related_uuid);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("relate issue #{} to #{}", issue_id, related_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Remove a relation between two issues.
    pub fn remove_relation(&self, db: &Database, issue_id: i64, related_id: i64) -> Result<()> {
        let related_uuid = self.resolve_uuid(related_id, db)?;

        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                if let Some(pos) = issue.related.iter().position(|u| u == &related_uuid) {
                    issue.related.remove(pos);
                    issue.updated_at = Utc::now();
                }
                let json = serde_json::to_vec_pretty(&issue)?;
                Ok(WriteSet {
                    files: vec![(format!("issues/{}.json", issue.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("unrelate issue #{} from #{}", issue_id, related_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Create a milestone on the coordination branch.
    ///
    /// Returns the assigned milestone display ID.
    pub fn create_milestone(
        &self,
        db: &Database,
        name: &str,
        description: Option<&str>,
    ) -> Result<i64> {
        let uuid = Uuid::new_v4();
        let now = Utc::now();
        let name_owned = name.to_string();
        let desc_owned = description.map(|s| s.to_string());
        let display_id = Cell::new(0i64);

        let _ = self.write_commit_push(
            |writer| {
                let (id, counters) = writer.claim_milestone_id()?;
                display_id.set(id);
                let entry = MilestoneEntry {
                    uuid,
                    display_id: id,
                    name: name_owned.clone(),
                    description: desc_owned.clone(),
                    status: "open".to_string(),
                    created_at: now,
                    closed_at: None,
                };
                let mut json = Vec::new();
                serde_json::to_writer_pretty(&mut json, &entry)?;
                Ok(WriteSet {
                    files: vec![(format!("meta/milestones/{}.json", uuid), json)],
                    counters: Some(counters),
                    use_git_rm: false,
                })
            },
            &format!("create milestone: {}", name),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(display_id.get())
    }

    /// Close a milestone on the coordination branch.
    pub fn close_milestone(&self, db: &Database, milestone_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut entry = writer.load_milestone_by_id(milestone_id)?;
                entry.status = "closed".to_string();
                entry.closed_at = Some(Utc::now());
                let mut json = Vec::new();
                serde_json::to_writer_pretty(&mut json, &entry)?;
                Ok(WriteSet {
                    files: vec![(format!("meta/milestones/{}.json", entry.uuid), json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("close milestone #{}", milestone_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Delete a milestone file from the coordination branch.
    pub fn delete_milestone(&self, db: &Database, milestone_id: i64) -> Result<()> {
        let entry = self.load_milestone_by_id(milestone_id)?;
        let rel_path = format!("meta/milestones/{}.json", entry.uuid);

        let _ = self.write_commit_push(
            |writer| {
                let path = writer
                    .cache_dir
                    .join("meta")
                    .join("milestones")
                    .join(format!("{}.json", entry.uuid));
                if path.exists() {
                    std::fs::remove_file(&path)?;
                }
                Ok(WriteSet {
                    files: vec![(rel_path.clone(), vec![])],
                    counters: None,
                    use_git_rm: true,
                })
            },
            &format!("delete milestone #{}", milestone_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Promote offline issues (`display_id: null`) to real display IDs.
    ///
    /// Called during sync when connectivity is restored. Scans the cache for
    /// issue files created by this agent with null display_id, bulk-claims
    /// N sequential IDs, rewrites the JSON files, and pushes.
    ///
    /// Returns a vec of `(old_negative_id, new_display_id, title)` for output.
    pub fn promote_offline_issues(&self, db: &Database) -> Result<Vec<(i64, i64, String)>> {
        let offline = self.find_offline_issues()?;
        if offline.is_empty() {
            return Ok(vec![]);
        }

        let count = offline.len() as i64;

        // Build uuid -> negative_id from current SQLite state
        let mut uuid_to_neg_id = std::collections::HashMap::new();
        for issue in &offline {
            if let Ok(neg_id) = db.get_issue_id_by_uuid(&issue.uuid.to_string()) {
                uuid_to_neg_id.insert(issue.uuid, neg_id);
            }
        }

        let offline_info: Vec<(Uuid, String)> =
            offline.iter().map(|i| (i.uuid, i.title.clone())).collect();

        let first_id = Cell::new(0i64);

        let outcome = self.write_commit_push(
            |writer| {
                let (start_id, counters) = writer.claim_display_id(count)?;
                first_id.set(start_id);

                let mut files = Vec::new();
                for (i, (uuid, _)) in offline_info.iter().enumerate() {
                    let path = writer.issue_path(uuid);
                    let mut issue = read_issue_file(&path)?;
                    issue.display_id = Some(start_id + i as i64);
                    let json = serde_json::to_vec_pretty(&issue)?;
                    files.push((format!("issues/{}.json", uuid), json));
                }

                Ok(WriteSet {
                    files,
                    counters: Some(counters),
                    use_git_rm: false,
                })
            },
            &format!("promote {} offline issue(s)", count),
        )?;

        if outcome == PushOutcome::LocalOnly {
            // Still offline — revert display_id assignments
            for (uuid, _) in &offline_info {
                let path = self.issue_path(uuid);
                if let Ok(mut issue) = read_issue_file(&path) {
                    issue.display_id = None;
                    if let Ok(json) = serde_json::to_string_pretty(&issue) {
                        let _ = std::fs::write(&path, json);
                    }
                }
            }
            // Revert counter
            if let Ok(mut counters) = self.read_counters() {
                counters.next_display_id -= count;
                let _ = self.write_counters_to_cache(&counters);
            }
            // Amend the commit to reflect reverted state
            let _ = self.git_in_cache(&["add", "."]);
            let _ = self.git_in_cache(&["commit", "--amend", "--no-edit"]);
            return Ok(vec![]);
        }

        // Re-hydrate with new positive IDs
        hydrate_to_sqlite(&self.cache_dir, db)?;

        let start_id = first_id.get();
        let mapping: Vec<(i64, i64, String)> = offline_info
            .iter()
            .enumerate()
            .map(|(i, (uuid, title))| {
                let neg_id = uuid_to_neg_id.get(uuid).copied().unwrap_or(0);
                let new_id = start_id + i as i64;
                (neg_id, new_id, title.clone())
            })
            .collect();

        Ok(mapping)
    }

    /// Rewrite `Lx` references in comments, descriptions, and session notes
    /// after offline issues have been promoted to real display IDs.
    ///
    /// Returns stats on how many text fields were updated.
    pub fn rewrite_local_references(
        &self,
        db: &Database,
        mapping: &[(i64, i64, String)],
    ) -> Result<RewriteStats> {
        if mapping.is_empty() {
            return Ok(RewriteStats::default());
        }

        // Build replacement map: "L1" → "#5", "L2" → "#6"
        let replacements: Vec<(String, String)> = mapping
            .iter()
            .filter(|(neg_id, _, _)| *neg_id != 0)
            .map(|(neg_id, new_id, _)| {
                (
                    format!("L{}", neg_id.unsigned_abs()),
                    format!("#{}", new_id),
                )
            })
            .collect();

        if replacements.is_empty() {
            return Ok(RewriteStats::default());
        }

        let mut stats = RewriteStats::default();

        // 1. Rewrite comments and descriptions in JSON files + SQLite
        let mut json_changed = false;
        for (_, new_id, _) in mapping {
            // Update comments in SQLite
            let comments = db.get_comments(*new_id)?;
            for comment in &comments {
                if let Some(new_content) = replace_local_refs(&comment.content, &replacements) {
                    db.update_comment_content(comment.id, &new_content)?;
                    stats.comments_updated += 1;
                }
            }

            // Update description in SQLite
            if let Ok(Some(issue)) = db.get_issue(*new_id) {
                if let Some(ref desc) = issue.description {
                    if let Some(new_desc) = replace_local_refs(desc, &replacements) {
                        db.update_issue(*new_id, None, Some(&new_desc), None)?;
                        stats.descriptions_updated += 1;
                    }
                }
            }
        }

        // Update JSON files on coordination branch
        for (_, new_id, _) in mapping {
            let issue_file = match self.load_issue_by_display_id(*new_id) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let mut changed = false;
            let mut updated_issue = issue_file.clone();

            // Rewrite comments in JSON
            for comment in &mut updated_issue.comments {
                if let Some(new_content) = replace_local_refs(&comment.content, &replacements) {
                    comment.content = new_content;
                    changed = true;
                }
            }

            // Rewrite description in JSON
            if let Some(ref desc) = updated_issue.description {
                if let Some(new_desc) = replace_local_refs(desc, &replacements) {
                    updated_issue.description = Some(new_desc);
                    changed = true;
                }
            }

            if changed {
                let json = serde_json::to_string_pretty(&updated_issue)?;
                let path = self.issue_path(&updated_issue.uuid);
                std::fs::write(&path, json)?;
                json_changed = true;
            }
        }

        // Commit JSON changes if any
        if json_changed {
            let _ = self.git_in_cache(&["add", "issues/"]);
            let _ = self.git_in_cache(&[
                "commit",
                "-m",
                &format!(
                    "{}: rewrite local references after promotion",
                    self.agent.agent_id
                ),
            ]);
            // Best-effort push
            let _ = self.git_in_cache(&["push", "origin", crate::sync::HUB_BRANCH]);
        }

        // 2. Rewrite session notes in SQLite
        let sessions = db.get_all_sessions_with_notes()?;
        for session in &sessions {
            if let Some(ref notes) = session.handoff_notes {
                if let Some(new_notes) = replace_local_refs(notes, &replacements) {
                    db.update_session_notes(session.id, &new_notes)?;
                    stats.sessions_updated += 1;
                }
            }
        }

        Ok(stats)
    }

    // ───────────────────── Private helpers ─────────────────────

    /// Find all issue files in the cache with `display_id: null` created by this agent.
    fn find_offline_issues(&self) -> Result<Vec<IssueFile>> {
        let issues_dir = self.cache_dir.join("issues");
        let mut offline = Vec::new();
        if !issues_dir.exists() {
            return Ok(offline);
        }
        for entry in std::fs::read_dir(&issues_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(issue) = read_issue_file(&path) {
                if issue.display_id.is_none() && issue.created_by == self.agent.agent_id {
                    offline.push(issue);
                }
            }
        }
        // Sort by created_at for deterministic ID assignment
        offline.sort_by_key(|i| i.created_at);
        Ok(offline)
    }

    /// Claim N sequential display IDs from `meta/counters.json`.
    ///
    /// Returns `(first_claimed_id, updated_counters)`.
    fn claim_display_id(&self, count: i64) -> Result<(i64, Counters)> {
        let mut counters = self.read_counters()?;
        let first = counters.next_display_id;
        counters.next_display_id += count;
        Ok((first, counters))
    }

    /// Claim a milestone display ID from `meta/counters.json`.
    ///
    /// Returns `(claimed_id, updated_counters)`.
    fn claim_milestone_id(&self) -> Result<(i64, Counters)> {
        let mut counters = self.read_counters()?;
        let id = counters.next_milestone_id;
        counters.next_milestone_id += 1;
        Ok((id, counters))
    }

    /// Load a milestone entry by display_id from per-file storage.
    fn load_milestone_by_id(&self, display_id: i64) -> Result<MilestoneEntry> {
        let milestones_dir = self.cache_dir.join("meta").join("milestones");
        if milestones_dir.exists() {
            for entry in std::fs::read_dir(&milestones_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Ok(ms) = read_milestone_file(&path) {
                    if ms.display_id == display_id {
                        return Ok(ms);
                    }
                }
            }
        }
        bail!("Milestone #{} not found in shared cache", display_id)
    }

    /// Rewrite a just-committed issue to set `display_id: null` and revert the
    /// counter bump. Used when a push failed (offline/exhausted retries) so the
    /// locally-claimed display ID doesn't collide with remote state.
    fn rewrite_as_offline(&self, uuid: Uuid) -> Result<()> {
        let path = self.issue_path(&uuid);
        let mut issue = read_issue_file(&path)?;
        issue.display_id = None;
        let json = serde_json::to_string_pretty(&issue)?;
        std::fs::write(&path, json)?;

        // Revert the counter bump (the remote never saw it)
        let mut counters = self.read_counters()?;
        if counters.next_display_id > 1 {
            counters.next_display_id -= 1;
        }
        self.write_counters_to_cache(&counters)?;

        // Amend the local commit with the reverted files
        self.git_in_cache(&["add", &format!("issues/{}.json", uuid)])?;
        self.git_in_cache(&["add", "meta/counters.json"])?;
        self.git_in_cache(&["commit", "--amend", "--no-edit"])?;
        Ok(())
    }

    /// Read counters from the cache.
    fn read_counters(&self) -> Result<Counters> {
        let path = self.cache_dir.join("meta").join("counters.json");
        read_counters(&path)
    }

    /// Write counters to the cache.
    fn write_counters_to_cache(&self, counters: &Counters) -> Result<()> {
        let path = self.cache_dir.join("meta").join("counters.json");
        write_counters(&path, counters)
    }

    /// Path to an issue JSON file in the cache.
    fn issue_path(&self, uuid: &Uuid) -> PathBuf {
        self.cache_dir.join("issues").join(format!("{}.json", uuid))
    }

    /// Load an issue JSON file by its display ID.
    ///
    /// Scans the issues directory for a file matching the display ID.
    fn load_issue_by_display_id(&self, display_id: i64) -> Result<IssueFile> {
        let issues_dir = self.cache_dir.join("issues");
        for entry in std::fs::read_dir(&issues_dir)
            .with_context(|| format!("Cannot read issues dir: {}", issues_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(issue) = read_issue_file(&path) {
                if issue.display_id == Some(display_id) {
                    return Ok(issue);
                }
            }
        }
        bail!("Issue #{} not found in shared cache", display_id)
    }

    /// Load an issue by ID, supporting both positive (real) and negative (offline) IDs.
    ///
    /// For negative IDs, consults SQLite to resolve the UUID first.
    fn load_issue_by_id(&self, id: i64, db: &Database) -> Result<IssueFile> {
        if id >= 0 {
            self.load_issue_by_display_id(id)
        } else {
            let uuid_str = db.get_issue_uuid_by_id(id)?;
            let uuid: Uuid = uuid_str
                .parse()
                .with_context(|| format!("Invalid UUID for local issue L{}", id.unsigned_abs()))?;
            read_issue_file(&self.issue_path(&uuid))
        }
    }

    /// Resolve an issue ID (positive or negative) to its UUID.
    ///
    /// For positive IDs, scans issue files by display_id.
    /// For negative IDs, looks up the UUID from SQLite.
    fn resolve_uuid(&self, id: i64, db: &Database) -> Result<Uuid> {
        if id >= 0 {
            let issue = self.load_issue_by_display_id(id)?;
            Ok(issue.uuid)
        } else {
            let uuid_str = db.get_issue_uuid_by_id(id)?;
            uuid_str
                .parse()
                .with_context(|| format!("Invalid UUID for local issue L{}", id.unsigned_abs()))
        }
    }

    /// Generate content, commit, and push with retry.
    ///
    /// The `prepare` closure is called on **every** attempt, so it must
    /// re-read any mutable state (counters, issue files) from the cache
    /// which may have changed after a rebase pull.  This prevents stale
    /// display-ID collisions when two agents race.
    fn write_commit_push<F>(&self, mut prepare: F, message: &str) -> Result<PushOutcome>
    where
        F: FnMut(&Self) -> Result<WriteSet>,
    {
        for attempt in 0..MAX_RETRIES {
            // (Re-)generate content — reads fresh counters/files after rebase
            let write_set = prepare(self)?;

            // Write files to cache (skip for deletions — files already removed)
            if !write_set.use_git_rm {
                for (rel_path, content) in &write_set.files {
                    let full = self.cache_dir.join(rel_path);
                    if let Some(parent) = full.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&full, content)?;
                }
            }
            if let Some(ref c) = write_set.counters {
                self.write_counters_to_cache(c)?;
            }

            // Collect relative paths for staging
            let mut paths: Vec<String> = write_set.files.iter().map(|(p, _)| p.clone()).collect();
            if write_set.counters.is_some() {
                paths.push("meta/counters.json".to_string());
            }

            // Stage
            for path in &paths {
                if write_set.use_git_rm {
                    let _ = self.git_in_cache(&["rm", "--cached", "--ignore-unmatch", path]);
                } else {
                    self.git_in_cache(&["add", path])?;
                }
            }

            // Commit
            let commit_msg = format!(
                "{}: {} at {}",
                self.agent.agent_id,
                message,
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
            );
            let commit_result = self.git_in_cache(&["commit", "-m", &commit_msg]);
            if let Err(e) = &commit_result {
                let err_str = e.to_string();
                if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                    return Ok(PushOutcome::Pushed);
                }
                commit_result?;
            }

            // Push
            let push_result = self.git_in_cache(&["push", "origin", crate::sync::HUB_BRANCH]);
            match push_result {
                Ok(_) => return Ok(PushOutcome::Pushed),
                Err(e) => {
                    let err_str = e.to_string();
                    // Offline — commit is local, will push on next sync
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        return Ok(PushOutcome::LocalOnly);
                    }
                    // Conflict — reset our commit, pull latest, then retry
                    // (the closure will re-read fresh state on the next iteration)
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < MAX_RETRIES - 1 {
                            let _ = self.git_in_cache(&["reset", "HEAD~1"]);
                            self.git_in_cache(&[
                                "pull",
                                "--rebase",
                                "origin",
                                crate::sync::HUB_BRANCH,
                            ])?;
                            continue;
                        }
                        // All retries exhausted — keep as local-only
                        return Ok(PushOutcome::LocalOnly);
                    }
                    // Other error — propagate
                    return Err(e);
                }
            }
        }
        Ok(PushOutcome::Pushed)
    }

    /// Run a git command in the cache worktree.
    fn git_in_cache(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = std::process::Command::new("git")
            .current_dir(&self.cache_dir)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {:?} in cache", args))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {:?} in cache failed: {}", args, stderr);
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue_file::{write_issue_file, IssueFile};
    use chrono::Utc;
    use tempfile::tempdir;

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

    #[test]
    fn test_new_returns_none_without_agent_config() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let writer = SharedWriter::new(&crosslink_dir).unwrap();
        assert!(writer.is_none());
    }

    #[test]
    fn test_claim_display_id() {
        // Test the counter logic directly using file I/O
        let dir = tempdir().unwrap();
        let meta_dir = dir.path().join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();

        let counters_path = meta_dir.join("counters.json");

        // Start from defaults
        let counters = read_counters(&counters_path).unwrap();
        assert_eq!(counters.next_display_id, 1);

        // Claim 1 ID
        let first = counters.next_display_id;
        let mut updated = counters;
        updated.next_display_id += 1;
        write_counters(&counters_path, &updated).unwrap();

        assert_eq!(first, 1);

        // Claim another
        let counters = read_counters(&counters_path).unwrap();
        assert_eq!(counters.next_display_id, 2);
    }

    #[test]
    fn test_load_issue_by_display_id() {
        let dir = tempdir().unwrap();
        let issues_dir = dir.path().join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();

        let issue1 = make_issue(1, "First");
        let issue2 = make_issue(2, "Second");
        write_issue_file(&issues_dir.join(format!("{}.json", issue1.uuid)), &issue1).unwrap();
        write_issue_file(&issues_dir.join(format!("{}.json", issue2.uuid)), &issue2).unwrap();

        // Simulate the scan logic
        let found = scan_for_display_id(&issues_dir, 2).unwrap();
        assert_eq!(found.title, "Second");
        assert_eq!(found.uuid, issue2.uuid);
    }

    #[test]
    fn test_load_issue_by_display_id_not_found() {
        let dir = tempdir().unwrap();
        let issues_dir = dir.path().join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();

        let result = scan_for_display_id(&issues_dir, 99);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_uuid_from_files() {
        let dir = tempdir().unwrap();
        let issues_dir = dir.path().join("issues");
        std::fs::create_dir_all(&issues_dir).unwrap();

        let issue = make_issue(42, "Target");
        write_issue_file(&issues_dir.join(format!("{}.json", issue.uuid)), &issue).unwrap();

        let found = scan_for_display_id(&issues_dir, 42).unwrap();
        assert_eq!(found.uuid, issue.uuid);
    }

    #[test]
    fn test_counters_sequential_claim() {
        let dir = tempdir().unwrap();
        let meta_dir = dir.path().join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        let path = meta_dir.join("counters.json");

        // Claim 3 sequential IDs
        let mut counters = read_counters(&path).unwrap();
        let ids: Vec<i64> = (0..3)
            .map(|_| {
                let id = counters.next_display_id;
                counters.next_display_id += 1;
                id
            })
            .collect();

        write_counters(&path, &counters).unwrap();

        assert_eq!(ids, vec![1, 2, 3]);
        let reloaded = read_counters(&path).unwrap();
        assert_eq!(reloaded.next_display_id, 4);
    }

    #[test]
    fn test_replace_local_refs_basic() {
        let replacements = vec![
            ("L1".to_string(), "#5".to_string()),
            ("L2".to_string(), "#6".to_string()),
        ];
        let result = replace_local_refs("See L1 and L2 for details", &replacements);
        assert_eq!(result, Some("See #5 and #6 for details".to_string()));
    }

    #[test]
    fn test_replace_local_refs_no_match() {
        let replacements = vec![("L1".to_string(), "#5".to_string())];
        let result = replace_local_refs("No local refs here", &replacements);
        assert!(result.is_none());
    }

    #[test]
    fn test_replace_local_refs_non_matching_id() {
        let replacements = vec![("L1".to_string(), "#5".to_string())];
        let result = replace_local_refs("See L99 for info", &replacements);
        assert!(result.is_none());
    }

    #[test]
    fn test_replace_local_refs_word_boundary() {
        let replacements = vec![("L1".to_string(), "#5".to_string())];
        // "FILE1" should NOT be rewritten (L1 is preceded by alphanumeric)
        let result = replace_local_refs("Check FILE1 now", &replacements);
        assert!(result.is_none());

        // "L1." should be rewritten (punctuation after is ok)
        let result = replace_local_refs("Fixed L1.", &replacements);
        assert_eq!(result, Some("Fixed #5.".to_string()));

        // "L1," in a list
        let result = replace_local_refs(
            "L1, L2 are done",
            &[
                ("L1".to_string(), "#5".to_string()),
                ("L2".to_string(), "#6".to_string()),
            ],
        );
        assert_eq!(result, Some("#5, #6 are done".to_string()));
    }

    #[test]
    fn test_replace_local_refs_start_end() {
        let replacements = vec![("L1".to_string(), "#5".to_string())];
        // At start of string
        let result = replace_local_refs("L1 is done", &replacements);
        assert_eq!(result, Some("#5 is done".to_string()));

        // At end of string
        let result = replace_local_refs("Working on L1", &replacements);
        assert_eq!(result, Some("Working on #5".to_string()));

        // Entire string
        let result = replace_local_refs("L1", &replacements);
        assert_eq!(result, Some("#5".to_string()));
    }

    /// Helper for tests: scan issues dir for a display_id (mirrors SharedWriter logic).
    fn scan_for_display_id(issues_dir: &Path, display_id: i64) -> Result<IssueFile> {
        for entry in std::fs::read_dir(issues_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(issue) = read_issue_file(&path) {
                if issue.display_id == Some(display_id) {
                    return Ok(issue);
                }
            }
        }
        bail!("Issue #{} not found", display_id)
    }
}
