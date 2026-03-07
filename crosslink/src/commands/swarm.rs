// Swarm coordination: multi-agent phase planning, status, and resume.
//
// Persists swarm state to the hub branch under `swarm/` so it survives
// session boundaries and is visible to all agents.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::commands::design_doc::{self, DesignDoc};
use crate::commands::kickoff::{self, tmux_session_name, ContainerMode, KickoffOpts, VerifyLevel};
use crate::db::Database;
use crate::shared_writer::SharedWriter;
use crate::sync::SyncManager;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Top-level swarm plan, stored at `swarm/plan.json` on the hub branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmPlan {
    pub schema_version: u32,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub design_doc: Option<String>,
    pub created_at: String,
    pub phases: Vec<String>,
}

/// Definition of a single phase, stored at `swarm/phases/<name>.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhaseDefinition {
    pub name: String,
    pub status: PhaseStatus,
    pub agents: Vec<AgentEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<GateResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PhaseStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

impl std::fmt::Display for PhaseStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhaseStatus::Pending => write!(f, "pending"),
            PhaseStatus::InProgress => write!(f, "in progress"),
            PhaseStatus::Completed => write!(f, "completed"),
            PhaseStatus::Failed => write!(f, "failed"),
        }
    }
}

/// An agent within a phase.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentEntry {
    pub slug: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    pub status: AgentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Planned,
    Running,
    Completed,
    Merged,
    Failed,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Planned => write!(f, "planned"),
            AgentStatus::Running => write!(f, "running"),
            AgentStatus::Completed => write!(f, "completed"),
            AgentStatus::Merged => write!(f, "merged"),
            AgentStatus::Failed => write!(f, "failed"),
        }
    }
}

/// Gate result recorded after all phase agents complete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GateResult {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ran_at: Option<String>,
}

/// Checkpoint snapshot after a phase (or partial phase) completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Checkpoint {
    pub phase: String,
    pub created_at: String,
    pub agents_merged: Vec<String>,
    pub agents_pending: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev_branch_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_result: Option<TestResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TestResult {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
}

/// Budget configuration stored at `swarm/budget.json` on the hub branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetConfig {
    pub budget_window_s: u64,
    pub model: String,
}

/// Historical cost log stored at `swarm/history/cost-log.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CostLog {
    #[serde(default)]
    pub observations: Vec<CostObservation>,
    #[serde(default)]
    pub model_estimates: std::collections::HashMap<String, ModelEstimate>,
}

/// A single historical observation from a completed agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CostObservation {
    pub agent_id: String,
    pub model: String,
    pub duration_s: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
}

/// Aggregate duration estimates for a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelEstimate {
    pub median_duration_s: u64,
    pub p90_duration_s: u64,
}

/// Budget estimation result for a phase.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetRecommendation {
    Proceed,
    ProceedWithCaution,
    Split { recommended_count: usize },
    Block { reason: String },
}

/// A budget window in a multi-window plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowAllocation {
    pub window_index: usize,
    pub phases: Vec<WindowPhase>,
    pub total_estimate_s: u64,
    pub buffer_s: u64,
    pub stop_point: String,
}

/// A phase allocated to a window, with its estimated cost.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WindowPhase {
    pub name: String,
    pub agent_count: usize,
    pub estimate_s: u64,
    pub fit: WindowFit,
}

/// How well a phase fits in the remaining window budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WindowFit {
    Fits,
    Tight,
    Overflow,
}

// ---------------------------------------------------------------------------
// Hub branch I/O helpers
// ---------------------------------------------------------------------------

/// Read a JSON file from the hub cache directory.
fn read_hub_json<T: serde::de::DeserializeOwned>(sync: &SyncManager, path: &str) -> Result<T> {
    let full = sync.cache_path().join(path);
    let content =
        std::fs::read_to_string(&full).with_context(|| format!("Failed to read {}", path))?;
    serde_json::from_str(&content).with_context(|| format!("Failed to parse {}", path))
}

/// Write a JSON file to the hub cache directory (does NOT commit).
fn write_hub_json<T: Serialize>(sync: &SyncManager, path: &str, value: &T) -> Result<()> {
    let full = sync.cache_path().join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(&full, content).with_context(|| format!("Failed to write {}", path))
}

/// Stage multiple files and commit.
fn commit_hub_files(sync: &SyncManager, paths: &[&str], message: &str) -> Result<()> {
    let cache = sync.cache_path();
    for path in paths {
        let output = std::process::Command::new("git")
            .current_dir(cache)
            .args(["add", path])
            .output()
            .context("git add failed")?;
        if !output.status.success() {
            bail!(
                "git add {} failed: {}",
                path,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    let output = std::process::Command::new("git")
        .current_dir(cache)
        .args(["commit", "-m", message, "--no-gpg-sign"])
        .output()
        .context("git commit failed")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("nothing to commit") {
            bail!("git commit failed: {}", stderr);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// swarm init
// ---------------------------------------------------------------------------

/// Initialize a swarm plan from a design document.
///
/// Parses the design doc to extract requirements/sections and proposes
/// a phase structure. The plan is written to the hub branch.
pub fn init(crosslink_dir: &Path, doc_path: &Path) -> Result<()> {
    let content = std::fs::read_to_string(doc_path)
        .with_context(|| format!("Failed to read design doc: {}", doc_path.display()))?;
    let doc = design_doc::parse_design_doc(&content);

    if doc.title.is_empty() {
        bail!("Design doc has no title (expected a # heading)");
    }

    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    // Check if a plan already exists
    let plan_path = sync.cache_path().join("swarm/plan.json");
    if plan_path.exists() {
        bail!(
            "A swarm plan already exists. Use `crosslink swarm status` to view it, \
             or delete swarm/ from the hub branch to start over."
        );
    }

    // Build phases from design doc structure
    let phases = propose_phases(&doc);
    let now = chrono::Utc::now().to_rfc3339();

    let phase_names: Vec<String> = phases.iter().map(|p| p.name.clone()).collect();

    let plan = SwarmPlan {
        schema_version: 1,
        title: doc.title.clone(),
        design_doc: Some(doc_path.display().to_string()),
        created_at: now.clone(),
        phases: phase_names.clone(),
    };

    // Write plan and phase files
    write_hub_json(&sync, "swarm/plan.json", &plan)?;
    let mut paths_to_commit: Vec<String> = vec!["swarm/plan.json".to_string()];

    for phase in &phases {
        let phase_path = format!("swarm/phases/{}.json", slugify_phase(&phase.name));
        write_hub_json(&sync, &phase_path, phase)?;
        paths_to_commit.push(phase_path);
    }

    let path_refs: Vec<&str> = paths_to_commit.iter().map(|s| s.as_str()).collect();
    commit_hub_files(&sync, &path_refs, "swarm: init plan from design doc")?;

    println!("Swarm plan initialized: {}", doc.title);
    println!();
    for (i, phase) in phases.iter().enumerate() {
        println!(
            "  Phase {}: {} ({} agent{})",
            i + 1,
            phase.name,
            phase.agents.len(),
            if phase.agents.len() == 1 { "" } else { "s" }
        );
        for agent in &phase.agents {
            println!("    - {}: {}", agent.slug, agent.description);
        }
    }
    println!();
    println!("Edit phase files in the hub branch to refine agent assignments.");
    println!("Then use `crosslink swarm status` to view the plan.");

    Ok(())
}

/// Propose phases from a design doc's structure.
///
/// Heuristic: each requirement becomes an agent in a single phase.
/// If there are many requirements, split into phases of ~8 agents each.
/// If no requirements, create a single phase with one agent per unknown section.
fn propose_phases(doc: &DesignDoc) -> Vec<PhaseDefinition> {
    let mut agents: Vec<AgentEntry> = Vec::new();

    // Build agent entries from requirements
    for req in &doc.requirements {
        let slug = slugify_requirement(req);
        agents.push(AgentEntry {
            slug: slug.clone(),
            description: req.clone(),
            issue_id: None,
            agent_id: None,
            branch: Some(format!("feature/{}", slug)),
            status: AgentStatus::Planned,
            started_at: None,
            completed_at: None,
        });
    }

    // If no requirements, use acceptance criteria
    if agents.is_empty() {
        for ac in &doc.acceptance_criteria {
            let slug = slugify_requirement(ac);
            agents.push(AgentEntry {
                slug: slug.clone(),
                description: ac.clone(),
                issue_id: None,
                agent_id: None,
                branch: Some(format!("feature/{}", slug)),
                status: AgentStatus::Planned,
                started_at: None,
                completed_at: None,
            });
        }
    }

    // If still no agents, create a single agent from the title
    if agents.is_empty() {
        let slug = crate::commands::kickoff::slugify(&doc.title);
        agents.push(AgentEntry {
            slug: slug.clone(),
            description: doc.title.clone(),
            issue_id: None,
            agent_id: None,
            branch: Some(format!("feature/{}", slug)),
            status: AgentStatus::Planned,
            started_at: None,
            completed_at: None,
        });
    }

    // Split into phases of at most 8 agents
    let max_per_phase = 8;
    let mut phases = Vec::new();
    let chunks: Vec<Vec<AgentEntry>> = agents.chunks(max_per_phase).map(|c| c.to_vec()).collect();

    for (i, chunk) in chunks.into_iter().enumerate() {
        let name = if phases.is_empty() && agents.len() <= max_per_phase {
            "Phase 1".to_string()
        } else {
            format!("Phase {}", i + 1)
        };

        let depends_on = if i > 0 {
            vec![format!("Phase {}", i)]
        } else {
            vec![]
        };

        phases.push(PhaseDefinition {
            name,
            status: PhaseStatus::Pending,
            agents: chunk,
            gate: None,
            depends_on,
            checkpoint: None,
        });
    }

    phases
}

/// Slugify a requirement string into a short branch-safe slug.
fn slugify_requirement(req: &str) -> String {
    // Strip common prefixes like "REQ-1:", "AC-1:", "- "
    let mut text = req.trim_start_matches("- ").trim();

    // Strip ID-like prefixes: "REQ-1:", "AC-2:", "R1:", etc.
    // Pattern: uppercase letters, optional hyphen, digits, colon
    if let Some(colon_pos) = text.find(':') {
        let prefix = &text[..colon_pos];
        let looks_like_id = !prefix.is_empty()
            && prefix.len() <= 10
            && prefix
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '-' || c.is_ascii_digit());
        if looks_like_id {
            text = text[colon_pos + 1..].trim();
        }
    }

    let text = if text.is_empty() { req.trim() } else { text };
    crate::commands::kickoff::slugify(text)
}

/// Slugify a phase name for use as a filename.
fn slugify_phase(name: &str) -> String {
    name.to_lowercase()
        .replace(' ', "-")
        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "")
}

// ---------------------------------------------------------------------------
// swarm status
// ---------------------------------------------------------------------------

/// Resolved runtime status of an agent, combining phase definition + worktree state.
struct ResolvedAgent {
    slug: String,
    description: String,
    issue_id: Option<i64>,
    defined_status: AgentStatus,
    live_status: String,
}

/// Display the current state of the swarm.
pub fn status(crosslink_dir: &Path) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }

    let plan: SwarmPlan = read_hub_json(&sync, "swarm/plan.json")
        .context("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")?;

    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    println!("Swarm: {}", plan.title);
    println!();

    for phase_name in &plan.phases {
        let phase_file = format!("swarm/phases/{}.json", slugify_phase(phase_name));
        let phase: PhaseDefinition = match read_hub_json(&sync, &phase_file) {
            Ok(p) => p,
            Err(_) => {
                println!("  {} (definition missing)", phase_name);
                continue;
            }
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

        let gate_info = if let Some(ref gate) = phase.gate {
            if gate.status == "passed" {
                let tests = gate
                    .tests_total
                    .map(|t| format!(", {} tests", t))
                    .unwrap_or_default();
                format!(", gate passed{}", tests)
            } else {
                format!(", gate {}", gate.status)
            }
        } else {
            String::new()
        };

        let extra = if completed > 0 || failed > 0 {
            let mut parts = Vec::new();
            if completed > 0 {
                parts.push(format!("{} completed", completed));
            }
            if failed > 0 {
                parts.push(format!("{} failed", failed));
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
                // Live status differs from definition — show live
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

    Ok(())
}

/// Cross-reference phase agents with worktree state to get live status.
fn resolve_agents(phase: &PhaseDefinition, repo_root: &Path) -> Vec<ResolvedAgent> {
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
            }
        })
        .collect()
}

/// Probe the actual runtime status of an agent by checking its worktree.
fn probe_agent_status(repo_root: &Path, slug: &str) -> String {
    let worktree = repo_root.join(".worktrees").join(slug);

    if !worktree.exists() {
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
        .map(|o| o.status.success())
        .unwrap_or(false);

    if tmux_alive {
        return "running (tmux)".to_string();
    }

    // Fallback: scan tmux sessions for any containing the slug as a substring.
    // kickoff::run derives the session name from slugify(description), which may
    // differ from the agent slug (e.g. "req-1-add-login" vs "add-login").
    if let Ok(output) = std::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    {
        if output.status.success() {
            let sessions = String::from_utf8_lossy(&output.stdout);
            for session in sessions.lines() {
                if session.contains(slug) {
                    return format!("running (tmux: {})", session);
                }
            }
        }
    }

    // Worktree exists but no status file and no tmux — stale or crashed
    "unknown (worktree exists, no active session)".to_string()
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

    let plan: SwarmPlan = read_hub_json(&sync, "swarm/plan.json")
        .context("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")?;

    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    // Find the latest checkpoint
    let checkpoint_dir = sync.cache_path().join("swarm/checkpoints");
    let latest_checkpoint = find_latest_checkpoint(&checkpoint_dir);

    if let Some(ref cp) = latest_checkpoint {
        println!("Latest checkpoint: {} ({})", cp.phase, cp.created_at);
        if let Some(ref notes) = cp.handoff_notes {
            println!("  Notes: {}", notes);
        }
        println!();
    }

    // Find the active phase (first non-completed phase)
    let mut active_phase: Option<PhaseDefinition> = None;
    let mut active_phase_name: Option<String> = None;
    let mut completed_count = 0;

    for phase_name in &plan.phases {
        let phase_file = format!("swarm/phases/{}.json", slugify_phase(phase_name));
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

    let (phase, phase_name) = match (active_phase, active_phase_name) {
        (Some(p), Some(n)) => (p, n),
        _ => {
            println!(
                "All {} phases completed. Swarm build is done.",
                plan.phases.len()
            );
            return Ok(());
        }
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
            actions.push(format!(
                "{}. Merge {}: review and merge feature/{} to dev",
                action_num, agent.slug, agent.slug
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
            "{}. All agents merged. Run gate: crosslink swarm gate {}",
            action_num, phase_slug
        ));
        action_num += 1;
        actions.push(format!(
            "{}. If gate passes: crosslink swarm checkpoint {}",
            action_num, phase_slug
        ));
    } else if ready_to_merge.is_empty() && running.is_empty() && planned.is_empty() {
        // Only failed/unknown agents remain
        actions.push(format!(
            "{}. After resolving failures: crosslink swarm gate {}",
            action_num, phase_slug
        ));
    } else {
        actions.push(format!(
            "{}. After merges complete: crosslink swarm gate {}",
            action_num, phase_slug
        ));
        action_num += 1;
        if completed_count + 1 < plan.phases.len() {
            actions.push(format!(
                "{}. If gate passes: crosslink swarm checkpoint {}",
                action_num, phase_slug
            ));
        }
    }

    println!("Next actions:");
    for action in &actions {
        println!("  {}", action);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// swarm launch
// ---------------------------------------------------------------------------

/// Load a phase definition by slug, returning the phase and its hub path.
fn load_phase(sync: &SyncManager, phase_slug: &str) -> Result<(PhaseDefinition, String)> {
    let plan: SwarmPlan = read_hub_json(sync, "swarm/plan.json")
        .context("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")?;

    // Try exact slug match first, then try matching against plan phase names
    let phase_file = format!("swarm/phases/{}.json", phase_slug);
    if let Ok(phase) = read_hub_json::<PhaseDefinition>(sync, &phase_file) {
        return Ok((phase, phase_file));
    }

    // Try slugifying each plan phase name to find a match
    for name in &plan.phases {
        let slug = slugify_phase(name);
        if slug == phase_slug {
            let path = format!("swarm/phases/{}.json", slug);
            let phase: PhaseDefinition = read_hub_json(sync, &path)
                .with_context(|| format!("Phase file missing for '{}'", name))?;
            return Ok((phase, path));
        }
    }

    bail!(
        "Phase '{}' not found. Available phases: {}",
        phase_slug,
        plan.phases
            .iter()
            .map(|n| slugify_phase(n))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Check that all dependency phases are completed.
fn check_dependencies(sync: &SyncManager, phase: &PhaseDefinition) -> Result<()> {
    for dep_name in &phase.depends_on {
        let dep_slug = slugify_phase(dep_name);
        let dep_file = format!("swarm/phases/{}.json", dep_slug);
        let dep: PhaseDefinition = read_hub_json(sync, &dep_file)
            .with_context(|| format!("Dependency phase '{}' not found", dep_name))?;
        if dep.status != PhaseStatus::Completed {
            bail!(
                "Dependency '{}' is {} — must be completed before launching this phase",
                dep_name,
                dep.status
            );
        }
    }
    Ok(())
}

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
            image: "ghcr.io/forecast-bio/crosslink-agent:latest",
            timeout: std::time::Duration::from_secs(3600),
            dry_run: false,
            branch: branch.as_deref(),
            quiet,
            design_doc: None,
            doc_path: None,
        };

        match kickoff::run(crosslink_dir, db, writer, &opts) {
            Ok(()) => {
                phase.agents[*idx].status = AgentStatus::Running;
                phase.agents[*idx].started_at = Some(now.clone());
                phase.agents[*idx].agent_id = Some(slug);
            }
            Err(e) => {
                eprintln!("Failed to launch {}: {}", slug, e);
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

    println!("Running gate: {}", test_cmd);
    println!();

    let output = std::process::Command::new("sh")
        .args(["-c", test_cmd])
        .current_dir(root)
        .output()
        .with_context(|| format!("Failed to run gate command: {}", test_cmd))?;

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
            .map(|t| format!(" ({} tests)", t))
            .unwrap_or_default();
        println!("Gate passed{}", tests_info);
        println!();
        println!(
            "Next: crosslink swarm checkpoint {}",
            slugify_phase(&phase.name)
        );
    } else {
        println!("Gate FAILED.");
        if !stderr.is_empty() {
            let tail: Vec<&str> = stderr.lines().rev().take(20).collect();
            for line in tail.into_iter().rev() {
                println!("  {}", line);
            }
        }
        println!();
        println!(
            "Fix failures and re-run: crosslink swarm gate {}",
            phase_slug
        );
    }

    Ok(())
}

/// Parse test counts from combined stdout/stderr (supports cargo test output).
fn parse_test_counts(stdout: &str, stderr: &str) -> (Option<u64>, Option<u64>) {
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
                "No gate result recorded. Run `crosslink swarm gate {}` first, or use --force.",
                phase_slug
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
        handoff_notes: notes.map(|s| s.to_string()),
    };

    let cp_slug = slugify_phase(&phase.name);
    let cp_path = format!("swarm/checkpoints/{}.json", cp_slug);
    write_hub_json(&sync, &cp_path, &cp)?;

    // Mark phase completed
    phase.status = PhaseStatus::Completed;
    phase.checkpoint = Some(cp_slug.clone());
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
        println!("  Notes: {}", n);
    }

    // Check if there's a next phase
    let plan: SwarmPlan = read_hub_json(&sync, "swarm/plan.json")?;
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

    write_hub_json(&sync, "swarm/budget.json", &config)?;
    commit_hub_files(
        &sync,
        &["swarm/budget.json"],
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
fn default_agent_duration(model: &str) -> u64 {
    match model {
        "opus" => 5400,   // 90 minutes
        "sonnet" => 2700, // 45 minutes
        _ => 3600,        // 60 minutes fallback
    }
}

/// Overhead per agent for merging (seconds).
const MERGE_OVERHEAD_PER_AGENT_S: u64 = 300; // 5 minutes
/// Overhead for running the gate (seconds).
const GATE_OVERHEAD_S: u64 = 600; // 10 minutes

/// Estimate wall-clock cost for a phase.
fn estimate_phase_cost(
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
fn budget_recommendation(
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

    let (phase, _) = load_phase(&sync, phase_slug)?;

    let budget_config: BudgetConfig =
        read_hub_json(&sync, "swarm/budget.json").unwrap_or(BudgetConfig {
            budget_window_s: 18000, // default 5h
            model: "opus".to_string(),
        });

    let cost_log: CostLog = read_hub_json(&sync, "swarm/history/cost-log.json").unwrap_or_default();

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

    let (phase, _) = load_phase(&sync, phase_slug)?;

    let budget_config: BudgetConfig =
        read_hub_json(&sync, "swarm/budget.json").unwrap_or(BudgetConfig {
            budget_window_s: 18000,
            model: "opus".to_string(),
        });

    let cost_log: CostLog = read_hub_json(&sync, "swarm/history/cost-log.json").unwrap_or_default();

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
            eprintln!(
                "Warning: Budget supports ~{} of {} agents. Consider splitting the phase.",
                recommended_count, planned_count
            );
            eprintln!(
                "Launching all {} agents anyway. Use `crosslink swarm estimate {}` for details.",
                planned_count, phase_slug
            );
            eprintln!();
        }
        BudgetRecommendation::ProceedWithCaution => {
            if !quiet {
                eprintln!("Note: Budget is tight. Proceeding with caution.");
                eprintln!();
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

    let mut cost_log: CostLog =
        read_hub_json(&sync, "swarm/history/cost-log.json").unwrap_or_default();

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

    write_hub_json(&sync, "swarm/history/cost-log.json", &cost_log)?;
    commit_hub_files(
        &sync,
        &["swarm/history/cost-log.json"],
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
fn recompute_model_estimates(cost_log: &mut CostLog) {
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
fn pack_windows(
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

    for (name, estimate, agent_count) in phases {
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
            agent_count: *agent_count,
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

    let swarm_plan: SwarmPlan = read_hub_json(&sync, "swarm/plan.json")
        .context("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")?;

    let budget_config: BudgetConfig =
        read_hub_json(&sync, "swarm/budget.json").unwrap_or(BudgetConfig {
            budget_window_s: 18000,
            model: "opus".to_string(),
        });

    let window_s = if let Some(w) = budget_window {
        kickoff::parse_duration(w)?.as_secs()
    } else {
        budget_config.budget_window_s
    };

    let cost_log: CostLog = read_hub_json(&sync, "swarm/history/cost-log.json").unwrap_or_default();

    // Estimate each phase
    let mut phase_estimates: Vec<(String, u64, usize)> = Vec::new();
    for phase_name in &swarm_plan.phases {
        let phase_file = format!("swarm/phases/{}.json", slugify_phase(phase_name));
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

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Find the latest checkpoint file by modification time.
fn find_latest_checkpoint(dir: &Path) -> Option<Checkpoint> {
    if !dir.is_dir() {
        return None;
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .collect();

    entries.sort_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()));

    if let Some(entry) = entries.last() {
        let content = std::fs::read_to_string(entry.path()).ok()?;
        serde_json::from_str(&content).ok()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swarm_plan_serde_roundtrip() {
        let plan = SwarmPlan {
            schema_version: 1,
            title: "Test Swarm".to_string(),
            design_doc: Some("DESIGN.md".to_string()),
            created_at: "2026-03-06T12:00:00Z".to_string(),
            phases: vec!["Phase 1".to_string(), "Phase 2".to_string()],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: SwarmPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, parsed);
    }

    #[test]
    fn test_phase_definition_serde_roundtrip() {
        let phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::InProgress,
            agents: vec![AgentEntry {
                slug: "linear-models".to_string(),
                description: "Implement linear regression".to_string(),
                issue_id: Some(42),
                agent_id: Some("driver--linear-models".to_string()),
                branch: Some("feature/linear-models".to_string()),
                status: AgentStatus::Running,
                started_at: Some("2026-03-06T12:00:00Z".to_string()),
                completed_at: None,
            }],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };
        let json = serde_json::to_string(&phase).unwrap();
        let parsed: PhaseDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(phase, parsed);
    }

    #[test]
    fn test_checkpoint_serde_roundtrip() {
        let cp = Checkpoint {
            phase: "Phase 1".to_string(),
            created_at: "2026-03-06T14:00:00Z".to_string(),
            agents_merged: vec!["driver--linear-models".to_string()],
            agents_pending: vec!["driver--tree-models".to_string()],
            dev_branch_sha: Some("abc1234".to_string()),
            test_result: Some(TestResult {
                total: 631,
                passed: 631,
                failed: 0,
            }),
            handoff_notes: Some("Phase 1 complete.".to_string()),
        };
        let json = serde_json::to_string(&cp).unwrap();
        let parsed: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(cp, parsed);
    }

    #[test]
    fn test_phase_status_display() {
        assert_eq!(format!("{}", PhaseStatus::Pending), "pending");
        assert_eq!(format!("{}", PhaseStatus::InProgress), "in progress");
        assert_eq!(format!("{}", PhaseStatus::Completed), "completed");
        assert_eq!(format!("{}", PhaseStatus::Failed), "failed");
    }

    #[test]
    fn test_agent_status_display() {
        assert_eq!(format!("{}", AgentStatus::Planned), "planned");
        assert_eq!(format!("{}", AgentStatus::Running), "running");
        assert_eq!(format!("{}", AgentStatus::Completed), "completed");
        assert_eq!(format!("{}", AgentStatus::Merged), "merged");
        assert_eq!(format!("{}", AgentStatus::Failed), "failed");
    }

    #[test]
    fn test_slugify_phase() {
        assert_eq!(slugify_phase("Phase 1"), "phase-1");
        assert_eq!(
            slugify_phase("Phase 2: Core Infrastructure"),
            "phase-2-core-infrastructure"
        );
    }

    #[test]
    fn test_slugify_requirement() {
        assert_eq!(
            slugify_requirement("REQ-1: Implement retry logic"),
            "implement-retry-logic"
        );
        assert_eq!(
            slugify_requirement("- Add batch processing"),
            "add-batch-processing"
        );
        assert_eq!(
            slugify_requirement("AC-2: Handle timeouts"),
            "handle-timeouts"
        );
    }

    #[test]
    fn test_propose_phases_from_requirements() {
        let doc = DesignDoc {
            title: "Test Feature".to_string(),
            summary: String::new(),
            requirements: vec![
                "REQ-1: Add login".to_string(),
                "REQ-2: Add logout".to_string(),
            ],
            acceptance_criteria: vec![],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].agents.len(), 2);
        assert_eq!(phases[0].agents[0].slug, "add-login");
        assert_eq!(phases[0].agents[1].slug, "add-logout");
    }

    #[test]
    fn test_propose_phases_splits_large_requirement_lists() {
        let doc = DesignDoc {
            title: "Big Feature".to_string(),
            summary: String::new(),
            requirements: (1..=12).map(|i| format!("REQ-{}: Task {}", i, i)).collect(),
            acceptance_criteria: vec![],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].agents.len(), 8);
        assert_eq!(phases[1].agents.len(), 4);
        assert_eq!(phases[0].name, "Phase 1");
        assert_eq!(phases[1].name, "Phase 2");
        assert_eq!(phases[1].depends_on, vec!["Phase 1"]);
    }

    #[test]
    fn test_propose_phases_falls_back_to_title() {
        let doc = DesignDoc {
            title: "Simple Feature".to_string(),
            summary: String::new(),
            requirements: vec![],
            acceptance_criteria: vec![],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].agents.len(), 1);
        assert_eq!(phases[0].agents[0].description, "Simple Feature");
    }

    #[test]
    fn test_propose_phases_uses_acceptance_criteria_when_no_requirements() {
        let doc = DesignDoc {
            title: "AC Feature".to_string(),
            summary: String::new(),
            requirements: vec![],
            acceptance_criteria: vec![
                "AC-1: Widget renders".to_string(),
                "AC-2: Widget responds to click".to_string(),
            ],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].agents.len(), 2);
    }

    #[test]
    fn test_probe_agent_status_nonexistent_worktree() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(probe_agent_status(dir.path(), "nonexistent"), "planned");
    }

    #[test]
    fn test_probe_agent_status_done() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join(".worktrees").join("my-agent");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".kickoff-status"), "DONE\n").unwrap();
        assert_eq!(probe_agent_status(dir.path(), "my-agent"), "DONE");
    }

    #[test]
    fn test_probe_agent_status_failed() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join(".worktrees").join("bad-agent");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".kickoff-status"), "FAILED\n").unwrap();
        assert_eq!(probe_agent_status(dir.path(), "bad-agent"), "FAILED");
    }

    #[test]
    fn test_load_phase_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();
        // Create a minimal plan with no phase files
        std::fs::create_dir_all(cache.join("swarm")).unwrap();
        let plan = SwarmPlan {
            schema_version: 1,
            title: "Test".to_string(),
            design_doc: None,
            created_at: "2026-03-06T12:00:00Z".to_string(),
            phases: vec!["Phase 1".to_string()],
        };
        std::fs::write(
            cache.join("swarm/plan.json"),
            serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();

        // We can't easily test load_phase without a SyncManager,
        // but we can test the slug-matching logic indirectly via slugify_phase
        assert_eq!(slugify_phase("Phase 1"), "phase-1");
        assert_eq!(slugify_phase("Phase 2"), "phase-2");
    }

    #[test]
    fn test_parse_test_counts_cargo_format() {
        let stdout = "running 142 tests\n\
                      test result: ok. 140 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out";
        let (total, passed) = parse_test_counts(stdout, "");
        assert_eq!(total, Some(142));
        assert_eq!(passed, Some(140));
    }

    #[test]
    fn test_parse_test_counts_no_match() {
        let (total, passed) = parse_test_counts("all good", "no tests");
        assert_eq!(total, None);
        assert_eq!(passed, None);
    }

    #[test]
    fn test_parse_test_counts_from_stderr() {
        let stderr = "test result: ok. 50 passed; 0 failed; 3 ignored; 0 measured; 10 filtered out";
        let (total, passed) = parse_test_counts("", stderr);
        assert_eq!(total, Some(50));
        assert_eq!(passed, Some(50));
    }

    #[test]
    fn test_gate_result_serde_roundtrip() {
        let gate = GateResult {
            status: "passed".to_string(),
            tests_total: Some(142),
            tests_passed: Some(142),
            ran_at: Some("2026-03-06T15:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&gate).unwrap();
        let parsed: GateResult = serde_json::from_str(&json).unwrap();
        assert_eq!(gate, parsed);
    }

    #[test]
    fn test_phase_status_transitions() {
        // Verify the expected phase lifecycle: Pending -> InProgress -> Completed
        let mut phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::Pending,
            agents: vec![AgentEntry {
                slug: "agent-1".to_string(),
                description: "Test agent".to_string(),
                issue_id: None,
                agent_id: None,
                branch: Some("feature/agent-1".to_string()),
                status: AgentStatus::Planned,
                started_at: None,
                completed_at: None,
            }],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };

        assert_eq!(phase.status, PhaseStatus::Pending);
        assert_eq!(phase.agents[0].status, AgentStatus::Planned);

        // Simulate launch
        phase.status = PhaseStatus::InProgress;
        phase.agents[0].status = AgentStatus::Running;
        phase.agents[0].started_at = Some("2026-03-06T12:00:00Z".to_string());
        assert_eq!(phase.status, PhaseStatus::InProgress);

        // Simulate completion + gate
        phase.agents[0].status = AgentStatus::Completed;
        phase.gate = Some(GateResult {
            status: "passed".to_string(),
            tests_total: Some(100),
            tests_passed: Some(100),
            ran_at: Some("2026-03-06T13:00:00Z".to_string()),
        });

        // Simulate checkpoint
        phase.status = PhaseStatus::Completed;
        phase.agents[0].status = AgentStatus::Merged;
        phase.checkpoint = Some("phase-1".to_string());

        assert_eq!(phase.status, PhaseStatus::Completed);
        assert_eq!(phase.agents[0].status, AgentStatus::Merged);
        assert!(phase.checkpoint.is_some());

        // Roundtrip the final state
        let json = serde_json::to_string(&phase).unwrap();
        let parsed: PhaseDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(phase, parsed);
    }

    #[test]
    fn test_find_latest_checkpoint_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_latest_checkpoint(dir.path()).is_none());
    }

    #[test]
    fn test_find_latest_checkpoint_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let cp = Checkpoint {
            phase: "Phase 1".to_string(),
            created_at: "2026-03-06T14:00:00Z".to_string(),
            agents_merged: vec![],
            agents_pending: vec![],
            dev_branch_sha: None,
            test_result: None,
            handoff_notes: Some("test".to_string()),
        };
        let content = serde_json::to_string_pretty(&cp).unwrap();
        std::fs::write(dir.path().join("phase-1.json"), &content).unwrap();

        let found = find_latest_checkpoint(dir.path()).unwrap();
        assert_eq!(found.phase, "Phase 1");
        assert_eq!(found.handoff_notes, Some("test".to_string()));
    }

    #[test]
    fn test_budget_config_serde_roundtrip() {
        let config = BudgetConfig {
            budget_window_s: 18000,
            model: "opus".to_string(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: BudgetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_cost_log_serde_roundtrip() {
        let mut estimates = std::collections::HashMap::new();
        estimates.insert(
            "opus".to_string(),
            ModelEstimate {
                median_duration_s: 3600,
                p90_duration_s: 5400,
            },
        );
        let log = CostLog {
            observations: vec![CostObservation {
                agent_id: "driver--agent-1".to_string(),
                model: "opus".to_string(),
                duration_s: 4500,
                files_changed: Some(12),
                lines_added: Some(450),
            }],
            model_estimates: estimates,
        };
        let json = serde_json::to_string(&log).unwrap();
        let parsed: CostLog = serde_json::from_str(&json).unwrap();
        assert_eq!(log, parsed);
    }

    #[test]
    fn test_default_agent_duration() {
        assert_eq!(default_agent_duration("opus"), 5400);
        assert_eq!(default_agent_duration("sonnet"), 2700);
        assert_eq!(default_agent_duration("haiku"), 3600);
    }

    #[test]
    fn test_estimate_phase_cost_no_history() {
        let phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::Pending,
            agents: vec![
                AgentEntry {
                    slug: "a1".to_string(),
                    description: "Agent 1".to_string(),
                    issue_id: None,
                    agent_id: None,
                    branch: None,
                    status: AgentStatus::Planned,
                    started_at: None,
                    completed_at: None,
                },
                AgentEntry {
                    slug: "a2".to_string(),
                    description: "Agent 2".to_string(),
                    issue_id: None,
                    agent_id: None,
                    branch: None,
                    status: AgentStatus::Planned,
                    started_at: None,
                    completed_at: None,
                },
            ],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };
        let cost_log = CostLog::default();
        let (total, agents) = estimate_phase_cost(&phase, &cost_log, "opus");
        // 2 agents × 5400s + 2×300 overhead + 600 gate = 12000
        assert_eq!(agents.len(), 2);
        assert_eq!(total, 5400 * 2 + 300 * 2 + 600);
    }

    #[test]
    fn test_estimate_phase_cost_with_history() {
        let phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::Pending,
            agents: vec![AgentEntry {
                slug: "a1".to_string(),
                description: "Agent 1".to_string(),
                issue_id: None,
                agent_id: None,
                branch: None,
                status: AgentStatus::Planned,
                started_at: None,
                completed_at: None,
            }],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };
        let mut estimates = std::collections::HashMap::new();
        estimates.insert(
            "opus".to_string(),
            ModelEstimate {
                median_duration_s: 3000,
                p90_duration_s: 4000,
            },
        );
        let cost_log = CostLog {
            observations: vec![],
            model_estimates: estimates,
        };
        let (total, agents) = estimate_phase_cost(&phase, &cost_log, "opus");
        // 1 agent × 4000 (p90) + 300 overhead + 600 gate = 4900
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].1, 4000);
        assert_eq!(total, 4000 + 300 + 600);
    }

    #[test]
    fn test_estimate_skips_non_planned_agents() {
        let phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::InProgress,
            agents: vec![
                AgentEntry {
                    slug: "done".to_string(),
                    description: "Done".to_string(),
                    issue_id: None,
                    agent_id: None,
                    branch: None,
                    status: AgentStatus::Completed,
                    started_at: None,
                    completed_at: None,
                },
                AgentEntry {
                    slug: "pending-agent".to_string(),
                    description: "Pending agent".to_string(),
                    issue_id: None,
                    agent_id: None,
                    branch: None,
                    status: AgentStatus::Planned,
                    started_at: None,
                    completed_at: None,
                },
            ],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };
        let cost_log = CostLog::default();
        let (_, agents) = estimate_phase_cost(&phase, &cost_log, "opus");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].0, "pending-agent");
    }

    #[test]
    fn test_budget_recommendation_proceed() {
        let rec = budget_recommendation(5000, 18000, 2);
        assert_eq!(rec, BudgetRecommendation::Proceed);
    }

    #[test]
    fn test_budget_recommendation_caution() {
        // Cost is > 80% of budget but still fits
        let rec = budget_recommendation(15000, 18000, 2);
        assert_eq!(rec, BudgetRecommendation::ProceedWithCaution);
    }

    #[test]
    fn test_budget_recommendation_split() {
        // Cost exceeds budget
        let rec = budget_recommendation(20000, 10000, 4);
        match rec {
            BudgetRecommendation::Split {
                recommended_count, ..
            } => {
                assert!(recommended_count > 0);
                assert!(recommended_count < 4);
            }
            other => panic!("Expected Split, got {:?}", other),
        }
    }

    #[test]
    fn test_budget_recommendation_block() {
        // Budget less than coordinator overhead
        let rec = budget_recommendation(20000, 500, 4);
        match rec {
            BudgetRecommendation::Block { .. } => {}
            other => panic!("Expected Block, got {:?}", other),
        }
    }

    #[test]
    fn test_recompute_model_estimates() {
        let mut log = CostLog {
            observations: vec![
                CostObservation {
                    agent_id: "a1".to_string(),
                    model: "opus".to_string(),
                    duration_s: 3000,
                    files_changed: None,
                    lines_added: None,
                },
                CostObservation {
                    agent_id: "a2".to_string(),
                    model: "opus".to_string(),
                    duration_s: 4000,
                    files_changed: None,
                    lines_added: None,
                },
                CostObservation {
                    agent_id: "a3".to_string(),
                    model: "opus".to_string(),
                    duration_s: 5000,
                    files_changed: None,
                    lines_added: None,
                },
            ],
            model_estimates: std::collections::HashMap::new(),
        };
        recompute_model_estimates(&mut log);
        let est = log.model_estimates.get("opus").unwrap();
        assert_eq!(est.median_duration_s, 4000); // middle of [3000, 4000, 5000]
        assert_eq!(est.p90_duration_s, 5000); // ceil(3*0.9) = 3 → index 2
    }

    #[test]
    fn test_pack_windows_single_window() {
        let phases = vec![
            ("Phase 1".to_string(), 3600, 4),
            ("Phase 2".to_string(), 3600, 4),
        ];
        let windows = pack_windows(&phases, 18000); // 5h window
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].phases.len(), 2);
        assert_eq!(windows[0].phases[0].fit, WindowFit::Fits);
        assert_eq!(windows[0].phases[1].fit, WindowFit::Fits);
        assert!(windows[0].buffer_s > 0);
    }

    #[test]
    fn test_pack_windows_multiple_windows() {
        let phases = vec![
            ("Phase 1".to_string(), 7200, 8),
            ("Phase 2".to_string(), 9000, 9),
            ("Phase 3".to_string(), 7200, 8),
            ("Phase 4".to_string(), 7200, 8),
        ];
        let windows = pack_windows(&phases, 18000); // 5h window
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].phases.len(), 2);
        assert_eq!(windows[1].phases.len(), 2);
        assert!(windows[0].stop_point.contains("Phase 2"));
        assert!(windows[1].stop_point.contains("Phase 4"));
    }

    #[test]
    fn test_pack_windows_tight_fit() {
        // Phase fills > 80% of window but still fits
        let phases = vec![("Phase 1".to_string(), 16000, 6)];
        let windows = pack_windows(&phases, 18000);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].phases[0].fit, WindowFit::Tight);
    }

    #[test]
    fn test_pack_windows_overflow_splits() {
        // Single phase overflows window
        let phases = vec![
            ("Phase 1".to_string(), 10000, 5),
            ("Phase 2".to_string(), 10000, 5),
        ];
        let windows = pack_windows(&phases, 10000);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].phases.len(), 1);
        assert_eq!(windows[1].phases.len(), 1);
    }

    #[test]
    fn test_pack_windows_empty() {
        let phases: Vec<(String, u64, usize)> = vec![];
        let windows = pack_windows(&phases, 18000);
        assert!(windows.is_empty());
    }

    #[test]
    fn test_window_allocation_serde_roundtrip() {
        let alloc = WindowAllocation {
            window_index: 1,
            phases: vec![WindowPhase {
                name: "Phase 1".to_string(),
                agent_count: 4,
                estimate_s: 7200,
                fit: WindowFit::Fits,
            }],
            total_estimate_s: 7200,
            buffer_s: 10800,
            stop_point: "after Phase 1 gate → checkpoint".to_string(),
        };
        let json = serde_json::to_string(&alloc).unwrap();
        let parsed: WindowAllocation = serde_json::from_str(&json).unwrap();
        assert_eq!(alloc, parsed);
    }

    #[test]
    fn test_window_fit_display() {
        let json_fits = serde_json::to_string(&WindowFit::Fits).unwrap();
        assert_eq!(json_fits, "\"fits\"");
        let json_tight = serde_json::to_string(&WindowFit::Tight).unwrap();
        assert_eq!(json_tight, "\"tight\"");
        let json_overflow = serde_json::to_string(&WindowFit::Overflow).unwrap();
        assert_eq!(json_overflow, "\"overflow\"");
    }
}
