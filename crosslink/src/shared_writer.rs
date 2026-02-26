//! Write-side operations for multi-agent shared issue tracking.
//!
//! `SharedWriter` wraps a `SyncManager` and `AgentConfig` to provide
//! write operations that persist issue data as JSON on the coordination
//! branch and then update local SQLite. In single-agent mode (no
//! `agent.json`), `SharedWriter::new()` returns `None` and all commands
//! fall back to direct `Database` writes.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::db::Database;
use crate::hydration::hydrate_to_sqlite;
use crate::identity::AgentConfig;
use crate::issue_file::{
    read_counters, read_issue_file, write_counters, write_issue_file, CommentEntry, Counters,
    IssueFile,
};
use crate::sync::SyncManager;

/// Maximum number of push retries on conflict before giving up.
const MAX_RETRIES: usize = 3;

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
        std::fs::create_dir_all(cache_dir.join("meta"))?;

        Ok(Some(SharedWriter {
            sync,
            agent,
            cache_dir,
        }))
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

        let (display_id, counters) = self.claim_display_id(1)?;

        let issue = IssueFile {
            uuid,
            display_id: Some(display_id),
            title: title.to_string(),
            description: description.map(|s| s.to_string()),
            status: "open".to_string(),
            priority: priority.to_string(),
            parent_uuid: None,
            created_by: self.agent.agent_id.clone(),
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

        self.write_and_push(
            &[(&issue, true)],
            Some(&counters),
            &format!("create issue #{}: {}", display_id, title),
        )?;

        // Update local SQLite
        hydrate_to_sqlite(&self.cache_dir, db)?;

        Ok(display_id)
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
        let parent_uuid = self.resolve_uuid(parent_id)?;
        let uuid = Uuid::new_v4();
        let now = Utc::now();

        let (display_id, counters) = self.claim_display_id(1)?;

        let issue = IssueFile {
            uuid,
            display_id: Some(display_id),
            title: title.to_string(),
            description: description.map(|s| s.to_string()),
            status: "open".to_string(),
            priority: priority.to_string(),
            parent_uuid: Some(parent_uuid),
            created_by: self.agent.agent_id.clone(),
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

        self.write_and_push(
            &[(&issue, true)],
            Some(&counters),
            &format!("create subissue #{} under #{}", display_id, parent_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(display_id)
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
        let mut issue = self.load_issue_by_display_id(display_id)?;

        if let Some(t) = title {
            issue.title = t.to_string();
        }
        if let Some(d) = description {
            issue.description = d.map(|s| s.to_string());
        }
        if let Some(s) = status {
            issue.status = s.to_string();
        }
        if let Some(p) = priority {
            issue.priority = p.to_string();
        }
        issue.updated_at = Utc::now();

        self.write_and_push(
            &[(&issue, false)],
            None,
            &format!("update issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Close an issue (set status to "closed" and record closed_at).
    pub fn close_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let mut issue = self.load_issue_by_display_id(display_id)?;
        let now = Utc::now();
        issue.status = "closed".to_string();
        issue.closed_at = Some(now);
        issue.updated_at = now;

        self.write_and_push(
            &[(&issue, false)],
            None,
            &format!("close issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Reopen an issue (set status to "open", clear closed_at).
    pub fn reopen_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let mut issue = self.load_issue_by_display_id(display_id)?;
        issue.status = "open".to_string();
        issue.closed_at = None;
        issue.updated_at = Utc::now();

        self.write_and_push(
            &[(&issue, false)],
            None,
            &format!("reopen issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Delete an issue JSON file from the coordination branch.
    pub fn delete_issue(&self, db: &Database, display_id: i64) -> Result<()> {
        let issue = self.load_issue_by_display_id(display_id)?;
        let path = self.issue_path(&issue.uuid);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }

        let filename = format!("issues/{}.json", issue.uuid);
        self.commit_and_push(&[&filename], true, &format!("delete issue #{}", display_id))?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Add a comment to an issue.
    ///
    /// Returns the comment ID.
    pub fn add_comment(&self, db: &Database, display_id: i64, content: &str) -> Result<i64> {
        let mut issue = self.load_issue_by_display_id(display_id)?;
        let mut counters = self.read_counters()?;

        let comment_id = counters.next_comment_id;
        counters.next_comment_id += 1;

        issue.comments.push(CommentEntry {
            id: comment_id,
            author: self.agent.agent_id.clone(),
            content: content.to_string(),
            created_at: Utc::now(),
        });
        issue.updated_at = Utc::now();

        self.write_and_push(
            &[(&issue, false)],
            Some(&counters),
            &format!("comment on issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(comment_id)
    }

    /// Add a label to an issue.
    pub fn add_label(&self, db: &Database, display_id: i64, label: &str) -> Result<()> {
        let mut issue = self.load_issue_by_display_id(display_id)?;
        if !issue.labels.contains(&label.to_string()) {
            issue.labels.push(label.to_string());
            issue.updated_at = Utc::now();

            self.write_and_push(
                &[(&issue, false)],
                None,
                &format!("label issue #{} with {}", display_id, label),
            )?;

            hydrate_to_sqlite(&self.cache_dir, db)?;
        }
        Ok(())
    }

    /// Remove a label from an issue.
    pub fn remove_label(&self, db: &Database, display_id: i64, label: &str) -> Result<()> {
        let mut issue = self.load_issue_by_display_id(display_id)?;
        let label_str = label.to_string();
        if let Some(pos) = issue.labels.iter().position(|l| l == &label_str) {
            issue.labels.remove(pos);
            issue.updated_at = Utc::now();

            self.write_and_push(
                &[(&issue, false)],
                None,
                &format!("unlabel {} from issue #{}", label, display_id),
            )?;

            hydrate_to_sqlite(&self.cache_dir, db)?;
        }
        Ok(())
    }

    /// Add a blocker dependency: `blocked_id` is blocked by `blocker_id`.
    ///
    /// Only modifies the blocked issue's file (single-direction storage).
    pub fn add_blocker(&self, db: &Database, blocked_id: i64, blocker_id: i64) -> Result<()> {
        let blocker_uuid = self.resolve_uuid(blocker_id)?;
        let mut issue = self.load_issue_by_display_id(blocked_id)?;

        if !issue.blockers.contains(&blocker_uuid) {
            issue.blockers.push(blocker_uuid);
            issue.updated_at = Utc::now();

            self.write_and_push(
                &[(&issue, false)],
                None,
                &format!("block issue #{} on #{}", blocked_id, blocker_id),
            )?;

            hydrate_to_sqlite(&self.cache_dir, db)?;
        }
        Ok(())
    }

    /// Remove a blocker dependency.
    pub fn remove_blocker(&self, db: &Database, blocked_id: i64, blocker_id: i64) -> Result<()> {
        let blocker_uuid = self.resolve_uuid(blocker_id)?;
        let mut issue = self.load_issue_by_display_id(blocked_id)?;

        if let Some(pos) = issue.blockers.iter().position(|u| u == &blocker_uuid) {
            issue.blockers.remove(pos);
            issue.updated_at = Utc::now();

            self.write_and_push(
                &[(&issue, false)],
                None,
                &format!("unblock issue #{} from #{}", blocked_id, blocker_id),
            )?;

            hydrate_to_sqlite(&self.cache_dir, db)?;
        }
        Ok(())
    }

    /// Add a relation between two issues (single-direction storage).
    pub fn add_relation(&self, db: &Database, issue_id: i64, related_id: i64) -> Result<()> {
        let related_uuid = self.resolve_uuid(related_id)?;
        let mut issue = self.load_issue_by_display_id(issue_id)?;

        if !issue.related.contains(&related_uuid) {
            issue.related.push(related_uuid);
            issue.updated_at = Utc::now();

            self.write_and_push(
                &[(&issue, false)],
                None,
                &format!("relate issue #{} to #{}", issue_id, related_id),
            )?;

            hydrate_to_sqlite(&self.cache_dir, db)?;
        }
        Ok(())
    }

    /// Remove a relation between two issues.
    pub fn remove_relation(&self, db: &Database, issue_id: i64, related_id: i64) -> Result<()> {
        let related_uuid = self.resolve_uuid(related_id)?;
        let mut issue = self.load_issue_by_display_id(issue_id)?;

        if let Some(pos) = issue.related.iter().position(|u| u == &related_uuid) {
            issue.related.remove(pos);
            issue.updated_at = Utc::now();

            self.write_and_push(
                &[(&issue, false)],
                None,
                &format!("unrelate issue #{} from #{}", issue_id, related_id),
            )?;

            hydrate_to_sqlite(&self.cache_dir, db)?;
        }
        Ok(())
    }

    // ───────────────────── Private helpers ─────────────────────

    /// Claim N sequential display IDs from `meta/counters.json`.
    ///
    /// Returns `(first_claimed_id, updated_counters)`.
    fn claim_display_id(&self, count: i64) -> Result<(i64, Counters)> {
        let mut counters = self.read_counters()?;
        let first = counters.next_display_id;
        counters.next_display_id += count;
        Ok((first, counters))
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

    /// Resolve a display ID to its UUID by scanning the issue files.
    fn resolve_uuid(&self, display_id: i64) -> Result<Uuid> {
        let issue = self.load_issue_by_display_id(display_id)?;
        Ok(issue.uuid)
    }

    /// Write issue files and counters, then commit and push with retry.
    ///
    /// `issues` is a slice of `(issue, is_new)` pairs. `is_new` is only
    /// used for the commit message.
    fn write_and_push(
        &self,
        issues: &[(&IssueFile, bool)],
        counters: Option<&Counters>,
        message: &str,
    ) -> Result<()> {
        // Write files
        for (issue, _is_new) in issues {
            let path = self.issue_path(&issue.uuid);
            write_issue_file(&path, issue)?;
        }
        if let Some(c) = counters {
            self.write_counters_to_cache(c)?;
        }

        // Collect paths to stage
        let mut paths: Vec<String> = issues
            .iter()
            .map(|(issue, _)| format!("issues/{}.json", issue.uuid))
            .collect();
        if counters.is_some() {
            paths.push("meta/counters.json".to_string());
        }

        let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
        self.commit_and_push(&path_refs, false, message)
    }

    /// Stage files, commit, and push with rebase-retry.
    ///
    /// If `use_git_rm` is true, stages removals instead of additions.
    fn commit_and_push(&self, paths: &[&str], use_git_rm: bool, message: &str) -> Result<()> {
        for attempt in 0..MAX_RETRIES {
            // Stage files
            for path in paths {
                if use_git_rm {
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
                    return Ok(());
                }
                commit_result?;
            }

            // Push
            let push_result = self.git_in_cache(&["push", "origin", "crosslink/locks"]);
            match push_result {
                Ok(_) => return Ok(()),
                Err(e) => {
                    let err_str = e.to_string();
                    // Offline — commit is local, will push on next sync
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        return Ok(());
                    }
                    // Conflict — rebase and retry
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < MAX_RETRIES - 1 {
                            self.git_in_cache(&["pull", "--rebase", "origin", "crosslink/locks"])?;
                            // Re-read counter if it was part of this operation
                            // (caller will re-claim on next attempt via full retry)
                            continue;
                        }
                        bail!(
                            "Push failed after {} retries. Another agent may be rapidly writing.",
                            MAX_RETRIES
                        );
                    }
                    // Other error — propagate
                    return Err(e);
                }
            }
        }
        Ok(())
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
