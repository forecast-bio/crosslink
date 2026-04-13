use std::time::Duration;

use crate::commands::kickoff::VerifyLevel;

use super::config::SentinelConfig;
use super::sources::{Signal, SignalDecision, SourceKind};

/// What the triage engine decides to do with a signal.
#[derive(Debug, Clone)]
pub enum Disposition {
    /// Spawn a kickoff agent with this scope.
    Dispatch {
        description: String,
        scope: AgentScope,
        attempt: u32,
    },
    /// Create a crosslink issue for human review.
    Triage {
        priority: String,
        labels: Vec<String>,
    },
    /// Already handled or no matching rule — skip.
    Skip { reason: String },
    /// Eligible but cannot dispatch right now (e.g. at concurrent agent capacity).
    /// Will be retried on the next cycle.
    Defer { reason: String },
}

/// Constrains what a dispatched agent can do.
#[derive(Debug, Clone)]
pub struct AgentScope {
    /// Path prefixes the agent is allowed to write to (e.g. `["tests/", "src/"]`).
    /// Enforced via the kickoff prompt + allowed-tools whitelist.
    pub allowed_paths: Vec<String>,
    pub verify: VerifyLevel,
    pub timeout: Duration,
    pub model: String,
}

/// Run a signal through the triage engine to determine its disposition.
///
/// If `tuning` is provided, it may override the default model for signals
/// where historical data shows poor Sonnet performance.
pub fn triage(
    signal: &Signal,
    decision: &SignalDecision,
    config: &SentinelConfig,
    tuning: Option<&super::tuning::TuningOverride>,
) -> Disposition {
    let label = signal
        .metadata
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (model, attempt) = match decision {
        SignalDecision::New => {
            // Check if self-tuning recommends a different model for this label
            let model = tuning
                .and_then(|t| t.model_for_label(label))
                .map_or_else(|| config.default_agent.model.clone(), String::from);
            (model, 1u32)
        }
        SignalDecision::Escalate => (config.escalation.model.clone(), 2u32),
        SignalDecision::Skip(reason) => {
            return Disposition::Skip {
                reason: reason.to_string(),
            };
        }
    };

    let base_timeout_secs = config.default_agent.timeout_minutes * 60;
    let timeout_secs = if attempt > 1 {
        base_timeout_secs * u64::from(config.escalation.timeout_multiplier_pct) / 100
    } else {
        base_timeout_secs
    };

    match &signal.source {
        SourceKind::GitHub => match label {
            "agent-todo: replicate" => {
                let gh_num = signal
                    .metadata
                    .get("number")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);

                let description = build_replicate_prompt(gh_num, &signal.title, &signal.body);

                Disposition::Dispatch {
                    description,
                    scope: AgentScope {
                        allowed_paths: vec!["tests/".into()],
                        // Replicate uses the config default verify level (typically Local)
                        verify: config.default_agent.verify_level(),
                        timeout: Duration::from_secs(timeout_secs),
                        model,
                    },
                    attempt,
                }
            }
            "agent-todo: fix" => {
                let gh_num = signal
                    .metadata
                    .get("number")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);

                // Fix agents get more time: 60min base (vs 30min for replicate)
                let fix_timeout_secs = (config.default_agent.timeout_minutes * 60) * 2;
                let fix_timeout = if attempt > 1 {
                    fix_timeout_secs * u64::from(config.escalation.timeout_multiplier_pct) / 100
                } else {
                    fix_timeout_secs
                };

                let description = build_fix_prompt(gh_num, &signal.title, &signal.body);

                Disposition::Dispatch {
                    description,
                    scope: AgentScope {
                        allowed_paths: vec!["src/".into(), "tests/".into()],
                        // Fix dispatches always use Ci to push branch + open draft PR
                        verify: VerifyLevel::Ci,
                        timeout: Duration::from_secs(fix_timeout),
                        model,
                    },
                    attempt,
                }
            }
            other => Disposition::Skip {
                reason: format!("unrecognized agent-todo label: {other}"),
            },
        },
        SourceKind::Internal => {
            // Internal hygiene signals are triaged for human review, not auto-dispatched
            Disposition::Triage {
                priority: "low".into(),
                labels: vec!["hygiene".into()],
            }
        }
        SourceKind::CI => {
            // CI failures are triaged for human review with high priority
            Disposition::Triage {
                priority: "high".into(),
                labels: vec!["ci-failure".into()],
            }
        }
    }
}

fn truncate_body(body: &str, max_len: usize) -> String {
    if body.len() > max_len {
        format!("{}...\n\n(truncated)", &body[..max_len])
    } else {
        body.to_string()
    }
}

fn build_replicate_prompt(gh_issue_number: i64, title: &str, body: &str) -> String {
    let body_truncated = truncate_body(body, 4000);

    format!(
        "Reproduce the bug described in GitHub issue #{gh_issue_number}.

Title: {title}
Body:
{body_truncated}

Your task:
1. Read the issue carefully and understand the expected vs actual behavior
2. Explore the codebase to find the relevant code paths
3. Write a failing test that demonstrates the bug
4. Run the test suite to confirm your test fails for the right reason
5. Record your findings as a crosslink comment (--kind observation)
6. If you cannot reproduce, explain why (--kind resolution)

Constraints:
- You may ONLY create or modify files in tests/ directories
- Do NOT fix the bug — only reproduce it
- Do NOT push code or create PRs
- Time limit: 30 minutes"
    )
}

fn build_fix_prompt(gh_issue_number: i64, title: &str, body: &str) -> String {
    let body_truncated = truncate_body(body, 4000);

    format!(
        "Fix the bug described in GitHub issue #{gh_issue_number}.

Title: {title}
Body:
{body_truncated}

Your task:
1. Read the issue carefully and understand the expected vs actual behavior
2. Explore the codebase to find the root cause
3. Write a failing test that demonstrates the bug
4. Implement the fix
5. Run the full test suite to confirm the fix works and nothing else breaks
6. Record your findings as a crosslink comment (--kind resolution)
7. Push your branch and open a draft PR linking GH#{gh_issue_number}

Draft PR title: fix: {title} (sentinel GH#{gh_issue_number})

Constraints:
- You may modify files in src/ and tests/
- Push your branch when tests pass
- Open a DRAFT PR (not ready for review) — a human will review it
- Time limit: 60 minutes"
    )
}
