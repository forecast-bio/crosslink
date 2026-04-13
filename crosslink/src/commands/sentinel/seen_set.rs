use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::db::Database;

use super::config::SentinelConfig;
use super::sources::SignalDecision;

/// Record of the most recent dispatch for a signal, used for dedup decisions.
struct SeenRecord {
    outcome: String,
    attempt_number: i32,
    completed_at: Option<DateTime<Utc>>,
}

/// In-memory dedup cache loaded from `sentinel_dispatches`.
///
/// Determines whether a signal should be dispatched (New), retried with
/// escalation (Escalate), or skipped entirely (Skip).
pub struct SeenSet {
    /// `signal_ref` -> most recent dispatch record
    seen: HashMap<String, SeenRecord>,
}

impl SeenSet {
    /// Load the `SeenSet` from the database. Takes the most recent dispatch
    /// per `signal_ref`.
    pub fn load(db: &Database) -> Result<Self> {
        let dispatches = db.load_dispatch_seen_set()?;
        let mut seen = HashMap::with_capacity(dispatches.len());

        for d in dispatches {
            let completed_at = d.completed_at.as_ref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            });
            seen.insert(
                d.signal_ref.clone(),
                SeenRecord {
                    outcome: d.outcome,
                    attempt_number: d.attempt_number,
                    completed_at,
                },
            );
        }

        Ok(Self { seen })
    }

    /// Evaluate whether a signal should be dispatched, escalated, or skipped.
    pub fn evaluate(&self, signal_ref: &str, config: &SentinelConfig) -> SignalDecision {
        let Some(record) = self.seen.get(signal_ref) else {
            return SignalDecision::New;
        };

        match record.outcome.as_str() {
            "pending" => SignalDecision::Skip("agent in-flight"),
            "success" => SignalDecision::Skip("already resolved"),
            "exhausted" => SignalDecision::Skip("both attempts failed"),

            "failure" | "timeout" | "orphaned" => Self::evaluate_retry(record, config),

            _ => SignalDecision::Skip("unknown state"),
        }
    }

    /// Check if a failed/orphaned dispatch is eligible for escalation retry.
    fn evaluate_retry(record: &SeenRecord, config: &SentinelConfig) -> SignalDecision {
        if !config.escalation.enabled {
            return SignalDecision::Skip("escalation disabled");
        }
        if record.attempt_number >= config.escalation.max_attempts as i32 {
            return SignalDecision::Skip("max attempts reached");
        }
        if let Some(completed) = &record.completed_at {
            let elapsed = Utc::now().signed_duration_since(*completed);
            if elapsed.num_minutes() < config.escalation.cooldown_minutes as i64 {
                return SignalDecision::Skip("cooldown not elapsed");
            }
        }
        SignalDecision::Escalate
    }
}

/// Layer 3: Authoritative database dedup check.
///
/// Even if the in-memory `SeenSet` is stale (e.g., sentinel restarted mid-cycle),
/// this check prevents duplicate dispatches.
pub fn db_dedup_check(
    db: &Database,
    gh_issue_number: i64,
    label: &str,
    config: &SentinelConfig,
) -> Result<SignalDecision> {
    let Some(dispatch) = db.get_latest_dispatch_for_signal(gh_issue_number, label)? else {
        return Ok(SignalDecision::New);
    };

    // Mirror the SeenSet logic against the authoritative DB record
    match dispatch.outcome.as_str() {
        "pending" => Ok(SignalDecision::Skip("agent in-flight (db)")),
        "success" => Ok(SignalDecision::Skip("already resolved (db)")),
        "exhausted" => Ok(SignalDecision::Skip("both attempts failed (db)")),
        "failure" | "timeout" | "orphaned" => {
            if !config.escalation.enabled {
                return Ok(SignalDecision::Skip("escalation disabled"));
            }
            if dispatch.attempt_number >= config.escalation.max_attempts as i32 {
                return Ok(SignalDecision::Skip("max attempts reached (db)"));
            }
            let cooldown_ok = dispatch
                .completed_at
                .as_ref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .is_none_or(|dt| {
                    let elapsed = Utc::now().signed_duration_since(dt.with_timezone(&Utc));
                    elapsed.num_minutes() >= config.escalation.cooldown_minutes as i64
                });
            if cooldown_ok {
                Ok(SignalDecision::Escalate)
            } else {
                Ok(SignalDecision::Skip("cooldown not elapsed (db)"))
            }
        }
        _ => Ok(SignalDecision::Skip("unknown state (db)")),
    }
}

/// Layer 4: GitHub comment dedup check.
///
/// Before posting a result comment, verify we haven't already posted for this
/// dispatch ID. Checks for the marker string `sentinel #<dispatch-id>` in
/// existing comments.
pub fn gh_comment_already_posted(gh_issue_number: i64, dispatch_id: i64) -> bool {
    let marker = format!("sentinel #{dispatch_id}");
    let output = std::process::Command::new("gh")
        .args([
            "issue",
            "view",
            &gh_issue_number.to_string(),
            "--json",
            "comments",
            "--jq",
            ".comments[].body",
        ])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout.contains(&marker)
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(
                "gh comment dedup check failed for GH#{}: {}",
                gh_issue_number,
                stderr.trim()
            );
            // On failure, assume not posted (proceed cautiously)
            false
        }
        Err(e) => {
            tracing::warn!("gh command failed for comment dedup: {e}");
            false
        }
    }
}

/// Extract the GH issue number from a signal reference like "GH#499:replicate".
pub fn parse_gh_issue_number(signal_ref: &str) -> Option<i64> {
    let rest = signal_ref.strip_prefix("GH#")?;
    let num_str = rest.split(':').next()?;
    num_str.parse().ok()
}

/// Extract the label from a signal reference like "GH#499:replicate" -> "replicate".
pub fn parse_signal_label_suffix(signal_ref: &str) -> Option<&str> {
    signal_ref.split(':').nth(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_gh_issue_number() {
        assert_eq!(parse_gh_issue_number("GH#499:replicate"), Some(499));
        assert_eq!(parse_gh_issue_number("GH#1:fix"), Some(1));
        assert_eq!(parse_gh_issue_number("GH#0:replicate"), Some(0));
        assert_eq!(parse_gh_issue_number("not-a-signal"), None);
        assert_eq!(parse_gh_issue_number("GH#abc:fix"), None);
        assert_eq!(parse_gh_issue_number(""), None);
    }

    #[test]
    fn test_parse_signal_label_suffix() {
        assert_eq!(
            parse_signal_label_suffix("GH#499:replicate"),
            Some("replicate")
        );
        assert_eq!(parse_signal_label_suffix("GH#1:fix"), Some("fix"));
        assert_eq!(parse_signal_label_suffix("no-colon"), None);
    }

    #[test]
    fn test_seen_set_new_signal() {
        let seen = SeenSet {
            seen: HashMap::new(),
        };
        let config = SentinelConfig::default();
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::New
        );
    }

    #[test]
    fn test_seen_set_pending_skips() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "pending".to_string(),
                attempt_number: 1,
                completed_at: None,
            },
        );
        let seen = SeenSet { seen: seen_map };
        let config = SentinelConfig::default();
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::Skip("agent in-flight")
        );
    }

    #[test]
    fn test_seen_set_success_skips() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "success".to_string(),
                attempt_number: 1,
                completed_at: Some(Utc::now()),
            },
        );
        let seen = SeenSet { seen: seen_map };
        let config = SentinelConfig::default();
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::Skip("already resolved")
        );
    }

    #[test]
    fn test_seen_set_failure_with_cooldown_elapsed_escalates() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "failure".to_string(),
                attempt_number: 1,
                // Completed 2 hours ago — well past 30min cooldown
                completed_at: Some(Utc::now() - chrono::Duration::hours(2)),
            },
        );
        let seen = SeenSet { seen: seen_map };
        let config = SentinelConfig::default();
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::Escalate
        );
    }

    #[test]
    fn test_seen_set_failure_within_cooldown_skips() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "failure".to_string(),
                attempt_number: 1,
                // Completed 5 minutes ago — within 30min cooldown
                completed_at: Some(Utc::now() - chrono::Duration::minutes(5)),
            },
        );
        let seen = SeenSet { seen: seen_map };
        let config = SentinelConfig::default();
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::Skip("cooldown not elapsed")
        );
    }

    #[test]
    fn test_seen_set_max_attempts_skips() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "failure".to_string(),
                attempt_number: 2, // max_attempts = 2
                completed_at: Some(Utc::now() - chrono::Duration::hours(2)),
            },
        );
        let seen = SeenSet { seen: seen_map };
        let config = SentinelConfig::default();
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::Skip("max attempts reached")
        );
    }

    #[test]
    fn test_seen_set_exhausted_skips() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "exhausted".to_string(),
                attempt_number: 2,
                completed_at: Some(Utc::now()),
            },
        );
        let seen = SeenSet { seen: seen_map };
        let config = SentinelConfig::default();
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::Skip("both attempts failed")
        );
    }

    #[test]
    fn test_seen_set_orphaned_escalates() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "orphaned".to_string(),
                attempt_number: 1,
                completed_at: Some(Utc::now() - chrono::Duration::hours(1)),
            },
        );
        let seen = SeenSet { seen: seen_map };
        let config = SentinelConfig::default();
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::Escalate
        );
    }

    #[test]
    fn test_seen_set_escalation_disabled_skips() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "failure".to_string(),
                attempt_number: 1,
                completed_at: Some(Utc::now() - chrono::Duration::hours(2)),
            },
        );
        let seen = SeenSet { seen: seen_map };
        let mut config = SentinelConfig::default();
        config.escalation.enabled = false;
        assert_eq!(
            seen.evaluate("GH#499:replicate", &config),
            SignalDecision::Skip("escalation disabled")
        );
    }

    #[test]
    fn test_different_labels_independent() {
        let mut seen_map = HashMap::new();
        seen_map.insert(
            "GH#499:replicate".to_string(),
            SeenRecord {
                outcome: "success".to_string(),
                attempt_number: 1,
                completed_at: Some(Utc::now()),
            },
        );
        let seen = SeenSet { seen: seen_map };
        let config = SentinelConfig::default();
        // Same issue, different label = new signal
        assert_eq!(seen.evaluate("GH#499:fix", &config), SignalDecision::New);
    }
}
