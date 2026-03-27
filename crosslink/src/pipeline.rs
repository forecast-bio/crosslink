// End-to-end swarm review --fix pipeline orchestrator.
//
// Wires together all swarm review stages into a coherent flow:
// partition → review → consolidate → human-checkpoint → file-issues → fix → merge → PR.
//
// State is persisted to `.crosslink/pipeline.json` so the pipeline survives
// session boundaries and can be resumed after human checkpoints.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// Represents the full review→fix pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pipeline {
    pub id: String,
    /// When the pipeline was created (#490: DateTime instead of String).
    pub created_at: DateTime<Utc>,
    pub current_stage: PipelineStage,
    pub config: PipelineConfig,
    pub history: Vec<StageTransition>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    /// Partitioning the codebase
    Partition,
    /// Review agents running
    Review,
    /// Waiting for review agents to complete
    AwaitReview,
    /// Consolidating findings
    Consolidate,
    /// Human checkpoint — waiting for triage confirmation
    HumanCheckpoint,
    /// Filing issues from findings
    FileIssues,
    /// Fix agents running
    Fix,
    /// Waiting for fix agents to complete
    AwaitFix,
    /// Merging agent changes
    Merge,
    /// Opening pull request
    PullRequest,
    /// Pipeline complete
    Done,
    /// Pipeline failed
    Failed,
}

impl std::fmt::Display for PipelineStage {
    /// Display uses snake_case to match the serde serialization format (#489).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Partition => write!(f, "partition"),
            Self::Review => write!(f, "review"),
            Self::AwaitReview => write!(f, "await_review"),
            Self::Consolidate => write!(f, "consolidate"),
            Self::HumanCheckpoint => write!(f, "human_checkpoint"),
            Self::FileIssues => write!(f, "file_issues"),
            Self::Fix => write!(f, "fix"),
            Self::AwaitFix => write!(f, "await_fix"),
            Self::Merge => write!(f, "merge"),
            Self::PullRequest => write!(f, "pull_request"),
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineConfig {
    pub agent_count: usize,
    pub mandate: String,
    pub auto_fix: bool,
    pub auto_file_issues: bool,
    pub target_branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageTransition {
    pub from: PipelineStage,
    pub to: PipelineStage,
    /// When this transition occurred (#490: DateTime instead of String).
    pub timestamp: DateTime<Utc>,
    pub notes: Option<String>,
}

// ---------------------------------------------------------------------------
// Pipeline implementation
// ---------------------------------------------------------------------------

impl Pipeline {
    /// Create a new pipeline starting at the Partition stage.
    pub fn new(config: PipelineConfig) -> Self {
        let now = Utc::now();
        let id = format!(
            "pipeline-{}-{}",
            now.format("%Y%m%d-%H%M%S"),
            &Uuid::new_v4().to_string()[..8],
        );
        Self {
            id,
            created_at: now,
            current_stage: PipelineStage::Partition,
            config,
            history: Vec::new(),
        }
    }

    /// Return valid next stages for a given stage.
    pub fn valid_transitions(stage: PipelineStage) -> Vec<PipelineStage> {
        match stage {
            PipelineStage::Partition => vec![PipelineStage::Review, PipelineStage::Failed],
            PipelineStage::Review => vec![PipelineStage::AwaitReview, PipelineStage::Failed],
            PipelineStage::AwaitReview => {
                vec![PipelineStage::Consolidate, PipelineStage::Failed]
            }
            PipelineStage::Consolidate => {
                vec![PipelineStage::HumanCheckpoint, PipelineStage::Failed]
            }
            PipelineStage::HumanCheckpoint => {
                vec![PipelineStage::FileIssues, PipelineStage::Failed]
            }
            PipelineStage::FileIssues => vec![PipelineStage::Fix, PipelineStage::Failed],
            PipelineStage::Fix => vec![PipelineStage::AwaitFix, PipelineStage::Failed],
            PipelineStage::AwaitFix => vec![PipelineStage::Merge, PipelineStage::Failed],
            PipelineStage::Merge => vec![PipelineStage::PullRequest, PipelineStage::Failed],
            PipelineStage::PullRequest => vec![PipelineStage::Done, PipelineStage::Failed],
            PipelineStage::Done => vec![],
            PipelineStage::Failed => vec![],
        }
    }

    /// Move to the next stage in the normal (non-failure) sequence.
    ///
    /// Returns the new stage on success, or an error if the transition is
    /// invalid (e.g. pipeline is already Done/Failed, or at a checkpoint
    /// that requires explicit confirmation).
    pub fn advance(&mut self) -> Result<PipelineStage> {
        if self.current_stage == PipelineStage::HumanCheckpoint {
            bail!(
                "Pipeline is at a human checkpoint. \
                 Use `crosslink swarm review-continue` to proceed."
            );
        }

        let valid = Self::valid_transitions(self.current_stage);
        // The first entry (if any) is always the "happy path" successor;
        // Failed is last.
        let next = valid
            .into_iter()
            .find(|s| *s != PipelineStage::Failed)
            .context("Pipeline has already reached a terminal stage")?;

        self.record_transition(next, None);
        Ok(next)
    }

    /// Returns true if the given stage is a human checkpoint.
    pub fn is_checkpoint(stage: PipelineStage) -> bool {
        stage == PipelineStage::HumanCheckpoint
    }

    /// Advance past a human checkpoint.
    pub fn confirm_checkpoint(&mut self) -> Result<()> {
        if self.current_stage != PipelineStage::HumanCheckpoint {
            bail!(
                "Pipeline is not at a human checkpoint (current stage: {})",
                self.current_stage
            );
        }
        self.record_transition(
            PipelineStage::FileIssues,
            Some("Human checkpoint confirmed"),
        );
        Ok(())
    }

    /// Mark the pipeline as failed with explanatory notes.
    pub fn fail(&mut self, notes: &str) {
        self.record_transition(PipelineStage::Failed, Some(notes));
    }

    /// Human-readable pipeline status summary.
    pub fn summary(&self) -> String {
        let mut lines = Vec::new();
        lines.push(format!("Pipeline: {}", self.id));
        lines.push(format!("Created:  {}", self.created_at.to_rfc3339()));
        lines.push(format!("Stage:    {}", self.current_stage));
        lines.push(format!("Agents:   {}", self.config.agent_count));
        lines.push(format!("Mandate:  {}", self.config.mandate));
        lines.push(format!("Branch:   {}", self.config.target_branch));
        lines.push(format!("Auto-fix: {}", self.config.auto_fix));
        lines.push(format!(
            "Auto-file-issues: {}",
            self.config.auto_file_issues
        ));

        if !self.history.is_empty() {
            lines.push(String::new());
            lines.push("History:".to_string());
            for t in &self.history {
                let notes = t
                    .notes
                    .as_deref()
                    .map(|n| format!(" ({})", n))
                    .unwrap_or_default();
                lines.push(format!(
                    "  {} -> {} at {}{}",
                    t.from,
                    t.to,
                    t.timestamp.to_rfc3339(),
                    notes
                ));
            }
        }

        lines.join("\n")
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    fn record_transition(&mut self, to: PipelineStage, notes: Option<&str>) {
        let from = self.current_stage;
        self.history.push(StageTransition {
            from,
            to,
            timestamp: Utc::now(),
            notes: notes.map(|s| s.to_string()),
        });
        self.current_stage = to;
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

const PIPELINE_FILE: &str = "pipeline.json";

/// Persist pipeline state to `.crosslink/pipeline.json`.
pub fn save_pipeline(crosslink_dir: &Path, pipeline: &Pipeline) -> Result<()> {
    let path = crosslink_dir.join(PIPELINE_FILE);
    let json =
        serde_json::to_string_pretty(pipeline).context("Failed to serialize pipeline to JSON")?;
    std::fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

/// Load pipeline state from `.crosslink/pipeline.json`.
///
/// Returns `None` if the file does not exist.
pub fn load_pipeline(crosslink_dir: &Path) -> Result<Option<Pipeline>> {
    let path = crosslink_dir.join(PIPELINE_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let data = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;
    let pipeline: Pipeline = serde_json::from_str(&data)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(pipeline))
}

// ---------------------------------------------------------------------------
// Pipeline runner
// ---------------------------------------------------------------------------

/// Main entry point: create or resume a pipeline and drive it forward.
///
/// For now, each stage just prints what WOULD happen. Real implementations
/// will be wired in from other modules in subsequent PRs.
pub fn run_pipeline(crosslink_dir: &Path, config: PipelineConfig) -> Result<()> {
    let mut pipeline = match load_pipeline(crosslink_dir)? {
        Some(p) => {
            // Warn if the caller-supplied config differs from the persisted one (#487).
            if p.config.agent_count != config.agent_count
                || p.config.mandate != config.mandate
                || p.config.auto_fix != config.auto_fix
                || p.config.auto_file_issues != config.auto_file_issues
                || p.config.target_branch != config.target_branch
            {
                tracing::warn!(
                    "Resuming existing pipeline — config parameter ignored. \
                     Using persisted config (agents={}, mandate='{}').",
                    p.config.agent_count,
                    p.config.mandate,
                );
            }
            println!("Resuming pipeline {} at stage: {}", p.id, p.current_stage);
            p
        }
        None => {
            let p = Pipeline::new(config);
            println!("Created pipeline: {}", p.id);
            p
        }
    };

    loop {
        let stage = pipeline.current_stage;
        print_stage_action(stage, &pipeline.config);

        // Human checkpoint: save and exit so the user can inspect findings.
        if Pipeline::is_checkpoint(stage) {
            save_pipeline(crosslink_dir, &pipeline)?;
            println!();
            println!("Pipeline paused at human checkpoint. Review the findings above, then run:");
            println!("  crosslink swarm review-continue");
            return Ok(());
        }

        // Terminal stages: we're done.
        if stage == PipelineStage::Done || stage == PipelineStage::Failed {
            break;
        }

        // Advance to the next stage and persist.
        pipeline.advance()?;
        save_pipeline(crosslink_dir, &pipeline)?;
    }

    save_pipeline(crosslink_dir, &pipeline)?;
    println!();
    println!(
        "Pipeline {} finished at stage: {}",
        pipeline.id, pipeline.current_stage
    );
    Ok(())
}

/// Print a human-readable description of what a stage does (placeholder).
fn print_stage_action(stage: PipelineStage, config: &PipelineConfig) {
    println!();
    match stage {
        PipelineStage::Partition => {
            println!(
                "[partition] Would partition the codebase for {} review agents.",
                config.agent_count
            );
        }
        PipelineStage::Review => {
            println!(
                "[review] Would launch {} review agents with mandate: \"{}\".",
                config.agent_count, config.mandate
            );
        }
        PipelineStage::AwaitReview => {
            println!("[await-review] Would wait for all review agents to complete.");
        }
        PipelineStage::Consolidate => {
            println!("[consolidate] Would consolidate findings from all review agents.");
        }
        PipelineStage::HumanCheckpoint => {
            println!("[human-checkpoint] Findings are ready for human review and triage.");
        }
        PipelineStage::FileIssues => {
            if config.auto_file_issues {
                println!("[file-issues] Would automatically file issues from triaged findings.");
            } else {
                println!("[file-issues] Would file issues from triaged findings (manual mode).");
            }
        }
        PipelineStage::Fix => {
            if config.auto_fix {
                println!("[fix] Would launch fix agents for each filed issue.");
            } else {
                println!(
                    "[fix] Fix stage reached. Run `crosslink swarm fix` to launch fix agents."
                );
            }
        }
        PipelineStage::AwaitFix => {
            println!("[await-fix] Would wait for all fix agents to complete.");
        }
        PipelineStage::Merge => {
            println!(
                "[merge] Would merge fix agent branches into target branch: {}.",
                config.target_branch
            );
        }
        PipelineStage::PullRequest => {
            println!(
                "[pull-request] Would open a pull request against {}.",
                config.target_branch
            );
        }
        PipelineStage::Done => {
            println!("[done] Pipeline complete.");
        }
        PipelineStage::Failed => {
            println!("[failed] Pipeline has failed.");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PipelineConfig {
        PipelineConfig {
            agent_count: 4,
            mandate: "security review".to_string(),
            auto_fix: true,
            auto_file_issues: true,
            target_branch: "main".to_string(),
        }
    }

    #[test]
    fn test_new_starts_at_partition() {
        let p = Pipeline::new(test_config());
        assert_eq!(p.current_stage, PipelineStage::Partition);
        assert!(p.history.is_empty());
        assert!(p.id.starts_with("pipeline-"));
    }

    #[test]
    fn test_advance_follows_correct_sequence() {
        let mut p = Pipeline::new(test_config());
        let expected = [
            PipelineStage::Review,
            PipelineStage::AwaitReview,
            PipelineStage::Consolidate,
            PipelineStage::HumanCheckpoint,
        ];
        for &expected_stage in &expected {
            let next = p.advance().unwrap();
            assert_eq!(next, expected_stage);
            assert_eq!(p.current_stage, expected_stage);
        }
    }

    #[test]
    fn test_advance_rejects_at_checkpoint() {
        let mut p = Pipeline::new(test_config());
        // Advance to HumanCheckpoint
        p.advance().unwrap(); // Review
        p.advance().unwrap(); // AwaitReview
        p.advance().unwrap(); // Consolidate
        p.advance().unwrap(); // HumanCheckpoint

        let err = p.advance().unwrap_err();
        assert!(
            err.to_string().contains("human checkpoint"),
            "Expected checkpoint error, got: {}",
            err
        );
    }

    #[test]
    fn test_advance_rejects_terminal_done() {
        let mut p = Pipeline::new(test_config());
        // Fast-forward to Done
        p.current_stage = PipelineStage::PullRequest;
        p.advance().unwrap(); // -> Done
        assert_eq!(p.current_stage, PipelineStage::Done);

        let err = p.advance().unwrap_err();
        assert!(
            err.to_string().contains("terminal"),
            "Expected terminal error, got: {}",
            err
        );
    }

    #[test]
    fn test_advance_rejects_terminal_failed() {
        let mut p = Pipeline::new(test_config());
        p.fail("something broke");
        assert_eq!(p.current_stage, PipelineStage::Failed);

        let err = p.advance().unwrap_err();
        assert!(
            err.to_string().contains("terminal"),
            "Expected terminal error, got: {}",
            err
        );
    }

    #[test]
    fn test_is_checkpoint() {
        assert!(Pipeline::is_checkpoint(PipelineStage::HumanCheckpoint));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Review));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Done));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Partition));
    }

    #[test]
    fn test_confirm_checkpoint_advances() {
        let mut p = Pipeline::new(test_config());
        // Advance to HumanCheckpoint
        p.advance().unwrap(); // Review
        p.advance().unwrap(); // AwaitReview
        p.advance().unwrap(); // Consolidate
        p.advance().unwrap(); // HumanCheckpoint

        p.confirm_checkpoint().unwrap();
        assert_eq!(p.current_stage, PipelineStage::FileIssues);
    }

    #[test]
    fn test_confirm_checkpoint_rejects_non_checkpoint() {
        let mut p = Pipeline::new(test_config());
        let err = p.confirm_checkpoint().unwrap_err();
        assert!(
            err.to_string().contains("not at a human checkpoint"),
            "Expected non-checkpoint error, got: {}",
            err
        );
    }

    #[test]
    fn test_fail_sets_failed_state() {
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap(); // Review
        p.fail("test failure");
        assert_eq!(p.current_stage, PipelineStage::Failed);
        assert_eq!(p.history.len(), 2); // advance + fail
        let last = p.history.last().unwrap();
        assert_eq!(last.to, PipelineStage::Failed);
        assert_eq!(last.notes.as_deref(), Some("test failure"));
    }

    #[test]
    fn test_summary_produces_readable_output() {
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap(); // Review
        let summary = p.summary();
        assert!(summary.contains("Pipeline:"));
        assert!(summary.contains("Stage:    review"), "Summary: {}", summary);
        assert!(summary.contains("Agents:   4"));
        assert!(summary.contains("Mandate:  security review"));
        assert!(summary.contains("History:"));
        assert!(summary.contains("partition -> review"));
    }

    #[test]
    fn test_pipeline_stage_display_matches_serde() {
        // Display must match serde rename_all = "snake_case" (#489)
        assert_eq!(PipelineStage::Partition.to_string(), "partition");
        assert_eq!(PipelineStage::Review.to_string(), "review");
        assert_eq!(PipelineStage::AwaitReview.to_string(), "await_review");
        assert_eq!(PipelineStage::Consolidate.to_string(), "consolidate");
        assert_eq!(
            PipelineStage::HumanCheckpoint.to_string(),
            "human_checkpoint"
        );
        assert_eq!(PipelineStage::FileIssues.to_string(), "file_issues");
        assert_eq!(PipelineStage::Fix.to_string(), "fix");
        assert_eq!(PipelineStage::AwaitFix.to_string(), "await_fix");
        assert_eq!(PipelineStage::Merge.to_string(), "merge");
        assert_eq!(PipelineStage::PullRequest.to_string(), "pull_request");
        assert_eq!(PipelineStage::Done.to_string(), "done");
        assert_eq!(PipelineStage::Failed.to_string(), "failed");

        // Verify Display matches serde roundtrip
        for stage in [
            PipelineStage::Partition,
            PipelineStage::AwaitReview,
            PipelineStage::HumanCheckpoint,
            PipelineStage::FileIssues,
            PipelineStage::AwaitFix,
            PipelineStage::PullRequest,
        ] {
            let json = serde_json::to_string(&stage).unwrap();
            let serde_name = json.trim_matches('"');
            assert_eq!(
                stage.to_string(),
                serde_name,
                "Display and serde mismatch for {:?}",
                stage
            );
        }
    }

    #[test]
    fn test_serde_roundtrip_pipeline() {
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap(); // Review
        p.advance().unwrap(); // AwaitReview

        let json = serde_json::to_string(&p).unwrap();
        let parsed: Pipeline = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.id, p.id);
        assert_eq!(parsed.created_at, p.created_at);
        assert_eq!(parsed.current_stage, p.current_stage);
        assert_eq!(parsed.history.len(), p.history.len());
        assert_eq!(parsed.config.agent_count, p.config.agent_count);
        assert_eq!(parsed.config.mandate, p.config.mandate);
    }

    #[test]
    fn test_serde_roundtrip_config() {
        let config = test_config();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: PipelineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_count, config.agent_count);
        assert_eq!(parsed.mandate, config.mandate);
        assert_eq!(parsed.auto_fix, config.auto_fix);
        assert_eq!(parsed.auto_file_issues, config.auto_file_issues);
        assert_eq!(parsed.target_branch, config.target_branch);
    }

    #[test]
    fn test_serde_roundtrip_stage_transition() {
        let t = StageTransition {
            from: PipelineStage::Review,
            to: PipelineStage::AwaitReview,
            timestamp: Utc::now(),
            notes: Some("test note".to_string()),
        };
        let json = serde_json::to_string(&t).unwrap();
        let parsed: StageTransition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.from, t.from);
        assert_eq!(parsed.to, t.to);
        assert_eq!(parsed.timestamp, t.timestamp);
        assert_eq!(parsed.notes, t.notes);
    }

    #[test]
    fn test_valid_transitions_for_each_stage() {
        // Partition -> Review or Failed
        let v = Pipeline::valid_transitions(PipelineStage::Partition);
        assert!(v.contains(&PipelineStage::Review));
        assert!(v.contains(&PipelineStage::Failed));
        assert_eq!(v.len(), 2);

        // Review -> AwaitReview or Failed
        let v = Pipeline::valid_transitions(PipelineStage::Review);
        assert!(v.contains(&PipelineStage::AwaitReview));
        assert!(v.contains(&PipelineStage::Failed));

        // AwaitReview -> Consolidate or Failed
        let v = Pipeline::valid_transitions(PipelineStage::AwaitReview);
        assert!(v.contains(&PipelineStage::Consolidate));

        // Consolidate -> HumanCheckpoint or Failed
        let v = Pipeline::valid_transitions(PipelineStage::Consolidate);
        assert!(v.contains(&PipelineStage::HumanCheckpoint));

        // HumanCheckpoint -> FileIssues or Failed
        let v = Pipeline::valid_transitions(PipelineStage::HumanCheckpoint);
        assert!(v.contains(&PipelineStage::FileIssues));

        // FileIssues -> Fix or Failed
        let v = Pipeline::valid_transitions(PipelineStage::FileIssues);
        assert!(v.contains(&PipelineStage::Fix));

        // Fix -> AwaitFix or Failed
        let v = Pipeline::valid_transitions(PipelineStage::Fix);
        assert!(v.contains(&PipelineStage::AwaitFix));

        // AwaitFix -> Merge or Failed
        let v = Pipeline::valid_transitions(PipelineStage::AwaitFix);
        assert!(v.contains(&PipelineStage::Merge));

        // Merge -> PullRequest or Failed
        let v = Pipeline::valid_transitions(PipelineStage::Merge);
        assert!(v.contains(&PipelineStage::PullRequest));

        // PullRequest -> Done or Failed
        let v = Pipeline::valid_transitions(PipelineStage::PullRequest);
        assert!(v.contains(&PipelineStage::Done));

        // Terminal stages have no transitions
        assert!(Pipeline::valid_transitions(PipelineStage::Done).is_empty());
        assert!(Pipeline::valid_transitions(PipelineStage::Failed).is_empty());
    }

    #[test]
    fn test_history_records_all_transitions() {
        let mut p = Pipeline::new(test_config());
        assert!(p.history.is_empty());

        p.advance().unwrap(); // Partition -> Review
        assert_eq!(p.history.len(), 1);
        assert_eq!(p.history[0].from, PipelineStage::Partition);
        assert_eq!(p.history[0].to, PipelineStage::Review);

        p.advance().unwrap(); // Review -> AwaitReview
        assert_eq!(p.history.len(), 2);
        assert_eq!(p.history[1].from, PipelineStage::Review);
        assert_eq!(p.history[1].to, PipelineStage::AwaitReview);

        p.advance().unwrap(); // AwaitReview -> Consolidate
        p.advance().unwrap(); // Consolidate -> HumanCheckpoint
        p.confirm_checkpoint().unwrap(); // HumanCheckpoint -> FileIssues
        assert_eq!(p.history.len(), 5);
        assert_eq!(p.history[4].from, PipelineStage::HumanCheckpoint);
        assert_eq!(p.history[4].to, PipelineStage::FileIssues);
        assert_eq!(
            p.history[4].notes.as_deref(),
            Some("Human checkpoint confirmed")
        );
    }

    #[test]
    fn test_save_and_load_pipeline() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap();

        save_pipeline(dir.path(), &p).unwrap();
        let loaded = load_pipeline(dir.path()).unwrap().unwrap();

        assert_eq!(loaded.id, p.id);
        assert_eq!(loaded.current_stage, p.current_stage);
        assert_eq!(loaded.history.len(), p.history.len());
    }

    #[test]
    fn test_load_pipeline_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = load_pipeline(dir.path()).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_summary_no_history() {
        let p = Pipeline::new(test_config());
        let summary = p.summary();
        assert!(summary.contains("Pipeline:"));
        assert!(summary.contains("Stage:    partition"));
        assert!(summary.contains("Agents:   4"));
        assert!(summary.contains("Mandate:  security review"));
        assert!(summary.contains("Branch:   main"));
        assert!(summary.contains("Auto-fix: true"));
        assert!(summary.contains("Auto-file-issues: true"));
        assert!(!summary.contains("History:"));
    }

    #[test]
    fn test_summary_with_notes_in_history() {
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap();
        p.fail("something went wrong");
        let summary = p.summary();
        assert!(summary.contains("History:"));
        assert!(summary.contains("(something went wrong)"));
        assert!(summary.contains("review -> failed"));
    }

    #[test]
    fn test_summary_history_without_notes() {
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap();
        let summary = p.summary();
        assert!(summary.contains("History:"));
        assert!(summary.contains("partition -> review"));
        assert!(!summary.contains("("));
    }

    #[test]
    fn test_advance_full_happy_path_after_checkpoint() {
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap();
        p.advance().unwrap();
        p.advance().unwrap();
        p.advance().unwrap();

        p.confirm_checkpoint().unwrap();
        assert_eq!(p.current_stage, PipelineStage::FileIssues);

        p.advance().unwrap();
        assert_eq!(p.current_stage, PipelineStage::Fix);
        p.advance().unwrap();
        assert_eq!(p.current_stage, PipelineStage::AwaitFix);
        p.advance().unwrap();
        assert_eq!(p.current_stage, PipelineStage::Merge);
        p.advance().unwrap();
        assert_eq!(p.current_stage, PipelineStage::PullRequest);
        p.advance().unwrap();
        assert_eq!(p.current_stage, PipelineStage::Done);
    }

    #[test]
    fn test_pipeline_config_auto_flags() {
        let config = PipelineConfig {
            agent_count: 2,
            mandate: "test".to_string(),
            auto_fix: false,
            auto_file_issues: false,
            target_branch: "develop".to_string(),
        };
        let p = Pipeline::new(config);
        assert!(!p.config.auto_fix);
        assert!(!p.config.auto_file_issues);
        assert_eq!(p.config.target_branch, "develop");
        assert_eq!(p.config.agent_count, 2);
    }

    #[test]
    fn test_fail_from_partition() {
        let mut p = Pipeline::new(test_config());
        assert_eq!(p.current_stage, PipelineStage::Partition);
        p.fail("partition error");
        assert_eq!(p.current_stage, PipelineStage::Failed);
        assert_eq!(p.history.len(), 1);
        assert_eq!(p.history[0].from, PipelineStage::Partition);
        assert_eq!(p.history[0].to, PipelineStage::Failed);
    }

    #[test]
    fn test_serde_stage_rename_all() {
        let stages = vec![
            PipelineStage::Partition,
            PipelineStage::Review,
            PipelineStage::AwaitReview,
            PipelineStage::Consolidate,
            PipelineStage::HumanCheckpoint,
            PipelineStage::FileIssues,
            PipelineStage::Fix,
            PipelineStage::AwaitFix,
            PipelineStage::Merge,
            PipelineStage::PullRequest,
            PipelineStage::Done,
            PipelineStage::Failed,
        ];
        for stage in stages {
            let json = serde_json::to_string(&stage).unwrap();
            let parsed: PipelineStage = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, stage);
        }
    }

    #[test]
    fn test_save_pipeline_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Pipeline::new(test_config());
        save_pipeline(dir.path(), &p).unwrap();

        p.advance().unwrap();
        save_pipeline(dir.path(), &p).unwrap();

        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::Review);
        assert_eq!(loaded.history.len(), 1);
    }

    #[test]
    fn test_load_pipeline_invalid_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        std::fs::write(&path, "not valid json").unwrap();
        let result = load_pipeline(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_run_pipeline_creates_and_pauses_at_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let config = test_config();
        run_pipeline(dir.path(), config).unwrap();

        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::HumanCheckpoint);
    }

    #[test]
    fn test_run_pipeline_resume_at_done() {
        let dir = tempfile::tempdir().unwrap();
        // Manually create a pipeline at Done stage
        let mut p = Pipeline::new(test_config());
        p.current_stage = PipelineStage::Done;
        save_pipeline(dir.path(), &p).unwrap();

        // Running should resume and detect terminal stage
        run_pipeline(dir.path(), test_config()).unwrap();

        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::Done);
    }

    #[test]
    fn test_transition_notes_preserved() {
        let mut p = Pipeline::new(test_config());
        p.record_transition(PipelineStage::Review, Some("custom note"));
        assert_eq!(p.history.len(), 1);
        assert_eq!(p.history[0].notes.as_deref(), Some("custom note"));
    }

    #[test]
    fn test_transition_notes_none() {
        let mut p = Pipeline::new(test_config());
        p.record_transition(PipelineStage::Review, None);
        assert_eq!(p.history.len(), 1);
        assert!(p.history[0].notes.is_none());
    }

    #[test]
    fn test_pipeline_id_format() {
        let p = Pipeline::new(test_config());
        assert!(p.id.starts_with("pipeline-"));
        let parts: Vec<&str> = p.id.splitn(2, '-').collect();
        assert_eq!(parts[0], "pipeline");
        assert!(parts[1].len() > 15);
    }

    #[test]
    fn test_pipeline_created_at_is_datetime() {
        let p = Pipeline::new(test_config());
        // created_at is now DateTime<Utc>; verify it serializes to valid RFC3339
        let json = serde_json::to_string(&p.created_at).unwrap();
        let parsed = chrono::DateTime::parse_from_rfc3339(json.trim_matches('"'));
        assert!(
            parsed.is_ok(),
            "created_at should serialize to valid RFC3339: {}",
            json
        );
    }

    #[test]
    fn test_is_checkpoint_all_stages() {
        assert!(!Pipeline::is_checkpoint(PipelineStage::Partition));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Review));
        assert!(!Pipeline::is_checkpoint(PipelineStage::AwaitReview));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Consolidate));
        assert!(Pipeline::is_checkpoint(PipelineStage::HumanCheckpoint));
        assert!(!Pipeline::is_checkpoint(PipelineStage::FileIssues));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Fix));
        assert!(!Pipeline::is_checkpoint(PipelineStage::AwaitFix));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Merge));
        assert!(!Pipeline::is_checkpoint(PipelineStage::PullRequest));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Done));
        assert!(!Pipeline::is_checkpoint(PipelineStage::Failed));
    }

    // -----------------------------------------------------------------------
    // Additional coverage tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_print_stage_action_all_stages() {
        // Exercise every branch in print_stage_action to ensure no panics
        // and to cover all match arms.
        let config_auto = test_config(); // auto_fix: true, auto_file_issues: true
        let config_manual = PipelineConfig {
            agent_count: 2,
            mandate: "manual review".to_string(),
            auto_fix: false,
            auto_file_issues: false,
            target_branch: "develop".to_string(),
        };

        let all_stages = [
            PipelineStage::Partition,
            PipelineStage::Review,
            PipelineStage::AwaitReview,
            PipelineStage::Consolidate,
            PipelineStage::HumanCheckpoint,
            PipelineStage::FileIssues,
            PipelineStage::Fix,
            PipelineStage::AwaitFix,
            PipelineStage::Merge,
            PipelineStage::PullRequest,
            PipelineStage::Done,
            PipelineStage::Failed,
        ];

        for stage in all_stages {
            print_stage_action(stage, &config_auto);
            print_stage_action(stage, &config_manual);
        }
    }

    #[test]
    fn test_run_pipeline_resume_at_failed() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Pipeline::new(test_config());
        p.fail("earlier failure");
        save_pipeline(dir.path(), &p).unwrap();

        // Running should resume and detect the Failed terminal stage
        run_pipeline(dir.path(), test_config()).unwrap();

        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::Failed);
    }

    #[test]
    fn test_serde_stage_snake_case_values() {
        // Verify the rename_all = "snake_case" produces expected JSON strings
        assert_eq!(
            serde_json::to_string(&PipelineStage::Partition).unwrap(),
            "\"partition\""
        );
        assert_eq!(
            serde_json::to_string(&PipelineStage::AwaitReview).unwrap(),
            "\"await_review\""
        );
        assert_eq!(
            serde_json::to_string(&PipelineStage::HumanCheckpoint).unwrap(),
            "\"human_checkpoint\""
        );
        assert_eq!(
            serde_json::to_string(&PipelineStage::FileIssues).unwrap(),
            "\"file_issues\""
        );
        assert_eq!(
            serde_json::to_string(&PipelineStage::AwaitFix).unwrap(),
            "\"await_fix\""
        );
        assert_eq!(
            serde_json::to_string(&PipelineStage::PullRequest).unwrap(),
            "\"pull_request\""
        );
        assert_eq!(
            serde_json::to_string(&PipelineStage::Done).unwrap(),
            "\"done\""
        );
        assert_eq!(
            serde_json::to_string(&PipelineStage::Failed).unwrap(),
            "\"failed\""
        );
    }

    #[test]
    fn test_serde_stage_deserialize_from_snake_case_string() {
        // Confirm we can deserialize from the snake_case JSON strings
        let stage: PipelineStage = serde_json::from_str("\"await_review\"").unwrap();
        assert_eq!(stage, PipelineStage::AwaitReview);

        let stage: PipelineStage = serde_json::from_str("\"human_checkpoint\"").unwrap();
        assert_eq!(stage, PipelineStage::HumanCheckpoint);

        let stage: PipelineStage = serde_json::from_str("\"file_issues\"").unwrap();
        assert_eq!(stage, PipelineStage::FileIssues);

        let stage: PipelineStage = serde_json::from_str("\"pull_request\"").unwrap();
        assert_eq!(stage, PipelineStage::PullRequest);
    }

    #[test]
    fn test_serde_stage_invalid_value_errors() {
        let result = serde_json::from_str::<PipelineStage>("\"nonexistent_stage\"");
        assert!(result.is_err());

        let result = serde_json::from_str::<PipelineStage>("42");
        assert!(result.is_err());

        let result = serde_json::from_str::<PipelineStage>("null");
        assert!(result.is_err());
    }

    #[test]
    fn test_save_pipeline_to_nonexistent_directory() {
        let dir = Path::new("/tmp/crosslink_test_nonexistent_dir_that_should_not_exist_29384");
        assert!(!dir.exists());
        let p = Pipeline::new(test_config());
        let result = save_pipeline(dir, &p);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Failed to write"),
            "Expected write error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_load_pipeline_partial_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        // Valid JSON but missing required fields
        std::fs::write(&path, r#"{"id": "test"}"#).unwrap();
        let result = load_pipeline(dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_confirm_checkpoint_rejects_at_various_stages() {
        // Test confirm_checkpoint fails at stages other than Partition (already tested)
        let stages = [
            PipelineStage::Review,
            PipelineStage::AwaitReview,
            PipelineStage::Consolidate,
            PipelineStage::FileIssues,
            PipelineStage::Fix,
            PipelineStage::AwaitFix,
            PipelineStage::Merge,
            PipelineStage::PullRequest,
            PipelineStage::Done,
            PipelineStage::Failed,
        ];
        for stage in stages {
            let mut p = Pipeline::new(test_config());
            p.current_stage = stage;
            let err = p.confirm_checkpoint().unwrap_err();
            assert!(
                err.to_string().contains("not at a human checkpoint"),
                "Stage {:?} should reject confirm_checkpoint, got: {}",
                stage,
                err
            );
        }
    }

    #[test]
    fn test_summary_with_auto_flags_false() {
        let config = PipelineConfig {
            agent_count: 1,
            mandate: "perf audit".to_string(),
            auto_fix: false,
            auto_file_issues: false,
            target_branch: "release".to_string(),
        };
        let p = Pipeline::new(config);
        let summary = p.summary();
        assert!(summary.contains("Auto-fix: false"));
        assert!(summary.contains("Auto-file-issues: false"));
        assert!(summary.contains("Branch:   release"));
        assert!(summary.contains("Agents:   1"));
        assert!(summary.contains("Mandate:  perf audit"));
    }

    #[test]
    fn test_fail_at_every_non_terminal_stage() {
        let stages = [
            PipelineStage::Partition,
            PipelineStage::Review,
            PipelineStage::AwaitReview,
            PipelineStage::Consolidate,
            PipelineStage::HumanCheckpoint,
            PipelineStage::FileIssues,
            PipelineStage::Fix,
            PipelineStage::AwaitFix,
            PipelineStage::Merge,
            PipelineStage::PullRequest,
        ];
        for stage in stages {
            let mut p = Pipeline::new(test_config());
            p.current_stage = stage;
            p.fail("forced failure");
            assert_eq!(
                p.current_stage,
                PipelineStage::Failed,
                "fail() should set Failed from {:?}",
                stage
            );
            let last = p.history.last().unwrap();
            assert_eq!(last.from, stage);
            assert_eq!(last.to, PipelineStage::Failed);
            assert_eq!(last.notes.as_deref(), Some("forced failure"));
        }
    }

    #[test]
    fn test_multiple_save_load_cycles() {
        let dir = tempfile::tempdir().unwrap();
        let mut p = Pipeline::new(test_config());

        // Save at Partition
        save_pipeline(dir.path(), &p).unwrap();
        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::Partition);
        assert_eq!(loaded.history.len(), 0);

        // Advance, save, load
        p.advance().unwrap();
        save_pipeline(dir.path(), &p).unwrap();
        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::Review);
        assert_eq!(loaded.history.len(), 1);

        // Fail, save, load
        p.fail("oops");
        save_pipeline(dir.path(), &p).unwrap();
        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::Failed);
        assert_eq!(loaded.history.len(), 2);
        assert_eq!(loaded.history[1].notes.as_deref(), Some("oops"));
    }

    #[test]
    fn test_history_timestamps_are_datetime() {
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap();
        p.advance().unwrap();
        p.fail("done");

        for (i, t) in p.history.iter().enumerate() {
            // timestamp is now DateTime<Utc>; verify it serializes to valid RFC3339
            let json = serde_json::to_string(&t.timestamp).unwrap();
            let parsed = chrono::DateTime::parse_from_rfc3339(json.trim_matches('"'));
            assert!(
                parsed.is_ok(),
                "history[{}] timestamp should serialize to valid RFC3339: {}",
                i,
                json
            );
        }
    }

    #[test]
    fn test_record_transition_updates_current_stage() {
        let mut p = Pipeline::new(test_config());
        assert_eq!(p.current_stage, PipelineStage::Partition);

        p.record_transition(PipelineStage::Failed, Some("direct fail"));
        assert_eq!(p.current_stage, PipelineStage::Failed);
        assert_eq!(p.history.len(), 1);
        assert_eq!(p.history[0].from, PipelineStage::Partition);
        assert_eq!(p.history[0].to, PipelineStage::Failed);
    }

    #[test]
    fn test_stage_transition_serde_without_notes() {
        let t = StageTransition {
            from: PipelineStage::Partition,
            to: PipelineStage::Review,
            timestamp: Utc::now(),
            notes: None,
        };
        let json = serde_json::to_string(&t).unwrap();
        let parsed: StageTransition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.from, PipelineStage::Partition);
        assert_eq!(parsed.to, PipelineStage::Review);
        assert!(parsed.notes.is_none());
    }

    #[test]
    fn test_pipeline_config_preserves_empty_mandate() {
        let config = PipelineConfig {
            agent_count: 0,
            mandate: String::new(),
            auto_fix: false,
            auto_file_issues: false,
            target_branch: String::new(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: PipelineConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_count, 0);
        assert!(parsed.mandate.is_empty());
        assert!(parsed.target_branch.is_empty());
    }

    #[test]
    fn test_valid_transitions_always_have_failed_or_empty() {
        // Every non-terminal stage should include Failed as a valid transition;
        // terminal stages should have empty transitions.
        let non_terminal = [
            PipelineStage::Partition,
            PipelineStage::Review,
            PipelineStage::AwaitReview,
            PipelineStage::Consolidate,
            PipelineStage::HumanCheckpoint,
            PipelineStage::FileIssues,
            PipelineStage::Fix,
            PipelineStage::AwaitFix,
            PipelineStage::Merge,
            PipelineStage::PullRequest,
        ];
        for stage in non_terminal {
            let v = Pipeline::valid_transitions(stage);
            assert!(
                v.contains(&PipelineStage::Failed),
                "{:?} should have Failed as valid transition",
                stage
            );
            assert!(
                v.len() == 2,
                "{:?} should have exactly 2 transitions (happy + Failed), got {}",
                stage,
                v.len()
            );
        }
        assert!(Pipeline::valid_transitions(PipelineStage::Done).is_empty());
        assert!(Pipeline::valid_transitions(PipelineStage::Failed).is_empty());
    }

    #[test]
    fn test_summary_contains_created_at() {
        let p = Pipeline::new(test_config());
        let summary = p.summary();
        assert!(
            summary.contains("Created:"),
            "Summary should include Created: field"
        );
        // DateTime<Utc> Display produces RFC3339 format
        let created_str = p.created_at.to_rfc3339();
        assert!(
            summary.contains(&created_str),
            "Summary should include the actual created_at value: {}",
            summary
        );
    }

    #[test]
    fn test_advance_from_each_non_terminal_non_checkpoint_stage() {
        // Ensure advance() works for every non-terminal, non-checkpoint stage
        let test_cases = [
            (PipelineStage::Partition, PipelineStage::Review),
            (PipelineStage::Review, PipelineStage::AwaitReview),
            (PipelineStage::AwaitReview, PipelineStage::Consolidate),
            (PipelineStage::Consolidate, PipelineStage::HumanCheckpoint),
            (PipelineStage::FileIssues, PipelineStage::Fix),
            (PipelineStage::Fix, PipelineStage::AwaitFix),
            (PipelineStage::AwaitFix, PipelineStage::Merge),
            (PipelineStage::Merge, PipelineStage::PullRequest),
            (PipelineStage::PullRequest, PipelineStage::Done),
        ];
        for (from, expected_to) in test_cases {
            let mut p = Pipeline::new(test_config());
            p.current_stage = from;
            let result = p.advance().unwrap();
            assert_eq!(
                result, expected_to,
                "advance from {:?} should go to {:?}",
                from, expected_to
            );
            assert_eq!(p.current_stage, expected_to);
        }
    }

    #[test]
    fn test_pipeline_clone() {
        let mut p = Pipeline::new(test_config());
        p.advance().unwrap();
        let cloned = p.clone();
        assert_eq!(cloned.id, p.id);
        assert_eq!(cloned.current_stage, p.current_stage);
        assert_eq!(cloned.history.len(), p.history.len());
        assert_eq!(cloned.config.mandate, p.config.mandate);
    }

    #[test]
    fn test_run_pipeline_full_happy_path_after_checkpoint_resume() {
        let dir = tempfile::tempdir().unwrap();

        // First run: creates pipeline, stops at HumanCheckpoint
        run_pipeline(dir.path(), test_config()).unwrap();
        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::HumanCheckpoint);

        // Manually confirm checkpoint and save
        let mut p = loaded;
        p.confirm_checkpoint().unwrap();
        save_pipeline(dir.path(), &p).unwrap();

        // Second run: resumes from FileIssues, runs through to Done
        // (the pipeline won't hit HumanCheckpoint again since we're past it)
        run_pipeline(dir.path(), test_config()).unwrap();
        let loaded = load_pipeline(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.current_stage, PipelineStage::Done);
    }
}
