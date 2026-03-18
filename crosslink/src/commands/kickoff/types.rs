// E-ana tablet — kickoff types: shared data structures
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// Container runtime for agent execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ContainerMode {
    /// Run as a local process (tmux session with claude CLI).
    None,
    /// Run inside a Docker container.
    Docker,
    /// Run inside a Podman container.
    Podman,
}

/// Post-implementation verification level.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifyLevel {
    /// Local tests and self-review checklist only.
    Local,
    /// Push branch, open draft PR, wait for CI.
    Ci,
    /// CI plus structured adversarial self-review.
    Thorough,
}

/// A single acceptance criterion extracted from a design document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Criterion {
    pub id: String,
    pub text: String,
    #[serde(rename = "type")]
    pub criterion_type: String,
}

/// Machine-readable acceptance criteria file (`.kickoff-criteria.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CriteriaFile {
    pub source_doc: String,
    pub extracted_at: String,
    pub criteria: Vec<Criterion>,
}

/// Metadata written at agent launch (`.kickoff-metadata.json`).
///
/// Records the timeout and start time so that `status` / `list` can detect
/// agents that have exceeded their time budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KickoffMetadata {
    /// ISO-8601 UTC timestamp of when the agent was launched.
    pub started_at: String,
    /// Timeout in seconds (matches `--timeout` flag).
    pub timeout_secs: u64,
}

/// Options for `crosslink kickoff run`.
pub struct KickoffOpts<'a> {
    pub description: &'a str,
    pub issue: Option<i64>,
    pub container: ContainerMode,
    pub verify: VerifyLevel,
    pub model: &'a str,
    pub image: &'a str,
    pub timeout: Duration,
    pub dry_run: bool,
    pub branch: Option<&'a str>,
    pub quiet: bool,
    pub design_doc: Option<&'a super::super::design_doc::DesignDoc>,
    pub doc_path: Option<&'a str>,
    pub skip_permissions: bool,
}

/// A single criterion verdict in the validation report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CriterionVerdict {
    pub id: String,
    pub verdict: String,
    pub evidence: String,
}

/// Summary counts in the validation report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportSummary {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub partial: usize,
    pub not_applicable: usize,
    pub needs_clarification: usize,
}

/// Timing and metrics for a single phase of agent work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PhaseTiming {
    pub duration_s: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_modified: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines_removed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_run: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments_added: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria_checked: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues_found: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues_fixed: Option<u64>,
}

/// Phase-level timing breakdown for a kickoff run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PhaseTimings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exploration: Option<PhaseTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning: Option<PhaseTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<PhaseTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub testing: Option<PhaseTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation: Option<PhaseTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<PhaseTiming>,
}

/// The `.kickoff-report.json` file contents.
///
/// Phase 3 fields (`validated_at`, `criteria`, `summary`) are always required.
/// Phase 4 fields are optional with serde defaults for backward compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KickoffReport {
    // Phase 3 fields (backward compat — always present)
    pub validated_at: String,
    pub criteria: Vec<CriterionVerdict>,
    pub summary: ReportSummary,

    // Phase 4 fields (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<PhaseTimings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresolved_questions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_changed: Option<Vec<String>>,
}

/// Output format for the kickoff report command.
#[derive(Debug, Clone, PartialEq)]
pub enum ReportFormat {
    /// Human-readable table with symbols.
    Table,
    /// Raw JSON output.
    Json,
    /// PR-ready markdown format.
    Markdown,
}

/// Options for `crosslink kickoff plan`.
pub struct PlanOpts<'a> {
    pub doc: &'a super::super::design_doc::DesignDoc,
    pub model: &'a str,
    pub timeout: Duration,
    pub dry_run: bool,
    pub issue: Option<i64>,
    pub quiet: bool,
}

/// Detect project conventions from the repo root.
pub(crate) struct ProjectConventions {
    pub(crate) test_command: Option<String>,
    pub(crate) lint_commands: Vec<String>,
    pub(crate) allowed_tools: Vec<String>,
}

/// Result of a successful pre-flight check.
pub(crate) struct PreflightResult {
    /// The resolved timeout command (`timeout` or `gtimeout`).
    pub timeout_cmd: &'static str,
    /// Optional sandbox wrapper command from hook-config.json `sandbox.command`.
    pub sandbox_command: Option<String>,
}

/// Detected platform for generating targeted install instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Platform {
    MacOS,
    Linux(LinuxDistro),
    Windows,
}

/// Known Linux distribution families.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinuxDistro {
    Debian,
    Fedora,
    Arch,
    Alpine,
    Other,
}

/// Watchdog configuration for detecting and nudging idle agents.
pub(super) struct WatchdogConfig {
    /// Whether the watchdog is enabled (default: true)
    pub enabled: bool,
    /// Seconds of heartbeat staleness before nudging (default: 300)
    pub staleness_secs: u64,
    /// Maximum number of nudges before giving up (default: 5)
    pub max_nudges: u32,
    /// Seconds between watchdog checks (default: 120)
    pub check_interval_secs: u64,
    /// Grace period before watchdog starts checking (default: 300)
    pub grace_period_secs: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            staleness_secs: 300,
            max_nudges: 5,
            check_interval_secs: 120,
            grace_period_secs: 300,
        }
    }
}

/// Information about a discovered kickoff agent.
#[derive(Debug, Clone, Serialize)]
pub(super) struct AgentInfo {
    pub id: String,
    pub issue: Option<String>,
    pub status: String,
    pub session: Option<String>,
    pub worktree: String,
    pub docker: Option<String>,
}

/// Classification of an agent for cleanup purposes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(super) enum CleanupClass {
    /// Agent confirmed done — safe to remove.
    Done,
    /// Agent appears stale (no tmux/container, no DONE sentinel).
    Stale,
    /// Agent is still active — do not touch.
    Active,
}

/// Result of a single agent cleanup action.
#[derive(Debug, Serialize)]
pub(super) struct CleanupResult {
    pub id: String,
    pub class: CleanupClass,
    pub worktree_removed: bool,
    pub tmux_killed: bool,
    pub container_removed: bool,
    pub error: Option<String>,
}

/// Parse a container mode string into the enum.
pub fn parse_container_mode(s: &str) -> Result<ContainerMode> {
    match s.to_lowercase().as_str() {
        "none" | "local" => Ok(ContainerMode::None),
        "docker" => Ok(ContainerMode::Docker),
        "podman" => Ok(ContainerMode::Podman),
        _ => bail!(
            "Unknown container runtime '{}'. Use: none, docker, podman",
            s
        ),
    }
}

/// Parse a verification level string into the enum.
pub fn parse_verify_level(s: &str) -> Result<VerifyLevel> {
    match s.to_lowercase().as_str() {
        "local" => Ok(VerifyLevel::Local),
        "ci" => Ok(VerifyLevel::Ci),
        "thorough" => Ok(VerifyLevel::Thorough),
        _ => bail!(
            "Unknown verification level '{}'. Use: local, ci, thorough",
            s
        ),
    }
}

/// Parse a human-readable duration string (e.g. "1h", "30m", "90s") into Duration.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("Empty duration string");
    }

    let (num_str, unit) = if let Some(n) = s.strip_suffix('h') {
        (n, 'h')
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 'm')
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 's')
    } else {
        // Bare number defaults to seconds
        (s, 's')
    };

    let value: u64 = num_str
        .parse()
        .with_context(|| format!("Invalid duration number: '{}'", num_str))?;

    let secs = match unit {
        'h' => value * 3600,
        'm' => value * 60,
        's' => value,
        _ => unreachable!(),
    };

    if secs == 0 {
        bail!("Duration must be greater than zero");
    }

    Ok(Duration::from_secs(secs))
}

/// Check if an agent has exceeded its timeout based on `.kickoff-metadata.json`.
///
/// Returns `true` if the metadata file exists, contains a valid start time and
/// timeout, and the elapsed wall-clock time exceeds the configured timeout.
pub(super) fn is_timed_out(wt_path: &Path) -> bool {
    let meta_path = wt_path.join(".kickoff-metadata.json");
    let content = match std::fs::read_to_string(&meta_path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let meta: KickoffMetadata = match serde_json::from_str(&content) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let started = match chrono::DateTime::parse_from_rfc3339(&meta.started_at) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => return false,
    };
    let elapsed = chrono::Utc::now().signed_duration_since(started);
    elapsed.num_seconds() > meta.timeout_secs as i64
}

use anyhow::Context;
