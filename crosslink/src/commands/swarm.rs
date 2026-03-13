// Swarm coordination: multi-agent phase planning, status, and resume.
//
// Persists swarm state to the hub branch under `swarm/` so it survives
// session boundaries and is visible to all agents.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::commands::design_doc::{self, DesignDoc};
use crate::commands::kickoff::{self, tmux_session_name, ContainerMode, KickoffOpts, VerifyLevel};
use crate::db::Database;
use crate::findings;
use crate::issue_filing;
use crate::pipeline::{self, Pipeline, PipelineConfig};
use crate::seam;
use crate::shared_writer::SharedWriter;
use crate::sync::SyncManager;
use crate::trust_model;

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
// Merge orchestration data model
// ---------------------------------------------------------------------------

/// Top-level merge plan, stored at `swarm/merge-plan.json` on the hub branch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergePlan {
    pub target_branch: String,
    pub agents: Vec<MergeSource>,
    pub conflicts: Vec<FileConflict>,
    pub merge_order: Vec<String>, // agent slugs in application order
}

/// A single agent's worktree as a merge source.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MergeSource {
    pub agent_slug: String,
    pub worktree_path: PathBuf,
    pub changed_files: Vec<String>,
    pub commit_count: usize,
}

/// A file conflict between multiple agents.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileConflict {
    pub file: String,
    pub agents: Vec<String>,
    pub conflict_type: ConflictType,
}

/// Classification of a file conflict.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConflictType {
    /// Multiple agents modified the same file but different regions
    NonOverlapping,
    /// Multiple agents modified overlapping regions
    Overlapping,
    /// One agent created, another modified
    CreateModify,
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
        // Worktree removed — check if the agent's branch was merged or exists.
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

/// Check if a branch has been merged into the default branch (main/master).
fn is_branch_merged(repo_root: &Path, slug: &str) -> bool {
    // Try common branch naming patterns for swarm agents
    for branch in &[slug.to_string(), format!("swarm/{}", slug)] {
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
fn branch_exists(repo_root: &Path, slug: &str) -> bool {
    for branch in &[slug.to_string(), format!("swarm/{}", slug)] {
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
// swarm review — parallel adversarial review
// ---------------------------------------------------------------------------

/// The overall review plan stored at `swarm/review-plan.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPlan {
    pub mandate: String,
    pub mandate_prompt: String,
    pub agent_count: usize,
    pub created_at: String,
    pub agents: Vec<ReviewAgentAssignment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc_output: Option<PathBuf>,
}

/// Assignment of a partition to a review agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewAgentAssignment {
    pub agent_slug: String,
    pub partition_label: String,
    pub files: Vec<String>,
}

// Mandate prompt templates
const MANDATE_ADVERSARIAL: &str = "You are the ha-satan, the loyal accuser. \
    Find real problems that would cause failures in production. \
    Ignore style nits, focus on correctness, safety, and robustness.";

const MANDATE_SECURITY: &str = "Review for trust boundary violations, injection vectors, \
    data integrity issues, and unsafe operations.";

const MANDATE_ROBUSTNESS: &str = "Find crash paths, resource leaks, error handling gaps, \
    and unhandled edge cases.";

const MANDATE_CORRECTNESS: &str = "Find logic errors, race conditions, invariant violations, \
    and incorrect algorithm implementations.";

/// Map a mandate name to its prompt text.
pub fn mandate_prompt(mandate: &str) -> &str {
    match mandate {
        "adversarial" => MANDATE_ADVERSARIAL,
        "security" => MANDATE_SECURITY,
        "robustness" => MANDATE_ROBUSTNESS,
        "correctness" => MANDATE_CORRECTNESS,
        _ => mandate, // Custom mandate text passed through as-is
    }
}

/// Assign partitions to agents using round-robin distribution.
fn assign_partitions(
    partitions: Vec<seam::Partition>,
    agent_count: usize,
) -> Vec<ReviewAgentAssignment> {
    let agent_count = agent_count.max(1);
    let mut assignments: Vec<ReviewAgentAssignment> = (0..agent_count)
        .map(|i| ReviewAgentAssignment {
            agent_slug: format!("reviewer-{}", i + 1),
            partition_label: String::new(),
            files: Vec::new(),
        })
        .collect();

    for (i, partition) in partitions.into_iter().enumerate() {
        let agent_idx = i % agent_count;
        if !assignments[agent_idx].partition_label.is_empty() {
            assignments[agent_idx].partition_label.push_str(", ");
        }
        assignments[agent_idx]
            .partition_label
            .push_str(&partition.label);
        assignments[agent_idx].files.extend(
            partition
                .files
                .into_iter()
                .map(|f| f.to_string_lossy().to_string()),
        );
    }

    // Filter out agents with no files assigned
    assignments.retain(|a| !a.files.is_empty());
    assignments
}

/// Launch a parallel adversarial review across codebase partitions.
pub fn review(
    crosslink_dir: &Path,
    agent_count: usize,
    mandate: &str,
    doc: Option<&Path>,
    file_issues: bool,
    fix: bool,
) -> Result<()> {
    let repo_root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    // Discover source partitions via seam detection
    let partitions = seam::detect_seams(repo_root, agent_count)?;
    if partitions.is_empty() {
        bail!("No source files found in repo root. Nothing to review.");
    }

    println!(
        "Discovered {} source partition(s) in {}",
        partitions.len(),
        repo_root.display()
    );
    for p in &partitions {
        println!(
            "  {} ({} files, {} lines)",
            p.label,
            p.files.len(),
            p.line_count
        );
    }
    println!();

    // Assign partitions to agents
    let assignments = assign_partitions(partitions, agent_count);
    let prompt_text = mandate_prompt(mandate);
    let now = chrono::Utc::now().to_rfc3339();

    let plan = ReviewPlan {
        mandate: mandate.to_string(),
        mandate_prompt: prompt_text.to_string(),
        agent_count: assignments.len(),
        created_at: now,
        agents: assignments.clone(),
        doc_output: doc.map(|p| p.to_path_buf()),
    };

    // Persist plan to hub branch
    write_hub_json(&sync, "swarm/review-plan.json", &plan)?;
    commit_hub_files(
        &sync,
        &["swarm/review-plan.json"],
        "swarm: store review plan",
    )?;

    // Print summary
    println!("Review plan ({} mandate):", mandate);
    println!("  Prompt: {}", prompt_text);
    println!();
    println!("Agent assignments:");
    for agent in &plan.agents {
        println!(
            "  {} — partitions: [{}] ({} files)",
            agent.agent_slug,
            agent.partition_label,
            agent.files.len()
        );
    }
    println!();

    if let Some(doc_path) = doc {
        println!("Findings will be consolidated to: {}", doc_path.display());
    }

    println!("Plan saved to hub branch at swarm/review-plan.json");

    if file_issues || fix {
        // Run the pipeline for post-review stages
        let config = PipelineConfig {
            agent_count: assignments.len(),
            mandate: mandate.to_string(),
            auto_fix: fix,
            auto_file_issues: file_issues,
            target_branch: "develop".to_string(),
        };
        run_review_pipeline(crosslink_dir, config)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Review pipeline orchestration
// ---------------------------------------------------------------------------

/// Convert consolidated finding groups into the format expected by issue_filing.
fn findings_to_filing(groups: &[findings::FindingGroup]) -> Vec<issue_filing::FindingForFiling> {
    groups
        .iter()
        .map(|g| issue_filing::FindingForFiling {
            title: g.canonical.title.clone(),
            severity: g.effective_severity.to_string(),
            file: g.canonical.file.clone(),
            line: g.canonical.line,
            description: g.canonical.description.clone(),
            suggested_fix: g.canonical.suggested_fix.clone(),
            consensus_count: g.consensus_count,
        })
        .collect()
}

/// Consolidate review findings from agent reports on the hub branch.
fn consolidate_review_findings(crosslink_dir: &Path) -> Result<findings::ConsolidatedReport> {
    let sync = SyncManager::new(crosslink_dir)?;
    let findings_dir = sync.cache_path().join("swarm");
    let reports = findings::parse_reports(&findings_dir)?;
    if reports.is_empty() {
        bail!("No review findings found. Run review agents first.");
    }
    let consolidated = findings::consolidate(reports);

    // Persist consolidated report
    write_hub_json(&sync, "swarm/consolidated-report.json", &consolidated)?;
    let markdown = findings::generate_markdown_report(&consolidated);
    let md_path = sync
        .cache_path()
        .join("swarm")
        .join("consolidated-report.md");
    if let Some(parent) = md_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&md_path, &markdown)?;
    commit_hub_files(
        &sync,
        &[
            "swarm/consolidated-report.json",
            "swarm/consolidated-report.md",
        ],
        "swarm: consolidate review findings",
    )?;

    println!(
        "Consolidated {} findings from {} agents ({} after dedup)",
        consolidated.total_findings, consolidated.agent_count, consolidated.deduplicated_findings,
    );

    Ok(consolidated)
}

/// Apply trust model filtering to consolidated findings.
fn apply_trust_filtering(
    crosslink_dir: &Path,
    report: &findings::ConsolidatedReport,
) -> Vec<findings::FindingGroup> {
    let config = match trust_model::load_trust_config(crosslink_dir) {
        Ok(c) => c,
        Err(_) => return report.groups.clone(),
    };

    // Convert finding groups to tuples for the trust model batch API
    let finding_tuples: Vec<(String, String, String)> = report
        .groups
        .iter()
        .map(|g| {
            (
                g.canonical.title.clone(),
                g.canonical.description.clone(),
                g.effective_severity.to_string(),
            )
        })
        .collect();

    let annotated = trust_model::apply_trust_model(&config, finding_tuples);

    let mut kept = Vec::new();
    let mut by_design_count = 0;
    for (i, (_title, _desc, _sev, result)) in annotated.into_iter().enumerate() {
        let group = &report.groups[i];
        match result {
            trust_model::TriageResult::Valid => kept.push(group.clone()),
            trust_model::TriageResult::ByDesign { reason } => {
                println!("  [by-design] {} — {}", group.canonical.title, reason);
                by_design_count += 1;
            }
            trust_model::TriageResult::Downgraded { reason, .. } => {
                println!("  [downgraded] {} — {}", group.canonical.title, reason);
                kept.push(group.clone());
            }
        }
    }
    if by_design_count > 0 {
        println!("  {} finding(s) triaged as by-design", by_design_count);
    }
    kept
}

/// Drive the review pipeline through its stages.
fn run_review_pipeline(crosslink_dir: &Path, config: PipelineConfig) -> Result<()> {
    let mut pipe = match pipeline::load_pipeline(crosslink_dir)? {
        Some(p) => {
            println!("Resuming existing pipeline at stage: {}", p.current_stage);
            p
        }
        None => Pipeline::new(config),
    };

    loop {
        // Check for human checkpoints using the pipeline API
        if Pipeline::is_checkpoint(pipe.current_stage) {
            println!("\nPipeline paused for human review.");
            println!("Review findings in .crosslink/ or on the hub branch.");
            println!("Run `crosslink swarm review-continue` to proceed.");
            pipeline::save_pipeline(crosslink_dir, &pipe)?;
            return Ok(());
        }

        let stage_result: Result<()> = match pipe.current_stage {
            pipeline::PipelineStage::Partition | pipeline::PipelineStage::Review => {
                // Partitioning and agent launch already handled by review()
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::AwaitReview => {
                println!("Review agents launched. Check progress with `crosslink swarm status`.");
                println!("Run `crosslink swarm review-continue` when agents complete.");
                pipeline::save_pipeline(crosslink_dir, &pipe)?;
                return Ok(());
            }
            pipeline::PipelineStage::Consolidate => {
                let report = consolidate_review_findings(crosslink_dir)?;
                let filtered = apply_trust_filtering(crosslink_dir, &report);
                println!("{} findings after trust model filtering", filtered.len());
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::HumanCheckpoint => {
                // Handled by is_checkpoint check above
                unreachable!()
            }
            pipeline::PipelineStage::FileIssues => {
                if pipe.config.auto_file_issues {
                    let sync = SyncManager::new(crosslink_dir)?;
                    let report: findings::ConsolidatedReport =
                        read_hub_json(&sync, "swarm/consolidated-report.json")?;
                    let filtered = apply_trust_filtering(crosslink_dir, &report);

                    // Deduplicate against existing GitHub issues with the review label
                    let existing_titles = fetch_existing_review_titles();
                    let deduped = findings::cross_reference_issues(&filtered, &existing_titles);
                    if deduped.len() < filtered.len() {
                        println!(
                            "  Skipped {} finding(s) that match existing issues",
                            filtered.len() - deduped.len()
                        );
                    }

                    let for_filing = findings_to_filing(&deduped);
                    issue_filing::file_issues_batch(&for_filing, false)?;
                }
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::Fix => {
                if pipe.config.auto_fix {
                    println!("Launching fix agents...");
                    fix(crosslink_dir, None, Some("review-finding"), 6, false)?;
                }
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::AwaitFix => {
                println!("Fix agents launched. Check progress with `crosslink swarm status`.");
                println!("Run `crosslink swarm review-continue` when agents complete.");
                pipeline::save_pipeline(crosslink_dir, &pipe)?;
                return Ok(());
            }
            pipeline::PipelineStage::Merge | pipeline::PipelineStage::PullRequest => {
                println!(
                    "Stage {}: run `crosslink swarm merge` to combine changes.",
                    pipe.current_stage
                );
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::Done => {
                println!("Pipeline complete.");
                break;
            }
            pipeline::PipelineStage::Failed => {
                println!("Pipeline failed.");
                break;
            }
        };

        // On stage failure, mark the pipeline as failed and persist
        if let Err(e) = stage_result {
            pipe.fail(&e.to_string());
            pipeline::save_pipeline(crosslink_dir, &pipe)?;
            return Err(e);
        }

        pipeline::save_pipeline(crosslink_dir, &pipe)?;
    }

    Ok(())
}

/// Fetch titles of existing GitHub issues labeled "review-finding" for deduplication.
fn fetch_existing_review_titles() -> Vec<String> {
    match fetch_issues_by_label("review-finding") {
        Ok(issues) => issues.into_iter().map(|(_, title, _, _)| title).collect(),
        Err(_) => Vec::new(),
    }
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
// swarm fix — parallel issue-to-agent fix execution
// ---------------------------------------------------------------------------

/// Plan for parallel fix execution, stored at `swarm/fix-plan.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixPlan {
    pub schema_version: u32,
    pub created_at: String,
    pub issues: Vec<FixTarget>,
}

/// A single issue targeted for an agent fix.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixTarget {
    pub issue_number: u64,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub agent_slug: String,
    pub status: AgentStatus,
}

/// Fetch details for a single GitHub issue via `gh issue view`.
fn fetch_issue_details(number: u64) -> Result<(String, String, Vec<String>)> {
    let output = std::process::Command::new("gh")
        .args([
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "title,body,labels",
        ])
        .output()
        .context("Failed to run gh issue view")?;

    if !output.status.success() {
        bail!(
            "gh issue view {} failed: {}",
            number,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("Failed to parse gh issue view output")?;

    let title = parsed["title"].as_str().unwrap_or_default().to_string();
    let body = parsed["body"].as_str().unwrap_or_default().to_string();
    let labels = parsed["labels"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok((title, body, labels))
}

/// An issue fetched from GitHub with its number, title, body, and labels.
type LabeledIssue = (u64, String, String, Vec<String>);

/// Fetch issues matching a label via `gh issue list`.
fn fetch_issues_by_label(label: &str) -> Result<Vec<LabeledIssue>> {
    let output = std::process::Command::new("gh")
        .args([
            "issue",
            "list",
            "--label",
            label,
            "--json",
            "number,title,body,labels",
            "--limit",
            "100",
        ])
        .output()
        .context("Failed to run gh issue list")?;

    if !output.status.success() {
        bail!(
            "gh issue list --label {} failed: {}",
            label,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let parsed: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).context("Failed to parse gh issue list output")?;

    let mut results = Vec::new();
    for item in parsed {
        let number = item["number"].as_u64().unwrap_or(0);
        if number == 0 {
            continue;
        }
        let title = item["title"].as_str().unwrap_or_default().to_string();
        let body = item["body"].as_str().unwrap_or_default().to_string();
        let labels = item["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        results.push((number, title, body, labels));
    }

    Ok(results)
}

/// Create a slug for a fix agent from the issue number and title.
///
/// Example: `slugify_fix_target(326, "Buffer overflow in parser")` → `"fix-326-buffer-overflow-in-parser"`
fn slugify_fix_target(issue_number: u64, title: &str) -> String {
    let slug_part: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    // Truncate slug_part to keep the total slug reasonable
    let max_slug_len: usize = 50;
    let prefix = format!("fix-{}-", issue_number);
    let remaining = max_slug_len.saturating_sub(prefix.len());
    let truncated = if slug_part.len() > remaining {
        // Cut at a word boundary if possible
        match slug_part[..remaining].rfind('-') {
            Some(pos) if pos > 0 => &slug_part[..pos],
            _ => &slug_part[..remaining],
        }
    } else {
        &slug_part
    };

    format!("{}{}", prefix, truncated)
}

/// Parse comma-separated issue numbers from a string.
fn parse_issue_numbers(input: &str) -> Result<Vec<u64>> {
    input
        .split(',')
        .map(|s| {
            let trimmed = s.trim();
            trimmed
                .parse::<u64>()
                .with_context(|| format!("Invalid issue number: {:?}", trimmed))
        })
        .collect()
}

/// Build and persist a fix plan for parallel issue resolution.
pub fn fix(
    crosslink_dir: &Path,
    issues: Option<&str>,
    from_label: Option<&str>,
    max_agents: usize,
    budget_aware: bool,
) -> Result<()> {
    // Resolve issues from the provided source
    let issue_data: Vec<(u64, String, String, Vec<String>)> = match (issues, from_label) {
        (Some(ids), _) => {
            let numbers = parse_issue_numbers(ids)?;
            let mut data = Vec::new();
            for num in numbers {
                let (title, body, labels) = fetch_issue_details(num)?;
                data.push((num, title, body, labels));
            }
            data
        }
        (None, Some(label)) => fetch_issues_by_label(label)?,
        (None, None) => {
            bail!(
                "Either --issues or --from-label is required.\n\n\
                 Usage:\n  \
                   crosslink swarm fix --issues 326,327,328\n  \
                   crosslink swarm fix --from-label review-finding"
            );
        }
    };

    if issue_data.is_empty() {
        bail!("No issues found matching the given criteria.");
    }

    // Build fix targets
    let targets: Vec<FixTarget> = issue_data
        .into_iter()
        .map(|(number, title, body, labels)| {
            let agent_slug = slugify_fix_target(number, &title);
            FixTarget {
                issue_number: number,
                title,
                body,
                labels,
                agent_slug,
                status: AgentStatus::Planned,
            }
        })
        .collect();

    let now = chrono::Utc::now().to_rfc3339();
    let plan = FixPlan {
        schema_version: 1,
        created_at: now,
        issues: targets,
    };

    // Persist to hub branch
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    write_hub_json(&sync, "swarm/fix-plan.json", &plan)?;
    commit_hub_files(&sync, &["swarm/fix-plan.json"], "swarm: persist fix plan")?;

    // Print summary
    println!("Fix plan created with {} issue(s):\n", plan.issues.len());
    println!("  {:<8} {:<40} Labels", "Issue", "Agent Slug");
    println!("  {:<8} {:<40} ------", "-----", "----------");
    for target in &plan.issues {
        let labels_str = if target.labels.is_empty() {
            String::from("-")
        } else {
            target.labels.join(", ")
        };
        println!(
            "  #{:<7} {:<40} {}",
            target.issue_number, target.agent_slug, labels_str
        );
    }

    if plan.issues.len() > max_agents {
        println!(
            "\nNote: {} issues exceed max_agents ({}). Some will queue.",
            plan.issues.len(),
            max_agents
        );
    }

    if budget_aware {
        println!("\nBudget checking not yet integrated.");
    }

    println!("\nPlan persisted to hub branch at swarm/fix-plan.json");

    Ok(())
}

// ---------------------------------------------------------------------------
// swarm merge
// ---------------------------------------------------------------------------

/// Discover agent worktrees that have commits beyond the base branch (develop).
fn discover_worktrees(repo_root: &Path) -> Result<Vec<MergeSource>> {
    let worktrees_dir = repo_root.join(".worktrees");
    if !worktrees_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut sources = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(&worktrees_dir)
        .context("Failed to read .worktrees")?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let wt_path = entry.path();
        if !wt_path.is_dir() {
            continue;
        }

        let slug = entry.file_name().to_string_lossy().to_string();

        // Get changed files relative to develop
        let diff_output = std::process::Command::new("git")
            .current_dir(&wt_path)
            .args(["diff", "--name-only", "develop...HEAD"])
            .output();

        let changed_files = match diff_output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string())
                    .collect::<Vec<_>>()
            }
            _ => continue, // Skip worktrees where git diff fails
        };

        if changed_files.is_empty() {
            continue;
        }

        // Count commits beyond develop
        let log_output = std::process::Command::new("git")
            .current_dir(&wt_path)
            .args(["log", "--oneline", "develop..HEAD"])
            .output();

        let commit_count = match log_output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.lines().count()
            }
            _ => 0,
        };

        sources.push(MergeSource {
            agent_slug: slug,
            worktree_path: wt_path,
            changed_files,
            commit_count,
        });
    }

    Ok(sources)
}

/// Extract line ranges modified by a diff for a specific file in a worktree.
fn extract_diff_ranges(worktree: &Path, file: &str) -> Result<Vec<(usize, usize)>> {
    let output = std::process::Command::new("git")
        .current_dir(worktree)
        .args(["diff", "develop...HEAD", "--", file])
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut ranges = Vec::new();

    for line in stdout.lines() {
        // Parse unified diff hunk headers: @@ -start,count +start,count @@
        if let Some(rest) = line.strip_prefix("@@ ") {
            // Extract the +start,count part (new file ranges)
            if let Some(plus_part) = rest.split(' ').find(|s| s.starts_with('+')) {
                let nums = plus_part.trim_start_matches('+');
                let parts: Vec<&str> = nums.split(',').collect();
                if let Ok(start) = parts[0].parse::<usize>() {
                    let count = if parts.len() > 1 {
                        parts[1]
                            .split_whitespace()
                            .next()
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(1)
                    } else {
                        1
                    };
                    if count > 0 {
                        ranges.push((start, start + count - 1));
                    }
                }
            }
        }
    }

    Ok(ranges)
}

/// Check if two sets of line ranges overlap.
fn ranges_overlap(a: &[(usize, usize)], b: &[(usize, usize)]) -> bool {
    for &(a_start, a_end) in a {
        for &(b_start, b_end) in b {
            if a_start <= b_end && b_start <= a_end {
                return true;
            }
        }
    }
    false
}

/// Detect file conflicts between multiple merge sources.
fn detect_file_conflicts(sources: &[MergeSource]) -> Vec<FileConflict> {
    // Build map: file → list of agent slugs that modified it
    let mut file_agents: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();

    for source in sources {
        for file in &source.changed_files {
            file_agents
                .entry(file.clone())
                .or_default()
                .push(source.agent_slug.clone());
        }
    }

    let mut conflicts = Vec::new();

    for (file, agents) in &file_agents {
        if agents.len() < 2 {
            continue;
        }

        // Build a lookup for worktree paths by agent slug
        let slug_to_source: std::collections::HashMap<&str, &MergeSource> =
            sources.iter().map(|s| (s.agent_slug.as_str(), s)).collect();

        // Check if we can determine overlap by inspecting diff ranges
        let mut all_ranges: Vec<(&str, Vec<(usize, usize)>)> = Vec::new();
        let mut range_extraction_ok = true;

        for agent_slug in agents {
            if let Some(source) = slug_to_source.get(agent_slug.as_str()) {
                match extract_diff_ranges(&source.worktree_path, file) {
                    Ok(ranges) if !ranges.is_empty() => {
                        all_ranges.push((agent_slug.as_str(), ranges));
                    }
                    Ok(_) => {
                        // Empty ranges could mean the file was created or binary
                        range_extraction_ok = false;
                        break;
                    }
                    Err(_) => {
                        range_extraction_ok = false;
                        break;
                    }
                }
            }
        }

        let conflict_type = if !range_extraction_ok {
            // If we can't extract ranges, check if file is new in any worktree
            ConflictType::CreateModify
        } else {
            // Check pairwise for overlapping ranges
            let mut has_overlap = false;
            'outer: for i in 0..all_ranges.len() {
                for j in (i + 1)..all_ranges.len() {
                    if ranges_overlap(&all_ranges[i].1, &all_ranges[j].1) {
                        has_overlap = true;
                        break 'outer;
                    }
                }
            }
            if has_overlap {
                ConflictType::Overlapping
            } else {
                ConflictType::NonOverlapping
            }
        };

        conflicts.push(FileConflict {
            file: file.clone(),
            agents: agents.clone(),
            conflict_type,
        });
    }

    conflicts
}

/// Compute merge order: non-conflicting agents first, then non-overlapping, then overlapping.
fn compute_merge_order(sources: &[MergeSource], conflicts: &[FileConflict]) -> Vec<String> {
    // Classify each agent's worst conflict level
    let mut agent_worst: std::collections::BTreeMap<&str, u8> = std::collections::BTreeMap::new();

    // Start all agents at level 0 (no conflicts)
    for source in sources {
        agent_worst.insert(&source.agent_slug, 0);
    }

    for conflict in conflicts {
        let level = match conflict.conflict_type {
            ConflictType::NonOverlapping => 1,
            ConflictType::CreateModify => 2,
            ConflictType::Overlapping => 3,
        };
        for agent in &conflict.agents {
            if let Some(current) = agent_worst.get_mut(agent.as_str()) {
                if level > *current {
                    *current = level;
                }
            }
        }
    }

    // Sort: lowest conflict level first, then alphabetically for stability
    let mut order: Vec<(&str, u8)> = agent_worst.into_iter().collect();
    order.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0)));

    order.iter().map(|(slug, _)| slug.to_string()).collect()
}

/// Orchestrate merging agent worktree changes into a single branch.
pub fn merge(
    crosslink_dir: &Path,
    branch: &str,
    dry_run: bool,
    agents_filter: Option<&str>,
) -> Result<()> {
    let repo_root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    // Discover agent worktrees with changes
    let mut sources = discover_worktrees(repo_root)?;

    if sources.is_empty() {
        println!("No agent worktrees with changes found.");
        return Ok(());
    }

    // Filter by agent slugs if --agents provided
    if let Some(filter) = agents_filter {
        let slugs: std::collections::HashSet<&str> = filter.split(',').map(|s| s.trim()).collect();
        sources.retain(|s| slugs.contains(s.agent_slug.as_str()));
        if sources.is_empty() {
            bail!("No matching agent worktrees found for filter: {}", filter);
        }
    }

    // Detect file conflicts
    let conflicts = detect_file_conflicts(&sources);

    // Compute merge order
    let merge_order = compute_merge_order(&sources, &conflicts);

    // Build the merge plan
    let plan = MergePlan {
        target_branch: branch.to_string(),
        agents: sources.clone(),
        conflicts: conflicts.clone(),
        merge_order: merge_order.clone(),
    };

    // Print summary
    println!("Merge Plan");
    println!("==========");
    println!("Target branch: {}", branch);
    println!(
        "Agents:        {} ({} total commits)",
        sources.len(),
        sources.iter().map(|s| s.commit_count).sum::<usize>()
    );
    println!();

    // Agent details table
    println!("Agent Worktrees:");
    for source in &sources {
        println!(
            "  {} — {} file{}, {} commit{}",
            source.agent_slug,
            source.changed_files.len(),
            if source.changed_files.len() == 1 {
                ""
            } else {
                "s"
            },
            source.commit_count,
            if source.commit_count == 1 { "" } else { "s" },
        );
    }
    println!();

    // Conflict analysis
    if conflicts.is_empty() {
        println!("Conflicts:     none detected");
    } else {
        println!(
            "Conflicts:     {} file{}",
            conflicts.len(),
            if conflicts.len() == 1 { "" } else { "s" }
        );
        for conflict in &conflicts {
            let type_label = match conflict.conflict_type {
                ConflictType::NonOverlapping => "non-overlapping",
                ConflictType::Overlapping => "OVERLAPPING",
                ConflictType::CreateModify => "create/modify",
            };
            println!(
                "  {} [{}] — agents: {}",
                conflict.file,
                type_label,
                conflict.agents.join(", ")
            );
        }

        let overlapping_count = conflicts
            .iter()
            .filter(|c| c.conflict_type == ConflictType::Overlapping)
            .count();
        if overlapping_count > 0 {
            println!();
            println!(
                "WARNING: {} file{} with overlapping changes will need manual resolution.",
                overlapping_count,
                if overlapping_count == 1 { "" } else { "s" }
            );
        }
    }
    println!();

    // Merge order
    println!("Merge order:");
    for (i, slug) in merge_order.iter().enumerate() {
        println!("  {}. {}", i + 1, slug);
    }
    println!();

    // Persist the plan to hub branch
    let sync = SyncManager::new(crosslink_dir)?;
    if sync.is_initialized() {
        sync.fetch()?;
        write_hub_json(&sync, "swarm/merge-plan.json", &plan)?;
        commit_hub_files(
            &sync,
            &["swarm/merge-plan.json"],
            &format!(
                "swarm: merge plan for {} agents → {}",
                sources.len(),
                branch
            ),
        )?;
        println!("Plan saved to hub branch (swarm/merge-plan.json).");
    }

    if dry_run {
        println!("Dry run — no changes applied.");
        return Ok(());
    }

    // Create the target branch from develop
    let create_branch = std::process::Command::new("git")
        .current_dir(repo_root)
        .args(["checkout", "-b", branch, "develop"])
        .output()
        .context("Failed to create target branch")?;

    if !create_branch.status.success() {
        let stderr = String::from_utf8_lossy(&create_branch.stderr);
        // If branch already exists, try to check it out
        if stderr.contains("already exists") {
            let checkout = std::process::Command::new("git")
                .current_dir(repo_root)
                .args(["checkout", branch])
                .output()
                .context("Failed to checkout existing target branch")?;
            if !checkout.status.success() {
                bail!(
                    "Failed to checkout branch '{}': {}",
                    branch,
                    String::from_utf8_lossy(&checkout.stderr)
                );
            }
            println!("Checked out existing branch '{}'.", branch);
        } else {
            bail!("Failed to create branch '{}': {}", branch, stderr);
        }
    } else {
        println!("Created branch '{}' from develop.", branch);
    }

    // Apply each agent's diff in merge order
    let slug_to_source: std::collections::HashMap<&str, &MergeSource> =
        sources.iter().map(|s| (s.agent_slug.as_str(), s)).collect();

    let mut applied = 0usize;
    let mut failed = Vec::new();

    for slug in &merge_order {
        let source = match slug_to_source.get(slug.as_str()) {
            Some(s) => s,
            None => continue,
        };

        println!("Applying changes from '{}'...", slug);

        // Generate the diff from the agent's worktree
        let diff_output = std::process::Command::new("git")
            .current_dir(&source.worktree_path)
            .args(["diff", "develop...HEAD"])
            .output()
            .context("Failed to generate diff")?;

        if !diff_output.status.success() {
            eprintln!(
                "  Failed to generate diff for '{}': {}",
                slug,
                String::from_utf8_lossy(&diff_output.stderr)
            );
            failed.push(slug.clone());
            continue;
        }

        let diff_content = diff_output.stdout;
        if diff_content.is_empty() {
            println!("  No diff to apply for '{}'.", slug);
            continue;
        }

        // Apply the diff using git apply
        let mut apply_cmd = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["apply", "--3way", "--stat", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to start git apply")?;

        if let Some(mut stdin) = apply_cmd.stdin.take() {
            use std::io::Write;
            stdin.write_all(&diff_content)?;
        }

        let apply_result = apply_cmd.wait_with_output()?;

        if !apply_result.status.success() {
            let stderr = String::from_utf8_lossy(&apply_result.stderr);
            eprintln!("  Failed to apply diff for '{}': {}", slug, stderr);
            eprintln!("  This agent's changes need manual resolution.");
            failed.push(slug.clone());

            // Abort any partial apply
            let _ = std::process::Command::new("git")
                .current_dir(repo_root)
                .args(["checkout", "."])
                .output();
            continue;
        }

        // Stage and commit the applied changes
        let _ = std::process::Command::new("git")
            .current_dir(repo_root)
            .args(["add", "-A"])
            .output()?;

        let commit_msg = format!("merge: apply changes from agent '{}'", slug);
        let commit_output = std::process::Command::new("git")
            .current_dir(repo_root)
            .args([
                "commit",
                "-m",
                &commit_msg,
                "--no-gpg-sign",
                "--allow-empty",
            ])
            .output()?;

        if commit_output.status.success() {
            println!("  Applied and committed changes from '{}'.", slug);
            applied += 1;
        } else {
            let stderr = String::from_utf8_lossy(&commit_output.stderr);
            if stderr.contains("nothing to commit") {
                println!("  No new changes from '{}' (already applied).", slug);
            } else {
                eprintln!("  Commit failed for '{}': {}", slug, stderr);
                failed.push(slug.clone());
            }
        }
    }

    println!();
    println!(
        "Merge complete: {} applied, {} failed.",
        applied,
        failed.len()
    );
    if !failed.is_empty() {
        println!("Failed agents: {}", failed.join(", "));
        println!("These agents' changes need manual resolution.");
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Pipeline wrappers
// ---------------------------------------------------------------------------

/// Continue a paused pipeline past a human checkpoint.
pub fn review_continue(crosslink_dir: &Path) -> Result<()> {
    let mut pipeline = crate::pipeline::load_pipeline(crosslink_dir)?
        .context("No active pipeline found. Start one with `crosslink swarm review`")?;
    pipeline.confirm_checkpoint()?;
    crate::pipeline::save_pipeline(crosslink_dir, &pipeline)?;
    println!(
        "Pipeline resumed from checkpoint. Current stage: {}",
        pipeline.current_stage
    );
    Ok(())
}

/// Show the current pipeline status.
pub fn review_status(crosslink_dir: &Path) -> Result<()> {
    match crate::pipeline::load_pipeline(crosslink_dir)? {
        Some(pipeline) => println!("{}", pipeline.summary()),
        None => println!("No active pipeline."),
    }
    Ok(())
}

/// Run the standalone pipeline driver (crosslink swarm pipeline).
///
/// This uses [`pipeline::run_pipeline`] which logs each stage transition
/// and pauses at human checkpoints.
pub fn run_pipeline_cmd(
    crosslink_dir: &Path,
    agents: usize,
    mandate: &str,
    target_branch: &str,
    auto_fix: bool,
    auto_file_issues: bool,
) -> Result<()> {
    let config = PipelineConfig {
        agent_count: agents,
        mandate: mandate.to_string(),
        auto_fix,
        auto_file_issues,
        target_branch: target_branch.to_string(),
    };
    pipeline::run_pipeline(crosslink_dir, config)
}

/// Initialize trust model configuration (crosslink swarm trust-init).
pub fn trust_init(crosslink_dir: &Path, model: &str) -> Result<()> {
    trust_model::write_default_config(crosslink_dir, model)?;
    let config = trust_model::generate_default_config(model);
    println!("Trust model configuration written to swarm.toml");
    println!("  Model:       {}", config.trust.model);
    println!("  Description: {}", config.trust.description);
    if !config.ignore.patterns.is_empty() {
        println!("  Ignore patterns: {}", config.ignore.patterns.join(", "));
    }
    if !config.boundaries.external.is_empty() {
        println!(
            "  External boundaries: {}",
            config.boundaries.external.join(", ")
        );
    }
    if !config.boundaries.internal.is_empty() {
        println!(
            "  Internal boundaries: {}",
            config.boundaries.internal.join(", ")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{Finding, FindingSeverity, ReviewReport};

    /// Helper to build seam::Partition from a label and file list (for tests).
    fn make_partition(label: &str, files: Vec<&str>) -> seam::Partition {
        seam::Partition {
            label: label.to_string(),
            files: files
                .into_iter()
                .map(|s| std::path::PathBuf::from(s))
                .collect(),
            line_count: 0,
        }
    }

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
        // No git repo, no worktree, no branch → planned
        assert_eq!(probe_agent_status(dir.path(), "nonexistent"), "planned");
    }

    #[test]
    fn test_probe_agent_status_worktree_removed_branch_merged() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();

        // Set up a git repo with a branch that's been merged
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        for args in [
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
        ] {
            std::process::Command::new("git")
                .current_dir(repo)
                .args(&args)
                .output()
                .unwrap();
        }
        std::fs::write(repo.join("README.md"), "# test\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-m", "init", "--no-gpg-sign"])
            .output()
            .unwrap();

        // Create and merge a branch
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["checkout", "-b", "test-agent"])
            .output()
            .unwrap();
        std::fs::write(repo.join("agent-work.txt"), "work\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-m", "agent work", "--no-gpg-sign"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["checkout", "main"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["merge", "test-agent", "--no-gpg-sign"])
            .output()
            .unwrap();

        // No worktree exists, but branch is merged → should be "completed (merged)"
        assert_eq!(probe_agent_status(repo, "test-agent"), "completed (merged)");
    }

    #[test]
    fn test_probe_agent_status_worktree_removed_branch_exists() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();

        std::process::Command::new("git")
            .current_dir(repo)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        for args in [
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
        ] {
            std::process::Command::new("git")
                .current_dir(repo)
                .args(&args)
                .output()
                .unwrap();
        }
        std::fs::write(repo.join("README.md"), "# test\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-m", "init", "--no-gpg-sign"])
            .output()
            .unwrap();

        // Create a branch with a commit that isn't merged
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["checkout", "-b", "unmerged-agent"])
            .output()
            .unwrap();
        std::fs::write(repo.join("unmerged-work.txt"), "unmerged\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-m", "unmerged work", "--no-gpg-sign"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["checkout", "main"])
            .output()
            .unwrap();

        // No worktree, branch exists but not merged → "completed (worktree removed)"
        assert_eq!(
            probe_agent_status(repo, "unmerged-agent"),
            "completed (worktree removed)"
        );
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

    // -----------------------------------------------------------------------
    // swarm review tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_mandate_prompt_adversarial() {
        let prompt = mandate_prompt("adversarial");
        assert!(prompt.contains("ha-satan"));
        assert!(prompt.contains("correctness, safety, and robustness"));
    }

    #[test]
    fn test_mandate_prompt_security() {
        let prompt = mandate_prompt("security");
        assert!(prompt.contains("trust boundary"));
        assert!(prompt.contains("injection vectors"));
    }

    #[test]
    fn test_mandate_prompt_robustness() {
        let prompt = mandate_prompt("robustness");
        assert!(prompt.contains("crash paths"));
        assert!(prompt.contains("resource leaks"));
    }

    #[test]
    fn test_mandate_prompt_correctness() {
        let prompt = mandate_prompt("correctness");
        assert!(prompt.contains("logic errors"));
        assert!(prompt.contains("race conditions"));
    }

    #[test]
    fn test_mandate_prompt_custom_passthrough() {
        let custom = "Check for off-by-one errors everywhere";
        assert_eq!(mandate_prompt(custom), custom);
    }

    #[test]
    fn test_finding_serde_roundtrip() {
        let finding = Finding {
            title: "Unchecked unwrap in parser".to_string(),
            severity: FindingSeverity::High,
            file: "src/parser.rs".to_string(),
            line: Some(42),
            description: "This unwrap will panic on malformed input".to_string(),
            suggested_fix: Some("Use ? operator instead".to_string()),
            agent: "reviewer-1".to_string(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let parsed: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, finding.title);
        assert_eq!(parsed.severity, finding.severity);
        assert_eq!(parsed.file, finding.file);
        assert_eq!(parsed.line, finding.line);
        assert_eq!(parsed.description, finding.description);
        assert_eq!(parsed.suggested_fix, finding.suggested_fix);
        assert_eq!(parsed.agent, finding.agent);
    }

    #[test]
    fn test_finding_minimal_serde_roundtrip() {
        let finding = Finding {
            title: "Minor issue".to_string(),
            severity: FindingSeverity::Info,
            file: "src/lib.rs".to_string(),
            line: None,
            description: "Consider adding docs".to_string(),
            suggested_fix: None,
            agent: "reviewer-2".to_string(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let parsed: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.line, None);
        assert_eq!(parsed.suggested_fix, None);
    }

    #[test]
    fn test_review_report_serde_roundtrip() {
        let report = ReviewReport {
            agent: "reviewer-1".to_string(),
            partition_label: "src, lib".to_string(),
            mandate: "adversarial".to_string(),
            findings: vec![Finding {
                title: "Buffer overflow".to_string(),
                severity: FindingSeverity::Critical,
                file: "src/buffer.rs".to_string(),
                line: Some(100),
                description: "Writes past allocated size".to_string(),
                suggested_fix: Some("Add bounds check".to_string()),
                agent: "reviewer-1".to_string(),
            }],
            completed_at: Some("2026-03-12T10:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&report).unwrap();
        let parsed: ReviewReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent, report.agent);
        assert_eq!(parsed.partition_label, report.partition_label);
        assert_eq!(parsed.mandate, report.mandate);
        assert_eq!(parsed.findings.len(), 1);
        assert_eq!(parsed.findings[0].title, "Buffer overflow");
        assert_eq!(parsed.completed_at, report.completed_at);
    }

    #[test]
    fn test_finding_severity_ordering() {
        // Derived PartialOrd/Ord uses variant declaration order
        assert!(FindingSeverity::Critical < FindingSeverity::High);
        assert!(FindingSeverity::High < FindingSeverity::Medium);
        assert!(FindingSeverity::Medium < FindingSeverity::Low);
        assert!(FindingSeverity::Low < FindingSeverity::Info);

        // Sort a mixed list and verify order
        let mut severities = vec![
            FindingSeverity::Low,
            FindingSeverity::Critical,
            FindingSeverity::Info,
            FindingSeverity::High,
            FindingSeverity::Medium,
        ];
        severities.sort();
        assert_eq!(
            severities,
            vec![
                FindingSeverity::Critical,
                FindingSeverity::High,
                FindingSeverity::Medium,
                FindingSeverity::Low,
                FindingSeverity::Info,
            ]
        );
    }

    #[test]
    fn test_slugify_fix_target_basic() {
        assert_eq!(
            slugify_fix_target(326, "Buffer overflow in parser"),
            "fix-326-buffer-overflow-in-parser"
        );
    }

    #[test]
    fn test_assign_partitions_round_robin() {
        let partitions = vec![
            make_partition("alpha", vec!["a/1.rs"]),
            make_partition("beta", vec!["b/1.rs"]),
            make_partition("gamma", vec!["c/1.rs"]),
            make_partition("delta", vec!["d/1.rs"]),
            make_partition("epsilon", vec!["e/1.rs"]),
        ];
        let assignments = assign_partitions(partitions, 3);

        assert_eq!(assignments.len(), 3);
        // Agent 0 gets partitions 0, 3 (alpha, delta)
        assert!(assignments[0].partition_label.contains("alpha"));
        assert!(assignments[0].partition_label.contains("delta"));
        assert_eq!(assignments[0].files.len(), 2);
        // Agent 1 gets partition 1, 4 (beta, epsilon)
        assert!(assignments[1].partition_label.contains("beta"));
        assert!(assignments[1].partition_label.contains("epsilon"));
        assert_eq!(assignments[1].files.len(), 2);
        // Agent 2 gets partition 2 (gamma)
        assert!(assignments[2].partition_label.contains("gamma"));
        assert_eq!(assignments[2].files.len(), 1);
    }

    #[test]
    fn test_assign_partitions_more_agents_than_partitions() {
        let partitions = vec![
            make_partition("src", vec!["src/main.rs"]),
            make_partition("lib", vec!["lib/mod.rs"]),
        ];
        let assignments = assign_partitions(partitions, 5);

        // Only 2 agents should have files; the rest are filtered out
        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].agent_slug, "reviewer-1");
        assert_eq!(assignments[1].agent_slug, "reviewer-2");
    }

    #[test]
    fn test_assign_partitions_single_agent() {
        let partitions = vec![
            make_partition("a", vec!["a/1.rs"]),
            make_partition("b", vec!["b/1.rs"]),
            make_partition("c", vec!["c/1.rs"]),
        ];
        let assignments = assign_partitions(partitions, 1);

        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].files.len(), 3);
        assert!(assignments[0].partition_label.contains("a"));
        assert!(assignments[0].partition_label.contains("b"));
        assert!(assignments[0].partition_label.contains("c"));
    }

    #[test]
    fn test_assign_partitions_empty() {
        let partitions: Vec<seam::Partition> = vec![];
        let assignments = assign_partitions(partitions, 4);
        assert!(assignments.is_empty());
    }

    #[test]
    fn test_assign_partitions_zero_agents_defaults_to_one() {
        let partitions = vec![make_partition("src", vec!["src/main.rs"])];
        let assignments = assign_partitions(partitions, 0);
        assert_eq!(assignments.len(), 1);
    }

    #[test]
    fn test_review_plan_serde_roundtrip() {
        let plan = ReviewPlan {
            mandate: "adversarial".to_string(),
            mandate_prompt: MANDATE_ADVERSARIAL.to_string(),
            agent_count: 2,
            created_at: "2026-03-12T10:00:00Z".to_string(),
            agents: vec![
                ReviewAgentAssignment {
                    agent_slug: "reviewer-1".to_string(),
                    partition_label: "src".to_string(),
                    files: vec!["src/main.rs".to_string()],
                },
                ReviewAgentAssignment {
                    agent_slug: "reviewer-2".to_string(),
                    partition_label: "lib".to_string(),
                    files: vec!["lib/mod.rs".to_string()],
                },
            ],
            doc_output: Some(std::path::PathBuf::from("review-findings.md")),
        };
        let json = serde_json::to_string_pretty(&plan).unwrap();
        let parsed: ReviewPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.mandate, plan.mandate);
        assert_eq!(parsed.agent_count, 2);
        assert_eq!(parsed.agents.len(), 2);
        assert_eq!(parsed.agents[0].agent_slug, "reviewer-1");
        assert_eq!(parsed.doc_output, plan.doc_output);
    }

    #[test]
    fn test_finding_severity_serde_values() {
        // Verify the rename_all = "snake_case" produces expected strings
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Critical).unwrap(),
            "\"critical\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::High).unwrap(),
            "\"high\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Low).unwrap(),
            "\"low\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Info).unwrap(),
            "\"info\""
        );
    }

    #[test]
    fn test_slugify_fix_target_special_chars() {
        assert_eq!(
            slugify_fix_target(42, "Fix: memory leak (critical!)"),
            "fix-42-fix-memory-leak-critical"
        );
    }

    #[test]
    fn test_slugify_fix_target_long_title_truncates() {
        let long_title =
            "This is a very long title that should be truncated to keep the slug reasonable";
        let slug = slugify_fix_target(1, long_title);
        assert!(slug.len() <= 50, "slug too long: {} ({})", slug, slug.len());
        assert!(slug.starts_with("fix-1-"));
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn test_slugify_fix_target_empty_title() {
        assert_eq!(slugify_fix_target(99, ""), "fix-99-");
    }

    #[test]
    fn test_fix_plan_serde_roundtrip() {
        let plan = FixPlan {
            schema_version: 1,
            created_at: "2026-03-12T10:00:00Z".to_string(),
            issues: vec![
                FixTarget {
                    issue_number: 326,
                    title: "Buffer overflow".to_string(),
                    body: "Details here".to_string(),
                    labels: vec!["bug".to_string(), "review-finding".to_string()],
                    agent_slug: "fix-326-buffer-overflow".to_string(),
                    status: AgentStatus::Planned,
                },
                FixTarget {
                    issue_number: 327,
                    title: "Memory leak".to_string(),
                    body: "".to_string(),
                    labels: vec![],
                    agent_slug: "fix-327-memory-leak".to_string(),
                    status: AgentStatus::Running,
                },
            ],
        };
        let json = serde_json::to_string_pretty(&plan).unwrap();
        let parsed: FixPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, parsed);
    }

    // -----------------------------------------------------------------------
    // Merge orchestration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_merge_plan_serde_roundtrip() {
        let plan = MergePlan {
            target_branch: "swarm-combined".to_string(),
            agents: vec![MergeSource {
                agent_slug: "agent-a".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["src/main.rs".to_string()],
                commit_count: 3,
            }],
            conflicts: vec![],
            merge_order: vec!["agent-a".to_string()],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: MergePlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, parsed);
    }

    #[test]
    fn test_parse_issue_numbers_valid() {
        let nums = parse_issue_numbers("326,327,328").unwrap();
        assert_eq!(nums, vec![326, 327, 328]);
    }

    #[test]
    fn test_parse_issue_numbers_with_spaces() {
        let nums = parse_issue_numbers("1, 2, 3").unwrap();
        assert_eq!(nums, vec![1, 2, 3]);
    }

    #[test]
    fn test_parse_issue_numbers_single() {
        let nums = parse_issue_numbers("42").unwrap();
        assert_eq!(nums, vec![42]);
    }

    #[test]
    fn test_parse_issue_numbers_invalid() {
        let result = parse_issue_numbers("326,abc,328");
        assert!(result.is_err());
    }

    #[test]
    fn test_fix_requires_issues_or_label() {
        let result = parse_issue_numbers("");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // swarm merge tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_conflict_type_serde_roundtrip() {
        let cases = vec![
            (ConflictType::NonOverlapping, "\"non_overlapping\""),
            (ConflictType::Overlapping, "\"overlapping\""),
            (ConflictType::CreateModify, "\"create_modify\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json);
            let parsed: ConflictType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_detect_file_conflicts_no_overlaps() {
        let sources = vec![
            MergeSource {
                agent_slug: "agent-a".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["src/foo.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-b".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-b"),
                changed_files: vec!["src/bar.rs".to_string()],
                commit_count: 2,
            },
        ];
        let conflicts = detect_file_conflicts(&sources);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn test_detect_file_conflicts_shared_files() {
        let sources = vec![
            MergeSource {
                agent_slug: "agent-a".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-b".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-b"),
                changed_files: vec!["src/main.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-c".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-c"),
                changed_files: vec!["src/lib.rs".to_string(), "src/utils.rs".to_string()],
                commit_count: 1,
            },
        ];
        let conflicts = detect_file_conflicts(&sources);

        // src/main.rs: agent-a + agent-b
        // src/lib.rs: agent-a + agent-c
        assert_eq!(conflicts.len(), 2);

        let main_conflict = conflicts.iter().find(|c| c.file == "src/main.rs").unwrap();
        assert_eq!(main_conflict.agents, vec!["agent-a", "agent-b"]);

        let lib_conflict = conflicts.iter().find(|c| c.file == "src/lib.rs").unwrap();
        assert_eq!(lib_conflict.agents, vec!["agent-a", "agent-c"]);
    }

    #[test]
    fn test_compute_merge_order_non_conflicting_first() {
        let sources = vec![
            MergeSource {
                agent_slug: "agent-a".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["src/main.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-b".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-b"),
                changed_files: vec!["src/main.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-c".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-c"),
                changed_files: vec!["src/other.rs".to_string()],
                commit_count: 1,
            },
        ];

        let conflicts = vec![FileConflict {
            file: "src/main.rs".to_string(),
            agents: vec!["agent-a".to_string(), "agent-b".to_string()],
            conflict_type: ConflictType::Overlapping,
        }];

        let order = compute_merge_order(&sources, &conflicts);

        // agent-c has no conflicts, should be first
        assert_eq!(order[0], "agent-c");
        // agent-a and agent-b both have overlapping conflicts, sorted alphabetically
        assert_eq!(order[1], "agent-a");
        assert_eq!(order[2], "agent-b");
    }

    #[test]
    fn test_compute_merge_order_is_deterministic() {
        let sources = vec![
            MergeSource {
                agent_slug: "zebra".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-z"),
                changed_files: vec!["a.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "alpha".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["b.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "middle".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-m"),
                changed_files: vec!["c.rs".to_string()],
                commit_count: 1,
            },
        ];

        let conflicts = vec![];

        // Run multiple times to verify determinism
        let order1 = compute_merge_order(&sources, &conflicts);
        let order2 = compute_merge_order(&sources, &conflicts);
        assert_eq!(order1, order2);
        // All at same conflict level → alphabetical
        assert_eq!(order1, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn test_compute_merge_order_respects_conflict_levels() {
        let sources = vec![
            MergeSource {
                agent_slug: "agent-overlap".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-1"),
                changed_files: vec!["shared.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-nonoverlap".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-2"),
                changed_files: vec!["shared2.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-clean".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-3"),
                changed_files: vec!["unique.rs".to_string()],
                commit_count: 1,
            },
        ];

        let conflicts = vec![
            FileConflict {
                file: "shared.rs".to_string(),
                agents: vec!["agent-overlap".to_string(), "agent-nonoverlap".to_string()],
                conflict_type: ConflictType::Overlapping,
            },
            FileConflict {
                file: "shared2.rs".to_string(),
                agents: vec!["agent-nonoverlap".to_string(), "agent-clean".to_string()],
                conflict_type: ConflictType::NonOverlapping,
            },
        ];

        let order = compute_merge_order(&sources, &conflicts);
        // agent-clean is involved in NonOverlapping only → level 1
        // agent-nonoverlap has Overlapping → level 3
        // agent-overlap has Overlapping → level 3
        // Wait: agent-clean is in shared2.rs NonOverlapping conflict
        // So: agent-clean → level 1, agent-nonoverlap → level 3, agent-overlap → level 3
        assert_eq!(order[0], "agent-clean");
        assert_eq!(order[1], "agent-nonoverlap");
        assert_eq!(order[2], "agent-overlap");
    }

    #[test]
    fn test_ranges_overlap() {
        // Overlapping ranges
        assert!(ranges_overlap(&[(1, 10)], &[(5, 15)]));
        assert!(ranges_overlap(&[(5, 15)], &[(1, 10)]));
        assert!(ranges_overlap(&[(1, 10)], &[(10, 20)]));

        // Non-overlapping ranges
        assert!(!ranges_overlap(&[(1, 5)], &[(6, 10)]));
        assert!(!ranges_overlap(&[(10, 20)], &[(1, 5)]));

        // Multiple ranges, some overlap
        assert!(ranges_overlap(&[(1, 5), (20, 30)], &[(4, 6)]));
        assert!(!ranges_overlap(&[(1, 5), (20, 30)], &[(6, 19)]));
    }

    #[test]
    fn test_merge_source_serde_roundtrip() {
        let source = MergeSource {
            agent_slug: "my-agent".to_string(),
            worktree_path: PathBuf::from("/home/user/.worktrees/my-agent"),
            changed_files: vec!["src/main.rs".to_string(), "Cargo.toml".to_string()],
            commit_count: 5,
        };
        let json = serde_json::to_string(&source).unwrap();
        let parsed: MergeSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, parsed);
    }

    #[test]
    fn test_file_conflict_serde_roundtrip() {
        let conflict = FileConflict {
            file: "src/lib.rs".to_string(),
            agents: vec!["agent-a".to_string(), "agent-b".to_string()],
            conflict_type: ConflictType::NonOverlapping,
        };
        let json = serde_json::to_string(&conflict).unwrap();
        let parsed: FileConflict = serde_json::from_str(&json).unwrap();
        assert_eq!(conflict, parsed);
    }
}
