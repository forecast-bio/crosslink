//! External issue query commands.
//!
//! Handles `--repo` flag for `crosslink issue search/show/list`.

use anyhow::Result;
use std::path::Path;

use crate::external::{
    read_data_ttl, read_url_ttl, resolve_repo, ExternalCache, ExternalIssueReader, RepoSource,
};
use crate::issue_file::IssueFile;
use crate::utils::format_issue_id;

/// Get an ExternalIssueReader for the given repo value.
fn get_reader(
    crosslink_dir: &Path,
    repo_value: &str,
    refresh: bool,
) -> Result<(ExternalIssueReader, String)> {
    let source = resolve_repo(repo_value, crosslink_dir)?;
    match source {
        RepoSource::Local(path) => {
            let reader = ExternalIssueReader::for_local(&path)?;
            Ok((reader, repo_value.to_string()))
        }
        RepoSource::Remote(_) => {
            let cache = ExternalCache::new(crosslink_dir, repo_value);
            let data_ttl = read_data_ttl(crosslink_dir);
            let url_ttl = read_url_ttl(crosslink_dir);
            let hub_dir = cache.ensure_hub(data_ttl, url_ttl, refresh)?;
            let reader = ExternalIssueReader::from_hub_dir(&hub_dir)?;
            Ok((reader, repo_value.to_string()))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn list(
    crosslink_dir: &Path,
    repo_value: &str,
    status: Option<&str>,
    label: Option<&str>,
    priority: Option<&str>,
    refresh: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let (reader, label_str) = get_reader(crosslink_dir, repo_value, refresh)?;
    let issues = reader.list_issues(status, label, priority);

    if json {
        print_issues_json(&issues, &label_str);
        return Ok(());
    }

    if !quiet {
        println!("--- Results from {} ---\n", label_str);
    }

    if issues.is_empty() {
        if !quiet {
            println!("No issues found.");
        }
    } else {
        for issue in &issues {
            let id_str = issue
                .display_id
                .map(format_issue_id)
                .unwrap_or_else(|| "?".to_string());
            let status_display = format!("[{}]", issue.status);
            let date = issue.created_at.format("%Y-%m-%d");
            println!(
                "{:<5} {:8} {:<40} {:8} {}",
                id_str,
                status_display,
                crate::utils::truncate(&issue.title, 40),
                issue.priority,
                date
            );
        }
    }

    if !quiet {
        println!("\n--- End external results ---");
    }

    Ok(())
}

pub fn search(
    crosslink_dir: &Path,
    repo_value: &str,
    query: &str,
    refresh: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let (reader, label) = get_reader(crosslink_dir, repo_value, refresh)?;
    let results = reader.search_issues(query);

    if json {
        print_issues_json(&results, &label);
        return Ok(());
    }

    if !quiet {
        println!("--- Results from {} ---\n", label);
    }

    if results.is_empty() {
        if !quiet {
            println!("No issues found matching '{}'", query);
        }
    } else {
        if !quiet {
            println!("Found {} issue(s) matching '{}':\n", results.len(), query);
        }
        for issue in &results {
            let id_str = issue
                .display_id
                .map(format_issue_id)
                .unwrap_or_else(|| "?".to_string());
            let status_marker = if issue.status == crate::models::IssueStatus::Closed {
                "✓"
            } else {
                " "
            };
            println!(
                "{:<5} [{}] {:8} {} {}",
                id_str,
                status_marker,
                issue.priority,
                issue.title,
                if issue.status == crate::models::IssueStatus::Closed {
                    "(closed)"
                } else {
                    ""
                }
            );

            if let Some(ref desc) = issue.description {
                let query_lower = query.to_lowercase();
                if desc.to_lowercase().contains(&query_lower) {
                    let preview: String = desc.chars().take(60).collect();
                    let suffix = if desc.chars().count() > 60 { "..." } else { "" };
                    println!("      └─ {}{}", preview.replace('\n', " "), suffix);
                }
            }
        }
    }

    if !quiet {
        println!("\n--- End external results ---");
    }

    Ok(())
}

pub fn show(
    crosslink_dir: &Path,
    repo_value: &str,
    id: i64,
    refresh: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let (reader, label) = get_reader(crosslink_dir, repo_value, refresh)?;
    let issue = reader
        .get_issue(id)
        .ok_or_else(|| anyhow::anyhow!("Issue {} not found in {}", format_issue_id(id), label))?;

    if json {
        let mut obj = serde_json::to_value(issue)?;
        if let Some(map) = obj.as_object_mut() {
            map.insert(
                "source".to_string(),
                serde_json::Value::String(label.clone()),
            );
        }
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    if !quiet {
        println!("--- Results from {} ---\n", label);
    }

    let id_str = issue
        .display_id
        .map(format_issue_id)
        .unwrap_or_else(|| "?".to_string());

    println!("Issue {}: {}", id_str, issue.title);
    println!("Status: {}", issue.status);
    println!("Priority: {}", issue.priority);
    println!(
        "Created: {} by {}",
        issue.created_at.format("%Y-%m-%d %H:%M:%S"),
        issue.created_by
    );
    println!("Updated: {}", issue.updated_at.format("%Y-%m-%d %H:%M:%S"));

    if let Some(closed) = issue.closed_at {
        println!("Closed: {}", closed.format("%Y-%m-%d %H:%M:%S"));
    }

    if !issue.labels.is_empty() {
        println!("Labels: {}", issue.labels.join(", "));
    }

    if let Some(ref desc) = issue.description {
        if !desc.is_empty() {
            println!("\nDescription:");
            for line in desc.lines() {
                println!("  {}", line);
            }
        }
    }

    // Always show comments (per design decision Q1)
    if !issue.comments.is_empty() {
        println!("\nComments:");
        for comment in &issue.comments {
            let kind_prefix = if comment.kind != "note" {
                format!("[{}] ", comment.kind)
            } else {
                String::new()
            };
            let intervention_suffix = match (&comment.trigger_type, &comment.intervention_context) {
                (Some(trigger), Some(ctx)) => {
                    format!(" (trigger: {}, context: {})", trigger, ctx)
                }
                (Some(trigger), None) => format!(" (trigger: {})", trigger),
                _ => String::new(),
            };
            println!(
                "  [{}] {}{}{}",
                comment.created_at.format("%Y-%m-%d %H:%M"),
                kind_prefix,
                comment.content,
                intervention_suffix
            );
        }
    }

    if !issue.blockers.is_empty() {
        let blocker_strs: Vec<String> = issue.blockers.iter().map(|b| b.to_string()).collect();
        println!("\nBlocked by: {}", blocker_strs.join(", "));
    }

    if !quiet {
        println!("\n--- End external results ---");
    }

    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────
// JSON formatting
// ───────────────────────────────────────────────────────────────────────────

fn print_issues_json(issues: &[&IssueFile], source: &str) {
    let entries: Vec<serde_json::Value> = issues
        .iter()
        .map(|issue| {
            let mut obj = serde_json::to_value(issue).unwrap_or(serde_json::Value::Null);
            if let Some(map) = obj.as_object_mut() {
                map.insert(
                    "source".to_string(),
                    serde_json::Value::String(source.to_string()),
                );
            }
            obj
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
    );
}
