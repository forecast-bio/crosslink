// E-ana tablet — kickoff cleanup: remove stale agent artifacts
use anyhow::Result;
use serde::Serialize;
use std::path::Path;
use std::process::Command;

use super::helpers::*;
use super::monitor::discover_agents;
use super::types::*;

/// `crosslink kickoff cleanup`
///
/// Discover and remove stale kickoff agent artifacts: completed tmux sessions,
/// worktrees with DONE sentinels, and orphaned worktrees whose sessions no longer exist.
pub fn cleanup(
    crosslink_dir: &Path,
    dry_run: bool,
    force: bool,
    keep: usize,
    json_output: bool,
) -> Result<()> {
    let agents = discover_agents(crosslink_dir)?;

    // Classify and separate active agents from removable ones
    let (active, removable): (Vec<_>, Vec<_>) = agents
        .into_iter()
        .map(|a| {
            let class = classify_agent(&a);
            (a, class)
        })
        .partition(|(_, class)| *class == CleanupClass::Active);

    // Without --force, only clean Done agents (not Stale)
    let (mut to_clean, skipped_stale): (Vec<_>, Vec<_>) = if force {
        (removable, vec![])
    } else {
        removable
            .into_iter()
            .partition(|(_, class)| *class == CleanupClass::Done)
    };

    // Sort by worktree path (as a proxy for creation order) so --keep works predictably
    to_clean.sort_by(|a, b| a.0.worktree.cmp(&b.0.worktree));

    // Apply --keep: keep the N most recent (last N items after sorting)
    let to_clean = if keep > 0 && to_clean.len() > keep {
        to_clean[..to_clean.len() - keep].to_vec()
    } else if keep > 0 && to_clean.len() <= keep {
        vec![] // keep all
    } else {
        to_clean
    };

    // --- Dry-run / JSON output ---
    if json_output {
        #[derive(Serialize)]
        struct CleanupPlan {
            to_clean: Vec<CleanupPlanEntry>,
            skipped_stale: Vec<CleanupPlanEntry>,
            active: Vec<CleanupPlanEntry>,
            dry_run: bool,
        }
        #[derive(Serialize)]
        struct CleanupPlanEntry {
            id: String,
            status: String,
            class: CleanupClass,
            worktree: String,
            session: Option<String>,
            docker: Option<String>,
        }
        let to_entry = |items: &[(AgentInfo, CleanupClass)]| -> Vec<CleanupPlanEntry> {
            items
                .iter()
                .map(|(a, c)| CleanupPlanEntry {
                    id: a.id.clone(),
                    status: a.status.clone(),
                    class: c.clone(),
                    worktree: a.worktree.clone(),
                    session: a.session.clone(),
                    docker: a.docker.clone(),
                })
                .collect()
        };
        let plan = CleanupPlan {
            to_clean: to_entry(&to_clean),
            skipped_stale: to_entry(&skipped_stale),
            active: to_entry(&active),
            dry_run,
        };
        println!("{}", serde_json::to_string_pretty(&plan)?);
        if dry_run {
            return Ok(());
        }
    }

    if to_clean.is_empty() && skipped_stale.is_empty() {
        if !json_output {
            println!("No agents to clean up.");
        }
        return Ok(());
    }

    if dry_run || !json_output {
        // Print the plan
        if !to_clean.is_empty() {
            println!("Cleanup candidates:\n");
            for (agent, class) in &to_clean {
                let class_label = match class {
                    CleanupClass::Done => "DONE  ",
                    CleanupClass::Stale => "STALE ",
                    CleanupClass::Active => "      ",
                };
                let wt_display = if agent.worktree.is_empty() {
                    "-".to_string()
                } else {
                    std::path::Path::new(&agent.worktree)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&agent.worktree)
                        .to_string()
                };
                let session_info = agent
                    .session
                    .as_deref()
                    .map_or_else(|| "tmux: exited".to_string(), |s| format!("tmux: {s}"));
                let docker_info = agent
                    .docker
                    .as_deref()
                    .map(|d| format!("  docker: {d}"))
                    .unwrap_or_default();
                println!(
                    "  {}  {:<40} worktree: {:<30} {}{}",
                    class_label, agent.id, wt_display, session_info, docker_info
                );
            }
        }

        if !skipped_stale.is_empty() {
            println!(
                "\n{} stale agent(s) skipped (use --force to include):",
                skipped_stale.len()
            );
            for (agent, _) in &skipped_stale {
                let wt_display = if agent.worktree.is_empty() {
                    "-".to_string()
                } else {
                    std::path::Path::new(&agent.worktree)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&agent.worktree)
                        .to_string()
                };
                println!("  STALE  {:<40} worktree: {}", agent.id, wt_display);
            }
        }

        if dry_run {
            let wt_count = to_clean
                .iter()
                .filter(|(a, _)| !a.worktree.is_empty())
                .count();
            let tmux_count = to_clean.iter().filter(|(a, _)| a.session.is_some()).count();
            let docker_count = to_clean.iter().filter(|(a, _)| a.docker.is_some()).count();
            println!();
            print!("Would remove {wt_count} worktree(s)");
            if tmux_count > 0 {
                print!(", kill {tmux_count} tmux session(s)");
            }
            if docker_count > 0 {
                print!(", remove {docker_count} container(s)");
            }
            println!(".");
            println!("Run without --dry-run to proceed.");
            return Ok(());
        }

        println!();
    }

    // --- Execute cleanup ---
    let mut results: Vec<CleanupResult> = Vec::new();

    for (agent, class) in &to_clean {
        let mut result = CleanupResult {
            id: agent.id.clone(),
            class: class.clone(),
            worktree_removed: false,
            tmux_killed: false,
            container_removed: false,
            error: None,
        };

        // 1. Kill tmux session if it still exists
        if let Some(ref session_name) = agent.session {
            match Command::new("tmux")
                .args(["kill-session", "-t", session_name])
                .output()
            {
                Ok(o) if o.status.success() => {
                    result.tmux_killed = true;
                    if !json_output {
                        println!("  Killed tmux session: {session_name}");
                    }
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    tracing::warn!(
                        "failed to kill tmux session {}: {}",
                        session_name,
                        stderr.trim()
                    );
                }
                Err(e) => {
                    tracing::warn!("tmux error for {}: {}", session_name, e);
                }
            }
        }

        // 2. Remove Docker/Podman container if present
        if let Some(ref container_name) = agent.docker {
            for runtime in &["docker", "podman"] {
                if command_available(runtime) {
                    if let Ok(o) = Command::new(runtime)
                        .args(["rm", "-f", container_name])
                        .output()
                    {
                        if o.status.success() {
                            result.container_removed = true;
                            if !json_output {
                                println!("  Removed {runtime} container: {container_name}");
                            }
                            break;
                        }
                    }
                }
            }
        }

        // 3. Reconcile the matching pipeline run row before the worktree
        //    disappears (GH#614): once removed, lazy display reconcile can only
        //    ever see it as "aborted". Capture the truth now from the agent's
        //    terminal status — DONE → completed, failed → failed, anything else
        //    (stale/timed-out/stopped) → aborted.
        if !agent.worktree.is_empty() {
            if let Some(root) = crosslink_dir.parent() {
                let pipeline_status = match agent.status.as_str() {
                    "done" => "completed",
                    "failed" => "failed",
                    _ => "aborted",
                };
                let _ = super::pipeline::reconcile_completion_by_worktree(
                    root,
                    &agent.worktree,
                    pipeline_status,
                );
            }
        }

        // 4. Remove the git worktree
        if !agent.worktree.is_empty() && std::path::Path::new(&agent.worktree).exists() {
            match Command::new("git")
                .args(["worktree", "remove", "--force", &agent.worktree])
                .output()
            {
                Ok(o) if o.status.success() => {
                    result.worktree_removed = true;
                    let wt_display = std::path::Path::new(&agent.worktree)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&agent.worktree);
                    if !json_output {
                        println!("  Removed worktree: {wt_display}");
                    }
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    let msg = format!("git worktree remove failed: {}", stderr.trim());
                    tracing::warn!("{}", msg);
                    result.error = Some(msg);
                }
                Err(e) => {
                    let msg = format!("git worktree remove error: {e}");
                    tracing::warn!("{}", msg);
                    result.error = Some(msg);
                }
            }
        }

        results.push(result);
    }

    // --- Summary ---
    if json_output {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        let wt_removed = results.iter().filter(|r| r.worktree_removed).count();
        let tmux_killed = results.iter().filter(|r| r.tmux_killed).count();
        let containers_removed = results.iter().filter(|r| r.container_removed).count();
        let errors = results.iter().filter(|r| r.error.is_some()).count();

        println!();
        print!("Cleaned up {} agent(s)", results.len());
        if wt_removed > 0 {
            print!(": {wt_removed} worktree(s)");
        }
        if tmux_killed > 0 {
            print!(", {tmux_killed} tmux session(s)");
        }
        if containers_removed > 0 {
            print!(", {containers_removed} container(s)");
        }
        if errors > 0 {
            print!(" ({errors} error(s))");
        }
        println!(".");
    }

    Ok(())
}
