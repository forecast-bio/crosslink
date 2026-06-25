use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::db::Database;

use super::config::SentinelConfig;
use super::seen_set::gh_comment_already_posted;

/// Statistics from a result collection pass.
#[derive(Debug, Default)]
pub struct CollectStats {
    pub collected: u32,
    pub orphaned: u32,
    pub still_running: u32,
}

/// Worktree artifacts extracted for template rendering.
struct WorktreeArtifacts {
    test_file: Option<String>,
    test_output: Option<String>,
    pr_number: Option<String>,
    files_changed: Option<String>,
}

/// Inputs for rendering a result template.
struct TemplateContext<'a> {
    status: &'a str,
    agent_id: &'a str,
    model: &'a str,
    attempt: i32,
    duration: &'a str,
    findings: &'a str,
    artifacts: &'a WorktreeArtifacts,
    dispatch_id: i64,
}

/// Poll completed agents, read findings, post results to GitHub, update records.
///
/// Runs every sentinel cycle (after dispatch phase in oneshot, every cycle in watch).
pub fn collect_completed(
    db: &Database,
    crosslink_dir: &Path,
    config: Option<&SentinelConfig>,
) -> Result<CollectStats> {
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

        let Some(outcome) = classify_status(&status_content, dispatch.attempt_number) else {
            stats.still_running += 1;
            continue;
        };

        // Read agent findings from crosslink comments on the linked issue
        let findings = dispatch
            .crosslink_issue_id
            .map_or_else(String::new, |issue_id| read_agent_findings(db, issue_id));

        let duration = format_elapsed(&dispatch.created_at);

        // Extract worktree artifacts for template rendering
        let artifacts = extract_worktree_artifacts(&wt_path, dispatch.label.contains("fix"));

        // Build structured comment (template varies by dispatch type)
        let ctx = TemplateContext {
            status: outcome,
            agent_id,
            model: dispatch.model_used.as_deref().unwrap_or("unknown"),
            attempt: dispatch.attempt_number,
            duration: &duration,
            findings: &findings,
            artifacts: &artifacts,
            dispatch_id: dispatch.id,
        };
        let comment_body = if dispatch.label.contains("fix") {
            build_fix_template(&ctx)
        } else {
            build_replicate_template(&ctx)
        };

        // Post to GH issue (with Layer 4 dedup check)
        if let Some(gh_num) = dispatch.gh_issue_number {
            if gh_comment_already_posted(gh_num, dispatch.id) {
                tracing::debug!("sentinel #{} already posted to GH#{}", dispatch.id, gh_num);
            } else if let Err(e) = post_gh_comment(gh_num, &comment_body) {
                tracing::warn!("failed to post results to GH#{gh_num}: {e}");
            }
        }

        db.update_dispatch_outcome(dispatch.id, outcome, &findings)?;

        // Send outbound notification if configured
        if let Some(cfg) = config {
            super::notify::notify_dispatch_completed(
                &cfg.notifications,
                dispatch,
                outcome,
                &findings,
            );
        }

        stats.collected += 1;
    }

    Ok(stats)
}

/// Extract test file, test output, PR number, and diff stats from a worktree.
fn extract_worktree_artifacts(wt_path: &Path, is_fix: bool) -> WorktreeArtifacts {
    let test_file = find_test_file(wt_path);
    let test_output = read_test_output(wt_path);
    let pr_number = if is_fix {
        find_pr_number(wt_path)
    } else {
        None
    };
    let files_changed = if is_fix { git_diff_stat(wt_path) } else { None };

    WorktreeArtifacts {
        test_file,
        test_output,
        pr_number,
        files_changed,
    }
}

/// Find new/modified test files in the worktree via `git diff`.
/// Uses `--diff-filter=AM` (added or modified) against the merge base
/// to handle repos with any number of commits.
fn find_test_file(wt_path: &Path) -> Option<String> {
    // Find the merge base with the default branch
    let base_output = Command::new("git")
        .args(["merge-base", "HEAD", "HEAD~1"])
        .current_dir(wt_path)
        .output()
        .ok();

    let diff_args = base_output.as_ref().map_or_else(
        || {
            vec![
                "ls-files".to_string(),
                "--".to_string(),
                "tests/".to_string(),
            ]
        },
        |base| {
            if base.status.success() {
                let base_sha = String::from_utf8_lossy(&base.stdout).trim().to_string();
                vec![
                    "diff".to_string(),
                    "--name-only".to_string(),
                    "--diff-filter=AM".to_string(),
                    format!("{base_sha}..HEAD"),
                    "--".to_string(),
                    "tests/".to_string(),
                ]
            } else {
                // Fallback: list all tracked test files
                vec![
                    "ls-files".to_string(),
                    "--".to_string(),
                    "tests/".to_string(),
                ]
            }
        },
    );

    let args_refs: Vec<&str> = diff_args.iter().map(String::as_str).collect();
    let output = Command::new("git")
        .args(&args_refs)
        .current_dir(wt_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_test = stdout.lines().find(|l| l.contains("test"))?;
    Some(first_test.to_string())
}

/// Read test output from `.kickoff-report.json` if it exists.
fn read_test_output(wt_path: &Path) -> Option<String> {
    let report_path = wt_path.join(".kickoff-report.json");
    let content = std::fs::read_to_string(&report_path).ok()?;
    let report: serde_json::Value = serde_json::from_str(&content).ok()?;

    // Try to extract test output from the report's criteria or phases
    if let Some(phases) = report.get("phases") {
        if let Some(testing) = phases.get("testing") {
            let tests_run = testing.get("tests_run").and_then(serde_json::Value::as_u64);
            let tests_passed = testing
                .get("tests_passed")
                .and_then(serde_json::Value::as_u64);
            let tests_failed = testing
                .get("tests_failed")
                .and_then(serde_json::Value::as_u64);
            if let (Some(run), Some(passed), Some(failed)) = (tests_run, tests_passed, tests_failed)
            {
                return Some(format!(
                    "test result: {run} run, {passed} passed, {failed} failed"
                ));
            }
        }
    }
    None
}

/// Find the PR number for a fix dispatch by looking up the branch.
fn find_pr_number(wt_path: &Path) -> Option<String> {
    // Get the branch name
    let branch_output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(wt_path)
        .output()
        .ok()?;
    if !branch_output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();
    if branch.is_empty() {
        return None;
    }

    // Look up PR by head branch
    let pr_output = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            &branch,
            "--json",
            "number",
            "--jq",
            ".[0].number",
        ])
        .current_dir(wt_path)
        .output()
        .ok()?;
    if !pr_output.status.success() {
        return None;
    }
    let num = String::from_utf8_lossy(&pr_output.stdout)
        .trim()
        .to_string();
    if num.is_empty() || num == "null" {
        None
    } else {
        Some(num)
    }
}

/// Get `git diff --stat` summary for fix dispatches.
/// Uses `--stat` against the worktree's uncommitted + committed changes.
fn git_diff_stat(wt_path: &Path) -> Option<String> {
    // Try diffing against merge-base first; fall back to just `git diff --stat`
    let output = Command::new("git")
        .args(["diff", "--stat", "HEAD"])
        .current_dir(wt_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        return None;
    }
    // Return only the summary line (last line)
    stdout.lines().last().map(String::from)
}

/// Resolve the main repo root from a crosslink directory.
fn repo_root(crosslink_dir: &Path) -> Result<std::path::PathBuf> {
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

/// Classify a `.kickoff-status` file body into a dispatch outcome.
///
/// Returns `Some(outcome_code)` for a terminal state, or `None` if the
/// agent is still working (in which case the collect loop should leave
/// the dispatch pending and try again on the next cycle).
///
/// Agent states (per `crosslink/src/commands/kickoff/launch.rs`):
/// - `LAUNCHING` — tmux session being created
/// - `RUNNING`   — agent executing in tmux
/// - `FAILED`    — launch couldn't hand off to tmux
/// - `DONE`      — agent finished cleanly (written by the agent itself)
/// - `TIMEOUT`   — accepted as a future-compatible terminal state
///
/// Previously the collect loop treated every non-`DONE` value as a
/// terminal `failure`, which caused it to harvest dispatches within
/// seconds of their creation — producing `Duration | 0s` and
/// `No findings recorded` in the posted GitHub comment because the
/// agent hadn't yet written any observations (GH#561 defects 2 & 3).
fn classify_status(status_content: &str, attempt_number: i32) -> Option<&'static str> {
    let trimmed = status_content.trim();
    if trimmed.starts_with("DONE") {
        Some("success")
    } else if trimmed.starts_with("FAILED") || trimmed.starts_with("TIMEOUT") {
        if attempt_number >= 2 {
            Some("exhausted")
        } else {
            Some("failure")
        }
    } else {
        // RUNNING / LAUNCHING / empty / any other value — not terminal.
        None
    }
}

/// Read observation and resolution comments from a crosslink issue.
fn read_agent_findings(db: &Database, issue_id: i64) -> String {
    let Ok(comments) = db.get_comments(issue_id) else {
        return String::new();
    };

    comments
        .iter()
        .filter(|c| c.kind == "observation" || c.kind == "resolution")
        .map(|c| c.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

/// Compute human-readable duration from an RFC3339 start time to now.
pub fn format_elapsed(started_at: &str) -> String {
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
fn build_replicate_template(ctx: &TemplateContext<'_>) -> String {
    let status_display = match ctx.status {
        "success" => "Reproduced",
        "failure" => "Could not reproduce",
        "exhausted" => "Could not reproduce (all attempts exhausted)",
        _ => ctx.status,
    };

    let test_file_row = ctx
        .artifacts
        .test_file
        .as_ref()
        .map(|f| format!("| Test file | `{f}` |\n"))
        .unwrap_or_default();

    let findings_section = if ctx.findings.is_empty() {
        "No findings recorded.".to_string()
    } else {
        ctx.findings.to_string()
    };

    let test_output_section = ctx
        .artifacts
        .test_output
        .as_ref()
        .map(|output| {
            let truncated: String = output.lines().take(50).collect::<Vec<_>>().join("\n");
            format!("### Test output\n\n```\n{truncated}\n```\n")
        })
        .unwrap_or_default();

    let agent_id = ctx.agent_id;
    let model = ctx.model;
    let attempt = ctx.attempt;
    let duration = ctx.duration;
    let dispatch_id = ctx.dispatch_id;

    format!(
        "## Sentinel: Reproduction Report

| Field | Value |
|-------|-------|
| Status | {status_display} |
| Agent | `{agent_id}` |
| Model | {model} |
| Attempt | {attempt} of 2 |
| Duration | {duration} |
{test_file_row}
### Findings

{findings_section}

{test_output_section}
### Next steps

- [ ] Review the agent's findings
- [ ] Label `agent-todo: fix` to trigger an automated fix attempt

---
*Posted by crosslink sentinel | sentinel #{dispatch_id}*"
    )
}

/// Build the structured fix result template for GitHub.
fn build_fix_template(ctx: &TemplateContext<'_>) -> String {
    let status_display = match ctx.status {
        "success" => "Fixed",
        "failure" => "Could not fix",
        "exhausted" => "Could not fix (all attempts exhausted)",
        _ => ctx.status,
    };

    let pr_row = ctx
        .artifacts
        .pr_number
        .as_ref()
        .map(|n| format!("| PR | #{n} (draft) |\n"))
        .unwrap_or_default();

    let diff_row = ctx
        .artifacts
        .files_changed
        .as_ref()
        .map(|s| format!("| Changes | {s} |\n"))
        .unwrap_or_default();

    let findings_section = if ctx.findings.is_empty() {
        "No findings recorded.".to_string()
    } else {
        ctx.findings.to_string()
    };

    let test_output_section = ctx
        .artifacts
        .test_output
        .as_ref()
        .map(|output| {
            let truncated: String = output.lines().take(50).collect::<Vec<_>>().join("\n");
            format!("### Test results\n\n```\n{truncated}\n```\n")
        })
        .unwrap_or_default();

    let agent_id = ctx.agent_id;
    let model = ctx.model;
    let attempt = ctx.attempt;
    let duration = ctx.duration;
    let dispatch_id = ctx.dispatch_id;

    format!(
        "## Sentinel: Fix Report

| Field | Value |
|-------|-------|
| Status | {status_display} |
| Agent | `{agent_id}` |
| Model | {model} |
| Attempt | {attempt} of 2 |
| Duration | {duration} |
{pr_row}{diff_row}
### Findings

{findings_section}

{test_output_section}
### Next steps

- [ ] Review the draft PR
- [ ] Run CI and verify the fix

---
*Posted by crosslink sentinel | sentinel #{dispatch_id}*"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_status_done_is_success() {
        assert_eq!(classify_status("DONE", 1), Some("success"));
        assert_eq!(classify_status("DONE\n", 1), Some("success"));
        assert_eq!(classify_status("  DONE  ", 2), Some("success"));
        assert_eq!(classify_status("DONE with extra info", 1), Some("success"));
    }

    #[test]
    fn classify_status_failed_is_failure_then_exhausted() {
        assert_eq!(classify_status("FAILED\n", 1), Some("failure"));
        assert_eq!(classify_status("FAILED\n", 2), Some("exhausted"));
        assert_eq!(
            classify_status("FAILED: tmux send-keys", 1),
            Some("failure")
        );
    }

    #[test]
    fn classify_status_timeout_is_terminal() {
        assert_eq!(classify_status("TIMEOUT\n", 1), Some("failure"));
        assert_eq!(classify_status("TIMEOUT\n", 2), Some("exhausted"));
    }

    #[test]
    fn classify_status_running_leaves_pending() {
        // This is the GH#561 regression: RUNNING must not be treated as
        // a terminal outcome, otherwise the harvest runs while the agent
        // is still working and posts a premature "0s / no findings" comment.
        assert_eq!(classify_status("RUNNING\n", 1), None);
        assert_eq!(classify_status("LAUNCHING\n", 1), None);
        assert_eq!(classify_status("RUNNING", 2), None);
    }

    #[test]
    fn classify_status_unknown_leaves_pending() {
        // Defensive: anything we don't recognise should also be treated
        // as "agent is still working" rather than silently terminal.
        assert_eq!(classify_status("", 1), None);
        assert_eq!(classify_status("partial-write", 1), None);
        assert_eq!(classify_status("  ", 1), None);
    }
}
