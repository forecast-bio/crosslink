// Swarm status: display current swarm state and probe agent liveness.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::io::*;
use super::types::*;
use crate::commands::kickoff::tmux_session_name;
use crate::sync::SyncManager;

/// Display the current state of the swarm.
pub fn status(crosslink_dir: &Path, json: bool) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let ctx = resolve_swarm(&sync)?;
    let plan: SwarmPlan = read_hub_json(&sync, &ctx.plan_path())
        .context("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")?;

    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    if json {
        let mut phases_json = Vec::new();
        for phase_name in &plan.phases {
            let phase_file = ctx.phase_path(phase_name);
            let phase: PhaseDefinition = match read_hub_json(&sync, &phase_file) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let resolved = resolve_agents(&phase, root);
            phases_json.push(serde_json::json!({
                "name": phase.name,
                "status": phase.status,
                "gate": phase.gate,
                "depends_on": phase.depends_on,
                "agents": resolved,
            }));
        }
        let output = serde_json::json!({
            "title": plan.title,
            "phases": phases_json,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    println!("Swarm: {}", plan.title);
    println!();

    for phase_name in &plan.phases {
        let phase_file = ctx.phase_path(phase_name);
        let Ok(phase) = read_hub_json::<PhaseDefinition>(&sync, &phase_file) else {
            println!("  {phase_name} (definition missing)");
            continue;
        };

        let resolved = resolve_agents(&phase, root);
        let total = resolved.len();
        let merged = resolved
            .iter()
            .filter(|a| a.defined_status == AgentStatus::Merged)
            .count();
        let completed = resolved
            .iter()
            .filter(|a| {
                a.defined_status == AgentStatus::Completed
                    || a.live_status == "DONE"
                    || a.live_status == "completed"
            })
            .count();
        let failed = resolved
            .iter()
            .filter(|a| {
                a.defined_status == AgentStatus::Failed
                    || a.live_status == "FAILED"
                    || a.live_status == "failed"
            })
            .count();

        let gate_info = phase.gate.as_ref().map_or_else(String::new, |gate| {
            if gate.status == "passed" {
                let tests = gate
                    .tests_total
                    .map(|t| format!(", {t} tests"))
                    .unwrap_or_default();
                format!(", gate passed{tests}")
            } else {
                format!(", gate {}", gate.status)
            }
        });

        let extra = if completed > 0 || failed > 0 {
            let mut parts = Vec::new();
            if completed > 0 {
                parts.push(format!("{completed} completed"));
            }
            if failed > 0 {
                parts.push(format!("{failed} failed"));
            }
            format!(", {}", parts.join(", "))
        } else {
            String::new()
        };

        println!(
            "{} ({}): {}/{} agents merged{}{}",
            phase.name, phase.status, merged, total, extra, gate_info
        );

        for agent in &resolved {
            let icon = match agent.live_status.as_str() {
                "DONE" | "completed" | "merged" => "\u{2713}",
                "FAILED" | "failed" => "\u{2717}",
                s if s.starts_with("running") => "\u{25cf}",
                "planned" => " ",
                _ => "\u{23f8}",
            };

            let status_display = if agent.defined_status == AgentStatus::Merged {
                "merged".to_string()
            } else if agent.live_status != format!("{}", agent.defined_status) {
                // Live status differs from definition -- show live
                agent.live_status.clone()
            } else {
                format!("{}", agent.defined_status)
            };

            let issue_info = agent
                .issue_id
                .map(|id| format!(" (#{id})"))
                .unwrap_or_default();

            println!(
                "  {} {:<30} {:<12}{}",
                icon, agent.slug, status_display, issue_info
            );
            if !agent.description.is_empty() {
                println!("    {}", agent.description);
            }
        }
        println!();
    }

    // Next steps section -- find the active phase and suggest next action
    let mut active_phase_slug: Option<String> = None;
    let mut completed_phases = 0;
    let mut has_failed = false;
    let mut has_running = false;
    let mut has_planned = false;
    let mut has_ready_to_merge = false;
    let mut all_merged = true;

    for phase_name in &plan.phases {
        let phase_file = ctx.phase_path(phase_name);
        let phase: PhaseDefinition = match read_hub_json(&sync, &phase_file) {
            Ok(p) => p,
            Err(_) => continue,
        };

        if phase.status == PhaseStatus::Completed {
            completed_phases += 1;
            continue;
        }

        if active_phase_slug.is_none() {
            active_phase_slug = Some(slugify_phase(phase_name));
            let resolved = resolve_agents(&phase, root);
            for agent in &resolved {
                match agent.live_status.as_str() {
                    "FAILED" | "failed" => has_failed = true,
                    s if s.starts_with("running") => has_running = true,
                    "planned" => {
                        if agent.defined_status == AgentStatus::Planned {
                            has_planned = true;
                        }
                    }
                    "DONE" | "completed" => {
                        if agent.defined_status != AgentStatus::Merged {
                            has_ready_to_merge = true;
                            all_merged = false;
                        }
                    }
                    _ => {
                        all_merged = false;
                    }
                }
                if agent.defined_status != AgentStatus::Merged
                    && agent.defined_status != AgentStatus::Completed
                {
                    all_merged = false;
                }
            }
        }
    }

    if let Some(slug) = active_phase_slug {
        println!("Next steps:");
        if has_planned {
            println!("  crosslink swarm launch {slug}");
        }
        if has_failed {
            println!("  crosslink swarm launch {slug} --retry-failed");
        }
        if has_running {
            println!("  (waiting for running agents to complete)");
        }
        if has_ready_to_merge {
            println!("  (merge completed agents, then gate)");
        }
        if all_merged && !has_running && !has_planned {
            println!("  crosslink swarm gate {slug}");
            println!("  crosslink swarm checkpoint {slug}");
        }
    } else if completed_phases == plan.phases.len() {
        println!(
            "All phases completed. Run `crosslink swarm archive` to archive and start a new swarm."
        );
    }

    Ok(())
}

/// Cross-reference phase agents with worktree state to get live status.
pub(super) fn resolve_agents(phase: &PhaseDefinition, repo_root: &Path) -> Vec<ResolvedAgent> {
    phase
        .agents
        .iter()
        .map(|agent| {
            let live_status = probe_agent_status(repo_root, &agent.slug);
            ResolvedAgent {
                slug: agent.slug.clone(),
                description: agent.description.clone(),
                issue_id: agent.issue_id,
                defined_status: agent.status.clone(),
                live_status,
                branch: agent.branch.clone(),
            }
        })
        .collect()
}

/// Probe the actual runtime status of an agent by checking its worktree.
pub(super) fn probe_agent_status(repo_root: &Path, slug: &str) -> String {
    let worktree = repo_root.join(".worktrees").join(slug);

    if !worktree.exists() {
        // Worktree removed -- check if the agent's branch was merged or exists.
        // This handles the case where worktrees are cleaned up after PRs are merged.
        if is_branch_merged(repo_root, slug) {
            return "completed (merged)".to_string();
        }
        if branch_exists(repo_root, slug) {
            return "completed (worktree removed)".to_string();
        }
        return "planned".to_string();
    }

    // Check .kickoff-status
    let status_file = worktree.join(".kickoff-status");
    if status_file.exists() {
        if let Ok(content) = std::fs::read_to_string(&status_file) {
            let s = content.trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }

    // Check if tmux session is alive by exact slug match
    let session_name = tmux_session_name(slug);
    let tmux_alive = std::process::Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .output()
        .is_ok_and(|o| o.status.success());

    if tmux_alive {
        return "running (tmux)".to_string();
    }

    // Fallback: scan tmux sessions for any containing the slug as a substring.
    // kickoff::run derives the session name from slugify(description), which may
    // differ from the agent slug (e.g. "req-1-add-login" vs "add-login").
    if let Ok(output) = std::process::Command::new("tmux")
        .args(["list-sessions", "-F", concat!("#", "{session_name}")])
        .output()
    {
        if output.status.success() {
            let sessions = String::from_utf8_lossy(&output.stdout);
            for session in sessions.lines() {
                if session.contains(slug) {
                    return format!("running (tmux: {session})");
                }
            }
        }
    }

    // Worktree exists but no status file and no tmux -- the session died
    // before it could write any status. Treat as failed so gate can proceed.
    "failed (session died)".to_string()
}

/// Check if a branch has been merged into the default branch (main/master).
pub(super) fn is_branch_merged(repo_root: &Path, slug: &str) -> bool {
    // Try common branch naming patterns for swarm agents
    for branch in &[slug.to_string(), format!("swarm/{slug}")] {
        let output = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["branch", "--merged", "HEAD", "--list", branch])
            .output();
        if let Ok(out) = output {
            if out.status.success() && !out.stdout.is_empty() {
                let branches = String::from_utf8_lossy(&out.stdout);
                if branches.lines().any(|l| l.trim() == *branch) {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if a branch exists locally (even without a worktree).
pub(super) fn branch_exists(repo_root: &Path, slug: &str) -> bool {
    for branch in &[slug.to_string(), format!("swarm/{slug}")] {
        let output = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["rev-parse", "--verify", branch])
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                return true;
            }
        }
    }
    false
}
