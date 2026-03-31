// Swarm init: initialize a swarm plan from a design document.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::io::*;
use super::types::*;
use crate::commands::design_doc::{self, DesignDoc};
use crate::sync::SyncManager;

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

    // Check if an active swarm already exists (legacy or multi-swarm)
    let has_active = sync.cache_path().join("swarm/active.json").exists()
        || sync.cache_path().join("swarm/plan.json").exists();
    if has_active {
        bail!(
            "A swarm plan already exists. Use `crosslink swarm status` to view it, \
             or `crosslink swarm reset` to archive and start over."
        );
    }

    // Create a new UUID-based swarm slot
    let ctx = create_swarm_slot(&sync, &doc.title)?;

    // Build phases from design doc structure
    let mut phases = propose_phases(&doc);
    let now = chrono::Utc::now().to_rfc3339();

    // Greenfield detection: if no primary source files exist, prepend a scaffold phase
    // so shared files (Cargo.toml, src/lib.rs, etc.) are created before parallel agents (#393)
    let repo_root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;
    let is_greenfield = !repo_root.join("Cargo.toml").exists()
        && !repo_root.join("package.json").exists()
        && !repo_root.join("go.mod").exists()
        && !repo_root.join("pyproject.toml").exists()
        && !repo_root.join("mix.exs").exists()
        && !repo_root.join("src").is_dir();

    if is_greenfield && phases.len() > 1 {
        let scaffold_phase = PhaseDefinition {
            name: "Phase 0: Scaffold".to_string(),
            status: PhaseStatus::Pending,
            agents: vec![AgentEntry {
                slug: "project-scaffold".to_string(),
                description: format!(
                    "Create project skeleton for '{}': manifest files, directory structure, \
                     shared types/traits, and CI configuration. Subsequent phases depend on this.",
                    doc.title
                ),
                issue_id: None,
                agent_id: None,
                branch: Some("feature/project-scaffold".to_string()),
                status: AgentStatus::Planned,
                started_at: None,
                completed_at: None,
            }],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };

        // Make all existing phases depend on the scaffold
        for phase in &mut phases {
            if phase.depends_on.is_empty() {
                phase.depends_on.push("Phase 0: Scaffold".to_string());
            }
        }

        phases.insert(0, scaffold_phase);
        println!(
            "Note: Greenfield project detected — added Phase 0: Scaffold to create project skeleton first."
        );
    }

    let phase_names: Vec<String> = phases.iter().map(|p| p.name.clone()).collect();

    let plan = SwarmPlan {
        schema_version: 1,
        title: doc.title.clone(),
        design_doc: Some(doc_path.display().to_string()),
        created_at: now,
        phases: phase_names,
    };

    // Write plan and phase files
    let plan_path = ctx.plan_path();
    write_hub_json(&sync, &plan_path, &plan)?;
    let mut paths_to_commit: Vec<String> = vec!["swarm/active.json".to_string(), plan_path];

    for phase in &phases {
        let phase_path = ctx.phase_path(&phase.name);
        write_hub_json(&sync, &phase_path, phase)?;
        paths_to_commit.push(phase_path);
    }

    let path_refs: Vec<&str> = paths_to_commit.iter().map(String::as_str).collect();
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
pub(super) fn propose_phases(doc: &DesignDoc) -> Vec<PhaseDefinition> {
    // If the design doc has explicit layer/phase groups, use them as phase boundaries
    if !doc.requirement_groups.is_empty() {
        return propose_phases_from_groups(doc);
    }

    let mut agents: Vec<AgentEntry> = Vec::new();

    // Build agent entries from requirements
    for req in &doc.requirements {
        let slug = slugify_requirement(req);
        agents.push(AgentEntry {
            slug: slug.clone(),
            description: req.clone(),
            issue_id: None,
            agent_id: None,
            branch: Some(format!("feature/{slug}")),
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
                branch: Some(format!("feature/{slug}")),
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
            branch: Some(format!("feature/{slug}")),
            status: AgentStatus::Planned,
            started_at: None,
            completed_at: None,
        });
    }

    // Split into phases of at most 8 agents
    let max_per_phase = 8;
    let mut phases = Vec::new();
    let chunks: Vec<Vec<AgentEntry>> = agents
        .chunks(max_per_phase)
        .map(<[AgentEntry]>::to_vec)
        .collect();

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

/// Build phases from explicit layer/phase groups in the design doc.
fn propose_phases_from_groups(doc: &DesignDoc) -> Vec<PhaseDefinition> {
    let mut phases = Vec::new();

    for (i, group) in doc.requirement_groups.iter().enumerate() {
        let agents: Vec<AgentEntry> = group
            .items
            .iter()
            .map(|req| {
                let slug = slugify_requirement(req);
                AgentEntry {
                    slug: slug.clone(),
                    description: req.clone(),
                    issue_id: None,
                    agent_id: None,
                    branch: Some(format!("feature/{slug}")),
                    status: AgentStatus::Planned,
                    started_at: None,
                    completed_at: None,
                }
            })
            .collect();

        // Sequential groups depend on previous phase; parallel groups depend on nothing
        // (unless they're not the first phase, in which case they depend on the prior sequential)
        let depends_on = if i > 0 && group.execution_hint != "parallel" {
            vec![phases
                .last()
                .map(|p: &PhaseDefinition| p.name.clone())
                .unwrap_or_default()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect()
        } else if i > 0 {
            // Parallel phases still depend on the last phase before them
            phases
                .last()
                .map_or_else(Vec::new, |prev| vec![prev.name.clone()])
        } else {
            vec![]
        };

        let name = format!("Phase {}: {}", i + 1, group.name);

        phases.push(PhaseDefinition {
            name,
            status: PhaseStatus::Pending,
            agents,
            gate: None,
            depends_on,
            checkpoint: None,
        });
    }

    phases
}

/// Slugify a requirement string into a short branch-safe slug.
pub(super) fn slugify_requirement(req: &str) -> String {
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
