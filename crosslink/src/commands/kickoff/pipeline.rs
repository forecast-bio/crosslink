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
    format!("sha256:{result:x}")
}

/// Check if a plan is stale by comparing the stored hash with the current file content.
pub fn is_plan_stale(pipeline: &PipelineState, design_doc_path: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(design_doc_path) else {
        return false; // Can't read doc — don't flag as stale
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
    doc_path.with_file_name(format!("{stem}.pipeline.json"))
}

/// Derive the plan JSON path from a design doc path.
///
/// `.design/foo.md` → `.design/foo.plan.json`
pub fn plan_path_for_doc(doc_path: &Path) -> std::path::PathBuf {
    let stem = doc_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    doc_path.with_file_name(format!("{stem}.plan.json"))
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
    read_pipeline_state(doc_path).map_or_else(|| create_initial_pipeline(doc_path), Ok)
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
///
/// Called from the launch flow only after the worktree and agent identity
/// exist (see [`crate::commands::kickoff::run`]). The row therefore carries the
/// real `agent_id` and `worktree` path from creation — there are no more
/// `"pending"` literals in new rows, which is what GH#614 was about. The launch
/// is past its point of no return when this runs (the worktree is on disk and
/// the agent is initialized), so a row written here cannot be stranded as a
/// permanently-pending ghost: if the agent never starts, reconciliation will
/// later see the worktree gone (after cleanup) or the sentinel and resolve it.
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

/// Outcome of probing a single run row's worktree during reconciliation.
///
/// The reconcile core is generic over how the probe is obtained so it can be
/// unit-tested without touching the filesystem (see the test module). The
/// production probe is [`probe_run_worktree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunProbe {
    /// Worktree exists and the `.kickoff-status` sentinel says the agent finished.
    SentinelDone,
    /// Worktree exists and the sentinel says the agent failed (`FAILED` / `CI_FAILED` / error).
    SentinelFailed,
    /// Worktree exists, no terminal sentinel, and a live agent is still working it.
    LiveRunning,
    /// Worktree is gone (or unresolvable, e.g. a legacy `"pending"` path) and no
    /// live agent matches — the row is stale and should be marked aborted.
    Gone,
    /// Worktree exists but is in an indeterminate non-terminal state (e.g. status
    /// is RUNNING/LAUNCHING and no live-agent signal is available). Leave untouched.
    Indeterminate,
}

/// Probe a single run row against the real filesystem and a set of live agent ids.
///
/// `sentinel_mtime` is returned alongside the verdict so the caller can stamp
/// `completed_at` from the sentinel's modification time when available.
pub fn probe_run_worktree(
    run: &RunRecord,
    live_agent_ids: &[String],
) -> (RunProbe, Option<String>) {
    let is_live = live_agent_ids.iter().any(|id| id == &run.agent_id) && run.agent_id != "pending";

    // A "pending" worktree literal (legacy rows) or an empty path can never be
    // resolved on disk — treat as gone unless a live agent vouches for it.
    if run.worktree.is_empty() || run.worktree == "pending" {
        return if is_live {
            (RunProbe::LiveRunning, None)
        } else {
            (RunProbe::Gone, None)
        };
    }

    let wt = Path::new(&run.worktree);
    if !wt.exists() {
        return if is_live {
            (RunProbe::LiveRunning, None)
        } else {
            (RunProbe::Gone, None)
        };
    }

    let status_file = wt.join(".kickoff-status");
    let sentinel_mtime = std::fs::metadata(&status_file)
        .and_then(|m| m.modified())
        .ok()
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());

    if let Ok(raw) = std::fs::read_to_string(&status_file) {
        let lower = raw.to_lowercase();
        if lower.contains("done") {
            return (RunProbe::SentinelDone, sentinel_mtime);
        }
        if lower.contains("fail") || lower.contains("error") {
            return (RunProbe::SentinelFailed, sentinel_mtime);
        }
    }

    // Worktree present, no terminal sentinel. Trust the live-agent signal.
    if is_live {
        (RunProbe::LiveRunning, None)
    } else {
        (RunProbe::Indeterminate, None)
    }
}

/// Reconcile the `runs` array against the world, mutating stale rows in place.
///
/// For every row whose `status == "running"`, the `probe` closure reports the
/// real-world verdict (see [`RunProbe`]):
/// - `SentinelDone` → `status = "completed"`, `completed_at` from the sentinel
///   mtime (or now if unreadable);
/// - `SentinelFailed` → `status = "failed"`, `completed_at` likewise;
/// - `Gone` → `status = "aborted"`, `completed_at = now` (legacy `"pending"`
///   `agent_id` is left as-is — we never invent an identity, but the status no
///   longer lies);
/// - `LiveRunning` / `Indeterminate` → left untouched.
///
/// All rows are reconciled, not just the last. When the change leaves no row
/// actually running, [`stage`] is transitioned off `"running"` to a sensible
/// terminal/prior stage (see [`stage_after_runs_settle`]).
///
/// Returns `true` if anything changed (so callers persist only when needed).
/// `now` is injected for deterministic testing.
pub fn reconcile_runs<F>(state: &mut PipelineState, now: &str, mut probe: F) -> bool
where
    F: FnMut(&RunRecord) -> (RunProbe, Option<String>),
{
    let mut changed = false;
    for run in &mut state.runs {
        if run.status != "running" {
            continue;
        }
        let (verdict, sentinel_mtime) = probe(run);
        match verdict {
            RunProbe::SentinelDone => {
                run.status = "completed".to_string();
                run.completed_at = Some(sentinel_mtime.unwrap_or_else(|| now.to_string()));
                changed = true;
            }
            RunProbe::SentinelFailed => {
                run.status = "failed".to_string();
                run.completed_at = Some(sentinel_mtime.unwrap_or_else(|| now.to_string()));
                changed = true;
            }
            RunProbe::Gone => {
                run.status = "aborted".to_string();
                run.completed_at = Some(now.to_string());
                changed = true;
            }
            RunProbe::LiveRunning | RunProbe::Indeterminate => {}
        }
    }

    if changed && state.stage == "running" {
        let any_running = state.runs.iter().any(|r| r.status == "running");
        if !any_running {
            state.stage = stage_after_runs_settle(state);
        }
    }

    changed
}

/// Decide the stage to land on once no run row is `"running"` any more.
///
/// Mirrors the module's existing stage vocabulary (`designed` → `planned` →
/// `running` → `complete`):
/// - if the most recent run completed (or no run failed/aborted), the work
///   reached a terminal success → `"complete"`;
/// - otherwise the launch did not land (last row aborted/failed): fall back to
///   `"planned"` when a plan record exists, else `"designed"` so the pipeline UI
///   invites the operator to re-plan / re-run rather than claiming completion.
fn stage_after_runs_settle(state: &PipelineState) -> String {
    match state.runs.last().map(|r| r.status.as_str()) {
        Some("completed") => "complete".to_string(),
        _ if state.plans.iter().any(|p| p.status == "done") => "planned".to_string(),
        _ if !state.plans.is_empty() => "planned".to_string(),
        _ => "designed".to_string(),
    }
}

/// Reconcile a pipeline state's runs for a display read, using the real
/// filesystem and the supplied set of live agent ids, then persist if changed.
///
/// This is the seam invoked by the display read paths (`kickoff status`
/// overview and the wizard's doc-entry build): plain non-kickoff reads stay
/// cheap because they never call this, while the kickoff surfaces that already
/// pay for worktree/sentinel I/O reconcile their stale rows at view time.
pub fn reconcile_runs_for_display(
    doc_path: &Path,
    state: &mut PipelineState,
    live_agent_ids: &[String],
) -> bool {
    let now = chrono::Utc::now().to_rfc3339();
    let changed = reconcile_runs(state, &now, |run| probe_run_worktree(run, live_agent_ids));
    if changed {
        // Best-effort persist: a failed write must not break the display path.
        let _ = write_pipeline_state(doc_path, state);
    }
    changed
}

/// Mark the run row matching `worktree` (or, for legacy rows lacking a real
/// worktree, the row closest in `started_at` to `started_near`) as terminally
/// finished with the given `status` and a `completed_at` of now.
///
/// This is the positive-completion hook: callers that have just observed a
/// worktree finish (sentinel DONE/FAILED at status time, or a worktree being
/// pruned by cleanup) reconcile the matching row at the moment of truth rather
/// than waiting for the next lazy display reconcile. Returns `true` if a row
/// was updated and the state persisted.
pub fn mark_run_finished(
    doc_path: &Path,
    state: &mut PipelineState,
    worktree: &str,
    started_near: Option<&str>,
    status: &str,
) -> bool {
    let now = chrono::Utc::now().to_rfc3339();
    let idx = match_run_index(state, worktree, started_near);
    let Some(idx) = idx else {
        return false;
    };
    let run = &mut state.runs[idx];
    if run.status != "running" {
        return false;
    }
    run.status = status.to_string();
    run.completed_at = Some(now);

    if state.stage == "running" && !state.runs.iter().any(|r| r.status == "running") {
        state.stage = stage_after_runs_settle(state);
    }

    let _ = write_pipeline_state(doc_path, state);
    true
}

/// Find the index of the run row to reconcile for a completion event.
///
/// Prefers an exact worktree-path match; for legacy rows whose `worktree` is
/// `"pending"` (so no path match is possible), falls back to the still-running
/// row whose `started_at` is closest to `started_near` when provided.
fn match_run_index(
    state: &PipelineState,
    worktree: &str,
    started_near: Option<&str>,
) -> Option<usize> {
    if !worktree.is_empty() && worktree != "pending" {
        if let Some(i) = state
            .runs
            .iter()
            .position(|r| r.status == "running" && r.worktree == worktree)
        {
            return Some(i);
        }
    }

    let near = started_near.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())?;
    state
        .runs
        .iter()
        .enumerate()
        .filter(|(_, r)| r.status == "running")
        .filter_map(|(i, r)| {
            let started = chrono::DateTime::parse_from_rfc3339(&r.started_at).ok()?;
            let delta = (started - near).num_seconds().abs();
            Some((i, delta))
        })
        .min_by_key(|(_, delta)| *delta)
        .map(|(i, _)| i)
}

/// Positive-completion hook: a kickoff completion was just observed for a
/// worktree (a terminal `.kickoff-status` sentinel seen by `kickoff status`, or
/// a finished worktree being pruned by `kickoff cleanup`). Scan every pipeline
/// state under `.design/`, find the one whose runs include this worktree, and
/// mark that row finished with `status` at the moment of truth.
///
/// Returns `true` if a matching row was found and persisted. Best-effort and
/// cheap when there are no design docs (the common non-kickoff case never
/// reaches here because callers only invoke it after a sentinel/cleanup event).
pub fn reconcile_completion_by_worktree(repo_root: &Path, worktree: &str, status: &str) -> bool {
    if worktree.is_empty() {
        return false;
    }
    for (doc_path, mut state) in scan_pipeline_states(repo_root) {
        if state
            .runs
            .iter()
            .any(|r| r.status == "running" && r.worktree == worktree)
            && mark_run_finished(&doc_path, &mut state, worktree, None, status)
        {
            return true;
        }
    }
    false
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
        "planning" => pipeline.plans.last().map_or_else(
            || "planning \u{27f3}".to_string(),
            |plan| format!("planning \u{27f3}  {}", plan.agent_id),
        ),
        "planned" => pipeline.plans.last().map_or_else(
            || format!("planned{stale}"),
            |plan| {
                format!(
                    "planned \u{2713}{}   {}/{}",
                    stale, plan.blocking_gaps, plan.advisory_gaps
                )
            },
        ),
        "running" => pipeline.runs.last().map_or_else(
            || "running \u{27f3}".to_string(),
            |run| format!("running  {} \u{27f3}", run.agent_id),
        ),
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
        format!("({mins}m ago)")
    } else {
        let hours = mins / 60;
        format!("({hours}h ago)")
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
