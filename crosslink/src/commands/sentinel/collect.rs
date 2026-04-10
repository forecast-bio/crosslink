use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::db::Database;

use super::seen_set::gh_comment_already_posted;

/// Statistics from a result collection pass.
#[derive(Debug, Default)]
pub struct CollectStats {
    pub collected: u32,
    pub orphaned: u32,
    pub still_running: u32,
}

/// Poll completed agents, read findings, post results to GitHub, update records.
///
/// Runs every sentinel cycle (after dispatch phase in oneshot, every cycle in watch).
pub fn collect_completed(db: &Database, crosslink_dir: &Path) -> Result<CollectStats> {
    let pending = db.get_pending_dispatches()?;
    let mut stats = CollectStats::default();

    let root = repo_root(crosslink_dir)?;

    for dispatch in &pending {
        let Some(agent_id) = &dispatch.agent_id else {
            continue;
        };

        // Check if worktree still exists
        let wt_path = root.join(".worktrees").join(agent_id);
        if !wt_path.exists() {
            db.update_dispatch_outcome(dispatch.id, "orphaned", "worktree removed")?;
            stats.orphaned += 1;
            continue;
        }

        // Check sentinel file for completion
        let status_path = wt_path.join(".kickoff-status");
        let Ok(status_content) = std::fs::read_to_string(&status_path) else {
            stats.still_running += 1;
            continue;
        };

        let outcome = if status_content.trim().starts_with("DONE") {
            "success"
        } else {
            "failure"
        };

        // Read agent findings from crosslink comments on the linked issue
        let findings = if let Some(issue_id) = dispatch.crosslink_issue_id {
            read_agent_findings(db, issue_id)
        } else {
            String::new()
        };

        // Compute duration
        let duration = compute_duration(&dispatch.created_at);

        // Build structured comment
        let comment_body = build_replicate_template(
            outcome,
            agent_id,
            dispatch.model_used.as_deref().unwrap_or("unknown"),
            dispatch.attempt_number,
            &duration,
            &findings,
            dispatch.id,
        );

        // Post to GH issue (with Layer 4 dedup check)
        if let Some(gh_num) = dispatch.gh_issue_number {
            match gh_comment_already_posted(gh_num, dispatch.id) {
                Ok(true) => {
                    tracing::debug!("sentinel #{} already posted to GH#{}", dispatch.id, gh_num);
                }
                _ => {
                    if let Err(e) = post_gh_comment(gh_num, &comment_body) {
                        tracing::warn!("failed to post results to GH#{gh_num}: {e}");
                    }
                }
            }
        }

        db.update_dispatch_outcome(dispatch.id, outcome, &findings)?;
        stats.collected += 1;
    }

    Ok(stats)
}

/// Resolve the main repo root from a crosslink directory.
fn repo_root(crosslink_dir: &Path) -> Result<std::path::PathBuf> {
    // .crosslink lives at <repo>/.crosslink, so parent is repo root.
    // But in a worktree, crosslink_dir may be <repo>/.worktrees/<slug>/.crosslink.
    // Use git to resolve reliably.
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(crosslink_dir)
        .output()
        .context("Failed to run git rev-parse")?;
    if !output.status.success() {
        anyhow::bail!("Not in a git repository");
    }
    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(root))
}

/// Read observation and resolution comments from a crosslink issue.
fn read_agent_findings(db: &Database, issue_id: i64) -> String {
    let comments = match db.get_comments(issue_id) {
        Ok(c) => c,
        Err(_) => return String::new(),
    };

    comments
        .iter()
        .filter(|c| c.kind == "observation" || c.kind == "resolution")
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

/// Compute human-readable duration from an RFC3339 start time to now.
fn compute_duration(started_at: &str) -> String {
    let Ok(start) = chrono::DateTime::parse_from_rfc3339(started_at) else {
        return "unknown".to_string();
    };
    let elapsed = chrono::Utc::now().signed_duration_since(start.with_timezone(&chrono::Utc));
    let total_secs = elapsed.num_seconds();
    if total_secs < 60 {
        format!("{total_secs}s")
    } else if total_secs < 3600 {
        format!("{}m {}s", total_secs / 60, total_secs % 60)
    } else {
        format!("{}h {}m", total_secs / 3600, (total_secs % 3600) / 60)
    }
}

/// Build the structured reproduction result template for GitHub.
fn build_replicate_template(
    status: &str,
    agent_id: &str,
    model: &str,
    attempt: i32,
    duration: &str,
    findings: &str,
    dispatch_id: i64,
) -> String {
    let status_display = match status {
        "success" => "Reproduced",
        "failure" => "Could not reproduce",
        _ => status,
    };

    let findings_section = if findings.is_empty() {
        "No findings recorded.".to_string()
    } else {
        findings.to_string()
    };

    format!(
        r#"## Sentinel: Reproduction Report

| Field | Value |
|-------|-------|
| Status | {status_display} |
| Agent | `{agent_id}` |
| Model | {model} |
| Attempt | {attempt} of 2 |
| Duration | {duration} |

### Findings

{findings_section}

### Next steps

- [ ] Review the agent's findings
- [ ] Label `agent-todo: fix` to trigger an automated fix attempt

---
*Posted by crosslink sentinel | sentinel #{dispatch_id}*"#
    )
}

/// Post a comment to a GitHub issue via `gh`.
fn post_gh_comment(gh_issue_number: i64, body: &str) -> Result<()> {
    let output = Command::new("gh")
        .args([
            "issue",
            "comment",
            &gh_issue_number.to_string(),
            "--body",
            body,
        ])
        .output()
        .context("Failed to run `gh issue comment`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh issue comment failed: {}", stderr.trim());
    }

    Ok(())
}
