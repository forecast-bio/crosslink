// Swarm budget: config, estimate, budget-aware launch, harvest costs,
// multi-window plan.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::io::*;
use super::lifecycle::launch;
use super::types::*;
use crate::commands::kickoff;
use crate::db::Database;
use crate::shared_writer::SharedWriter;
use crate::sync::SyncManager;

// ---------------------------------------------------------------------------
// swarm config (budget)
// ---------------------------------------------------------------------------

/// Set budget parameters for the swarm.
pub fn config_budget(crosslink_dir: &Path, budget_window: &str, model: &str) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let budget_window_s = kickoff::parse_duration(budget_window)?.as_secs();

    let config = BudgetConfig {
        budget_window_s,
        model: model.to_string(),
    };

    let ctx = resolve_swarm(&sync)?;
    let budget_path = ctx.budget_path();
    write_hub_json(&sync, &budget_path, &config)?;
    commit_hub_files(
        &sync,
        &[&budget_path],
        &format!("swarm: set budget {}  model={}", budget_window, model),
    )?;

    println!(
        "Budget configured: {} window, model={}",
        kickoff::format_duration(budget_window_s),
        model
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// swarm estimate
// ---------------------------------------------------------------------------

/// Default per-agent duration estimates when no historical data exists.
pub(super) fn default_agent_duration(model: &str) -> u64 {
    match model {
        "opus" => 5400,   // 90 minutes
        "sonnet" => 2700, // 45 minutes
        _ => 3600,        // 60 minutes fallback
    }
}

/// Overhead per agent for merging (seconds).
pub(super) const MERGE_OVERHEAD_PER_AGENT_S: u64 = 300; // 5 minutes
/// Overhead for running the gate (seconds).
pub(super) const GATE_OVERHEAD_S: u64 = 600; // 10 minutes

/// Estimate wall-clock cost for a phase.
pub(super) fn estimate_phase_cost(
    phase: &PhaseDefinition,
    cost_log: &CostLog,
    model: &str,
) -> (u64, Vec<(String, u64)>) {
    let mut agent_estimates: Vec<(String, u64)> = Vec::new();

    let model_est = cost_log.model_estimates.get(model);

    for agent in &phase.agents {
        if agent.status != AgentStatus::Planned {
            continue; // already running/done
        }

        let duration = if let Some(est) = model_est {
            est.p90_duration_s
        } else {
            default_agent_duration(model)
        };

        agent_estimates.push((agent.slug.clone(), duration));
    }

    let agent_total: u64 = agent_estimates.iter().map(|(_, d)| *d).sum();
    let overhead = agent_estimates.len() as u64 * MERGE_OVERHEAD_PER_AGENT_S + GATE_OVERHEAD_S;
    let total = agent_total + overhead;

    (total, agent_estimates)
}

/// Compute a budget recommendation.
pub(super) fn budget_recommendation(
    phase_cost: u64,
    remaining_budget: u64,
    agent_count: usize,
) -> BudgetRecommendation {
    let overhead = agent_count as u64 * MERGE_OVERHEAD_PER_AGENT_S + GATE_OVERHEAD_S;

    if remaining_budget < overhead {
        return BudgetRecommendation::Block {
            reason: format!(
                "Remaining budget ({}) is less than coordinator overhead ({})",
                kickoff::format_duration(remaining_budget),
                kickoff::format_duration(overhead)
            ),
        };
    }

    if phase_cost > remaining_budget {
        // How many agents can we afford?
        let per_agent = if agent_count > 0 {
            (phase_cost - overhead) / agent_count as u64
        } else {
            0
        };
        let affordable = if per_agent > 0 {
            ((remaining_budget - overhead) / per_agent) as usize
        } else {
            0
        };
        return BudgetRecommendation::Split {
            recommended_count: affordable.max(1),
        };
    }

    let threshold = (remaining_budget as f64 * 0.8) as u64;
    if phase_cost < threshold {
        BudgetRecommendation::Proceed
    } else {
        BudgetRecommendation::ProceedWithCaution
    }
}

/// Estimate cost for a phase and display the breakdown.
pub fn estimate(crosslink_dir: &Path, phase_slug: &str) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let ctx = resolve_swarm(&sync)?;
    let (phase, _) = load_phase(&sync, phase_slug)?;

    let budget_config: BudgetConfig =
        read_hub_json(&sync, &ctx.budget_path()).unwrap_or(BudgetConfig {
            budget_window_s: 18000, // default 5h
            model: "opus".to_string(),
        });

    let cost_log: CostLog = read_hub_json(&sync, &ctx.history_path()).unwrap_or_default();

    let (total_cost, agent_estimates) =
        estimate_phase_cost(&phase, &cost_log, &budget_config.model);

    println!("Estimate for: {}", phase.name);
    println!("  Model: {}", budget_config.model);
    println!(
        "  Budget window: {}",
        kickoff::format_duration(budget_config.budget_window_s)
    );
    println!();

    for (slug, duration) in &agent_estimates {
        println!("  {:<35} {}", slug, kickoff::format_duration(*duration));
    }

    let agent_count = agent_estimates.len();
    let overhead = agent_count as u64 * MERGE_OVERHEAD_PER_AGENT_S + GATE_OVERHEAD_S;

    println!();
    println!(
        "  Agent time:       {}",
        kickoff::format_duration(total_cost - overhead)
    );
    println!(
        "  Coordinator overhead: {}",
        kickoff::format_duration(overhead)
    );
    println!(
        "  Total estimate:   {}",
        kickoff::format_duration(total_cost)
    );
    println!();

    let recommendation =
        budget_recommendation(total_cost, budget_config.budget_window_s, agent_count);

    match &recommendation {
        BudgetRecommendation::Proceed => {
            println!("Recommendation: PROCEED — fits comfortably within budget.");
        }
        BudgetRecommendation::ProceedWithCaution => {
            println!("Recommendation: PROCEED WITH CAUTION — tight fit.");
        }
        BudgetRecommendation::Split { recommended_count } => {
            println!(
                "Recommendation: SPLIT — budget supports ~{} of {} agents.",
                recommended_count, agent_count
            );
            println!(
                "  Suggest: launch first {} agents, checkpoint, then launch the rest.",
                recommended_count
            );
        }
        BudgetRecommendation::Block { reason } => {
            println!("Recommendation: BLOCK — {}", reason);
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Budget-aware launch wrapper
// ---------------------------------------------------------------------------

/// Launch with budget awareness: estimate first, warn/block if over budget.
pub fn launch_budget_aware(
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

    let ctx = resolve_swarm(&sync)?;
    let (phase, _) = load_phase(&sync, phase_slug)?;

    let budget_config: BudgetConfig =
        read_hub_json(&sync, &ctx.budget_path()).unwrap_or(BudgetConfig {
            budget_window_s: 18000,
            model: "opus".to_string(),
        });

    let cost_log: CostLog = read_hub_json(&sync, &ctx.history_path()).unwrap_or_default();

    let planned_count = phase
        .agents
        .iter()
        .filter(|a| a.status == AgentStatus::Planned)
        .count();

    let (total_cost, _) = estimate_phase_cost(&phase, &cost_log, &budget_config.model);
    let recommendation =
        budget_recommendation(total_cost, budget_config.budget_window_s, planned_count);

    match &recommendation {
        BudgetRecommendation::Block { reason } => {
            bail!(
                "Budget check BLOCKED launch: {}\n\
                 Use `crosslink swarm launch {}` (without --budget-aware) to override.",
                reason,
                phase_slug
            );
        }
        BudgetRecommendation::Split { recommended_count } => {
            tracing::warn!(
                "Budget supports ~{} of {} agents. Consider splitting the phase. Launching all {} agents anyway. Use `crosslink swarm estimate {}` for details.",
                recommended_count, planned_count, planned_count, phase_slug
            );
        }
        BudgetRecommendation::ProceedWithCaution => {
            if !quiet {
                tracing::info!("Budget is tight. Proceeding with caution.");
            }
        }
        BudgetRecommendation::Proceed => {}
    }

    // Delegate to the regular launch
    launch(crosslink_dir, db, writer, phase_slug, quiet)
}

// ---------------------------------------------------------------------------
// Cost log harvesting
// ---------------------------------------------------------------------------

/// Scan completed agent worktrees and update the cost log with observations.
pub fn harvest_costs(crosslink_dir: &Path) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    let worktrees_dir = root.join(".worktrees");
    if !worktrees_dir.is_dir() {
        println!("No worktrees found.");
        return Ok(());
    }

    let ctx = resolve_swarm(&sync)?;
    let mut cost_log: CostLog = read_hub_json(&sync, &ctx.history_path()).unwrap_or_default();

    let existing_ids: std::collections::HashSet<String> = cost_log
        .observations
        .iter()
        .map(|o| o.agent_id.clone())
        .collect();

    let mut new_observations = 0u32;

    let entries = std::fs::read_dir(&worktrees_dir).context("Failed to read .worktrees")?;
    for entry in entries.filter_map(|e| e.ok()) {
        let report_file = entry.path().join(".kickoff-report.json");
        if !report_file.exists() {
            continue;
        }

        let content = match std::fs::read_to_string(&report_file) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let report: kickoff::KickoffReport = match serde_json::from_str(&content) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let agent_id = report
            .agent_id
            .clone()
            .unwrap_or_else(|| entry.file_name().to_string_lossy().to_string());

        if existing_ids.contains(&agent_id) {
            continue;
        }

        // Extract total duration from phases
        let duration_s = report
            .phases
            .as_ref()
            .map(|p| {
                [
                    p.exploration.as_ref(),
                    p.planning.as_ref(),
                    p.implementation.as_ref(),
                    p.testing.as_ref(),
                    p.validation.as_ref(),
                    p.review.as_ref(),
                ]
                .iter()
                .filter_map(|t| t.map(|t| t.duration_s))
                .sum::<u64>()
            })
            .unwrap_or(0);

        if duration_s == 0 {
            continue;
        }

        let lines_added = report
            .phases
            .as_ref()
            .and_then(|p| p.implementation.as_ref().and_then(|t| t.lines_added));

        let files_changed = report.files_changed.as_ref().map(|f| f.len() as u64);

        let obs = CostObservation {
            agent_id,
            model: "opus".to_string(), // default; reports don't track model
            duration_s,
            files_changed,
            lines_added,
        };

        cost_log.observations.push(obs);
        new_observations += 1;
    }

    // Recompute model estimates from observations
    recompute_model_estimates(&mut cost_log);

    let history_path = ctx.history_path();
    write_hub_json(&sync, &history_path, &cost_log)?;
    commit_hub_files(
        &sync,
        &[&history_path],
        &format!("swarm: harvest {} cost observations", new_observations),
    )?;

    println!(
        "Harvested {} new observation{} ({} total).",
        new_observations,
        if new_observations == 1 { "" } else { "s" },
        cost_log.observations.len()
    );

    if let Some(est) = cost_log.model_estimates.get("opus") {
        println!(
            "  opus: median {}, p90 {}",
            kickoff::format_duration(est.median_duration_s),
            kickoff::format_duration(est.p90_duration_s)
        );
    }

    Ok(())
}

/// Recompute median and p90 estimates per model from observations.
pub(super) fn recompute_model_estimates(cost_log: &mut CostLog) {
    let mut by_model: std::collections::HashMap<String, Vec<u64>> =
        std::collections::HashMap::new();

    for obs in &cost_log.observations {
        by_model
            .entry(obs.model.clone())
            .or_default()
            .push(obs.duration_s);
    }

    cost_log.model_estimates.clear();
    for (model, mut durations) in by_model {
        durations.sort();
        let len = durations.len();
        let median = durations[len / 2];
        let p90_idx = ((len as f64) * 0.9).ceil() as usize;
        let p90 = durations[p90_idx.min(len - 1)];

        cost_log.model_estimates.insert(
            model,
            ModelEstimate {
                median_duration_s: median,
                p90_duration_s: p90,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// swarm plan (multi-window)
// ---------------------------------------------------------------------------

/// Bin-pack phases into budget windows and return the allocation plan.
pub(super) fn pack_windows(
    phases: &[(String, u64, usize)], // (name, estimate_s, agent_count)
    window_s: u64,
) -> Vec<WindowAllocation> {
    let mut windows: Vec<WindowAllocation> = Vec::new();
    let mut current = WindowAllocation {
        window_index: 1,
        phases: Vec::new(),
        total_estimate_s: 0,
        buffer_s: window_s,
        stop_point: String::new(),
    };

    for (name, estimate, _agent_count) in phases {
        let fit = if current.total_estimate_s + estimate <= (window_s as f64 * 0.8) as u64 {
            WindowFit::Fits
        } else if current.total_estimate_s + estimate <= window_s {
            WindowFit::Tight
        } else {
            WindowFit::Overflow
        };

        if fit == WindowFit::Overflow && !current.phases.is_empty() {
            // Close current window
            current.buffer_s = window_s.saturating_sub(current.total_estimate_s);
            current.stop_point = format!(
                "after {} gate → checkpoint",
                current
                    .phases
                    .last()
                    .map(|p| p.name.as_str())
                    .unwrap_or("?")
            );
            windows.push(current);

            current = WindowAllocation {
                window_index: windows.len() + 1,
                phases: Vec::new(),
                total_estimate_s: 0,
                buffer_s: window_s,
                stop_point: String::new(),
            };
        }

        let recalculated_fit =
            if current.total_estimate_s + estimate <= (window_s as f64 * 0.8) as u64 {
                WindowFit::Fits
            } else if current.total_estimate_s + estimate <= window_s {
                WindowFit::Tight
            } else {
                WindowFit::Overflow
            };

        current.total_estimate_s += estimate;
        current.phases.push(WindowPhase {
            name: name.clone(),
            agent_count: *_agent_count,
            estimate_s: *estimate,
            fit: recalculated_fit,
        });
    }

    // Close last window
    if !current.phases.is_empty() {
        current.buffer_s = window_s.saturating_sub(current.total_estimate_s);
        current.stop_point = format!(
            "after {} gate → final checkpoint",
            current
                .phases
                .last()
                .map(|p| p.name.as_str())
                .unwrap_or("?")
        );
        windows.push(current);
    }

    windows
}

/// Plan a multi-phase build across budget windows.
pub fn plan(crosslink_dir: &Path, budget_window: Option<&str>) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let ctx = resolve_swarm(&sync)?;
    let swarm_plan: SwarmPlan = read_hub_json(&sync, &ctx.plan_path())
        .context("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")?;

    let budget_config: BudgetConfig =
        read_hub_json(&sync, &ctx.budget_path()).unwrap_or(BudgetConfig {
            budget_window_s: 18000,
            model: "opus".to_string(),
        });

    let window_s = if let Some(w) = budget_window {
        kickoff::parse_duration(w)?.as_secs()
    } else {
        budget_config.budget_window_s
    };

    let cost_log: CostLog = read_hub_json(&sync, &ctx.history_path()).unwrap_or_default();

    // Estimate each phase
    let mut phase_estimates: Vec<(String, u64, usize)> = Vec::new();
    for phase_name in &swarm_plan.phases {
        let phase_file = ctx.phase_path(phase_name);
        let phase: PhaseDefinition = match read_hub_json(&sync, &phase_file) {
            Ok(p) => p,
            Err(_) => continue,
        };

        let planned_count = phase
            .agents
            .iter()
            .filter(|a| a.status == AgentStatus::Planned || a.status == AgentStatus::Running)
            .count();

        if phase.status == PhaseStatus::Completed {
            continue;
        }

        let (estimate, _) = estimate_phase_cost(&phase, &cost_log, &budget_config.model);
        phase_estimates.push((phase_name.clone(), estimate, planned_count));
    }

    if phase_estimates.is_empty() {
        println!("All phases completed. Nothing to plan.");
        return Ok(());
    }

    let windows = pack_windows(&phase_estimates, window_s);
    let total_estimate: u64 = phase_estimates.iter().map(|(_, e, _)| e).sum();

    // Display
    println!("Swarm: {}", swarm_plan.title);
    println!(
        "Estimated total cost: ~{} budget window{}",
        windows.len(),
        if windows.len() == 1 { "" } else { "s" }
    );
    println!();

    for window in &windows {
        println!(
            "Window {} ({}):",
            window.window_index,
            kickoff::format_duration(window_s)
        );
        for wp in &window.phases {
            let fit_label = match wp.fit {
                WindowFit::Fits => "fits",
                WindowFit::Tight => "fits, tight",
                WindowFit::Overflow => "OVERFLOW",
            };
            println!(
                "  {}: {} agent{}, est. ~{} ({})",
                wp.name,
                wp.agent_count,
                if wp.agent_count == 1 { "" } else { "s" },
                kickoff::format_duration(wp.estimate_s),
                fit_label
            );
        }
        println!("  Buffer: ~{}", kickoff::format_duration(window.buffer_s));
        println!("  Stop point: {}", window.stop_point);
        println!();
    }

    // Natural safe stops
    println!("Natural safe stops:");
    let total_phases = phase_estimates.len();
    for (i, (name, _, _)) in phase_estimates.iter().enumerate() {
        let is_window_boundary = windows
            .iter()
            .any(|w| w.phases.last().map(|p| p.name == *name).unwrap_or(false));
        let is_last = i == total_phases - 1;

        let qualifier = if is_last {
            "REQUIRED — build complete"
        } else if is_window_boundary {
            "REQUIRED — window boundary"
        } else {
            "optional, early exit"
        };

        println!("  After {} gate ({})", name, qualifier);
    }

    println!();
    println!(
        "Total estimate: {}",
        kickoff::format_duration(total_estimate)
    );

    Ok(())
}

/// Show the window plan (recomputes from current swarm state).
pub fn plan_show(crosslink_dir: &Path) -> Result<()> {
    plan(crosslink_dir, None)
}
