// Swarm coordination: multi-agent phase planning, status, and resume.
//
// Persists swarm state to the hub branch under `swarm/` so it survives
// session boundaries and is visible to all agents.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::commands::design_doc::{self, DesignDoc};
use crate::commands::kickoff::tmux_session_name;
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

    // Check if tmux session is alive
    let session_name = tmux_session_name(slug);
    let tmux_alive = std::process::Command::new("tmux")
        .args(["has-session", "-t", &session_name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if tmux_alive {
        return "running (tmux)".to_string();
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

    if active_phase.is_none() {
        println!(
            "All {} phases completed. Swarm build is done.",
            plan.phases.len()
        );
        return Ok(());
    }

    let phase = active_phase.unwrap();
    let phase_name = active_phase_name.unwrap();

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

    if all_agents_resolved {
        actions.push(format!(
            "{}. All agents merged. Run gate: cargo test (or project test command)",
            action_num
        ));
        action_num += 1;
        actions.push(format!(
            "{}. If gate passes: crosslink swarm checkpoint {}",
            action_num,
            slugify_phase(&phase_name)
        ));
    } else if ready_to_merge.is_empty() && running.is_empty() && planned.is_empty() {
        // Only failed/unknown agents remain
        actions.push(format!(
            "{}. After resolving failures: run gate and checkpoint",
            action_num
        ));
    } else {
        actions.push(format!(
            "{}. After merges complete: run gate (cargo test)",
            action_num
        ));
        action_num += 1;
        if completed_count + 1 < plan.phases.len() {
            actions.push(format!(
                "{}. If gate passes: checkpoint and start next phase",
                action_num
            ));
        }
    }

    println!("Next actions:");
    for action in &actions {
        println!("  {}", action);
    }

    Ok(())
}

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
}
