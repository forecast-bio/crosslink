// Hub branch I/O helpers for swarm coordination.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::Path;

use super::types::*;
use crate::sync::SyncManager;

/// Read a JSON file from the hub cache directory.
pub(super) fn read_hub_json<T: serde::de::DeserializeOwned>(
    sync: &SyncManager,
    path: &str,
) -> Result<T> {
    let full = sync.cache_path().join(path);
    let content =
        std::fs::read_to_string(&full).with_context(|| format!("Failed to read {path}"))?;
    serde_json::from_str(&content).with_context(|| format!("Failed to parse {path}"))
}

/// Write a JSON file to the hub cache directory (does NOT commit).
pub(super) fn write_hub_json<T: Serialize>(
    sync: &SyncManager,
    path: &str,
    value: &T,
) -> Result<()> {
    let full = sync.cache_path().join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(&full, content).with_context(|| format!("Failed to write {path}"))
}

/// Stage multiple files and commit.
pub(super) fn commit_hub_files(sync: &SyncManager, paths: &[&str], message: &str) -> Result<()> {
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
            bail!("git commit failed: {stderr}");
        }
    }
    Ok(())
}

/// A loaded swarm plan with its sync manager, plan metadata, and phase definitions with paths.
pub(super) type LoadedPlan = (SyncManager, SwarmPlan, Vec<(String, PhaseDefinition)>);

/// Helper: load the swarm plan, all phase definitions, and the sync manager.
pub(super) fn load_plan_and_phases(crosslink_dir: &Path) -> Result<LoadedPlan> {
    let sync = SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Hub cache not initialized. Run `crosslink sync` first.");
    }
    sync.fetch()?;

    let ctx = resolve_swarm(&sync)?;
    let plan: SwarmPlan = read_hub_json(&sync, &ctx.plan_path())
        .context("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")?;

    let mut phases = Vec::new();
    for phase_name in &plan.phases {
        let path = ctx.phase_path(phase_name);
        let phase: PhaseDefinition = read_hub_json(&sync, &path)
            .with_context(|| format!("Failed to load phase: {phase_name}"))?;
        phases.push((path, phase));
    }

    Ok((sync, plan, phases))
}

/// Helper: save modified phases and plan back to hub.
pub(super) fn save_plan_and_phases(
    sync: &SyncManager,
    plan: &SwarmPlan,
    phases: &[(String, PhaseDefinition)],
    message: &str,
) -> Result<()> {
    let ctx = resolve_swarm(sync)?;
    let plan_path = ctx.plan_path();
    write_hub_json(sync, &plan_path, plan)?;
    let mut paths = vec![plan_path];
    for (path, phase) in phases {
        write_hub_json(sync, path, phase)?;
        paths.push(path.clone());
    }
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    commit_hub_files(sync, &path_refs, message)?;
    Ok(())
}

/// Load a phase definition by slug, returning the phase and its hub path.
pub(super) fn load_phase(
    sync: &SyncManager,
    phase_slug: &str,
) -> Result<(PhaseDefinition, String)> {
    let ctx = resolve_swarm(sync)?;
    let plan: SwarmPlan = read_hub_json(sync, &ctx.plan_path())
        .context("No swarm plan found. Run `crosslink swarm init --doc <file>` first.")?;

    // Try exact slug match first
    let phase_file = ctx.phase_path(phase_slug);
    if let Ok(phase) = read_hub_json::<PhaseDefinition>(sync, &phase_file) {
        return Ok((phase, phase_file));
    }

    // Try slugifying each plan phase name to find a match
    for name in &plan.phases {
        let slug = slugify_phase(name);
        if slug == phase_slug {
            let path = ctx.phase_path(name);
            let phase: PhaseDefinition = read_hub_json(sync, &path)
                .with_context(|| format!("Phase file missing for '{name}'"))?;
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
pub(super) fn check_dependencies(sync: &SyncManager, phase: &PhaseDefinition) -> Result<()> {
    let ctx = resolve_swarm(sync)?;
    for dep_name in &phase.depends_on {
        let dep_file = ctx.phase_path(dep_name);
        let dep: PhaseDefinition = read_hub_json(sync, &dep_file)
            .with_context(|| format!("Dependency phase '{dep_name}' not found"))?;
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
