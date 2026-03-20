// E-ana tablet — kickoff pipeline: pipeline state tracking per design document
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Pipeline state sidecar file (`.design/<slug>.pipeline.json`).
///
/// Tracks the lifecycle of a design document through the plan → run pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineState {
    pub schema_version: u32,
    pub design_doc: String,
    pub doc_hash: String,
    pub stage: String,
    #[serde(default)]
    pub plans: Vec<PlanRecord>,
    #[serde(default)]
    pub runs: Vec<RunRecord>,
}

/// Record of a single plan (gap analysis) run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanRecord {
    pub agent_id: String,
    pub worktree: String,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub status: String,
    #[serde(default)]
    pub blocking_gaps: u32,
    #[serde(default)]
    pub advisory_gaps: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_file: Option<String>,
}

/// Record of a single implementation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub agent_id: String,
    pub worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<i64>,
    pub started_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub status: String,
}

/// Compute SHA-256 hash of file content, returning `sha256:<hex>` string.
pub fn compute_doc_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let result = hasher.finalize();
    format!("sha256:{:x}", result)
}

/// Check if a plan is stale by comparing the stored hash with the current file content.
pub fn is_plan_stale(pipeline: &PipelineState, design_doc_path: &Path) -> bool {
    let content = match std::fs::read_to_string(design_doc_path) {
        Ok(c) => c,
        Err(_) => return false, // Can't read doc — don't flag as stale
    };
    let current_hash = compute_doc_hash(&content);
    current_hash != pipeline.doc_hash
}

/// Derive the pipeline state file path from a design doc path.
///
/// `.design/foo.md` → `.design/foo.pipeline.json`
pub fn pipeline_path_for_doc(doc_path: &Path) -> std::path::PathBuf {
    let stem = doc_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    doc_path.with_file_name(format!("{}.pipeline.json", stem))
}

/// Derive the plan JSON path from a design doc path.
///
/// `.design/foo.md` → `.design/foo.plan.json`
pub fn plan_path_for_doc(doc_path: &Path) -> std::path::PathBuf {
    let stem = doc_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    doc_path.with_file_name(format!("{}.plan.json", stem))
}

/// Read the pipeline state for a design document, if it exists.
pub fn read_pipeline_state(doc_path: &Path) -> Option<PipelineState> {
    let path = pipeline_path_for_doc(doc_path);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write (create or update) the pipeline state for a design document.
pub fn write_pipeline_state(doc_path: &Path, state: &PipelineState) -> Result<()> {
    let path = pipeline_path_for_doc(doc_path);
    let json = serde_json::to_string_pretty(state).context("Failed to serialize pipeline state")?;
    std::fs::write(&path, json)
        .with_context(|| format!("Failed to write pipeline state to {}", path.display()))
}

/// Create a new pipeline state file with `stage: "designed"`.
pub fn create_initial_pipeline(doc_path: &Path) -> Result<PipelineState> {
    let content = std::fs::read_to_string(doc_path)
        .with_context(|| format!("Failed to read design doc: {}", doc_path.display()))?;
    let doc_hash = compute_doc_hash(&content);

    let state = PipelineState {
        schema_version: 1,
        design_doc: doc_path.to_string_lossy().to_string(),
        doc_hash,
        stage: "designed".to_string(),
        plans: Vec::new(),
        runs: Vec::new(),
    };

    write_pipeline_state(doc_path, &state)?;
    Ok(state)
}

/// Ensure a pipeline state file exists for a design document.
/// Returns the current (possibly newly created) state.
pub fn ensure_pipeline_state(doc_path: &Path) -> Result<PipelineState> {
    if let Some(state) = read_pipeline_state(doc_path) {
        Ok(state)
    } else {
        create_initial_pipeline(doc_path)
    }
}

/// Update pipeline state to "planning" stage with a new plan record.
pub fn mark_planning(doc_path: &Path, agent_id: &str, worktree: &str) -> Result<PipelineState> {
    let mut state = ensure_pipeline_state(doc_path)?;

    // Recompute doc hash at plan launch time
    if let Ok(content) = std::fs::read_to_string(doc_path) {
        state.doc_hash = compute_doc_hash(&content);
    }

    state.stage = "planning".to_string();
    state.plans.push(PlanRecord {
        agent_id: agent_id.to_string(),
        worktree: worktree.to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        completed_at: None,
        status: "running".to_string(),
        blocking_gaps: 0,
        advisory_gaps: 0,
        plan_file: None,
    });

    write_pipeline_state(doc_path, &state)?;
    Ok(state)
}

/// Update pipeline state to "planned" stage after plan completion.
///
/// Called by the watchdog or plan agent upon completion. Not yet wired into
/// the watchdog — will be connected when watchdog gains pipeline awareness.
#[allow(dead_code)]
pub fn mark_planned(
    doc_path: &Path,
    agent_id: &str,
    blocking_gaps: u32,
    advisory_gaps: u32,
    plan_file: &str,
) -> Result<PipelineState> {
    let mut state = ensure_pipeline_state(doc_path)?;
    state.stage = "planned".to_string();

    // Update the matching plan record
    if let Some(plan) = state
        .plans
        .iter_mut()
        .rev()
        .find(|p| p.agent_id == agent_id)
    {
        plan.completed_at = Some(chrono::Utc::now().to_rfc3339());
        plan.status = "done".to_string();
        plan.blocking_gaps = blocking_gaps;
        plan.advisory_gaps = advisory_gaps;
        plan.plan_file = Some(plan_file.to_string());
    }

    write_pipeline_state(doc_path, &state)?;
    Ok(state)
}

/// Update pipeline state to "running" stage with a new run record.
pub fn mark_running(
    doc_path: &Path,
    agent_id: &str,
    worktree: &str,
    issue_id: Option<i64>,
) -> Result<PipelineState> {
    let mut state = ensure_pipeline_state(doc_path)?;
    state.stage = "running".to_string();
    state.runs.push(RunRecord {
        agent_id: agent_id.to_string(),
        worktree: worktree.to_string(),
        issue_id,
        started_at: chrono::Utc::now().to_rfc3339(),
        completed_at: None,
        status: "running".to_string(),
    });

    write_pipeline_state(doc_path, &state)?;
    Ok(state)
}

/// Get a human-readable stage display string with optional staleness indicator.
pub fn stage_display(pipeline: &PipelineState, doc_path: &Path) -> String {
    let stale = if pipeline.stage == "planned" && is_plan_stale(pipeline, doc_path) {
        " \u{26a0} stale"
    } else {
        ""
    };

    match pipeline.stage.as_str() {
        "designed" => "designed".to_string(),
        "planning" => {
            if let Some(plan) = pipeline.plans.last() {
                format!("planning \u{27f3}  {}", plan.agent_id)
            } else {
                "planning \u{27f3}".to_string()
            }
        }
        "planned" => {
            if let Some(plan) = pipeline.plans.last() {
                format!(
                    "planned \u{2713}{}   {}/{}",
                    stale, plan.blocking_gaps, plan.advisory_gaps
                )
            } else {
                format!("planned{}", stale)
            }
        }
        "running" => {
            if let Some(run) = pipeline.runs.last() {
                format!("running  {} \u{27f3}", run.agent_id)
            } else {
                "running \u{27f3}".to_string()
            }
        }
        "complete" => "complete \u{2713}".to_string(),
        other => other.to_string(),
    }
}

/// Format a plan's age as a human-readable string.
#[allow(dead_code)]
fn plan_age_display(completed_at: &Option<String>) -> String {
    let Some(ts) = completed_at else {
        return String::new();
    };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return String::new();
    };
    let elapsed = chrono::Utc::now().signed_duration_since(dt.with_timezone(&chrono::Utc));
    let mins = elapsed.num_minutes();
    if mins < 60 {
        format!("({}m ago)", mins)
    } else {
        let hours = mins / 60;
        format!("({}h ago)", hours)
    }
}

/// Scan `.design/` for all pipeline state files and return structured info.
pub fn scan_pipeline_states(repo_root: &Path) -> Vec<(std::path::PathBuf, PipelineState)> {
    let design_dir = repo_root.join(".design");
    if !design_dir.is_dir() {
        return Vec::new();
    }

    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&design_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(state) = read_pipeline_state(&path) {
                    results.push((path, state));
                }
            }
        }
    }
    results
}

/// Scan `.design/` for all design documents (with or without pipeline state).
pub fn scan_design_docs(repo_root: &Path) -> Vec<std::path::PathBuf> {
    let design_dir = repo_root.join(".design");
    if !design_dir.is_dir() {
        return Vec::new();
    }

    let mut docs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&design_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("md") {
                docs.push(path);
            }
        }
    }
    docs.sort();
    docs
}
