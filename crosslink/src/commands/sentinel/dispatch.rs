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
    #[allow(dead_code)]
    Triage {
        priority: String,
        labels: Vec<String>,
    },
    /// Already handled or no matching rule — skip.
    Skip { reason: String },
    /// Eligible but cannot dispatch right now.
    #[allow(dead_code)]
    Defer { reason: String },
}

/// Constrains what a dispatched agent can do.
#[derive(Debug, Clone)]
pub struct AgentScope {
    #[allow(dead_code)]
    pub allowed_paths: Vec<String>,
    pub verify: VerifyLevel,
    pub timeout: Duration,
    pub model: String,
}

/// Run a signal through the triage engine to determine its disposition.
pub fn triage(signal: &Signal, decision: &SignalDecision, config: &SentinelConfig) -> Disposition {
    let (model, attempt) = match decision {
        SignalDecision::New => (config.default_agent.model.clone(), 1u32),
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
        SourceKind::GitHub => {
            let label = signal
                .metadata
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            match label {
                "agent-todo: replicate" => {
                    let gh_num = signal
                        .metadata
                        .get("number")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);

                    let description = build_replicate_prompt(gh_num, &signal.title, &signal.body);

                    Disposition::Dispatch {
                        description,
                        scope: AgentScope {
                            allowed_paths: vec!["tests/".into()],
                            verify: VerifyLevel::Local,
                            timeout: Duration::from_secs(timeout_secs),
                            model,
                        },
                        attempt,
                    }
                }
                other => Disposition::Skip {
                    reason: format!("unrecognized agent-todo label: {other}"),
                },
            }
        }
        _ => Disposition::Skip {
            reason: "no matching rule for source".into(),
        },
    }
}

fn build_replicate_prompt(gh_issue_number: i64, title: &str, body: &str) -> String {
    // Truncate body to avoid blowing up the prompt
    let body_truncated = if body.len() > 4000 {
        format!("{}...\n\n(truncated)", &body[..4000])
    } else {
        body.to_string()
    };

    format!(
        r#"Reproduce the bug described in GitHub issue #{gh_issue_number}.

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
- Time limit: 30 minutes"#
    )
}
