// Swarm lifecycle: reset, archive, list, retry-failed, adopt, sync-status,
// resume, launch, gate, checkpoint.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::io::*;
use super::status::{probe_agent_status, resolve_agents};
use super::types::*;
use crate::commands::kickoff::{self, ContainerMode, KickoffOpts, VerifyLevel};
use crate::db::Database;
use crate::shared_writer::SharedWriter;
use crate::sync::SyncManager;

// ---------------------------------------------------------------------------
// swarm reset / archive / list
// ---------------------------------------------------------------------------

/// Archive the current swarm plan to swarm/archive/{timestamp}/ and clear the active slot.
pub fn archive(crosslink_dir: &Path) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }
    sync.fetch()?;

    let ctx = resolve_swarm(&sync)?;
    let plan_path_str = ctx.plan_path();
    let plan_path = sync.cache_path().join(&plan_path_str);
    if !plan_path.exists() {
        bail!("No active swarm plan to archive.");
    }

    let plan: SwarmPlan =
        read_hub_json(&sync, &plan_path_str).context("Failed to read swarm plan")?;

    // Create archive directory with timestamp
    let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let archive_prefix = format!("swarm/archive/{timestamp}");

    // Copy plan.json to archive
    let plan_json = std::fs::read_to_string(&plan_path)?;
    let archive_plan = sync
        .cache_path()
        .join(format!("{archive_prefix}/plan.json"));
    if let Some(parent) = archive_plan.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&archive_plan, &plan_json)?;

    // Copy phase files to archive
    let phases_dir = sync.cache_path().join(format!("{}/phases", ctx.base));
    if phases_dir.is_dir() {
        let archive_phases = sync.cache_path().join(format!("{archive_prefix}/phases"));
        std::fs::create_dir_all(&archive_phases)?;
        if let Ok(entries) = std::fs::read_dir(&phases_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let dest = archive_phases.join(&name);
                // INTENTIONAL: archive copy is best-effort — partial archive is acceptable
                let _ = std::fs::copy(entry.path(), dest);
            }
        }
    }

    // Copy checkpoints to archive
    let checkpoints_dir = sync.cache_path().join(ctx.checkpoints_dir());
    if checkpoints_dir.is_dir() {
        let archive_cp = sync
            .cache_path()
            .join(format!("{archive_prefix}/checkpoints"));
        std::fs::create_dir_all(&archive_cp)?;
        if let Ok(entries) = std::fs::read_dir(&checkpoints_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let dest = archive_cp.join(&name);
                // INTENTIONAL: archive copy is best-effort — partial archive is acceptable
                let _ = std::fs::copy(entry.path(), dest);
            }
        }
    }

    // INTENTIONAL: swarm file cleanup is best-effort — partial removal is acceptable, git commit tracks the state
    let _ = std::fs::remove_file(&plan_path);
    let _ = std::fs::remove_dir_all(&phases_dir);
    let _ = std::fs::remove_dir_all(&checkpoints_dir);
    let _ = std::fs::remove_file(sync.cache_path().join("swarm/active.json"));
    // For multi-swarm, remove the UUID directory itself if empty
    if !ctx.is_legacy {
        let _ = std::fs::remove_dir_all(sync.cache_path().join(ctx.base));
    }

    // Stage all swarm/ changes (additions, modifications, and deletions) on the
    // hub branch cache. This is safe because the cache is a dedicated worktree
    // for crosslink/hub, not the user's working tree.
    let cache = sync.cache_path();
    if let Ok(o) = std::process::Command::new("git")
        .current_dir(cache)
        .args(["add", "--all", "--", "swarm/"])
        .output()
    {
        if !o.status.success() {
            tracing::warn!(
                "git add failed during swarm archive: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }
    if let Ok(o) = std::process::Command::new("git")
        .current_dir(cache)
        .args([
            "commit",
            "-m",
            &format!("swarm: archive '{}' to {}", plan.title, archive_prefix),
        ])
        .output()
    {
        if !o.status.success() {
            let msg = String::from_utf8_lossy(&o.stderr);
            if !msg.contains("nothing to commit") {
                tracing::warn!("git commit failed during swarm archive: {}", msg.trim());
            }
        }
    }
    let remote = sync.remote();
    if let Ok(o) = std::process::Command::new("git")
        .current_dir(cache)
        .args(["push", remote, crate::sync::HUB_BRANCH])
        .output()
    {
        if !o.status.success() {
            tracing::warn!(
                "could not push swarm archive to hub: {} — archive is saved locally",
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }

    println!("Archived swarm '{}' to {}/", plan.title, archive_prefix);
    println!("Active swarm slot is now clear. Run `crosslink swarm init` to start a new swarm.");
    Ok(())
}

/// Reset the active swarm. Archives first unless --no-archive.
pub fn reset(crosslink_dir: &Path, no_archive: bool) -> Result<()> {
    if !no_archive {
        return archive(crosslink_dir);
    }

    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized.");
    }
    sync.fetch()?;

    let ctx = resolve_swarm(&sync)?;
    let plan_path = sync.cache_path().join(ctx.plan_path());
    if !plan_path.exists() {
        bail!("No active swarm plan to reset.");
    }

    // INTENTIONAL: swarm file cleanup is best-effort — partial removal is acceptable, git commit tracks the state
    let _ = std::fs::remove_file(&plan_path);
    let _ = std::fs::remove_dir_all(sync.cache_path().join(format!("{}/phases", ctx.base)));
    let _ = std::fs::remove_dir_all(sync.cache_path().join(ctx.checkpoints_dir()));
    let _ = std::fs::remove_file(sync.cache_path().join("swarm/active.json"));
    if !ctx.is_legacy {
        let _ = std::fs::remove_dir_all(sync.cache_path().join(ctx.base));
    }

    // Stage all swarm/ changes on the hub branch cache (see archive() comment).
    let cache = sync.cache_path();
    if let Ok(o) = std::process::Command::new("git")
        .current_dir(cache)
        .args(["add", "--all", "--", "swarm/"])
        .output()
    {
        if !o.status.success() {
            tracing::warn!(
                "git add failed during swarm reset: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }
    if let Ok(o) = std::process::Command::new("git")
        .current_dir(cache)
        .args(["commit", "-m", "swarm: reset (no archive)"])
        .output()
    {
        if !o.status.success() {
            let msg = String::from_utf8_lossy(&o.stderr);
            if !msg.contains("nothing to commit") {
                tracing::warn!("git commit failed during swarm reset: {}", msg.trim());
            }
        }
    }
    let remote = sync.remote();
    if let Ok(o) = std::process::Command::new("git")
        .current_dir(cache)
        .args(["push", remote, crate::sync::HUB_BRANCH])
        .output()
    {
        if !o.status.success() {
            tracing::warn!(
                "could not push swarm reset to hub: {} — reset is saved locally",
                String::from_utf8_lossy(&o.stderr).trim()
            );
        }
    }

    println!("Swarm plan deleted. Active slot is clear.");
    Ok(())
}

/// List active and archived swarms.
pub fn list_swarms(crosslink_dir: &Path) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }
    sync.fetch()?;

    match resolve_swarm(&sync) {
        Ok(ctx) => {
            if let Ok(plan) = read_hub_json::<SwarmPlan>(&sync, &ctx.plan_path()) {
                let mode = if ctx.is_legacy { " (legacy)" } else { "" };
                println!(
                    "Active: {} (created {}){}",
                    plan.title, plan.created_at, mode
                );
            }
        }
        Err(_) => {
            println!("No active swarm.");
        }
    }

    let archive_dir = sync.cache_path().join("swarm/archive");
    if archive_dir.is_dir() {
        let mut archives: Vec<String> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&archive_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let plan_file = entry.path().join("plan.json");
                    let title = std::fs::read_to_string(&plan_file)
                        .ok()
                        .and_then(|c| serde_json::from_str::<SwarmPlan>(&c).ok())
                        .map_or_else(|| "(unknown)".to_string(), |p| p.title);
                    archives.push(format!("  {name} — {title}"));
                }
            }
        }
        archives.sort();
        if !archives.is_empty() {
            println!("\nArchived swarms:");
            for a in &archives {
                println!("{a}");
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// swarm launch --retry-failed
// ---------------------------------------------------------------------------

/// Relaunch agents that previously failed in a phase.
pub fn launch_retry_failed(
    crosslink_dir: &Path,
    db: &Database,
    writer: Option<&SharedWriter>,
    phase_slug: &str,
    quiet: bool,
) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let (mut phase, phase_file) = load_phase(&sync, phase_slug)?;

    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;
    let resolved = resolve_agents(&phase, root);

    // Find failed agents and reset them to planned
    let mut retry_count = 0;
    for agent in &mut phase.agents {
        let live = resolved
            .iter()
            .find(|r| r.slug == agent.slug)
            .map_or("planned", |r| r.live_status.as_str());

        if agent.status == AgentStatus::Failed || live == "FAILED" || live == "failed" {
            agent.status = AgentStatus::Planned;
            agent.started_at = None;
            agent.completed_at = None;
            retry_count += 1;
        }
    }

    if retry_count == 0 {
        println!("No failed agents to retry in '{}'.", phase.name);
        return Ok(());
    }

    write_hub_json(&sync, &phase_file, &phase)?;
    commit_hub_files(
        &sync,
        &[&phase_file],
        &format!("swarm: reset {retry_count} failed agents for retry"),
    )?;

    println!("Reset {retry_count} failed agent(s) to planned. Launching...");

    launch(crosslink_dir, db, writer, phase_slug, quiet)
}

// ---------------------------------------------------------------------------
// swarm adopt
// ---------------------------------------------------------------------------

/// Associate an external agent/branch with a swarm phase slot.
///
/// When an agent is launched manually (outside `swarm launch`), this command
/// lets you link it to a swarm slot so status/gate/merge see it correctly.
pub fn adopt(crosslink_dir: &Path, agent_slug: &str, slot_slug: &str) -> Result<()> {
    let (sync, plan, mut phases) = load_plan_and_phases(crosslink_dir)?;

    // Find the slot
    let mut found = false;
    for (_path, phase) in &mut phases {
        for agent in &mut phase.agents {
            if agent.slug == slot_slug {
                agent.status = AgentStatus::Running;
                agent.branch = Some(format!("feature/{agent_slug}"));
                agent.started_at = Some(chrono::Utc::now().to_rfc3339());
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }

    if !found {
        bail!(
            "Slot '{}' not found in any phase. Available slots: {}",
            slot_slug,
            phases
                .iter()
                .flat_map(|(_, p)| p.agents.iter().map(|a| a.slug.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    save_plan_and_phases(
        &sync,
        &plan,
        &phases,
        &format!("swarm: adopt agent '{agent_slug}' into slot '{slot_slug}'"),
    )?;
    println!("Adopted '{agent_slug}' into swarm slot '{slot_slug}' (branch: feature/{agent_slug})");
    Ok(())
}

// ---------------------------------------------------------------------------
// swarm sync-status
// ---------------------------------------------------------------------------

/// Sync live agent statuses from worktree probes back into phase JSON files.
///
/// Bridges the gap between `swarm status` (reads live state) and
/// `swarm merge`/`swarm gate` (reads phase JSON).
pub fn sync_status(crosslink_dir: &Path) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }
    sync.fetch()?;

    let ctx = resolve_swarm(&sync)?;
    let plan: SwarmPlan = read_hub_json(&sync, &ctx.plan_path()).context("No swarm plan found.")?;

    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    let mut updated_count = 0;
    let mut paths_to_commit: Vec<String> = Vec::new();

    for phase_name in &plan.phases {
        let phase_path = ctx.phase_path(phase_name);
        let mut phase: PhaseDefinition = match read_hub_json(&sync, &phase_path) {
            Ok(p) => p,
            Err(_) => continue,
        };

        if phase.status == PhaseStatus::Completed {
            continue;
        }

        let mut phase_changed = false;

        for agent in &mut phase.agents {
            let live = probe_agent_status(root, &agent.slug);

            let new_status =
                if live == "DONE" || live == "completed" || live.starts_with("completed") {
                    Some(AgentStatus::Completed)
                } else if live == "FAILED" || live == "failed" || live.starts_with("failed") {
                    Some(AgentStatus::Failed)
                } else if live.starts_with("running") {
                    Some(AgentStatus::Running)
                } else {
                    None
                };

            if let Some(status) = new_status {
                if agent.status != status {
                    let old = format!("{}", agent.status);
                    agent.status = status.clone();
                    if matches!(status, AgentStatus::Completed | AgentStatus::Failed)
                        && agent.completed_at.is_none()
                    {
                        agent.completed_at = Some(chrono::Utc::now().to_rfc3339());
                    }
                    println!("  {} {} → {}", agent.slug, old, agent.status);
                    phase_changed = true;
                    updated_count += 1;
                }
            }
        }

        if phase_changed {
            let all_done = phase.agents.iter().all(|a| {
                matches!(
                    a.status,
                    AgentStatus::Completed | AgentStatus::Merged | AgentStatus::Failed
                )
            });
            if all_done && phase.status == PhaseStatus::InProgress {
                let any_failed = phase.agents.iter().any(|a| a.status == AgentStatus::Failed);
                if any_failed {
                    phase.status = PhaseStatus::Failed;
                    println!("  Phase '{}' → failed", phase.name);
                }
            }

            write_hub_json(&sync, &phase_path, &phase)?;
            paths_to_commit.push(phase_path);
        }
    }

    if paths_to_commit.is_empty() {
        println!("All phase statuses are up to date.");
    } else {
        let refs: Vec<&str> = paths_to_commit.iter().map(String::as_str).collect();
        commit_hub_files(
            &sync,
            &refs,
            &format!("swarm: sync {updated_count} agent status(es) from live state"),
        )?;
        println!("Synced {updated_count} agent status update(s).");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// swarm resume
// ---------------------------------------------------------------------------

/// Reconstruct swarm state and output structured next-steps.
pub fn resume(crosslink_dir: &Path) -> Result<()> {
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

    // Find the latest checkpoint
    let checkpoint_dir = sync.cache_path().join(ctx.checkpoints_dir());
    let latest_checkpoint = find_latest_checkpoint(&checkpoint_dir);

    if let Some(ref cp) = latest_checkpoint {
        println!("Latest checkpoint: {} ({})", cp.phase, cp.created_at);
        if let Some(ref notes) = cp.handoff_notes {
            println!("  Notes: {notes}");
        }
        println!();
    }

    // Find the active phase (first non-completed phase)
    let mut active_phase: Option<PhaseDefinition> = None;
    let mut active_phase_name: Option<String> = None;
    let mut completed_count = 0;

    for phase_name in &plan.phases {
        let phase_file = ctx.phase_path(phase_name);
        let phase: PhaseDefinition = match read_hub_json(&sync, &phase_file) {
            Ok(p) => p,
            Err(_) => continue,
        };

        if phase.status == PhaseStatus::Completed {
            completed_count += 1;
            continue;
        }

        active_phase = Some(phase);
        active_phase_name = Some(phase_name.clone());
        break;
    }

    let (Some(phase), Some(phase_name)) = (active_phase, active_phase_name) else {
        println!(
            "All {} phases completed. Swarm build is done.",
            plan.phases.len()
        );
        return Ok(());
    };

    println!(
        "Resume point: {} ({}/{})",
        phase_name,
        completed_count,
        plan.phases.len()
    );
    println!();

    // Categorize agents by live status
    let resolved = resolve_agents(&phase, root);
    let mut actions: Vec<String> = Vec::new();
    let mut action_num = 1;

    // Agents completed but not merged
    let ready_to_merge: Vec<&ResolvedAgent> = resolved
        .iter()
        .filter(|a| a.live_status == "DONE" && a.defined_status != AgentStatus::Merged)
        .collect();

    if !ready_to_merge.is_empty() {
        for agent in &ready_to_merge {
            let branch = agent.branch.as_deref().unwrap_or_else(|| &agent.slug);
            actions.push(format!(
                "{}. Merge {}: review and merge {} to dev",
                action_num, agent.slug, branch
            ));
            action_num += 1;
        }
    }

    // Agents still running
    let running: Vec<&ResolvedAgent> = resolved
        .iter()
        .filter(|a| a.live_status.starts_with("running"))
        .collect();

    if !running.is_empty() {
        for agent in &running {
            actions.push(format!(
                "{}. Check {}: crosslink kickoff status {}",
                action_num, agent.slug, agent.slug
            ));
            action_num += 1;
        }
    }

    // Agents that failed
    let failed: Vec<&ResolvedAgent> = resolved
        .iter()
        .filter(|a| a.live_status == "FAILED" || a.live_status == "failed")
        .collect();

    if !failed.is_empty() {
        for agent in &failed {
            actions.push(format!(
                "{}. Investigate {} failure: crosslink kickoff report {}",
                action_num, agent.slug, agent.slug
            ));
            action_num += 1;
        }
    }

    // Agents not yet started
    let planned: Vec<&ResolvedAgent> = resolved
        .iter()
        .filter(|a| a.live_status == "planned")
        .collect();

    if !planned.is_empty() {
        let slugs: Vec<&str> = planned.iter().map(|a| a.slug.as_str()).collect();
        actions.push(format!(
            "{}. Launch remaining agents: {}",
            action_num,
            slugs.join(", ")
        ));
        action_num += 1;
    }

    // Unknown/stale agents
    let unknown: Vec<&ResolvedAgent> = resolved
        .iter()
        .filter(|a| a.live_status.starts_with("unknown"))
        .collect();

    if !unknown.is_empty() {
        for agent in &unknown {
            actions.push(format!(
                "{}. Check stale agent {}: worktree exists but no active session",
                action_num, agent.slug
            ));
            action_num += 1;
        }
    }

    // If all agents are done/merged, suggest gate
    let all_agents_resolved = ready_to_merge.is_empty()
        && running.is_empty()
        && failed.is_empty()
        && planned.is_empty()
        && unknown.is_empty();

    let phase_slug = slugify_phase(&phase_name);
    if all_agents_resolved {
        actions.push(format!(
            "{action_num}. All agents merged. Run gate: crosslink swarm gate {phase_slug}"
        ));
        action_num += 1;
        actions.push(format!(
            "{action_num}. If gate passes: crosslink swarm checkpoint {phase_slug}"
        ));
    } else if ready_to_merge.is_empty() && running.is_empty() && planned.is_empty() {
        // Only failed/unknown agents remain
        actions.push(format!(
            "{action_num}. After resolving failures: crosslink swarm gate {phase_slug}"
        ));
    } else {
        actions.push(format!(
            "{action_num}. After merges complete: crosslink swarm gate {phase_slug}"
        ));
        action_num += 1;
        if completed_count + 1 < plan.phases.len() {
            actions.push(format!(
                "{action_num}. If gate passes: crosslink swarm checkpoint {phase_slug}"
            ));
        }
    }

    println!("Next actions:");
    for action in &actions {
        println!("  {action}");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// swarm launch
// ---------------------------------------------------------------------------

/// Launch all planned agents for a phase via `kickoff run`.
pub fn launch(
    crosslink_dir: &Path,
    db: &Database,
    writer: Option<&SharedWriter>,
    phase_slug: &str,
    quiet: bool,
) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let (mut phase, phase_file) = load_phase(&sync, phase_slug)?;

    if phase.status == PhaseStatus::Completed {
        bail!("Phase '{}' is already completed", phase.name);
    }

    check_dependencies(&sync, &phase)?;

    let planned_agents: Vec<usize> = phase
        .agents
        .iter()
        .enumerate()
        .filter(|(_, a)| a.status == AgentStatus::Planned)
        .map(|(i, _)| i)
        .collect();

    if planned_agents.is_empty() {
        println!("No planned agents to launch in '{}'.", phase.name);
        println!("Use `crosslink swarm status` to see current agent states.");
        return Ok(());
    }

    let now = chrono::Utc::now().to_rfc3339();

    if !quiet {
        println!(
            "Launching {} agent{} for {}...",
            planned_agents.len(),
            if planned_agents.len() == 1 { "" } else { "s" },
            phase.name
        );
        println!();
    }

    for idx in &planned_agents {
        let slug = phase.agents[*idx].slug.clone();
        let description = phase.agents[*idx].description.clone();
        let issue_id = phase.agents[*idx].issue_id;
        let branch = phase.agents[*idx].branch.clone();

        let opts = KickoffOpts {
            description: &description,
            issue: issue_id,
            container: ContainerMode::None,
            verify: VerifyLevel::Local,
            model: "opus",
            image: kickoff::DEFAULT_AGENT_IMAGE,
            timeout: std::time::Duration::from_secs(3600),
            dry_run: false,
            branch: branch.as_deref(),
            quiet,
            design_doc: None,
            doc_path: None,
            skip_permissions: false,
        };

        match kickoff::run(crosslink_dir, db, writer, &opts) {
            Ok(compact_name) => {
                phase.agents[*idx].status = AgentStatus::Running;
                phase.agents[*idx].started_at = Some(now.clone());
                phase.agents[*idx].agent_id = Some(compact_name.clone());
                phase.agents[*idx].branch = Some(format!("feature/{compact_name}"));
            }
            Err(e) => {
                tracing::error!("Failed to launch {}: {}", slug, e);
                phase.agents[*idx].status = AgentStatus::Failed;
            }
        }
    }

    phase.status = PhaseStatus::InProgress;

    write_hub_json(&sync, &phase_file, &phase)?;
    commit_hub_files(
        &sync,
        &[phase_file.as_str()],
        &format!("swarm: launch {}", phase.name),
    )?;

    if !quiet {
        let running = phase
            .agents
            .iter()
            .filter(|a| a.status == AgentStatus::Running)
            .count();
        let failed = phase
            .agents
            .iter()
            .filter(|a| a.status == AgentStatus::Failed)
            .count();
        println!();
        println!(
            "{} agent{} launched, {} failed.",
            running,
            if running == 1 { "" } else { "s" },
            failed
        );
        println!();
        println!("Monitor with: crosslink swarm status");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// swarm gate
// ---------------------------------------------------------------------------

/// Run the project gate (test suite) for a phase and record the result.
pub fn gate(crosslink_dir: &Path, phase_slug: &str) -> Result<()> {
    // Auto-sync agent statuses before gating so phase JSON reflects live state
    if let Err(e) = sync_status(crosslink_dir) {
        tracing::warn!("could not sync agent statuses: {}", e);
    }

    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let (mut phase, phase_file) = load_phase(&sync, phase_slug)?;

    // Check that agents are resolved (all completed/merged/failed)
    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    let unresolved: Vec<&AgentEntry> = phase
        .agents
        .iter()
        .filter(|a| a.status == AgentStatus::Planned || a.status == AgentStatus::Running)
        .collect();

    if !unresolved.is_empty() {
        let live_unresolved: Vec<&AgentEntry> = unresolved
            .into_iter()
            .filter(|a| {
                let live = probe_agent_status(root, &a.slug);
                live == "planned" || live.starts_with("running")
            })
            .collect();

        if !live_unresolved.is_empty() {
            let names: Vec<&str> = live_unresolved.iter().map(|a| a.slug.as_str()).collect();
            bail!(
                "Cannot gate: {} agent(s) still unresolved: {}",
                live_unresolved.len(),
                names.join(", ")
            );
        }
    }

    // Detect project conventions and get test command
    let conventions = kickoff::detect_conventions(root);
    let test_cmd = conventions.test_command.as_deref().unwrap_or("cargo test");

    println!("Running gate: {test_cmd}");
    println!();

    let cmd_parts: Vec<&str> = test_cmd.split_whitespace().collect();
    let (program, args) = cmd_parts
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("Empty gate test command"))?;
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .with_context(|| format!("Failed to run gate command: {test_cmd}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let gate_passed = output.status.success();
    let now = chrono::Utc::now().to_rfc3339();

    // Try to parse test counts from output (Rust cargo test format)
    let (tests_total, tests_passed) = parse_test_counts(&stdout, &stderr);

    let gate_result = GateResult {
        status: if gate_passed {
            "passed".to_string()
        } else {
            "failed".to_string()
        },
        tests_total,
        tests_passed,
        ran_at: Some(now),
    };

    phase.gate = Some(gate_result);

    write_hub_json(&sync, &phase_file, &phase)?;
    commit_hub_files(
        &sync,
        &[phase_file.as_str()],
        &format!(
            "swarm: gate {} — {}",
            phase.name,
            if gate_passed { "passed" } else { "failed" }
        ),
    )?;

    if gate_passed {
        let tests_info = tests_total
            .map(|t| format!(" ({t} tests)"))
            .unwrap_or_default();
        println!("Gate passed{tests_info}");
        println!();
        println!(
            "Next: crosslink swarm checkpoint {}",
            slugify_phase(&phase.name)
        );
    } else {
        println!("Gate FAILED.");
        if !stderr.is_empty() {
            let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
            for line in tail.iter().rev() {
                println!("  {line}");
            }
        }
        println!();
        println!("Fix failures and re-run: crosslink swarm gate {phase_slug}");
    }

    Ok(())
}

/// Parse test counts from combined stdout/stderr (supports cargo test output).
pub(super) fn parse_test_counts(stdout: &str, stderr: &str) -> (Option<u64>, Option<u64>) {
    // cargo test format: "test result: ok. 142 passed; 0 failed; 0 ignored; ..."
    for text in [stdout, stderr] {
        for line in text.lines() {
            if line.starts_with("test result:") {
                let mut passed: Option<u64> = None;
                let mut failed: Option<u64> = None;

                for part in line.split(';') {
                    let part = part.trim();
                    if part.ends_with("passed") {
                        passed = part.split_whitespace().find_map(|w| w.parse::<u64>().ok());
                    } else if part.ends_with("failed") {
                        failed = part.split_whitespace().find_map(|w| w.parse::<u64>().ok());
                    }
                }

                if let (Some(p), Some(f)) = (passed, failed) {
                    return (Some(p + f), Some(p));
                }
            }
        }
    }
    (None, None)
}

// ---------------------------------------------------------------------------
// swarm checkpoint
// ---------------------------------------------------------------------------

/// Record a checkpoint after a phase completes.
pub fn checkpoint(
    crosslink_dir: &Path,
    phase_slug: &str,
    notes: Option<&str>,
    force: bool,
) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let (mut phase, phase_file) = load_phase(&sync, phase_slug)?;

    // Verify gate passed (unless --force)
    if !force {
        match &phase.gate {
            Some(g) if g.status == "passed" => {}
            Some(g) => bail!(
                "Gate status is '{}', not 'passed'. Use --force to checkpoint anyway.",
                g.status
            ),
            None => bail!(
                "No gate result recorded. Run `crosslink swarm gate {phase_slug}` first, or use --force."
            ),
        }
    }

    let now = chrono::Utc::now().to_rfc3339();

    // Get current dev branch SHA
    let dev_sha = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    let agents_merged: Vec<String> = phase
        .agents
        .iter()
        .filter(|a| a.status == AgentStatus::Merged || a.status == AgentStatus::Completed)
        .map(|a| a.slug.clone())
        .collect();

    let agents_pending: Vec<String> = phase
        .agents
        .iter()
        .filter(|a| a.status != AgentStatus::Merged && a.status != AgentStatus::Completed)
        .map(|a| a.slug.clone())
        .collect();

    let test_result = phase
        .gate
        .as_ref()
        .and_then(|g| match (g.tests_total, g.tests_passed) {
            (Some(total), Some(passed)) => Some(TestResult {
                total,
                passed,
                failed: total.saturating_sub(passed),
            }),
            _ => None,
        });

    let cp = Checkpoint {
        phase: phase.name.clone(),
        created_at: now.clone(),
        agents_merged,
        agents_pending,
        dev_branch_sha: dev_sha,
        test_result,
        handoff_notes: notes.map(ToString::to_string),
    };

    let ctx = resolve_swarm(&sync)?;
    let cp_slug = slugify_phase(&phase.name);
    let cp_path = ctx.checkpoint_path(&cp_slug);
    write_hub_json(&sync, &cp_path, &cp)?;

    // Mark phase completed
    phase.status = PhaseStatus::Completed;
    phase.checkpoint = Some(cp_slug);
    for agent in &mut phase.agents {
        if agent.status == AgentStatus::Completed {
            agent.status = AgentStatus::Merged;
            agent.completed_at = Some(now.clone());
        }
    }

    write_hub_json(&sync, &phase_file, &phase)?;
    commit_hub_files(
        &sync,
        &[phase_file.as_str(), cp_path.as_str()],
        &format!("swarm: checkpoint {}", phase.name),
    )?;

    println!("Checkpoint recorded for {}", phase.name);
    if let Some(n) = notes {
        println!("  Notes: {n}");
    }

    // Check if there's a next phase
    let plan: SwarmPlan = read_hub_json(&sync, &ctx.plan_path())?;
    let current_idx = plan
        .phases
        .iter()
        .position(|p| slugify_phase(p) == slugify_phase(&phase.name));

    if let Some(idx) = current_idx {
        if idx + 1 < plan.phases.len() {
            let next = &plan.phases[idx + 1];
            println!();
            println!(
                "Next phase: {} → crosslink swarm launch {}",
                next,
                slugify_phase(next)
            );
        } else {
            println!();
            println!("All phases completed. Swarm build is done.");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Find the latest checkpoint file by modification time.
pub(super) fn find_latest_checkpoint(dir: &Path) -> Option<Checkpoint> {
    if !dir.is_dir() {
        return None;
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();

    entries.sort_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()));

    if let Some(entry) = entries.last() {
        let content = std::fs::read_to_string(entry.path()).ok()?;
        serde_json::from_str(&content).ok()
    } else {
        None
    }
}
