//! Offline issue promotion and local reference rewriting.

use anyhow::Result;
use std::cell::Cell;
use uuid::Uuid;

use crate::db::Database;
use crate::issue_file::read_issue_file;

use super::core::{PushOutcome, SharedWriter, WriteSet};

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
pub(super) fn replace_local_refs(text: &str, replacements: &[(String, String)]) -> Option<String> {
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

impl SharedWriter {
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
            // Still offline -- revert display_id assignments
            for (uuid, _) in &offline_info {
                let path = self.issue_path(uuid);
                if let Ok(mut issue) = read_issue_file(&path) {
                    issue.display_id = None;
                    if let Ok(json) = serde_json::to_string_pretty(&issue) {
                        // INTENTIONAL: reverting display_id on disk is best-effort — offline issues will be re-assigned on next push
                        let _ = std::fs::write(&path, json);
                    }
                }
            }
            // Revert counter
            if let Ok(mut counters) = self.read_counters() {
                counters.next_display_id -= count;
                // INTENTIONAL: counter revert is best-effort — counters will be corrected on next push
                let _ = self.write_counters_to_cache(&counters);
            }
            // Amend the commit to reflect reverted state
            if let Err(e) = self.git_in_cache(&["add", "."]) {
                eprintln!("Warning: failed to stage reverted state: {}", e);
            }
            if let Err(e) = self.git_in_cache(&["commit", "--amend", "--no-edit"]) {
                eprintln!("Warning: failed to commit reverted state: {}", e);
                // INTENTIONAL: last-resort dirty state cleanup is best-effort — prevents poisoning future syncs
                let _ = self.sync.clean_dirty_state();
            }
            return Ok(vec![]);
        }

        // Re-hydrate with new positive IDs
        self.hydrate_with_retry(db)?;

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

        // Build replacement map: "L1" -> "#5", "L2" -> "#6"
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
            // INTENTIONAL: push is best-effort — rewritten references will be pushed on next sync
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
}
