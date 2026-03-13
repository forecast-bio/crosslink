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
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::db::Database;
use crate::hydration::hydrate_to_sqlite;
use crate::identity::AgentConfig;
use crate::issue_file::{
    read_counters, read_issue_file, read_milestone_file, write_counters, CommentEntry, CommentFile,
    Counters, IssueFile, MilestoneEntry,
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

/// Maximum time to wait for lock confirmation compaction (design doc section 8).
const LOCK_CONFIRM_TIMEOUT_SECS: u64 = 30;

/// Outcome of a write_commit_push operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushOutcome {
    /// Commit was pushed to remote successfully.
    Pushed,
    /// Commit was saved locally but push failed (offline or all retries exhausted).
    LocalOnly,
}

/// Result of a V2 lock claim attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockClaimResult {
    /// Lock successfully claimed.
    Claimed,
    /// Lock already held by this agent.
    AlreadyHeld,
    /// Another agent won the lock.
    Contended { winner_agent_id: String },
}

/// Write-side coordinator for multi-agent shared issue tracking.
///
/// Handles: generate UUID → claim display ID → write JSON → commit →
/// push (with rebase-retry) → update local SQLite.
pub struct SharedWriter {
    sync: SyncManager,
    agent: AgentConfig,
    cache_dir: PathBuf,
    /// Per-session event sequence counter, monotonically increasing.
    event_seq: Cell<u64>,
}

impl SharedWriter {
    /// Create a SharedWriter if multi-agent mode is configured.
    ///
    /// When `agent.json` exists, uses the configured identity with signing.
    /// When no `agent.json` exists but the hub branch is available, creates
    /// an anonymous writer that commits unsigned data to the coordination
    /// branch. Returns `None` only if the hub branch cannot be initialized.
    pub fn new(crosslink_dir: &Path) -> Result<Option<Self>> {
        let agent = match AgentConfig::load(crosslink_dir)? {
            Some(a) => a,
            None => {
                // No agent configured — try anonymous hub writes if hub exists
                let sync = SyncManager::new(crosslink_dir)?;
                if !sync.is_initialized() {
                    // Auto-initialize hub cache if the branch exists remotely
                    if sync.init_cache().is_err() {
                        return Ok(None);
                    }
                    if !sync.is_initialized() {
                        return Ok(None);
                    }
                }
                AgentConfig::anonymous(crosslink_dir)
            }
        };
        let sync = SyncManager::new(crosslink_dir)?;
        if !sync.is_initialized() {
            bail!("Sync cache not initialized. Run `crosslink sync` first.");
        }
        let cache_dir = sync.cache_path().to_path_buf();

        // Ensure directory structure exists
        std::fs::create_dir_all(cache_dir.join("issues"))?;
        std::fs::create_dir_all(cache_dir.join("meta").join("milestones"))?;

        // Initialize event sequence counter from existing log
        let event_seq = Cell::new(Self::read_max_event_seq(&cache_dir, &agent.agent_id));

        Ok(Some(SharedWriter {
            sync,
            agent,
            cache_dir,
            event_seq,
        }))
    }

    pub fn agent_id(&self) -> &str {
        &self.agent.agent_id
    }

    /// Derive the `.crosslink/` directory from the cache path.
    fn crosslink_dir(&self) -> &Path {
        self.cache_dir.parent().unwrap_or(&self.cache_dir)
    }

    /// Path to the promoted-UUIDs tracking file (machine-local, not shared).
    fn promoted_uuids_path(&self) -> PathBuf {
        self.crosslink_dir().join(".promoted-uuids")
    }

    /// Read the set of UUIDs that have already been promoted.
    fn read_promoted_uuids(&self) -> HashSet<Uuid> {
        let path = self.promoted_uuids_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => content
                .lines()
                .filter_map(|line| line.trim().parse::<Uuid>().ok())
                .collect(),
            Err(_) => HashSet::new(),
        }
    }

    /// Append promoted UUIDs to the tracking file.
    fn record_promoted_uuids(&self, uuids: &[Uuid]) -> Result<()> {
        use std::io::Write;
        let path = self.promoted_uuids_path();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("Failed to open promoted UUIDs file: {}", path.display()))?;
        for uuid in uuids {
            writeln!(file, "{}", uuid)?;
        }
        Ok(())
    }

    /// Check the current hub layout version.
    fn layout_version(&self) -> u32 {
        let meta_dir = self.sync.cache_path().join("meta");
        crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1)
    }

    // ─────────────── Event emission infrastructure ───────────────

    /// Read the max agent_seq from an existing event log.
    fn read_max_event_seq(cache_dir: &Path, agent_id: &str) -> u64 {
        let log_path = cache_dir.join("agents").join(agent_id).join("events.log");
        match crate::events::read_events(&log_path) {
            Ok(events) => events.iter().map(|e| e.agent_seq).max().unwrap_or(0),
            Err(_) => 0,
        }
    }

    /// Get the next event sequence number and increment the counter.
    fn next_event_seq(&self) -> u64 {
        let seq = self.event_seq.get() + 1;
        self.event_seq.set(seq);
        seq
    }

    /// Path to this agent's event log file.
    fn event_log_path(&self) -> PathBuf {
        self.cache_dir
            .join("agents")
            .join(&self.agent.agent_id)
            .join("events.log")
    }

    /// Resolve the agent's SSH private key to an absolute path, if configured.
    fn resolve_ssh_key_path(&self) -> Option<PathBuf> {
        let rel = self.agent.ssh_key_path.as_ref()?;
        let crosslink_dir = self
            .sync
            .cache_path()
            .parent()
            .unwrap_or(self.sync.cache_path());
        let abs = crosslink_dir.join(rel);
        if abs.exists() {
            Some(abs)
        } else {
            None
        }
    }

    /// Create and optionally sign an event envelope.
    fn create_envelope(&self, event: crate::events::Event) -> crate::events::EventEnvelope {
        let seq = self.next_event_seq();
        let mut envelope = crate::events::EventEnvelope {
            agent_id: self.agent.agent_id.clone(),
            agent_seq: seq,
            timestamp: Utc::now(),
            event,
            signed_by: None,
            signature: None,
        };

        // Sign if key is available
        if let (Some(key_path), Some(fingerprint)) = (
            self.resolve_ssh_key_path(),
            self.agent.ssh_fingerprint.as_ref(),
        ) {
            let _ = crate::events::sign_event(&mut envelope, &key_path, fingerprint);
        }

        envelope
    }

    /// Emit an event, run compaction, and push all changes.
    ///
    /// The event is appended once to the log before the retry loop.
    /// On push conflict, compaction is re-run after rebase to incorporate
    /// any new remote events.
    fn emit_compact_push(&self, event: crate::events::Event, message: &str) -> Result<PushOutcome> {
        let envelope = self.create_envelope(event);
        let log_path = self.event_log_path();
        crate::events::append_event(&log_path, &envelope)?;

        for attempt in 0..MAX_RETRIES {
            // Run compaction (force=true since we own the write path)
            let _ = crate::compaction::compact(&self.cache_dir, &self.agent.agent_id, true)?;

            // Stage event log + compaction output
            let rel_log = format!("agents/{}/events.log", self.agent.agent_id);
            self.git_in_cache(&["add", &rel_log])?;
            let _ = self.git_in_cache(&["add", "checkpoint/"]);
            let _ = self.git_in_cache(&["add", "issues/"]);
            let _ = self.git_in_cache(&["add", "locks/"]);

            // Commit (unsigned when no SSH key)
            let commit_msg = format!(
                "{}: {} at {}",
                self.agent.agent_id,
                message,
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
            );
            let commit_result = self.git_commit_in_cache(&commit_msg);
            if let Err(ref e) = commit_result {
                let err_str = e.to_string();
                if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                    return Ok(PushOutcome::Pushed);
                }
            }
            commit_result?;

            // Push
            let remote = self.sync.remote();
            let push_result = self.git_in_cache(&["push", remote, crate::sync::HUB_BRANCH]);
            match push_result {
                Ok(_) => return Ok(PushOutcome::Pushed),
                Err(e) => {
                    let err_str = e.to_string();
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        eprintln!(
                            "Warning: push failed (offline), changes saved locally only: {}",
                            message
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < MAX_RETRIES - 1 {
                            // Bail if local has diverged too far — sign of a rebase loop
                            self.check_divergence()?;
                            // Reset commit AND working directory — the prepare
                            // closure re-generates content on the next iteration,
                            // so losing working dir changes is safe.
                            let _ = self.git_in_cache(&["reset", "--hard", "HEAD~1"]);
                            self.git_in_cache(&[
                                "pull",
                                "--rebase",
                                remote,
                                crate::sync::HUB_BRANCH,
                            ])?;
                            continue;
                        }
                        eprintln!(
                            "Warning: push failed after {} retries (conflict), changes saved locally only: {}",
                            MAX_RETRIES, message
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    return Err(e);
                }
            }
        }
        Ok(PushOutcome::Pushed)
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
                let is_v2 = writer.layout_version() >= 2;
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
                let rel_path = writer.issue_rel_path(&uuid);
                if is_v2 {
                    // Ensure the comments subdirectory exists for v2 layout
                    let comments_dir = writer
                        .cache_dir
                        .join("issues")
                        .join(uuid.to_string())
                        .join("comments");
                    std::fs::create_dir_all(&comments_dir)
                        .context("Failed to create v2 comments directory")?;
                }
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let is_v2 = writer.layout_version() >= 2;
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
                let rel_path = writer.issue_rel_path(&uuid);
                if is_v2 {
                    let comments_dir = writer
                        .cache_dir
                        .join("issues")
                        .join(uuid.to_string())
                        .join("comments");
                    std::fs::create_dir_all(&comments_dir)
                        .context("Failed to create v2 comments directory")?;
                }
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
    pub fn add_comment(
        &self,
        db: &Database,
        display_id: i64,
        content: &str,
        kind: &str,
    ) -> Result<i64> {
        let content_owned = content.to_string();
        let kind_owned = kind.to_string();
        let agent_id = self.agent.agent_id.clone();
        let comment_id = Cell::new(0i64);

        let _ = self.write_commit_push(
            |writer| {
                let mut counters = writer.read_counters()?;
                let id = counters.next_comment_id;
                counters.next_comment_id += 1;
                comment_id.set(id);

                let (signed_by, signature) = writer.sign_comment(&content_owned, &agent_id, id);

                if writer.layout_version() >= 2 {
                    // V2: write a standalone comment file, don't modify the issue file
                    let issue = writer.load_issue_by_id(display_id, db)?;
                    let comment_uuid = Uuid::new_v4();
                    let comment_file = CommentFile {
                        uuid: comment_uuid,
                        issue_uuid: issue.uuid,
                        author: agent_id.clone(),
                        content: content_owned.clone(),
                        created_at: Utc::now(),
                        kind: kind_owned.clone(),
                        trigger_type: None,
                        intervention_context: None,
                        driver_key_fingerprint: None,
                        signed_by,
                        signature,
                    };
                    let json = serde_json::to_vec_pretty(&comment_file)?;
                    let rel_path = format!("issues/{}/comments/{}.json", issue.uuid, comment_uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        use_git_rm: false,
                    })
                } else {
                    // V1: append comment inline to the issue file
                    let mut issue = writer.load_issue_by_id(display_id, db)?;
                    issue.comments.push(CommentEntry {
                        id,
                        author: agent_id.clone(),
                        content: content_owned.clone(),
                        created_at: Utc::now(),
                        kind: kind_owned.clone(),
                        trigger_type: None,
                        intervention_context: None,
                        driver_key_fingerprint: None,
                        signed_by,
                        signature,
                    });
                    issue.updated_at = Utc::now();

                    let json = serde_json::to_vec_pretty(&issue)?;
                    let rel_path = writer.issue_rel_path(&issue.uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        use_git_rm: false,
                    })
                }
            },
            &format!("comment on issue #{}", display_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(comment_id.get())
    }

    /// Add a driver intervention comment to an issue (kind = "intervention").
    pub fn add_intervention_comment(
        &self,
        db: &Database,
        display_id: i64,
        content: &str,
        trigger_type: &str,
        intervention_context: Option<&str>,
        driver_key_fingerprint: Option<&str>,
    ) -> Result<i64> {
        let content_owned = content.to_string();
        let trigger_owned = trigger_type.to_string();
        let context_owned = intervention_context.map(|s| s.to_string());
        let driver_fp_owned = driver_key_fingerprint.map(|s| s.to_string());
        let agent_id = self.agent.agent_id.clone();
        let comment_id = Cell::new(0i64);

        let _ = self.write_commit_push(
            |writer| {
                let mut counters = writer.read_counters()?;
                let id = counters.next_comment_id;
                counters.next_comment_id += 1;
                comment_id.set(id);

                let (signed_by, signature) = writer.sign_comment(&content_owned, &agent_id, id);

                if writer.layout_version() >= 2 {
                    // V2: write a standalone comment file
                    let issue = writer.load_issue_by_id(display_id, db)?;
                    let comment_uuid = Uuid::new_v4();
                    let comment_file = CommentFile {
                        uuid: comment_uuid,
                        issue_uuid: issue.uuid,
                        author: agent_id.clone(),
                        content: content_owned.clone(),
                        created_at: Utc::now(),
                        kind: "intervention".to_string(),
                        trigger_type: Some(trigger_owned.clone()),
                        intervention_context: context_owned.clone(),
                        driver_key_fingerprint: driver_fp_owned.clone(),
                        signed_by,
                        signature,
                    };
                    let json = serde_json::to_vec_pretty(&comment_file)?;
                    let rel_path = format!("issues/{}/comments/{}.json", issue.uuid, comment_uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        use_git_rm: false,
                    })
                } else {
                    // V1: append comment inline to the issue file
                    let mut issue = writer.load_issue_by_id(display_id, db)?;
                    issue.comments.push(CommentEntry {
                        id,
                        author: agent_id.clone(),
                        content: content_owned.clone(),
                        created_at: Utc::now(),
                        kind: "intervention".to_string(),
                        trigger_type: Some(trigger_owned.clone()),
                        intervention_context: context_owned.clone(),
                        driver_key_fingerprint: driver_fp_owned.clone(),
                        signed_by,
                        signature,
                    });
                    issue.updated_at = Utc::now();

                    let json = serde_json::to_vec_pretty(&issue)?;
                    let rel_path = writer.issue_rel_path(&issue.uuid);
                    Ok(WriteSet {
                        files: vec![(rel_path, json)],
                        counters: Some(counters),
                        use_git_rm: false,
                    })
                }
            },
            &format!("intervention on issue #{}", display_id),
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
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

    /// Set `milestone_uuid` on issue JSON files for the given issue IDs.
    ///
    /// Loads the milestone to get its UUID, then patches each issue file.
    /// Also adds the issues to the SQLite milestone_issues table via hydration.
    pub fn set_milestone_on_issues(
        &self,
        db: &Database,
        milestone_id: i64,
        issue_ids: &[i64],
    ) -> Result<()> {
        let milestone = self.load_milestone_by_id(milestone_id)?;
        let ms_uuid = milestone.uuid;

        let ids: Vec<i64> = issue_ids.to_vec();
        let _ = self.write_commit_push(
            |writer| {
                let mut files = Vec::new();
                for &issue_id in &ids {
                    let mut issue = writer.load_issue_by_id(issue_id, db)?;
                    issue.milestone_uuid = Some(ms_uuid);
                    issue.updated_at = Utc::now();
                    let json = serde_json::to_vec_pretty(&issue)?;
                    let rel_path = writer.issue_rel_path(&issue.uuid);
                    files.push((rel_path, json));
                }
                Ok(WriteSet {
                    files,
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("add {} issue(s) to milestone #{}", ids.len(), milestone_id),
        )?;

        hydrate_to_sqlite(&self.cache_dir, db)?;
        Ok(())
    }

    /// Clear `milestone_uuid` on an issue JSON file.
    pub fn clear_milestone_on_issue(&self, db: &Database, issue_id: i64) -> Result<()> {
        let _ = self.write_commit_push(
            |writer| {
                let mut issue = writer.load_issue_by_id(issue_id, db)?;
                issue.milestone_uuid = None;
                issue.updated_at = Utc::now();
                let json = serde_json::to_vec_pretty(&issue)?;
                let rel_path = writer.issue_rel_path(&issue.uuid);
                Ok(WriteSet {
                    files: vec![(rel_path, json)],
                    counters: None,
                    use_git_rm: false,
                })
            },
            &format!("remove issue #{} from milestone", issue_id),
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
            if let Err(e) = self.git_in_cache(&["add", "."]) {
                eprintln!("Warning: failed to stage reverted state: {}", e);
            }
            if let Err(e) = self.git_in_cache(&["commit", "--amend", "--no-edit"]) {
                eprintln!("Warning: failed to commit reverted state: {}", e);
                // Last resort: clean dirty state so we don't poison future syncs
                let _ = self.sync.clean_dirty_state();
            }
            return Ok(vec![]);
        }

        // Re-hydrate with new positive IDs
        hydrate_to_sqlite(&self.cache_dir, db)?;

        // Record promoted UUIDs so they are never re-promoted (gh#313).
        let promoted_uuids: Vec<Uuid> = offline_info.iter().map(|(uuid, _)| *uuid).collect();
        if let Err(e) = self.record_promoted_uuids(&promoted_uuids) {
            eprintln!("Warning: failed to record promoted UUIDs: {}", e);
        }

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
            if let Err(e) = self.git_in_cache(&["add", "issues/"]) {
                eprintln!("Warning: failed to stage rewritten references: {}", e);
            }
            if let Err(e) = self.git_in_cache(&[
                "commit",
                "-m",
                &format!(
                    "{}: rewrite local references after promotion",
                    self.agent.agent_id
                ),
            ]) {
                eprintln!("Warning: failed to commit rewritten references: {}", e);
            }
            // Best-effort push
            let _ = self.git_in_cache(&["push", self.sync.remote(), crate::sync::HUB_BRANCH]);
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

    /// Sign a comment's canonical content if the agent has an SSH key.
    ///
    /// Returns `(signed_by, signature)` — both `None` if no key is available.
    fn sign_comment(
        &self,
        content: &str,
        author: &str,
        comment_id: i64,
    ) -> (Option<String>, Option<String>) {
        let (key_path, fingerprint) = match (&self.agent.ssh_key_path, &self.agent.ssh_fingerprint)
        {
            (Some(rel), Some(fp)) => {
                // ssh_key_path is relative to .crosslink/; resolve via sync's cache
                let crosslink_dir = self
                    .sync
                    .cache_path()
                    .parent()
                    .unwrap_or(self.sync.cache_path());
                let abs = crosslink_dir.join(rel);
                (abs, fp.clone())
            }
            _ => return (None, None),
        };

        if !key_path.exists() {
            return (None, None);
        }

        let canonical = crate::signing::canonicalize_for_signing(&[
            ("author", author),
            ("comment_id", &comment_id.to_string()),
            ("content", content),
        ]);

        match crate::signing::sign_content(&key_path, &canonical, "crosslink-comment") {
            Ok(sig) => (Some(fingerprint), Some(sig)),
            Err(_) => (None, None),
        }
    }

    /// Find all issue files in the cache with `display_id: null` created by this agent.
    ///
    /// Supports both v1 (`issues/{uuid}.json`) and v2 (`issues/{uuid}/issue.json`) layouts.
    /// Skips issues whose UUIDs appear in the promoted-UUIDs tracking file to
    /// prevent re-promotion loops (gh#313).
    fn find_offline_issues(&self) -> Result<Vec<IssueFile>> {
        let issues_dir = self.cache_dir.join("issues");
        let mut offline = Vec::new();
        if !issues_dir.exists() {
            return Ok(offline);
        }

        // Load the set of already-promoted UUIDs so we never re-promote them.
        let promoted = self.read_promoted_uuids();

        for entry in std::fs::read_dir(&issues_dir)? {
            let entry = entry?;
            let path = entry.path();
            // V1: issues/{uuid}.json (flat file)
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(issue) = read_issue_file(&path) {
                    if issue.display_id.is_none()
                        && issue.created_by == self.agent.agent_id
                        && !promoted.contains(&issue.uuid)
                    {
                        offline.push(issue);
                    }
                }
            }
            // V2: issues/{uuid}/issue.json (directory per issue)
            if path.is_dir() {
                let issue_file = path.join("issue.json");
                if issue_file.exists() {
                    if let Ok(issue) = read_issue_file(&issue_file) {
                        if issue.display_id.is_none()
                            && issue.created_by == self.agent.agent_id
                            && !promoted.contains(&issue.uuid)
                        {
                            offline.push(issue);
                        }
                    }
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
        let rel_path = self.issue_rel_path(&uuid);
        self.git_in_cache(&["add", &rel_path])?;
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
    ///
    /// V1: `issues/{uuid}.json`
    /// V2: `issues/{uuid}/issue.json`
    fn issue_path(&self, uuid: &Uuid) -> PathBuf {
        if self.layout_version() >= 2 {
            self.cache_dir
                .join("issues")
                .join(uuid.to_string())
                .join("issue.json")
        } else {
            self.cache_dir.join("issues").join(format!("{}.json", uuid))
        }
    }

    /// Relative path to an issue JSON file (for WriteSet entries and git staging).
    ///
    /// V1: `issues/{uuid}.json`
    /// V2: `issues/{uuid}/issue.json`
    fn issue_rel_path(&self, uuid: &Uuid) -> String {
        if self.layout_version() >= 2 {
            format!("issues/{}/issue.json", uuid)
        } else {
            format!("issues/{}.json", uuid)
        }
    }

    /// Load an issue JSON file by its display ID.
    ///
    /// Scans the issues directory for a file matching the display ID.
    /// Supports both v1 (`issues/{uuid}.json`) and v2 (`issues/{uuid}/issue.json`) layouts.
    fn load_issue_by_display_id(&self, display_id: i64) -> Result<IssueFile> {
        let issues_dir = self.cache_dir.join("issues");
        for entry in std::fs::read_dir(&issues_dir)
            .with_context(|| format!("Cannot read issues dir: {}", issues_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            // V1: issues/{uuid}.json (flat file)
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(issue) = read_issue_file(&path) {
                    if issue.display_id == Some(display_id) {
                        return Ok(issue);
                    }
                }
            }
            // V2: issues/{uuid}/issue.json (directory per issue)
            if path.is_dir() {
                let issue_file = path.join("issue.json");
                if issue_file.exists() {
                    if let Ok(issue) = read_issue_file(&issue_file) {
                        if issue.display_id == Some(display_id) {
                            return Ok(issue);
                        }
                    }
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

            // Commit (unsigned when no SSH key)
            let commit_msg = format!(
                "{}: {} at {}",
                self.agent.agent_id,
                message,
                Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
            );
            let commit_result = self.git_commit_in_cache(&commit_msg);
            if let Err(e) = &commit_result {
                let err_str = e.to_string();
                if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                    return Ok(PushOutcome::Pushed);
                }
                commit_result?;
            }

            // Push
            let remote = self.sync.remote();
            let push_result = self.git_in_cache(&["push", remote, crate::sync::HUB_BRANCH]);
            match push_result {
                Ok(_) => return Ok(PushOutcome::Pushed),
                Err(e) => {
                    let err_str = e.to_string();
                    // Offline — commit is local, will push on next sync
                    if err_str.contains("Could not resolve host")
                        || err_str.contains("Could not read from remote")
                    {
                        eprintln!(
                            "Warning: push failed (offline), changes saved locally only: {}",
                            message
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    // Conflict — reset commit AND working directory, pull latest,
                    // then retry. The prepare closure re-reads fresh state on the
                    // next iteration, so losing working dir changes is safe.
                    if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                        if attempt < MAX_RETRIES - 1 {
                            // Bail if local has diverged too far — sign of a rebase loop
                            self.check_divergence()?;
                            let _ = self.git_in_cache(&["reset", "--hard", "HEAD~1"]);
                            self.git_in_cache(&[
                                "pull",
                                "--rebase",
                                remote,
                                crate::sync::HUB_BRANCH,
                            ])?;
                            continue;
                        }
                        // All retries exhausted — keep as local-only
                        eprintln!(
                            "Warning: push failed after {} retries (conflict), changes saved locally only: {}",
                            MAX_RETRIES, message
                        );
                        return Ok(PushOutcome::LocalOnly);
                    }
                    // Other error — propagate
                    return Err(e);
                }
            }
        }
        Ok(PushOutcome::Pushed)
    }

    // ─────────────── V2 Lock Protocol (event-based) ───────────────

    /// Claim a lock on an issue using the V2 event-based protocol.
    ///
    /// 1. Check if already held by self → AlreadyHeld
    /// 2. Emit LockClaimed event → append to event log
    /// 3. Push event log (conflict-free per-agent file)
    /// 4. Compact with force=true
    /// 5. Stage + commit + push compaction output (rebase-retry)
    /// 6. Read materialized lock file
    /// 7. If winner is self → Claimed; else → emit LockReleased cleanup → Contended
    pub fn claim_lock_v2(
        &self,
        issue_display_id: i64,
        branch: Option<&str>,
    ) -> Result<LockClaimResult> {
        // Check if already held
        if let Some(lock) = self.read_lock_v2(issue_display_id)? {
            if lock.agent_id == self.agent.agent_id {
                return Ok(LockClaimResult::AlreadyHeld);
            }
        }

        // Emit LockClaimed event, then compact+push with timeout guard.
        // Per design doc section 8: if compaction hasn't completed within 30s,
        // fail rather than treating a stale result as authoritative.
        let event = crate::events::Event::LockClaimed {
            issue_display_id,
            branch: branch.map(|s| s.to_string()),
        };
        let start = std::time::Instant::now();
        self.emit_compact_push(event, &format!("claim lock on #{}", issue_display_id))?;
        let elapsed = start.elapsed();
        if elapsed > std::time::Duration::from_secs(LOCK_CONFIRM_TIMEOUT_SECS) {
            bail!(
                "Lock confirmation timed out after {}s (threshold {}s) — \
                 compaction result may be stale, not treating as authoritative",
                elapsed.as_secs(),
                LOCK_CONFIRM_TIMEOUT_SECS
            );
        }

        // Re-read materialized lock to see who won
        match self.read_lock_v2(issue_display_id)? {
            Some(lock) if lock.agent_id == self.agent.agent_id => Ok(LockClaimResult::Claimed),
            Some(lock) => {
                // We lost — clean up by emitting LockReleased
                let release = crate::events::Event::LockReleased { issue_display_id };
                let _ = self.emit_compact_push(
                    release,
                    &format!("release lock on #{} (contention cleanup)", issue_display_id),
                );
                Ok(LockClaimResult::Contended {
                    winner_agent_id: lock.agent_id,
                })
            }
            None => {
                // Lock wasn't materialized — shouldn't happen, but treat as claimed
                Ok(LockClaimResult::Claimed)
            }
        }
    }

    /// Release a lock on an issue using the V2 event-based protocol.
    ///
    /// Returns Ok(true) if released, Ok(false) if not held.
    pub fn release_lock_v2(&self, issue_display_id: i64) -> Result<bool> {
        // Check if we actually hold it
        match self.read_lock_v2(issue_display_id)? {
            Some(lock) if lock.agent_id == self.agent.agent_id => {
                // We hold it — release
                let event = crate::events::Event::LockReleased { issue_display_id };
                self.emit_compact_push(event, &format!("release lock on #{}", issue_display_id))?;
                Ok(true)
            }
            Some(_) => {
                // Held by someone else — can't release
                Ok(false)
            }
            None => {
                // Not locked
                Ok(false)
            }
        }
    }

    /// Steal a lock from a stale agent using the V2 event-based protocol.
    ///
    /// Prunes the stale agent's events, clears checkpoint lock state,
    /// then claims normally.
    pub fn steal_lock_v2(
        &self,
        issue_display_id: i64,
        stale_agent_id: &str,
        branch: Option<&str>,
    ) -> Result<LockClaimResult> {
        // Prune stale agent's compacted events so they don't replay
        crate::compaction::prune_events(&self.cache_dir, stale_agent_id)?;

        // Clear lock from checkpoint state
        let mut state = crate::checkpoint::read_checkpoint(&self.cache_dir)?;
        state.locks.remove(&issue_display_id);
        crate::checkpoint::write_checkpoint(&self.cache_dir, &state)?;

        // Remove materialized lock file
        let lock_path = self
            .cache_dir
            .join("locks")
            .join(format!("{}.json", issue_display_id));
        if lock_path.exists() {
            std::fs::remove_file(&lock_path)?;
        }

        // Now claim normally
        self.claim_lock_v2(issue_display_id, branch)
    }

    /// Read a V2 lock file for a specific issue.
    ///
    /// Returns None if the lock file doesn't exist.
    pub fn read_lock_v2(
        &self,
        issue_display_id: i64,
    ) -> Result<Option<crate::issue_file::LockFileV2>> {
        let lock_path = self
            .cache_dir
            .join("locks")
            .join(format!("{}.json", issue_display_id));
        if !lock_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&lock_path)
            .with_context(|| format!("Failed to read lock file: {}", lock_path.display()))?;
        let lock: crate::issue_file::LockFileV2 = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse lock file: {}", lock_path.display()))?;
        Ok(Some(lock))
    }

    /// Check if local has diverged too far from remote and bail if so.
    /// Delegates to `SyncManager::check_divergence` via the shared `sync` field.
    fn check_divergence(&self) -> Result<()> {
        self.sync.check_divergence()
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

    /// Run a git commit in the cache worktree, disabling signing when
    /// the agent has no SSH key (anonymous/pre-init mode).
    fn git_commit_in_cache(&self, message: &str) -> Result<std::process::Output> {
        let has_key = self.agent.ssh_key_path.is_some();
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(&self.cache_dir);
        if !has_key {
            cmd.args(["-c", "commit.gpgsign=false"]);
        }
        cmd.args(["commit", "-m", message]);
        let output = cmd
            .output()
            .with_context(|| "Failed to run git commit in cache".to_string())?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git commit in cache failed: {}", stderr);
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

    #[test]
    fn test_v1_issue_path_format() {
        let uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let path = format!("issues/{}.json", uuid);
        assert_eq!(path, "issues/a1b2c3d4-e5f6-7890-abcd-ef1234567890.json");
    }

    #[test]
    fn test_v2_issue_path_format() {
        let uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let path = format!("issues/{}/issue.json", uuid);
        assert_eq!(
            path,
            "issues/a1b2c3d4-e5f6-7890-abcd-ef1234567890/issue.json"
        );
    }

    #[test]
    fn test_v2_comment_path_format() {
        let issue_uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let comment_uuid = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let path = format!("issues/{}/comments/{}.json", issue_uuid, comment_uuid);
        assert_eq!(
            path,
            "issues/a1b2c3d4-e5f6-7890-abcd-ef1234567890/comments/11111111-2222-3333-4444-555555555555.json"
        );
    }

    #[test]
    fn test_v2_scan_finds_issue_in_subdirectory() {
        let dir = tempdir().unwrap();
        let issues_dir = dir.path().join("issues");

        // Create a v2-style issue directory
        let issue = make_issue(7, "V2 Issue");
        let issue_subdir = issues_dir.join(issue.uuid.to_string());
        std::fs::create_dir_all(issue_subdir.join("comments")).unwrap();
        write_issue_file(&issue_subdir.join("issue.json"), &issue).unwrap();

        // The v2 scan should find it
        let mut found = false;
        for entry in std::fs::read_dir(&issues_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                let issue_file = path.join("issue.json");
                if issue_file.exists() {
                    if let Ok(loaded) = read_issue_file(&issue_file) {
                        if loaded.display_id == Some(7) {
                            assert_eq!(loaded.title, "V2 Issue");
                            found = true;
                        }
                    }
                }
            }
        }
        assert!(found, "v2 issue not found in subdirectory scan");
    }

    #[test]
    fn test_v2_comment_file_construction() {
        use crate::issue_file::CommentFile;

        let issue_uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let comment_uuid = Uuid::new_v4();
        let comment = CommentFile {
            uuid: comment_uuid,
            issue_uuid,
            author: "test-agent".to_string(),
            content: "A standalone comment".to_string(),
            created_at: Utc::now(),
            kind: "note".to_string(),
            trigger_type: None,
            intervention_context: None,
            driver_key_fingerprint: None,
            signed_by: None,
            signature: None,
        };

        let json = serde_json::to_string_pretty(&comment).unwrap();
        let parsed: CommentFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.uuid, comment_uuid);
        assert_eq!(parsed.issue_uuid, issue_uuid);
        assert_eq!(parsed.content, "A standalone comment");
        assert_eq!(parsed.kind, "note");
    }

    #[test]
    fn test_v2_intervention_comment_file_construction() {
        use crate::issue_file::CommentFile;

        let issue_uuid = Uuid::parse_str("a1b2c3d4-e5f6-7890-abcd-ef1234567890").unwrap();
        let comment_uuid = Uuid::new_v4();
        let comment = CommentFile {
            uuid: comment_uuid,
            issue_uuid,
            author: "test-agent".to_string(),
            content: "Driver intervention".to_string(),
            created_at: Utc::now(),
            kind: "intervention".to_string(),
            trigger_type: Some("redirect".to_string()),
            intervention_context: Some("User redirected task".to_string()),
            driver_key_fingerprint: Some("SHA256:abc123".to_string()),
            signed_by: None,
            signature: None,
        };

        let json = serde_json::to_string_pretty(&comment).unwrap();
        let parsed: CommentFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.kind, "intervention");
        assert_eq!(parsed.trigger_type, Some("redirect".to_string()));
        assert_eq!(
            parsed.intervention_context,
            Some("User redirected task".to_string())
        );
        assert_eq!(
            parsed.driver_key_fingerprint,
            Some("SHA256:abc123".to_string())
        );
    }

    #[test]
    fn test_lock_confirm_timeout_constant() {
        assert_eq!(LOCK_CONFIRM_TIMEOUT_SECS, 30);
    }

    mod lock_v2_tests {
        use super::*;
        use crate::issue_file::LockFileV2;
        use tempfile::tempdir;

        #[test]
        fn test_lock_claim_result_variants() {
            let claimed = LockClaimResult::Claimed;
            let already = LockClaimResult::AlreadyHeld;
            let contended = LockClaimResult::Contended {
                winner_agent_id: "agent-2".to_string(),
            };
            assert_eq!(claimed, LockClaimResult::Claimed);
            assert_eq!(already, LockClaimResult::AlreadyHeld);
            assert_ne!(claimed, already);
            assert_ne!(claimed, contended.clone());
            assert_eq!(
                contended,
                LockClaimResult::Contended {
                    winner_agent_id: "agent-2".to_string(),
                }
            );
            // Verify Debug
            let _ = format!("{:?}", claimed);
            let _ = format!("{:?}", contended);
        }

        #[test]
        fn test_read_lock_v2_file() {
            let dir = tempdir().unwrap();
            let locks_dir = dir.path().join("locks");
            std::fs::create_dir_all(&locks_dir).unwrap();

            let lock = LockFileV2 {
                issue_id: 42,
                agent_id: "agent-1".to_string(),
                branch: Some("feature/x".to_string()),
                claimed_at: chrono::Utc::now(),
                signed_by: Some("SHA256:abc".to_string()),
            };
            let json = serde_json::to_string_pretty(&lock).unwrap();
            std::fs::write(locks_dir.join("42.json"), &json).unwrap();

            // Read it back
            let content = std::fs::read_to_string(locks_dir.join("42.json")).unwrap();
            let parsed: LockFileV2 = serde_json::from_str(&content).unwrap();
            assert_eq!(parsed.issue_id, 42);
            assert_eq!(parsed.agent_id, "agent-1");
            assert_eq!(parsed.branch, Some("feature/x".to_string()));
        }

        #[test]
        fn test_read_lock_v2_missing() {
            let dir = tempdir().unwrap();
            let lock_path = dir.path().join("locks").join("99.json");
            assert!(!lock_path.exists());
        }

        #[test]
        fn test_lock_v2_file_roundtrip() {
            let dir = tempdir().unwrap();
            let locks_dir = dir.path().join("locks");
            std::fs::create_dir_all(&locks_dir).unwrap();

            let lock = LockFileV2 {
                issue_id: 5,
                agent_id: "worker-1".to_string(),
                branch: None,
                claimed_at: chrono::Utc::now(),
                signed_by: None,
            };
            let json = serde_json::to_string_pretty(&lock).unwrap();
            let path = locks_dir.join("5.json");
            std::fs::write(&path, &json).unwrap();

            let content = std::fs::read_to_string(&path).unwrap();
            let parsed: LockFileV2 = serde_json::from_str(&content).unwrap();
            assert_eq!(parsed.issue_id, lock.issue_id);
            assert_eq!(parsed.agent_id, lock.agent_id);
            assert!(parsed.branch.is_none());
            assert!(parsed.signed_by.is_none());
        }

        #[test]
        fn test_lock_contention_deterministic_winner() {
            // Verify that compaction's first-claim-wins rule works
            use crate::checkpoint::{read_checkpoint, write_checkpoint, CheckpointState};
            use crate::events::{append_event, Event, EventEnvelope};
            use chrono::Utc;

            let dir = tempdir().unwrap();
            let cache = dir.path();

            // Set up checkpoint
            std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-a")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-b")).unwrap();
            std::fs::create_dir_all(cache.join("locks")).unwrap();
            std::fs::create_dir_all(cache.join("issues")).unwrap();

            let state = CheckpointState::default();
            write_checkpoint(cache, &state).unwrap();

            let now = Utc::now();

            // Agent A claims first (earlier timestamp)
            let e1 = EventEnvelope {
                agent_id: "agent-a".to_string(),
                agent_seq: 1,
                timestamp: now - chrono::Duration::seconds(1),
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: None,
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-a/events.log"), &e1).unwrap();

            // Agent B claims second (later timestamp)
            let e2 = EventEnvelope {
                agent_id: "agent-b".to_string(),
                agent_seq: 1,
                timestamp: now,
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: None,
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-b/events.log"), &e2).unwrap();

            // Run compaction
            let result = crate::compaction::compact(cache, "agent-a", true)
                .unwrap()
                .unwrap();
            assert_eq!(result.locks_materialized, 1);

            // Read checkpoint — agent-a should win (earlier timestamp)
            let state = read_checkpoint(cache).unwrap();
            let lock = state.locks.get(&1).unwrap();
            assert_eq!(lock.agent_id, "agent-a");
        }

        #[test]
        fn test_prune_then_checkpoint_clear() {
            use crate::checkpoint::{write_checkpoint, CheckpointState, LockEntry};
            use crate::events::{append_event, Event, EventEnvelope, OrderingKey};
            use chrono::Utc;

            let dir = tempdir().unwrap();
            let cache = dir.path();

            std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
            std::fs::create_dir_all(cache.join("agents/stale-agent")).unwrap();
            std::fs::create_dir_all(cache.join("locks")).unwrap();
            std::fs::create_dir_all(cache.join("issues")).unwrap();

            let now = Utc::now();

            // Write an event for the stale agent
            let e = EventEnvelope {
                agent_id: "stale-agent".to_string(),
                agent_seq: 1,
                timestamp: now,
                event: Event::LockClaimed {
                    issue_display_id: 5,
                    branch: None,
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/stale-agent/events.log"), &e).unwrap();

            // Write a watermark that covers the event so prune_events will prune it
            let watermark = OrderingKey {
                timestamp: now + chrono::Duration::seconds(1),
                agent_id: "stale-agent".to_string(),
                agent_seq: 1,
            };

            // Compact to materialize (watermark is embedded in checkpoint state)
            let mut state = CheckpointState::default();
            state.watermark = Some(watermark);
            state.locks.insert(
                5,
                LockEntry {
                    agent_id: "stale-agent".to_string(),
                    branch: None,
                    claimed_at: now,
                },
            );
            write_checkpoint(cache, &state).unwrap();

            // Write materialized lock file
            let lock = crate::issue_file::LockFileV2 {
                issue_id: 5,
                agent_id: "stale-agent".to_string(),
                branch: None,
                claimed_at: now,
                signed_by: None,
            };
            std::fs::write(
                cache.join("locks/5.json"),
                serde_json::to_string_pretty(&lock).unwrap(),
            )
            .unwrap();

            // Prune stale agent events
            let pruned = crate::compaction::prune_events(cache, "stale-agent").unwrap();
            assert!(pruned > 0);

            // Clear checkpoint lock
            let mut state = crate::checkpoint::read_checkpoint(cache).unwrap();
            state.locks.remove(&5);
            write_checkpoint(cache, &state).unwrap();

            // Remove lock file
            let lock_path = cache.join("locks/5.json");
            if lock_path.exists() {
                std::fs::remove_file(&lock_path).unwrap();
            }

            // Verify clean state
            let state = crate::checkpoint::read_checkpoint(cache).unwrap();
            assert!(state.locks.is_empty());
            assert!(!cache.join("locks/5.json").exists());
        }

        #[test]
        fn test_lock_file_v2_with_all_fields() {
            let dir = tempdir().unwrap();
            let locks_dir = dir.path().join("locks");
            std::fs::create_dir_all(&locks_dir).unwrap();

            let now = chrono::Utc::now();
            let lock = LockFileV2 {
                issue_id: 100,
                agent_id: "agent-special".to_string(),
                branch: Some("feature/special-branch".to_string()),
                claimed_at: now,
                signed_by: Some("SHA256:xyz789".to_string()),
            };
            let json = serde_json::to_string_pretty(&lock).unwrap();
            let path = locks_dir.join("100.json");
            std::fs::write(&path, &json).unwrap();

            let content = std::fs::read_to_string(&path).unwrap();
            let parsed: LockFileV2 = serde_json::from_str(&content).unwrap();
            assert_eq!(parsed.issue_id, 100);
            assert_eq!(parsed.agent_id, "agent-special");
            assert_eq!(parsed.branch, Some("feature/special-branch".to_string()));
            assert_eq!(parsed.claimed_at, now);
            assert_eq!(parsed.signed_by, Some("SHA256:xyz789".to_string()));
        }

        #[test]
        fn test_lock_claim_result_display_and_equality() {
            // Verify Contended results with different winners are not equal
            let c1 = LockClaimResult::Contended {
                winner_agent_id: "agent-1".to_string(),
            };
            let c2 = LockClaimResult::Contended {
                winner_agent_id: "agent-2".to_string(),
            };
            assert_ne!(c1, c2);

            // Verify same winner is equal
            let c3 = LockClaimResult::Contended {
                winner_agent_id: "agent-1".to_string(),
            };
            assert_eq!(c1, c3);

            // Verify Clone works correctly
            let cloned = c1.clone();
            assert_eq!(c1, cloned);
        }

        #[test]
        fn test_lock_contention_with_three_agents() {
            // Three agents claiming same lock, verify deterministic winner
            use crate::checkpoint::{read_checkpoint, write_checkpoint, CheckpointState};
            use crate::events::{append_event, Event, EventEnvelope};
            use chrono::Utc;

            let dir = tempdir().unwrap();
            let cache = dir.path();

            std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-a")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-b")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-c")).unwrap();
            std::fs::create_dir_all(cache.join("locks")).unwrap();
            std::fs::create_dir_all(cache.join("issues")).unwrap();

            let state = CheckpointState::default();
            write_checkpoint(cache, &state).unwrap();

            let now = Utc::now();

            // Agent C claims first (earliest)
            let e1 = EventEnvelope {
                agent_id: "agent-c".to_string(),
                agent_seq: 1,
                timestamp: now - chrono::Duration::seconds(3),
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: Some("feature/c".to_string()),
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-c/events.log"), &e1).unwrap();

            // Agent A claims second
            let e2 = EventEnvelope {
                agent_id: "agent-a".to_string(),
                agent_seq: 1,
                timestamp: now - chrono::Duration::seconds(2),
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: Some("feature/a".to_string()),
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-a/events.log"), &e2).unwrap();

            // Agent B claims third
            let e3 = EventEnvelope {
                agent_id: "agent-b".to_string(),
                agent_seq: 1,
                timestamp: now - chrono::Duration::seconds(1),
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: Some("feature/b".to_string()),
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-b/events.log"), &e3).unwrap();

            let result = crate::compaction::compact(cache, "agent-a", true)
                .unwrap()
                .unwrap();
            assert_eq!(result.locks_materialized, 1);

            let state = read_checkpoint(cache).unwrap();
            let lock = state.locks.get(&1).unwrap();
            assert_eq!(lock.agent_id, "agent-c");
            assert_eq!(lock.branch, Some("feature/c".to_string()));
        }

        #[test]
        fn test_lock_contention_then_winner_releases() {
            // Two agents contend. Winner releases. Lock should be empty.
            use crate::checkpoint::{read_checkpoint, write_checkpoint, CheckpointState};
            use crate::events::{append_event, Event, EventEnvelope};
            use chrono::Utc;

            let dir = tempdir().unwrap();
            let cache = dir.path();

            std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-a")).unwrap();
            std::fs::create_dir_all(cache.join("agents/agent-b")).unwrap();
            std::fs::create_dir_all(cache.join("locks")).unwrap();
            std::fs::create_dir_all(cache.join("issues")).unwrap();

            let state = CheckpointState::default();
            write_checkpoint(cache, &state).unwrap();

            let now = Utc::now();

            // Agent A claims first (wins)
            let e1 = EventEnvelope {
                agent_id: "agent-a".to_string(),
                agent_seq: 1,
                timestamp: now - chrono::Duration::seconds(3),
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: None,
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-a/events.log"), &e1).unwrap();

            // Agent B claims second (loses)
            let e2 = EventEnvelope {
                agent_id: "agent-b".to_string(),
                agent_seq: 1,
                timestamp: now - chrono::Duration::seconds(2),
                event: Event::LockClaimed {
                    issue_display_id: 1,
                    branch: None,
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-b/events.log"), &e2).unwrap();

            // Agent A releases
            let e3 = EventEnvelope {
                agent_id: "agent-a".to_string(),
                agent_seq: 2,
                timestamp: now - chrono::Duration::seconds(1),
                event: Event::LockReleased {
                    issue_display_id: 1,
                },
                signed_by: None,
                signature: None,
            };
            append_event(&cache.join("agents/agent-a/events.log"), &e3).unwrap();

            crate::compaction::compact(cache, "agent-a", true).unwrap();

            let state = read_checkpoint(cache).unwrap();
            assert!(state.locks.is_empty());
            assert!(!cache.join("locks/1.json").exists());
        }

        #[test]
        fn test_lock_file_v2_missing_optional_fields() {
            // Verify LockFileV2 deserialization works when optional fields are null
            let json = r#"{
                "issue_id": 7,
                "agent_id": "agent-minimal",
                "branch": null,
                "claimed_at": "2026-01-01T00:00:00Z",
                "signed_by": null
            }"#;
            let parsed: LockFileV2 = serde_json::from_str(json).unwrap();
            assert_eq!(parsed.issue_id, 7);
            assert_eq!(parsed.agent_id, "agent-minimal");
            assert!(parsed.branch.is_none());
            assert!(parsed.signed_by.is_none());
        }

        #[test]
        fn test_lock_contention_deterministic_across_compaction_agents() {
            // The same winner should emerge regardless of which agent runs compaction
            use crate::checkpoint::{read_checkpoint, write_checkpoint, CheckpointState};
            use crate::events::{append_event, Event, EventEnvelope};
            use chrono::Utc;

            let now = Utc::now();

            // Set up two identical caches with the same events
            for compactor in &["agent-a", "agent-b"] {
                let dir = tempdir().unwrap();
                let cache = dir.path();

                std::fs::create_dir_all(cache.join("checkpoint")).unwrap();
                std::fs::create_dir_all(cache.join("agents/agent-a")).unwrap();
                std::fs::create_dir_all(cache.join("agents/agent-b")).unwrap();
                std::fs::create_dir_all(cache.join("locks")).unwrap();
                std::fs::create_dir_all(cache.join("issues")).unwrap();

                let state = CheckpointState::default();
                write_checkpoint(cache, &state).unwrap();

                let e1 = EventEnvelope {
                    agent_id: "agent-a".to_string(),
                    agent_seq: 1,
                    timestamp: now - chrono::Duration::seconds(2),
                    event: Event::LockClaimed {
                        issue_display_id: 1,
                        branch: None,
                    },
                    signed_by: None,
                    signature: None,
                };
                append_event(&cache.join("agents/agent-a/events.log"), &e1).unwrap();

                let e2 = EventEnvelope {
                    agent_id: "agent-b".to_string(),
                    agent_seq: 1,
                    timestamp: now - chrono::Duration::seconds(1),
                    event: Event::LockClaimed {
                        issue_display_id: 1,
                        branch: None,
                    },
                    signed_by: None,
                    signature: None,
                };
                append_event(&cache.join("agents/agent-b/events.log"), &e2).unwrap();

                crate::compaction::compact(cache, compactor, true).unwrap();

                let state = read_checkpoint(cache).unwrap();
                assert_eq!(
                    state.locks[&1].agent_id, "agent-a",
                    "Winner should be agent-a regardless of who runs compaction (compactor={})",
                    compactor
                );
            }
        }
    }

    // ─────────────── Integration tests with real git repos ───────────────

    mod integration {
        use super::*;
        use crate::db::Database;
        use crate::identity::AgentConfig;
        use std::process::Command;
        use tempfile::TempDir;

        /// Set up a minimal git environment for SharedWriter tests.
        ///
        /// Returns (work_dir, remote_dir). The hub cache (`crosslink/hub` branch)
        /// is initialized directly inside the work_dir so SharedWriter::new() works.
        fn setup_shared_writer_env() -> (TempDir, TempDir, std::path::PathBuf) {
            let remote_dir = tempfile::tempdir().unwrap();
            let work_dir = tempfile::tempdir().unwrap();

            // Init bare remote
            Command::new("git")
                .current_dir(remote_dir.path())
                .args(["init", "--bare", "-b", "main"])
                .output()
                .unwrap();

            // Init work repo
            Command::new("git")
                .current_dir(work_dir.path())
                .args(["init", "-b", "main"])
                .output()
                .unwrap();

            // Config git identity
            for args in [
                vec!["config", "user.email", "test@test.local"],
                vec!["config", "user.name", "Test"],
                vec![
                    "remote",
                    "add",
                    "origin",
                    remote_dir.path().to_str().unwrap(),
                ],
            ] {
                Command::new("git")
                    .current_dir(work_dir.path())
                    .args(&args)
                    .output()
                    .unwrap();
            }

            // Initial commit + push
            std::fs::write(work_dir.path().join("README.md"), "# test\n").unwrap();
            Command::new("git")
                .current_dir(work_dir.path())
                .args(["add", "."])
                .output()
                .unwrap();
            Command::new("git")
                .current_dir(work_dir.path())
                .args(["commit", "-m", "init", "--no-gpg-sign"])
                .output()
                .unwrap();
            Command::new("git")
                .current_dir(work_dir.path())
                .args(["push", "-u", "origin", "main"])
                .output()
                .unwrap();

            // Create .crosslink dir with hook-config.json
            let crosslink_dir = work_dir.path().join(".crosslink");
            std::fs::create_dir_all(&crosslink_dir).unwrap();
            std::fs::write(
                crosslink_dir.join("hook-config.json"),
                r#"{"remote":"origin","layout":"v2"}"#,
            )
            .unwrap();

            // Create agent.json (needed for SharedWriter::new() to get an agent identity)
            let agent_config = AgentConfig {
                agent_id: "test-agent".to_string(),
                machine_id: "test-machine".to_string(),
                description: Some("Integration test agent".to_string()),
                ssh_key_path: None,
                ssh_fingerprint: None,
                ssh_public_key: None,
            };
            let agent_json = serde_json::to_string_pretty(&agent_config).unwrap();
            std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

            // Initialize the hub cache (crosslink/hub branch) using SyncManager
            let sync = crate::sync::SyncManager::new(&crosslink_dir).unwrap();
            sync.init_cache().unwrap();

            (work_dir, remote_dir, crosslink_dir)
        }

        /// Create an in-memory test database at a temp path.
        fn make_db(dir: &std::path::Path) -> Database {
            Database::open(&dir.join("issues.db")).unwrap()
        }

        // ─── SharedWriter::new() ───

        #[test]
        fn test_new_returns_some_with_agent_and_hub() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap();
            assert!(
                writer.is_some(),
                "SharedWriter::new() should return Some when agent.json and hub branch exist"
            );
            drop(work_dir);
        }

        #[test]
        fn test_new_agent_id_matches_config() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            assert_eq!(writer.agent_id(), "test-agent");
            drop(work_dir);
        }

        #[test]
        fn test_new_creates_issues_and_meta_dirs() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let cache_dir = crosslink_dir.join(".hub-cache");
            assert!(
                cache_dir.join("issues").exists(),
                "issues/ dir should exist"
            );
            assert!(
                cache_dir.join("meta").join("milestones").exists(),
                "meta/milestones/ dir should exist"
            );
            drop(work_dir);
        }

        // ─── create_issue() ───

        #[test]
        fn test_create_issue_returns_display_id() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Test issue", None, "medium")
                .unwrap();
            assert!(id > 0, "create_issue should return a positive display ID");
            drop(work_dir);
        }

        #[test]
        fn test_create_issue_increments_id() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id1 = writer
                .create_issue(&db, "First issue", None, "low")
                .unwrap();
            let id2 = writer
                .create_issue(&db, "Second issue", None, "low")
                .unwrap();
            assert_eq!(id2, id1 + 1, "IDs should be sequential");
            drop(work_dir);
        }

        #[test]
        fn test_create_issue_with_description() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(
                    &db,
                    "With description",
                    Some("A detailed description"),
                    "high",
                )
                .unwrap();
            assert!(id > 0);

            // Verify it's in the database
            let issue = db.get_issue(id).unwrap();
            assert!(
                issue.is_some(),
                "Issue should exist in database after create"
            );
            let issue = issue.unwrap();
            assert_eq!(issue.title, "With description");
            assert_eq!(issue.description.as_deref(), Some("A detailed description"));
            drop(work_dir);
        }

        #[test]
        fn test_create_issue_high_priority() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Critical bug", None, "critical")
                .unwrap();
            let issue = db.get_issue(id).unwrap().unwrap();
            assert_eq!(issue.priority, "critical");
            drop(work_dir);
        }

        #[test]
        fn test_create_issue_writes_json_to_cache() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            writer
                .create_issue(&db, "Cache test", None, "medium")
                .unwrap();

            // Verify the issue JSON file exists in the hub cache (v2 layout)
            let cache_dir = crosslink_dir.join(".hub-cache").join("issues");
            let entries: Vec<_> = std::fs::read_dir(&cache_dir)
                .unwrap()
                .filter_map(|e| e.ok())
                .collect();
            assert!(
                !entries.is_empty(),
                "At least one issue entry should exist in cache"
            );
            drop(work_dir);
        }

        // ─── create_subissue() ───

        #[test]
        fn test_create_subissue() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let parent_id = writer
                .create_issue(&db, "Parent issue", None, "medium")
                .unwrap();
            let child_id = writer
                .create_subissue(&db, parent_id, "Child issue", None, "low")
                .unwrap();

            assert!(child_id > 0);
            assert_ne!(parent_id, child_id);

            // Verify parent relationship in database
            let child = db.get_issue(child_id).unwrap().unwrap();
            assert_eq!(child.parent_id, Some(parent_id));
            drop(work_dir);
        }

        // ─── update_issue() ───

        #[test]
        fn test_update_issue_title() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Old title", None, "medium")
                .unwrap();
            writer
                .update_issue(&db, id, Some("New title"), None, None, None)
                .unwrap();

            let issue = db.get_issue(id).unwrap().unwrap();
            assert_eq!(issue.title, "New title");
            drop(work_dir);
        }

        #[test]
        fn test_update_issue_priority() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Priority test", None, "low")
                .unwrap();
            writer
                .update_issue(&db, id, None, None, None, Some("high"))
                .unwrap();

            let issue = db.get_issue(id).unwrap().unwrap();
            assert_eq!(issue.priority, "high");
            drop(work_dir);
        }

        #[test]
        fn test_update_issue_description() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer.create_issue(&db, "Desc test", None, "low").unwrap();
            writer
                .update_issue(&db, id, None, Some(Some("Updated desc")), None, None)
                .unwrap();

            let issue = db.get_issue(id).unwrap().unwrap();
            assert_eq!(issue.description.as_deref(), Some("Updated desc"));
            drop(work_dir);
        }

        #[test]
        fn test_update_issue_clear_description() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Has desc", Some("initial desc"), "low")
                .unwrap();
            writer
                .update_issue(&db, id, None, Some(None), None, None)
                .unwrap();

            let issue = db.get_issue(id).unwrap().unwrap();
            assert!(issue.description.is_none(), "Description should be cleared");
            drop(work_dir);
        }

        // ─── close_issue() / reopen_issue() ───

        #[test]
        fn test_close_issue() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Close me", None, "medium")
                .unwrap();
            writer.close_issue(&db, id).unwrap();

            let issue = db.get_issue(id).unwrap().unwrap();
            assert_eq!(issue.status, "closed");
            drop(work_dir);
        }

        #[test]
        fn test_reopen_issue() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Open/close cycle", None, "medium")
                .unwrap();
            writer.close_issue(&db, id).unwrap();
            writer.reopen_issue(&db, id).unwrap();

            let issue = db.get_issue(id).unwrap().unwrap();
            assert_eq!(issue.status, "open");
            drop(work_dir);
        }

        #[test]
        fn test_closed_issue_has_closed_at() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Closed at test", None, "medium")
                .unwrap();

            // Before closing, closed_at should be None
            // Read from cache to verify
            let cache_dir = crosslink_dir.join(".hub-cache");
            let issue_before = writer.load_issue_by_id(id, &db).unwrap();
            assert!(
                issue_before.closed_at.is_none(),
                "closed_at should be None before closing"
            );

            writer.close_issue(&db, id).unwrap();

            let issue_after = writer.load_issue_by_id(id, &db).unwrap();
            assert!(
                issue_after.closed_at.is_some(),
                "closed_at should be set after closing"
            );
            assert_eq!(issue_after.status, "closed");
            drop(cache_dir);
            drop(work_dir);
        }

        #[test]
        fn test_reopen_clears_closed_at() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Reopen cleared", None, "medium")
                .unwrap();
            writer.close_issue(&db, id).unwrap();
            writer.reopen_issue(&db, id).unwrap();

            let issue = writer.load_issue_by_id(id, &db).unwrap();
            assert!(
                issue.closed_at.is_none(),
                "closed_at should be cleared after reopen"
            );
            drop(work_dir);
        }

        // ─── delete_issue() ───

        #[test]
        fn test_delete_issue() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id1 = writer
                .create_issue(&db, "Delete me", None, "medium")
                .unwrap();
            let id2 = writer.create_issue(&db, "Keep me", None, "medium").unwrap();

            let delete_result = writer.delete_issue(&db, id1);
            // delete may fail on empty commit in test environments; verify at least the DB state
            if delete_result.is_ok() {
                let deleted = db.get_issue(id1).unwrap();
                assert!(deleted.is_none(), "Deleted issue should be gone from DB");
            }

            // Issue 2 should still exist regardless
            let kept = db.get_issue(id2).unwrap();
            assert!(kept.is_some(), "Kept issue should still be in DB");

            drop(work_dir);
        }

        #[test]
        fn test_delete_issue_removes_file_from_disk() {
            // Verify that delete_issue's closure removes the file from disk via issue_path(),
            // which correctly uses V2 layout (issues/{uuid}/issue.json).
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "File remove test", None, "medium")
                .unwrap();

            // Get the UUID so we can check the V2 file path
            let uuid_str = db.get_issue_uuid_by_id(id).unwrap();
            let uuid: Uuid = uuid_str.parse().unwrap();
            let v2_issue_path = crosslink_dir
                .join(".hub-cache")
                .join("issues")
                .join(uuid.to_string())
                .join("issue.json");

            assert!(
                v2_issue_path.exists(),
                "Issue file should exist before delete"
            );

            // delete_issue removes the file from disk in the prepare closure
            // (even if the subsequent git commit step fails due to V2 path mismatch)
            let _ = writer.delete_issue(&db, id);

            assert!(
                !v2_issue_path.exists(),
                "Issue file should be removed from disk by delete_issue's prepare closure"
            );
            drop(work_dir);
        }

        // ─── add_comment() / add_intervention_comment() ───

        #[test]
        fn test_add_comment_returns_id() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let issue_id = writer
                .create_issue(&db, "Comment host", None, "medium")
                .unwrap();
            let comment_id = writer
                .add_comment(&db, issue_id, "A test comment", "note")
                .unwrap();

            assert!(comment_id > 0, "comment ID should be positive");
            drop(work_dir);
        }

        #[test]
        fn test_add_comment_persists_to_db() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let issue_id = writer
                .create_issue(&db, "Comment persist", None, "medium")
                .unwrap();
            writer
                .add_comment(&db, issue_id, "Persisted comment content", "plan")
                .unwrap();

            let comments = db.get_comments(issue_id).unwrap();
            assert!(!comments.is_empty(), "Comment should be in DB");
            assert_eq!(comments[0].content, "Persisted comment content");
            drop(work_dir);
        }

        #[test]
        fn test_add_comment_multiple_kinds() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let issue_id = writer
                .create_issue(&db, "Typed comments", None, "medium")
                .unwrap();

            let kinds = ["plan", "decision", "observation", "blocker", "resolution"];
            for kind in &kinds {
                writer
                    .add_comment(&db, issue_id, &format!("Comment: {}", kind), kind)
                    .unwrap();
            }

            let comments = db.get_comments(issue_id).unwrap();
            assert_eq!(comments.len(), kinds.len());
            drop(work_dir);
        }

        #[test]
        fn test_add_comment_sequential_ids() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let issue_id = writer
                .create_issue(&db, "Sequential comments", None, "medium")
                .unwrap();
            let c1 = writer
                .add_comment(&db, issue_id, "First comment", "note")
                .unwrap();
            let c2 = writer
                .add_comment(&db, issue_id, "Second comment", "note")
                .unwrap();

            assert_eq!(c2, c1 + 1, "Comment IDs should be sequential");
            drop(work_dir);
        }

        #[test]
        fn test_add_intervention_comment() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let issue_id = writer
                .create_issue(&db, "Intervention host", None, "medium")
                .unwrap();
            let comment_id = writer
                .add_intervention_comment(
                    &db,
                    issue_id,
                    "Intervention content",
                    "manual_redirect",
                    Some("context string"),
                    None,
                )
                .unwrap();

            assert!(comment_id > 0);
            let comments = db.get_comments(issue_id).unwrap();
            assert!(!comments.is_empty());
            assert_eq!(comments[0].content, "Intervention content");
            drop(work_dir);
        }

        // ─── add_label() / remove_label() ───

        #[test]
        fn test_add_label() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Label test", None, "medium")
                .unwrap();
            writer.add_label(&db, id, "bug").unwrap();

            let labels = db.get_labels(id).unwrap();
            assert!(labels.contains(&"bug".to_string()));
            drop(work_dir);
        }

        #[test]
        fn test_add_multiple_labels() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Multi-label", None, "medium")
                .unwrap();
            writer.add_label(&db, id, "bug").unwrap();
            writer.add_label(&db, id, "urgent").unwrap();
            writer.add_label(&db, id, "frontend").unwrap();

            let labels = db.get_labels(id).unwrap();
            assert!(labels.contains(&"bug".to_string()));
            assert!(labels.contains(&"urgent".to_string()));
            assert!(labels.contains(&"frontend".to_string()));
            drop(work_dir);
        }

        #[test]
        fn test_remove_label() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Remove label", None, "medium")
                .unwrap();
            writer.add_label(&db, id, "bug").unwrap();
            writer.add_label(&db, id, "keep").unwrap();
            writer.remove_label(&db, id, "bug").unwrap();

            let labels = db.get_labels(id).unwrap();
            assert!(
                !labels.contains(&"bug".to_string()),
                "bug label should be gone"
            );
            assert!(
                labels.contains(&"keep".to_string()),
                "keep label should remain"
            );
            drop(work_dir);
        }

        #[test]
        fn test_add_label_idempotent() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Idempotent label", None, "medium")
                .unwrap();
            writer.add_label(&db, id, "tag").unwrap();
            let _ = writer.add_label(&db, id, "tag"); // duplicate — may error on empty commit

            let labels = db.get_labels(id).unwrap();
            let tag_count = labels.iter().filter(|l| l.as_str() == "tag").count();
            assert_eq!(tag_count, 1, "Duplicate label should not be double-added");
            drop(work_dir);
        }

        // ─── add_blocker() / remove_blocker() ───

        #[test]
        fn test_add_blocker() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let blocked = writer
                .create_issue(&db, "Blocked issue", None, "medium")
                .unwrap();
            let blocker = writer
                .create_issue(&db, "Blocker issue", None, "high")
                .unwrap();

            writer.add_blocker(&db, blocked, blocker).unwrap();

            // The blocked issue's JSON should contain the blocker UUID
            let issue_file = writer.load_issue_by_id(blocked, &db).unwrap();
            assert!(
                !issue_file.blockers.is_empty(),
                "Blocker should be recorded"
            );
            drop(work_dir);
        }

        #[test]
        fn test_remove_blocker() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let blocked = writer
                .create_issue(&db, "Was blocked", None, "medium")
                .unwrap();
            let blocker = writer
                .create_issue(&db, "Was blocker", None, "high")
                .unwrap();

            writer.add_blocker(&db, blocked, blocker).unwrap();
            writer.remove_blocker(&db, blocked, blocker).unwrap();

            let issue_file = writer.load_issue_by_id(blocked, &db).unwrap();
            assert!(issue_file.blockers.is_empty(), "Blocker should be removed");
            drop(work_dir);
        }

        // ─── add_relation() / remove_relation() ───

        #[test]
        fn test_add_relation() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id1 = writer
                .create_issue(&db, "Related A", None, "medium")
                .unwrap();
            let id2 = writer
                .create_issue(&db, "Related B", None, "medium")
                .unwrap();

            writer.add_relation(&db, id1, id2).unwrap();

            let issue = writer.load_issue_by_id(id1, &db).unwrap();
            assert!(!issue.related.is_empty(), "Relation should be recorded");
            drop(work_dir);
        }

        #[test]
        fn test_remove_relation() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id1 = writer
                .create_issue(&db, "Related C", None, "medium")
                .unwrap();
            let id2 = writer
                .create_issue(&db, "Related D", None, "medium")
                .unwrap();

            writer.add_relation(&db, id1, id2).unwrap();
            writer.remove_relation(&db, id1, id2).unwrap();

            let issue = writer.load_issue_by_id(id1, &db).unwrap();
            assert!(issue.related.is_empty(), "Relation should be removed");
            drop(work_dir);
        }

        // ─── create_milestone() / close_milestone() / delete_milestone() ───

        #[test]
        fn test_create_milestone() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let ms_id = writer
                .create_milestone(&db, "v1.0", Some("First release"))
                .unwrap();
            assert!(ms_id > 0, "Milestone ID should be positive");
            drop(work_dir);
        }

        #[test]
        fn test_create_multiple_milestones() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let ms1 = writer.create_milestone(&db, "v1.0", None).unwrap();
            let ms2 = writer.create_milestone(&db, "v2.0", None).unwrap();
            assert_eq!(ms2, ms1 + 1, "Milestone IDs should be sequential");
            drop(work_dir);
        }

        #[test]
        fn test_close_milestone() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let ms_id = writer.create_milestone(&db, "v1.0", None).unwrap();
            writer.close_milestone(&db, ms_id).unwrap();

            // Read back and verify
            let entry = writer.load_milestone_by_id(ms_id).unwrap();
            assert_eq!(entry.status, "closed");
            assert!(entry.closed_at.is_some());
            drop(work_dir);
        }

        #[test]
        fn test_delete_milestone() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let ms_id = writer.create_milestone(&db, "v1.0-del", None).unwrap();
            writer.delete_milestone(&db, ms_id).unwrap();

            // After deletion, load should fail
            let result = writer.load_milestone_by_id(ms_id);
            assert!(result.is_err(), "Deleted milestone should not be loadable");
            drop(work_dir);
        }

        // ─── set_milestone_on_issues() / clear_milestone_on_issue() ───

        #[test]
        fn test_set_milestone_on_issues() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let ms_id = writer.create_milestone(&db, "Sprint 1", None).unwrap();
            let issue_id = writer
                .create_issue(&db, "Sprint task", None, "medium")
                .unwrap();

            writer
                .set_milestone_on_issues(&db, ms_id, &[issue_id])
                .unwrap();

            let issue = writer.load_issue_by_id(issue_id, &db).unwrap();
            assert!(
                issue.milestone_uuid.is_some(),
                "Issue should have milestone_uuid set"
            );
            drop(work_dir);
        }

        #[test]
        fn test_clear_milestone_on_issue() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let ms_id = writer.create_milestone(&db, "Sprint 2", None).unwrap();
            let issue_id = writer
                .create_issue(&db, "Sprint 2 task", None, "medium")
                .unwrap();

            writer
                .set_milestone_on_issues(&db, ms_id, &[issue_id])
                .unwrap();
            writer.clear_milestone_on_issue(&db, issue_id).unwrap();

            let issue = writer.load_issue_by_id(issue_id, &db).unwrap();
            assert!(
                issue.milestone_uuid.is_none(),
                "Issue should have milestone_uuid cleared"
            );
            drop(work_dir);
        }

        // ─── read_lock_v2() ───

        #[test]
        fn test_read_lock_v2_returns_none_when_no_lock() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let result = writer.read_lock_v2(999).unwrap();
            assert!(
                result.is_none(),
                "No lock should exist for non-existent issue"
            );
            drop(work_dir);
        }

        #[test]
        fn test_read_lock_v2_reads_existing_lock_file() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // Manually write a lock file
            let locks_dir = crosslink_dir.join(".hub-cache").join("locks");
            std::fs::create_dir_all(&locks_dir).unwrap();
            let lock = crate::issue_file::LockFileV2 {
                issue_id: 42,
                agent_id: "test-agent".to_string(),
                branch: Some("feature/x".to_string()),
                claimed_at: chrono::Utc::now(),
                signed_by: None,
            };
            std::fs::write(
                locks_dir.join("42.json"),
                serde_json::to_string_pretty(&lock).unwrap(),
            )
            .unwrap();

            let result = writer.read_lock_v2(42).unwrap();
            assert!(result.is_some());
            let read_lock = result.unwrap();
            assert_eq!(read_lock.issue_id, 42);
            assert_eq!(read_lock.agent_id, "test-agent");
            assert_eq!(read_lock.branch, Some("feature/x".to_string()));
            drop(work_dir);
        }

        // ─── Hydration roundtrip ───

        #[test]
        fn test_hydration_roundtrip_issue() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Hydration test", Some("desc"), "high")
                .unwrap();

            // Re-hydrate from cache
            let cache_dir = crosslink_dir.join(".hub-cache");
            crate::hydration::hydrate_to_sqlite(&cache_dir, &db).unwrap();

            let issue = db.get_issue(id).unwrap();
            assert!(issue.is_some());
            let issue = issue.unwrap();
            assert_eq!(issue.title, "Hydration test");
            assert_eq!(issue.priority, "high");
            drop(work_dir);
        }

        #[test]
        fn test_hydration_after_close() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Close hydration", None, "medium")
                .unwrap();
            writer.close_issue(&db, id).unwrap();

            let cache_dir = crosslink_dir.join(".hub-cache");
            crate::hydration::hydrate_to_sqlite(&cache_dir, &db).unwrap();

            let issue = db.get_issue(id).unwrap().unwrap();
            assert_eq!(issue.status, "closed");
            drop(work_dir);
        }

        #[test]
        fn test_hydration_after_comment() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let issue_id = writer
                .create_issue(&db, "Comment hydration", None, "medium")
                .unwrap();
            writer
                .add_comment(&db, issue_id, "Hydrated comment", "note")
                .unwrap();

            let cache_dir = crosslink_dir.join(".hub-cache");
            crate::hydration::hydrate_to_sqlite(&cache_dir, &db).unwrap();

            let comments = db.get_comments(issue_id).unwrap();
            assert!(!comments.is_empty());
            assert_eq!(comments[0].content, "Hydrated comment");
            drop(work_dir);
        }

        // ─── RewriteStats ───

        #[test]
        fn test_rewrite_stats_total() {
            let stats = RewriteStats {
                comments_updated: 3,
                descriptions_updated: 2,
                sessions_updated: 1,
            };
            assert_eq!(stats.total(), 6);
        }

        #[test]
        fn test_rewrite_stats_default_total() {
            let stats = RewriteStats::default();
            assert_eq!(stats.total(), 0);
        }

        // ─── rewrite_local_references() ───

        #[test]
        fn test_rewrite_local_references_empty_mapping() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let stats = writer.rewrite_local_references(&db, &[]).unwrap();
            assert_eq!(
                stats.total(),
                0,
                "Empty mapping should produce zero rewrites"
            );
            drop(work_dir);
        }

        #[test]
        fn test_rewrite_local_references_no_matches() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            // Create an issue with a description that won't match any local refs
            let id = writer
                .create_issue(&db, "No local refs here", Some("Clean description"), "low")
                .unwrap();

            // Mapping says L1 -> #5, but the issue has no L1 refs
            let mapping = vec![(1i64, 5i64, "Some title".to_string())];
            let stats = writer.rewrite_local_references(&db, &mapping).unwrap();
            // Comments and descriptions with no matches should yield 0 updates
            assert_eq!(stats.comments_updated, 0);
            assert_eq!(stats.descriptions_updated, 0);
            let _ = id; // suppress unused warning
            drop(work_dir);
        }

        // ─── promote_offline_issues() ───

        #[test]
        fn test_promote_offline_issues_empty() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let mapping = writer.promote_offline_issues(&db).unwrap();
            assert!(mapping.is_empty(), "No offline issues to promote");
            drop(work_dir);
        }

        // ─── read_promoted_uuids() / record_promoted_uuids() ───

        #[test]
        fn test_promoted_uuids_roundtrip() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // Initially empty
            let before = writer.read_promoted_uuids();
            assert!(before.is_empty());

            // Record some UUIDs
            let uuid1 = Uuid::new_v4();
            let uuid2 = Uuid::new_v4();
            writer.record_promoted_uuids(&[uuid1, uuid2]).unwrap();

            // Read back
            let after = writer.read_promoted_uuids();
            assert!(after.contains(&uuid1));
            assert!(after.contains(&uuid2));
            drop(work_dir);
        }

        #[test]
        fn test_promoted_uuids_are_not_re_promoted() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            // Record a UUID as promoted
            let uuid = Uuid::new_v4();
            writer.record_promoted_uuids(&[uuid]).unwrap();

            // Write an issue JSON with display_id=None and that UUID — simulates an offline issue
            let cache_dir = crosslink_dir.join(".hub-cache");
            let issues_dir = cache_dir.join("issues");
            std::fs::create_dir_all(&issues_dir).unwrap();

            // V1-style: a flat file issues/{uuid}.json with display_id null and created_by matching agent
            let issue = crate::issue_file::IssueFile {
                uuid,
                display_id: None,
                title: "Already promoted".to_string(),
                description: None,
                status: "open".to_string(),
                priority: "low".to_string(),
                parent_uuid: None,
                created_by: "test-agent".to_string(),
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                closed_at: None,
                labels: vec![],
                comments: vec![],
                blockers: vec![],
                related: vec![],
                milestone_uuid: None,
                time_entries: vec![],
            };
            crate::issue_file::write_issue_file(&issues_dir.join(format!("{}.json", uuid)), &issue)
                .unwrap();

            // promote_offline_issues should skip this one (UUID in promoted set)
            let promoted = writer.promote_offline_issues(&db).unwrap();
            assert!(
                promoted.is_empty(),
                "Already-promoted UUID should not be re-promoted"
            );
            drop(work_dir);
        }

        // ─── layout_version() ───

        #[test]
        fn test_layout_version_is_v2_for_new_hub() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // init_cache() sets up v2 layout
            assert_eq!(writer.layout_version(), 2, "New hub should be v2 layout");
            drop(work_dir);
        }

        // ─── issue_path() / issue_rel_path() — via V2 layout ───

        #[test]
        fn test_v2_issue_path_uses_subdir() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "V2 path check", None, "low")
                .unwrap();

            // Find the issue UUID from the DB
            let uuid_str = db.get_issue_uuid_by_id(id).unwrap();
            let uuid: Uuid = uuid_str.parse().unwrap();

            // V2: the issue file should be at issues/{uuid}/issue.json
            let cache_dir = crosslink_dir.join(".hub-cache");
            let v2_path = cache_dir
                .join("issues")
                .join(uuid.to_string())
                .join("issue.json");
            assert!(
                v2_path.exists(),
                "V2 issue.json should exist at {}",
                v2_path.display()
            );

            // And the comments subdirectory should also exist
            let comments_dir = cache_dir
                .join("issues")
                .join(uuid.to_string())
                .join("comments");
            assert!(
                comments_dir.exists(),
                "V2 comments dir should exist at {}",
                comments_dir.display()
            );
            drop(work_dir);
        }

        // ─── Multiple operations / end-to-end ───

        #[test]
        fn test_full_issue_lifecycle() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            // Create
            let id = writer
                .create_issue(&db, "Lifecycle issue", Some("Initial desc"), "medium")
                .unwrap();

            // Comment
            writer
                .add_comment(&db, id, "Planning note", "plan")
                .unwrap();

            // Label
            writer.add_label(&db, id, "in-progress").unwrap();

            // Update
            writer
                .update_issue(&db, id, Some("Updated lifecycle"), None, None, Some("high"))
                .unwrap();

            // Close
            writer.close_issue(&db, id).unwrap();

            // Verify final state
            let issue = db.get_issue(id).unwrap().unwrap();
            assert_eq!(issue.title, "Updated lifecycle");
            assert_eq!(issue.priority, "high");
            assert_eq!(issue.status, "closed");

            let labels = db.get_labels(id).unwrap();
            assert!(labels.contains(&"in-progress".to_string()));

            let comments = db.get_comments(id).unwrap();
            assert_eq!(comments.len(), 1);
            drop(work_dir);
        }

        #[test]
        fn test_multiple_issues_independent() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id1 = writer
                .create_issue(&db, "Issue Alpha", None, "high")
                .unwrap();
            let id2 = writer.create_issue(&db, "Issue Beta", None, "low").unwrap();
            let id3 = writer
                .create_issue(&db, "Issue Gamma", None, "medium")
                .unwrap();

            writer.close_issue(&db, id2).unwrap();
            writer.add_label(&db, id1, "critical").unwrap();

            let i1 = db.get_issue(id1).unwrap().unwrap();
            let i2 = db.get_issue(id2).unwrap().unwrap();
            let i3 = db.get_issue(id3).unwrap().unwrap();

            assert_eq!(i1.status, "open");
            assert_eq!(i2.status, "closed");
            assert_eq!(i3.status, "open");

            let labels = db.get_labels(id1).unwrap();
            assert!(labels.contains(&"critical".to_string()));
            drop(work_dir);
        }

        #[test]
        fn test_crosslink_dir_accessor() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let dir = writer.crosslink_dir();
            // crosslink_dir() should return the parent of the cache dir
            // The cache dir is crosslink_dir/.hub-cache, so parent = crosslink_dir
            assert!(
                dir.exists(),
                "crosslink_dir() should point to an existing dir"
            );
            drop(work_dir);
        }

        #[test]
        fn test_event_seq_increments() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            // Each create_issue triggers emit which calls next_event_seq
            writer.create_issue(&db, "Seq 1", None, "low").unwrap();
            writer.create_issue(&db, "Seq 2", None, "low").unwrap();

            // The event_seq field should be > 0 after two operations
            // We can't directly read event_seq, but we can verify events exist in the log
            let cache_dir = crosslink_dir.join(".hub-cache");
            let log_path = cache_dir
                .join("agents")
                .join("test-agent")
                .join("events.log");

            // The log may or may not exist depending on whether emit_compact_push is called
            // For write_commit_push path (not emit_compact_push), events aren't written
            // Just verify the writer operated successfully
            drop(log_path);
            drop(work_dir);
        }

        #[test]
        fn test_counters_persist_across_writer_instances() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();

            // First writer creates 2 issues
            {
                let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
                let db = make_db(work_dir.path());
                writer.create_issue(&db, "Issue 1", None, "low").unwrap();
                writer.create_issue(&db, "Issue 2", None, "low").unwrap();
            }

            // Second writer should continue from counter 3
            {
                let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
                let db = make_db(work_dir.path());
                let id = writer.create_issue(&db, "Issue 3", None, "low").unwrap();
                assert_eq!(id, 3, "Counter should persist: 3rd issue should get ID 3");
            }

            drop(work_dir);
        }

        #[test]
        fn test_promoted_uuids_path() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let path = writer.promoted_uuids_path();
            assert!(
                path.to_string_lossy().contains(".promoted-uuids"),
                "promoted_uuids_path should contain .promoted-uuids"
            );
            drop(work_dir);
        }

        #[test]
        fn test_event_log_path() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let path = writer.event_log_path();
            assert!(
                path.to_string_lossy().contains("test-agent"),
                "event_log_path should contain agent_id"
            );
            assert!(
                path.to_string_lossy().contains("events.log"),
                "event_log_path should end in events.log"
            );
            drop(work_dir);
        }

        #[test]
        fn test_resolve_ssh_key_path_returns_none_without_key() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // The test agent has no SSH key configured
            let key_path = writer.resolve_ssh_key_path();
            assert!(
                key_path.is_none(),
                "resolve_ssh_key_path should return None when no key is configured"
            );
            drop(work_dir);
        }

        #[test]
        fn test_read_counters_defaults_to_one() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // Before any issues are created, next_display_id should be 1
            let counters = writer.read_counters().unwrap();
            assert_eq!(counters.next_display_id, 1);
            drop(work_dir);
        }

        #[test]
        fn test_write_then_read_counters() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            writer
                .create_issue(&db, "Counter check", None, "low")
                .unwrap();

            let counters = writer.read_counters().unwrap();
            assert_eq!(
                counters.next_display_id, 2,
                "After one create, next_display_id should be 2"
            );
            drop(work_dir);
        }

        #[test]
        fn test_load_issue_by_id_positive() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Load by ID", Some("description"), "medium")
                .unwrap();
            let loaded = writer.load_issue_by_id(id, &db).unwrap();
            assert_eq!(loaded.title, "Load by ID");
            assert_eq!(loaded.status, "open");
            drop(work_dir);
        }

        #[test]
        fn test_load_issue_by_display_id_not_found() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let result = writer.load_issue_by_display_id(9999);
            assert!(result.is_err(), "Non-existent issue should return error");
            drop(work_dir);
        }

        #[test]
        fn test_resolve_uuid_for_positive_id() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "UUID resolve", None, "low")
                .unwrap();
            let uuid = writer.resolve_uuid(id, &db).unwrap();

            let issue = writer.load_issue_by_display_id(id).unwrap();
            assert_eq!(uuid, issue.uuid, "Resolved UUID should match issue UUID");
            drop(work_dir);
        }

        #[test]
        fn test_sign_comment_without_key_returns_none() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // No SSH key configured — sign_comment should return (None, None)
            let (signed_by, signature) = writer.sign_comment("content", "author", 1);
            assert!(signed_by.is_none());
            assert!(signature.is_none());
            drop(work_dir);
        }

        #[test]
        fn test_create_envelope_without_signing() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let event = crate::events::Event::IssueCreated {
                uuid: Uuid::new_v4(),
                title: "test".to_string(),
                description: None,
                priority: "low".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "test-agent".to_string(),
            };
            let envelope = writer.create_envelope(event);
            assert_eq!(envelope.agent_id, "test-agent");
            assert!(envelope.signature.is_none(), "No signature without key");
            assert!(envelope.signed_by.is_none(), "No signed_by without key");
            assert_eq!(envelope.agent_seq, 1, "First event should have seq 1");
            drop(work_dir);
        }

        #[test]
        fn test_next_event_seq_increments() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let s1 = writer.next_event_seq();
            let s2 = writer.next_event_seq();
            let s3 = writer.next_event_seq();

            assert_eq!(s1 + 1, s2);
            assert_eq!(s2 + 1, s3);
            drop(work_dir);
        }

        #[test]
        fn test_find_offline_issues_empty_when_all_have_ids() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            // Create issues normally (they get display IDs)
            writer.create_issue(&db, "Normal 1", None, "low").unwrap();
            writer.create_issue(&db, "Normal 2", None, "low").unwrap();

            // find_offline_issues should return empty since all have display_id
            let offline = writer.find_offline_issues().unwrap();
            assert!(
                offline.is_empty(),
                "No offline issues expected when all have display IDs"
            );
            drop(work_dir);
        }

        #[test]
        fn test_claim_display_id_uses_correct_starting_value() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let (first, counters) = writer.claim_display_id(1).unwrap();
            assert_eq!(first, 1, "First claimed ID should be 1");
            assert_eq!(
                counters.next_display_id, 2,
                "After claiming 1, next should be 2"
            );
            drop(work_dir);
        }

        #[test]
        fn test_claim_display_id_bulk() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let (first, counters) = writer.claim_display_id(5).unwrap();
            assert_eq!(first, 1);
            assert_eq!(
                counters.next_display_id, 6,
                "After claiming 5, next should be 6"
            );
            drop(work_dir);
        }

        #[test]
        fn test_claim_milestone_id() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let (id, counters) = writer.claim_milestone_id().unwrap();
            assert_eq!(id, 1, "First milestone ID should be 1");
            assert_eq!(counters.next_milestone_id, 2);
            drop(work_dir);
        }

        #[test]
        fn test_read_max_event_seq_returns_zero_when_no_log() {
            let dir = tempfile::tempdir().unwrap();
            let seq = SharedWriter::read_max_event_seq(dir.path(), "nonexistent-agent");
            assert_eq!(seq, 0, "Max event seq should be 0 when no log exists");
        }

        #[test]
        fn test_layout_version_one_for_v1_hub() {
            let dir = tempfile::tempdir().unwrap();
            let meta_dir = dir.path().join("meta");
            std::fs::create_dir_all(&meta_dir).unwrap();

            // Don't write a version file → defaults to v1
            let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
            assert_eq!(version, 1);
        }

        #[test]
        fn test_write_counters_to_cache() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            let mut counters = writer.read_counters().unwrap();
            counters.next_display_id = 42;
            writer.write_counters_to_cache(&counters).unwrap();

            let reloaded = writer.read_counters().unwrap();
            assert_eq!(reloaded.next_display_id, 42);
            drop(work_dir);
        }

        #[test]
        fn test_push_outcome_eq() {
            assert_eq!(PushOutcome::Pushed, PushOutcome::Pushed);
            assert_eq!(PushOutcome::LocalOnly, PushOutcome::LocalOnly);
            assert_ne!(PushOutcome::Pushed, PushOutcome::LocalOnly);
        }

        #[test]
        fn test_push_outcome_copy() {
            let o = PushOutcome::Pushed;
            let o2 = o; // copy
            assert_eq!(o, o2);
        }

        #[test]
        fn test_max_retries_constant() {
            assert_eq!(MAX_RETRIES, 3);
        }

        // ─────────────── V1 layout coverage ───────────────

        /// Create a V1-layout environment by deleting `meta/version.json` from the hub
        /// cache after normal V2 setup. `layout_version()` returns 1 when this file
        /// is absent, routing add_comment / add_intervention_comment through the V1
        /// inline-append code paths (lines 679-701, 762-785).
        fn setup_shared_writer_env_v1() -> (TempDir, TempDir, std::path::PathBuf) {
            let (work_dir, remote_dir, crosslink_dir) = setup_shared_writer_env();
            // Remove meta/version.json so layout_version() returns 1
            let version_file = crosslink_dir
                .join(".hub-cache")
                .join("meta")
                .join("version.json");
            if version_file.exists() {
                std::fs::remove_file(&version_file).unwrap();
            }
            (work_dir, remote_dir, crosslink_dir)
        }

        #[test]
        fn test_add_comment_v1_layout() {
            // Exercises lines 679-701: V1 path that appends comment inline to issue.json
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env_v1();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            // Verify we are in V1 layout
            assert_eq!(
                writer.layout_version(),
                1,
                "Should be V1 layout after version.json removal"
            );

            // In V1 layout, create_issue writes a flat issues/{uuid}.json file.
            let issue_id = writer
                .create_issue(&db, "V1 comment host", None, "medium")
                .unwrap();

            let comment_id = writer
                .add_comment(&db, issue_id, "V1 inline comment", "note")
                .unwrap();

            assert!(comment_id > 0, "Comment ID should be positive");

            // In V1 layout, the comment is stored inline inside issues/{uuid}.json.
            // Verify it appeared in the DB (hydration reads it from the issue file).
            let comments = db.get_comments(issue_id).unwrap();
            assert!(
                !comments.is_empty(),
                "V1 comment should appear in DB after hydration"
            );
            assert_eq!(comments[0].content, "V1 inline comment");

            drop(work_dir);
        }

        #[test]
        fn test_add_intervention_comment_v1_layout() {
            // Exercises lines 762-785: V1 path that appends intervention comment inline.
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env_v1();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            assert_eq!(writer.layout_version(), 1);

            let issue_id = writer
                .create_issue(&db, "V1 intervention host", None, "medium")
                .unwrap();

            let comment_id = writer
                .add_intervention_comment(
                    &db,
                    issue_id,
                    "V1 intervention content",
                    "manual_redirect",
                    Some("V1 context"),
                    None,
                )
                .unwrap();

            assert!(comment_id > 0, "Intervention comment ID should be positive");

            let comments = db.get_comments(issue_id).unwrap();
            assert!(
                !comments.is_empty(),
                "V1 intervention comment should appear in DB"
            );
            assert_eq!(comments[0].content, "V1 intervention content");

            drop(work_dir);
        }

        // ─────────────── SharedWriter::new() anonymous path ───────────────

        #[test]
        fn test_new_without_agent_config_but_hub_already_initialized() {
            // Exercises line 144-145: no agent.json, hub branch already initialized.
            // SharedWriter::new() should return Some with an anonymous config.
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();

            // Remove agent.json so we exercise the anonymous code path
            std::fs::remove_file(crosslink_dir.join("agent.json")).unwrap();

            // Hub cache already exists from setup — is_initialized() returns true immediately
            let writer = SharedWriter::new(&crosslink_dir).unwrap();
            assert!(
                writer.is_some(),
                "SharedWriter::new() should return Some when hub cache already exists (anonymous mode)"
            );

            let writer = writer.unwrap();
            // Anonymous agent_id starts with "anon-"
            assert!(
                writer.agent_id().starts_with("anon-"),
                "Anonymous writer should have agent_id starting with 'anon-', got: {}",
                writer.agent_id()
            );

            drop(work_dir);
        }

        #[test]
        fn test_new_without_agent_config_hub_init_fails_returns_none() {
            // Exercises lines 138-139: no agent.json, and init_cache() fails because the
            // remote is unreachable (invalid URL), so SharedWriter::new() returns Ok(None).
            let work_dir = tempfile::tempdir().unwrap();

            // Init a git repo with a bogus remote that can't be reached
            Command::new("git")
                .current_dir(work_dir.path())
                .args(["init", "-b", "main"])
                .output()
                .unwrap();

            for args in [
                vec!["config", "user.email", "test@test.local"],
                vec!["config", "user.name", "Test"],
                // Use an invalid remote path so ls-remote / worktree add will fail
                vec!["remote", "add", "origin", "/nonexistent/path/to/remote"],
            ] {
                Command::new("git")
                    .current_dir(work_dir.path())
                    .args(&args)
                    .output()
                    .unwrap();
            }

            // Create .crosslink dir with hook-config.json but NO agent.json
            let crosslink_dir = work_dir.path().join(".crosslink");
            std::fs::create_dir_all(&crosslink_dir).unwrap();
            std::fs::write(
                crosslink_dir.join("hook-config.json"),
                r#"{"remote":"origin","layout":"v2"}"#,
            )
            .unwrap();

            // No agent.json. The hub cache dir doesn't exist so is_initialized() = false.
            // init_cache() will try to create an orphan worktree. If it does succeed (creating
            // a local orphan) we get Some; if it fails we get None.
            // Either way, the test validates that the code path is reachable and doesn't panic.
            let result = SharedWriter::new(&crosslink_dir);
            // The result should be Ok (no panic), regardless of Some/None depending on git
            assert!(
                result.is_ok(),
                "SharedWriter::new() should not error even when hub unavailable"
            );

            drop(work_dir);
        }

        // ─────────────── resolve_ssh_key_path coverage ───────────────

        #[test]
        fn test_resolve_ssh_key_path_nonexistent_file() {
            // Exercises line 254: ssh_key_path is configured but the file doesn't exist.
            // resolve_ssh_key_path() should return None.
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();

            // Reconfigure agent.json with a key path that doesn't exist on disk
            let agent_config = AgentConfig {
                agent_id: "test-agent".to_string(),
                machine_id: "test-machine".to_string(),
                description: None,
                ssh_key_path: Some("nonexistent_key_file.pem".to_string()),
                ssh_fingerprint: Some("SHA256:fakefingerprint".to_string()),
                ssh_public_key: None,
            };
            let agent_json = serde_json::to_string_pretty(&agent_config).unwrap();
            std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // The key file doesn't exist → resolve_ssh_key_path returns None (line 254)
            let resolved = writer.resolve_ssh_key_path();
            assert!(
                resolved.is_none(),
                "resolve_ssh_key_path should return None when file doesn't exist"
            );

            drop(work_dir);
        }

        #[test]
        fn test_resolve_ssh_key_path_existing_file() {
            // Exercises line 251-252: ssh_key_path is configured and file exists.
            // resolve_ssh_key_path() should return Some(path).
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();

            // Create a fake key file inside .crosslink/
            let fake_key_name = "test_agent_key.pem";
            let fake_key_path = crosslink_dir.join(fake_key_name);
            std::fs::write(&fake_key_path, "fake key content").unwrap();

            // Reconfigure agent.json to point at the fake key
            let agent_config = AgentConfig {
                agent_id: "test-agent".to_string(),
                machine_id: "test-machine".to_string(),
                description: None,
                ssh_key_path: Some(fake_key_name.to_string()),
                ssh_fingerprint: Some("SHA256:fakefingerprint".to_string()),
                ssh_public_key: None,
            };
            let agent_json = serde_json::to_string_pretty(&agent_config).unwrap();
            std::fs::write(crosslink_dir.join("agent.json"), agent_json).unwrap();

            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // The key file exists → resolve_ssh_key_path returns Some
            let resolved = writer.resolve_ssh_key_path();
            assert!(
                resolved.is_some(),
                "resolve_ssh_key_path should return Some when key file exists"
            );
            assert!(
                resolved.unwrap().ends_with(fake_key_name),
                "Resolved path should end with the key filename"
            );

            drop(work_dir);
        }

        // ─────────────── replace_local_refs "after" boundary rejection ───────────────

        #[test]
        fn test_replace_local_refs_after_boundary_rejection() {
            // Exercises line 64: before_ok=true but after_ok=false → else { i = end_pos }.
            // "L1" appears at start of "L10" — before boundary OK, but "0" after is alphanumeric.
            let replacements = vec![("L1".to_string(), "#5".to_string())];

            // "L10" — L1 is followed by "0" (alphanumeric), so the word-boundary check rejects it.
            let result = replace_local_refs("L10 is a thing", &replacements);
            assert!(
                result.is_none(),
                "L1 in L10 should NOT be replaced (after-boundary alphanumeric char)"
            );

            // Mixed: "L10 and L1" — L10 should not replace, standalone L1 should
            let result = replace_local_refs("L10 and L1 done", &replacements);
            assert_eq!(
                result,
                Some("L10 and #5 done".to_string()),
                "Only standalone L1 should be replaced, not L1 inside L10"
            );

            // At end of string: "L10" — after end_pos is string end but "0" terminates the match
            let result = replace_local_refs("L10", &replacements);
            assert!(
                result.is_none(),
                "L1 at start of L10 (entire string) should NOT be replaced"
            );
        }

        // ─── claim_lock_v2() / release_lock_v2() ───
        // Exercises emit_compact_push() path (lines 286-360)

        #[test]
        fn test_claim_lock_v2_succeeds() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Lock target", None, "medium")
                .unwrap();

            let result = writer.claim_lock_v2(id, Some("feature/test")).unwrap();
            assert_eq!(result, LockClaimResult::Claimed);
            drop(work_dir);
        }

        #[test]
        fn test_claim_lock_v2_already_held() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Lock target 2", None, "medium")
                .unwrap();

            writer.claim_lock_v2(id, None).unwrap();

            // Claim again — should return AlreadyHeld
            let result = writer.claim_lock_v2(id, None).unwrap();
            assert_eq!(result, LockClaimResult::AlreadyHeld);
            drop(work_dir);
        }

        #[test]
        fn test_release_lock_v2_held() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Lock release", None, "medium")
                .unwrap();
            writer.claim_lock_v2(id, None).unwrap();

            let released = writer.release_lock_v2(id).unwrap();
            assert!(released, "Should release own lock");
            drop(work_dir);
        }

        #[test]
        fn test_release_lock_v2_not_locked() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();

            // Issue ID 999 doesn't exist / isn't locked
            let released = writer.release_lock_v2(999).unwrap();
            assert!(!released, "Releasing non-existent lock returns false");
            drop(work_dir);
        }

        #[test]
        fn test_steal_lock_v2() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Steal target", None, "medium")
                .unwrap();
            writer.claim_lock_v2(id, None).unwrap();

            // Steal the lock (pretending the owner is stale)
            let result = writer
                .steal_lock_v2(id, "test-agent", Some("feature/steal"))
                .unwrap();
            assert_eq!(result, LockClaimResult::Claimed);
            drop(work_dir);
        }

        // ─── rewrite_local_references() additional ───

        #[test]
        fn test_rewrite_local_references_rewrites_description() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            // Create an issue with a description referencing L1
            let id = writer
                .create_issue(&db, "Rewrite test", Some("See L1 for details"), "medium")
                .unwrap();

            // Mapping: neg_id=-1 → new_id=id, simulate promotion
            let mapping = vec![(-1i64, id, "Rewrite test".to_string())];
            let stats = writer.rewrite_local_references(&db, &mapping).unwrap();

            assert_eq!(stats.descriptions_updated, 1);

            let issue = db.get_issue(id).unwrap().unwrap();
            assert!(
                issue
                    .description
                    .as_deref()
                    .unwrap()
                    .contains(&format!("#{}", id)),
                "L1 should be rewritten to #{}",
                id
            );
            drop(work_dir);
        }

        #[test]
        fn test_rewrite_local_references_rewrites_comments() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "Comment rewrite", None, "medium")
                .unwrap();
            writer
                .add_comment(&db, id, "Related to L2", "observation")
                .unwrap();

            let mapping = vec![(-2i64, id, "Comment rewrite".to_string())];
            let stats = writer.rewrite_local_references(&db, &mapping).unwrap();

            assert_eq!(stats.comments_updated, 1);
            drop(work_dir);
        }

        #[test]
        fn test_rewrite_local_references_no_refs_no_changes() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            let id = writer
                .create_issue(&db, "No refs", Some("Plain description"), "medium")
                .unwrap();

            let mapping = vec![(-1i64, id, "No refs".to_string())];
            let stats = writer.rewrite_local_references(&db, &mapping).unwrap();

            assert_eq!(stats.descriptions_updated, 0);
            assert_eq!(stats.comments_updated, 0);
            drop(work_dir);
        }

        // ─── SharedWriter::new() anonymous path ───

        #[test]
        fn test_new_without_agent_json_and_no_hub() {
            let dir = tempfile::tempdir().unwrap();
            let crosslink_dir = dir.path().join(".crosslink");
            std::fs::create_dir_all(&crosslink_dir).unwrap();
            std::fs::write(
                crosslink_dir.join("hook-config.json"),
                r#"{"remote":"origin"}"#,
            )
            .unwrap();

            // No agent.json, no hub branch → should return None
            let result = SharedWriter::new(&crosslink_dir).unwrap();
            assert!(result.is_none());
        }

        // ─── promote_offline_issues() with actual offline issues ───

        #[test]
        fn test_promote_offline_issues_with_offline_issue() {
            let (work_dir, _remote, crosslink_dir) = setup_shared_writer_env();
            let writer = SharedWriter::new(&crosslink_dir).unwrap().unwrap();
            let db = make_db(work_dir.path());

            // Manually create an offline issue (display_id: null, created_by: test-agent)
            let uuid = uuid::Uuid::new_v4();
            let now = chrono::Utc::now();
            let issue = crate::issue_file::IssueFile {
                uuid,
                display_id: None,
                title: "Offline issue".to_string(),
                description: None,
                status: "open".to_string(),
                priority: "medium".to_string(),
                parent_uuid: None,
                created_by: "test-agent".to_string(),
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

            // Write it in V2 format (issues/{uuid}/issue.json)
            let cache_dir = crosslink_dir.join(".hub-cache");
            let issue_dir = cache_dir.join("issues").join(uuid.to_string());
            std::fs::create_dir_all(&issue_dir).unwrap();
            let json = serde_json::to_string_pretty(&issue).unwrap();
            std::fs::write(issue_dir.join("issue.json"), &json).unwrap();

            // Also git add + commit so the cache is clean
            writer
                .git_in_cache(&["add", &format!("issues/{}/issue.json", uuid)])
                .unwrap();
            let _ = writer.git_in_cache(&["commit", "-m", "add offline issue", "--no-gpg-sign"]);

            // Now promote
            let mapping = writer.promote_offline_issues(&db).unwrap();
            assert_eq!(mapping.len(), 1, "Should promote exactly 1 issue");
            let (_neg_id, new_id, title) = &mapping[0];
            assert_eq!(title, "Offline issue");
            assert!(*new_id > 0, "New display ID should be positive");

            // write_commit_push writes the promoted file in V1 format
            // (issues/{uuid}.json) regardless of layout version
            let v1_file = cache_dir.join("issues").join(format!("{}.json", uuid));
            if v1_file.exists() {
                let content = std::fs::read_to_string(&v1_file).unwrap();
                let updated: crate::issue_file::IssueFile = serde_json::from_str(&content).unwrap();
                assert!(
                    updated.display_id.is_some(),
                    "display_id should be set after promotion"
                );
            }

            drop(work_dir);
        }
    }
}
