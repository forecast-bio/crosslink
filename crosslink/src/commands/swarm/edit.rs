// Swarm plan editing commands: move, merge, split, remove, reorder, rename.

use anyhow::{bail, Result};
use std::path::Path;

use super::io::*;
use super::types::*;

/// Move an agent from one phase to another.
pub fn move_agent(crosslink_dir: &Path, agent_slug: &str, to_phase: &str) -> Result<()> {
    let (sync, plan, mut phases) = load_plan_and_phases(crosslink_dir)?;

    // Find and remove agent from source phase
    let mut found_agent: Option<AgentEntry> = None;
    let mut source_phase_name = String::new();
    for (_path, phase) in &mut phases {
        if let Some(pos) = phase.agents.iter().position(|a| a.slug == agent_slug) {
            found_agent = Some(phase.agents.remove(pos));
            source_phase_name = phase.name.clone();
            break;
        }
    }

    let agent = found_agent
        .ok_or_else(|| anyhow::anyhow!("Agent '{}' not found in any phase", agent_slug))?;

    // Find target phase and add agent
    let target = phases
        .iter_mut()
        .find(|(_, p)| p.name == to_phase)
        .ok_or_else(|| anyhow::anyhow!("Phase '{}' not found", to_phase))?;
    target.1.agents.push(agent);

    save_plan_and_phases(
        &sync,
        &plan,
        &phases,
        &format!(
            "swarm: move {} from {} to {}",
            agent_slug, source_phase_name, to_phase
        ),
    )?;
    println!(
        "Moved '{}' from '{}' to '{}'",
        agent_slug, source_phase_name, to_phase
    );
    Ok(())
}

/// Merge two phases into one (keeps the first phase's name).
pub fn merge_phases(crosslink_dir: &Path, phase_a: &str, phase_b: &str) -> Result<()> {
    let (sync, mut plan, mut phases) = load_plan_and_phases(crosslink_dir)?;

    let idx_a = phases
        .iter()
        .position(|(_, p)| p.name == phase_a)
        .ok_or_else(|| anyhow::anyhow!("Phase '{}' not found", phase_a))?;
    let idx_b = phases
        .iter()
        .position(|(_, p)| p.name == phase_b)
        .ok_or_else(|| anyhow::anyhow!("Phase '{}' not found", phase_b))?;

    // Move agents from B into A
    let agents_b: Vec<AgentEntry> = phases[idx_b].1.agents.clone();
    phases[idx_a].1.agents.extend(agents_b);

    // Remove phase B from plan and phases list
    let removed_path = phases[idx_b].0.clone();
    phases.remove(idx_b);
    plan.phases.retain(|p| p != phase_b);

    // Delete the old phase file
    let cache_file = sync.cache_path().join(&removed_path);
    let _ = std::fs::remove_file(&cache_file);

    save_plan_and_phases(
        &sync,
        &plan,
        &phases,
        &format!("swarm: merge '{}' into '{}'", phase_b, phase_a),
    )?;
    println!("Merged '{}' into '{}'", phase_b, phase_a);
    Ok(())
}

/// Split a phase after a specific agent, creating a new phase.
pub fn split_phase(crosslink_dir: &Path, phase_name: &str, after_agent: &str) -> Result<()> {
    let (sync, mut plan, mut phases) = load_plan_and_phases(crosslink_dir)?;

    let idx = phases
        .iter()
        .position(|(_, p)| p.name == phase_name)
        .ok_or_else(|| anyhow::anyhow!("Phase '{}' not found", phase_name))?;

    let split_pos = phases[idx]
        .1
        .agents
        .iter()
        .position(|a| a.slug == after_agent)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Agent '{}' not found in phase '{}'",
                after_agent,
                phase_name
            )
        })?;

    if split_pos + 1 >= phases[idx].1.agents.len() {
        bail!(
            "Agent '{}' is the last agent in '{}' — nothing to split off",
            after_agent,
            phase_name
        );
    }

    // Split agents
    let ctx = resolve_swarm(&sync)?;
    let new_agents: Vec<AgentEntry> = phases[idx].1.agents.drain(split_pos + 1..).collect();
    let new_name = format!("{} (split)", phase_name);
    let new_path = ctx.phase_path(&new_name);

    let new_phase = PhaseDefinition {
        name: new_name.clone(),
        status: PhaseStatus::Pending,
        agents: new_agents,
        gate: None,
        depends_on: vec![phase_name.to_string()],
        checkpoint: None,
    };

    // Insert new phase right after the split phase
    let insert_at = idx + 1;
    phases.insert(insert_at, (new_path, new_phase));

    // Update plan phase list
    let plan_idx = plan
        .phases
        .iter()
        .position(|p| p == phase_name)
        .unwrap_or(plan.phases.len());
    plan.phases.insert(plan_idx + 1, new_name.clone());

    save_plan_and_phases(
        &sync,
        &plan,
        &phases,
        &format!("swarm: split '{}' after '{}'", phase_name, after_agent),
    )?;
    println!(
        "Split '{}' after '{}' — new phase: '{}'",
        phase_name, after_agent, new_name
    );
    Ok(())
}

/// Remove an agent from the swarm plan.
pub fn remove_agent(crosslink_dir: &Path, agent_slug: &str) -> Result<()> {
    let (sync, plan, mut phases) = load_plan_and_phases(crosslink_dir)?;

    let mut removed = false;
    let mut from_phase = String::new();
    for (_path, phase) in &mut phases {
        if let Some(pos) = phase.agents.iter().position(|a| a.slug == agent_slug) {
            phase.agents.remove(pos);
            from_phase = phase.name.clone();
            removed = true;
            break;
        }
    }

    if !removed {
        bail!("Agent '{}' not found in any phase", agent_slug);
    }

    save_plan_and_phases(
        &sync,
        &plan,
        &phases,
        &format!("swarm: remove agent '{}'", agent_slug),
    )?;
    println!("Removed '{}' from '{}'", agent_slug, from_phase);
    Ok(())
}

/// Reorder a phase to a new position (1-based).
pub fn reorder_phase(crosslink_dir: &Path, phase_name: &str, position: usize) -> Result<()> {
    let (sync, mut plan, mut phases) = load_plan_and_phases(crosslink_dir)?;

    if position == 0 || position > phases.len() {
        bail!("Position {} is out of range (1-{})", position, phases.len());
    }

    let current_idx = phases
        .iter()
        .position(|(_, p)| p.name == phase_name)
        .ok_or_else(|| anyhow::anyhow!("Phase '{}' not found", phase_name))?;

    let target_idx = position - 1;
    if current_idx == target_idx {
        println!("Phase '{}' is already at position {}", phase_name, position);
        return Ok(());
    }

    let entry = phases.remove(current_idx);
    phases.insert(target_idx, entry);

    // Update plan phase order to match
    plan.phases = phases.iter().map(|(_, p)| p.name.clone()).collect();

    save_plan_and_phases(
        &sync,
        &plan,
        &phases,
        &format!("swarm: reorder '{}' to position {}", phase_name, position),
    )?;
    println!("Moved '{}' to position {}", phase_name, position);
    Ok(())
}

/// Rename a phase.
pub fn rename_phase(crosslink_dir: &Path, old_name: &str, new_name: &str) -> Result<()> {
    let (sync, mut plan, mut phases) = load_plan_and_phases(crosslink_dir)?;

    let idx = phases
        .iter()
        .position(|(_, p)| p.name == old_name)
        .ok_or_else(|| anyhow::anyhow!("Phase '{}' not found", old_name))?;

    // Update the phase name
    phases[idx].1.name = new_name.to_string();

    // Update depends_on references in other phases
    for (_path, phase) in &mut phases {
        for dep in &mut phase.depends_on {
            if dep == old_name {
                *dep = new_name.to_string();
            }
        }
    }

    // Write new phase file, remove old one
    let ctx = resolve_swarm(&sync)?;
    let old_path = phases[idx].0.clone();
    let new_path = ctx.phase_path(new_name);
    phases[idx].0 = new_path;

    let old_cache_file = sync.cache_path().join(&old_path);
    let _ = std::fs::remove_file(&old_cache_file);

    // Update plan phase list
    for p in &mut plan.phases {
        if p == old_name {
            *p = new_name.to_string();
        }
    }

    save_plan_and_phases(
        &sync,
        &plan,
        &phases,
        &format!("swarm: rename '{}' to '{}'", old_name, new_name),
    )?;
    println!("Renamed '{}' to '{}'", old_name, new_name);
    Ok(())
}
