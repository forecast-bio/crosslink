//! Alert derivation from a [`super::reader::HubSnapshot`].
//!
//! For each tracked project the poll loop reads a snapshot, derives the
//! list of currently-true alerts from it, and reconciles that list
//! against the `alerts` table (opening new ones, resolving ones that
//! are no longer derived). See `DESIGN-CROSSLINK-DASHBOARD.md` §11.
//!
//! This module is pure: given a snapshot and some thresholds, it
//! returns the set of alerts that should be open right now. DB sync
//! lives in [`super::alerts_db`] (added in P1.6.B).
//!
//! Coverage:
//!
//! | Kind                  | Derived when                                | Severity |
//! |-----------------------|---------------------------------------------|----------|
//! | `stale_lock`          | Lock held longer than stale window          | warning  |
//! | `silent_agent`        | Agent holding a lock + heartbeat silent     | critical |
//! | `overdue_issue`       | Open issue with `due_at < now`              | warning  |
//! | `orphan_subissue`     | Closed parent with open subissues           | info     |
//! | `unreachable_project` | `project.status == "error"`                 | warning  |
//! | `ci_failure`          | `meta/ci-status.json.state == "failing"`    | warning  |
//! | `signature_invalid`   | Hub-tip commit signature verification failed| critical |
//!
//! Catalogue items that depend on telemetry crosslink doesn't yet
//! collect (`hub_diverged`, `hub_parse_error`, `untrusted_signer`,
//! `pending_request`, `compaction_lag`) remain deferred until the
//! respective signals land in `HubSnapshot`.

use chrono::{DateTime, Utc};
use std::collections::HashSet;
use uuid::Uuid;

use super::projects::Project;
use super::reader::HubSnapshot;

/// Severity bucket. Maps onto the `severity` text column on the
/// `alerts` table (and the tile colour classes on the frontend).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

impl Severity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// A single alert the reader believes is currently true.
///
/// `subject_ref` identifies the entity the alert is *about* — e.g.
/// `"lock:42"`, `"agent:jus4"`, `"issue:17"`. Together with `kind`
/// it's the identity key the DB reconciler uses to decide whether a
/// derived alert is "the same" as an already-open row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DerivedAlert {
    pub kind: &'static str,
    pub severity: Severity,
    pub subject_ref: String,
    pub detail: String,
}

/// Default thresholds, in minutes. `Project` / DB config could
/// override these later (out of scope for MVP).
pub const STALE_LOCK_MINUTES: i64 = 60;
pub const SILENT_AGENT_MINUTES: i64 = 10;

/// Walk the project row + snapshot and return every alert that should
/// be open right now. The result is order-independent; the DB sync
/// layer deduplicates by `(kind, subject_ref)`.
#[must_use]
pub fn derive_alerts(
    project: &Project,
    snapshot: &HubSnapshot,
    now: DateTime<Utc>,
) -> Vec<DerivedAlert> {
    let mut out = Vec::new();

    // Unreachable project — the poll loop marks project.status = "error"
    // when the latest fetch failed. No stale timestamp needed; the
    // alert resolves the next time we successfully fetch.
    if project.status == "error" {
        out.push(DerivedAlert {
            kind: "unreachable_project",
            severity: Severity::Warning,
            subject_ref: format!("project:{}", project.slug),
            detail: "dashboard could not fetch the hub branch".to_string(),
        });
    }

    // Stale locks + silent agents. A silent agent alert wins over a
    // stale lock alert when both apply to the same lock (the agent
    // holding it is unresponsive — critical — rather than the lock
    // just having aged — warning). We still emit both if applicable;
    // the frontend renders severity-sorted.
    let stale_window = chrono::Duration::minutes(STALE_LOCK_MINUTES);
    let silent_window = chrono::Duration::minutes(SILENT_AGENT_MINUTES);

    let silent_agents: HashSet<&str> = snapshot
        .agents
        .iter()
        .filter(|a| now - a.last_heartbeat > silent_window)
        .map(|a| a.agent_id.as_str())
        .collect();

    for record in &snapshot.locks {
        let age = now - record.lock.claimed_at;
        let is_stale = age > stale_window;
        if is_stale {
            out.push(DerivedAlert {
                kind: "stale_lock",
                severity: Severity::Warning,
                subject_ref: format!("lock:{}", record.issue_id),
                detail: format!(
                    "lock on issue #{} held by {} for {} minute(s)",
                    record.issue_id,
                    record.lock.agent_id,
                    age.num_minutes().max(0),
                ),
            });
        }
        // Silent-agent-while-holding-lock is a critical alert
        // because it usually means an agent process died mid-work.
        if silent_agents.contains(record.lock.agent_id.as_str()) {
            out.push(DerivedAlert {
                kind: "silent_agent",
                severity: Severity::Critical,
                subject_ref: format!("agent:{}", record.lock.agent_id),
                detail: format!(
                    "agent {} silent >{}m while holding lock on issue #{}",
                    record.lock.agent_id, SILENT_AGENT_MINUTES, record.issue_id,
                ),
            });
        }
    }

    // Overdue issues — one alert per issue so the frontend can link
    // directly. Info severity would be misleading here; overdue is a
    // real deadline miss.
    for issue in &snapshot.issues {
        if !matches!(issue.status, crate::models::IssueStatus::Open) {
            continue;
        }
        if let Some(due) = issue.due_at {
            if due < now {
                let label = issue
                    .display_id
                    .map_or_else(|| issue.uuid.to_string(), |d| format!("#{d}"));
                out.push(DerivedAlert {
                    kind: "overdue_issue",
                    severity: Severity::Warning,
                    subject_ref: format!(
                        "issue:{}",
                        issue
                            .display_id
                            .map_or_else(|| issue.uuid.to_string(), |d| d.to_string())
                    ),
                    detail: format!("{label} \"{}\" due {}", issue.title, due.to_rfc3339()),
                });
            }
        }
    }

    // CI failure — surfaces whatever the project's pipeline wrote
    // into `meta/ci-status.json` on the hub branch. Reader has
    // already filtered stale entries (sha mismatch).
    if let Some(ci) = &snapshot.ci_status {
        if ci.state == "failing" {
            let detail = ci.url.as_deref().map_or_else(
                || format!("CI failing on hub-tip ({})", ci.sha),
                |u| format!("CI failing on hub-tip ({}); {u}", ci.sha),
            );
            out.push(DerivedAlert {
                kind: "ci_failure",
                severity: Severity::Warning,
                subject_ref: format!("commit:{}", ci.sha),
                detail,
            });
        }
    }

    // Signature invalid on the hub-tip commit. Critical because it
    // means an unauthorized writer landed something on the shared
    // branch — operators should investigate immediately. We don't
    // alert on `Unsigned` (intentional unsigned setups exist) or
    // `Unknown` (verification path unavailable).
    if matches!(
        snapshot.signature_state,
        super::reader::SignatureState::Invalid
    ) {
        out.push(DerivedAlert {
            kind: "signature_invalid",
            severity: Severity::Critical,
            subject_ref: snapshot
                .hub_sha
                .as_deref()
                .map_or_else(|| "commit:unknown".to_string(), |s| format!("commit:{s}")),
            detail: "hub-tip commit signature failed verification".to_string(),
        });
    }

    // Orphan subissues — parent closed with open subissues. This is
    // a low-severity housekeeping signal, not an emergency.
    let by_uuid: std::collections::HashMap<Uuid, &crate::issue_file::IssueFile> =
        snapshot.issues.iter().map(|i| (i.uuid, i)).collect();
    for issue in &snapshot.issues {
        if !matches!(issue.status, crate::models::IssueStatus::Open) {
            continue;
        }
        let Some(parent_uuid) = issue.parent_uuid else {
            continue;
        };
        let Some(parent) = by_uuid.get(&parent_uuid) else {
            continue;
        };
        if matches!(parent.status, crate::models::IssueStatus::Closed) {
            let label = issue
                .display_id
                .map_or_else(|| issue.uuid.to_string(), |d| format!("#{d}"));
            let parent_label = parent
                .display_id
                .map_or_else(|| parent.uuid.to_string(), |d| format!("#{d}"));
            out.push(DerivedAlert {
                kind: "orphan_subissue",
                severity: Severity::Info,
                subject_ref: format!(
                    "issue:{}",
                    issue
                        .display_id
                        .map_or_else(|| issue.uuid.to_string(), |d| d.to_string())
                ),
                detail: format!("{label} is open but parent {parent_label} is closed"),
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::path::PathBuf;

    fn now_fixed() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap()
    }

    fn base_project() -> Project {
        Project {
            id: 1,
            slug: "forecast-bio/crosslink".into(),
            clone_path: PathBuf::from("/tmp/x"),
            default_branch: "main".into(),
            hub_sha: None,
            hub_fetched_at: None,
            status: "active".into(),
            added_at: "2026-04-20T00:00:00Z".into(),
            last_activity_at: None,
            pinned: false,
        }
    }

    fn empty_snapshot() -> HubSnapshot {
        HubSnapshot {
            hub_sha: None,
            layout_version: 2,
            issues: vec![],
            agents: vec![],
            locks: vec![],
            agent_requests: vec![],
            ci_status: None,
            signature_state: super::super::reader::SignatureState::Unknown,
            last_commit_at: None,
        }
    }

    fn make_issue(
        display_id: i64,
        status: crate::models::IssueStatus,
        due_at: Option<DateTime<Utc>>,
        parent_uuid: Option<Uuid>,
    ) -> crate::issue_file::IssueFile {
        crate::issue_file::IssueFile {
            uuid: Uuid::new_v4(),
            display_id: Some(display_id),
            title: format!("Issue {display_id}"),
            description: None,
            status,
            priority: crate::models::Priority::Medium,
            parent_uuid,
            created_by: "test".into(),
            created_at: now_fixed(),
            updated_at: now_fixed(),
            closed_at: None,
            scheduled_at: None,
            due_at,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        }
    }

    #[test]
    fn test_empty_snapshot_yields_no_alerts() {
        let alerts = derive_alerts(&base_project(), &empty_snapshot(), now_fixed());
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_unreachable_project_when_status_error() {
        let mut p = base_project();
        p.status = "error".into();
        let alerts = derive_alerts(&p, &empty_snapshot(), now_fixed());
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, "unreachable_project");
        assert_eq!(alerts[0].severity, Severity::Warning);
    }

    #[test]
    fn test_stale_lock_emits_warning() {
        let mut snap = empty_snapshot();
        snap.locks.push(super::super::reader::LockRecord {
            issue_id: 42,
            lock: crate::locks::Lock {
                agent_id: "jus4".into(),
                branch: None,
                claimed_at: now_fixed() - chrono::Duration::hours(2),
                signed_by: "SHA256:a".into(),
            },
        });
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].kind, "stale_lock");
        assert_eq!(alerts[0].subject_ref, "lock:42");
    }

    #[test]
    fn test_fresh_lock_does_not_alert() {
        let mut snap = empty_snapshot();
        snap.locks.push(super::super::reader::LockRecord {
            issue_id: 42,
            lock: crate::locks::Lock {
                agent_id: "jus4".into(),
                branch: None,
                claimed_at: now_fixed() - chrono::Duration::minutes(5),
                signed_by: "SHA256:a".into(),
            },
        });
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        assert!(alerts.is_empty());
    }

    #[test]
    fn test_silent_agent_holding_lock_is_critical() {
        let mut snap = empty_snapshot();
        snap.agents.push(crate::locks::Heartbeat {
            agent_id: "jus4".into(),
            last_heartbeat: now_fixed() - chrono::Duration::minutes(30),
            active_issue_id: Some(42),
            machine_id: "h".into(),
        });
        snap.locks.push(super::super::reader::LockRecord {
            issue_id: 42,
            lock: crate::locks::Lock {
                agent_id: "jus4".into(),
                branch: None,
                claimed_at: now_fixed() - chrono::Duration::minutes(5),
                signed_by: "SHA256:a".into(),
            },
        });
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        let silent = alerts.iter().find(|a| a.kind == "silent_agent");
        assert!(silent.is_some(), "expected silent_agent alert: {alerts:?}");
        assert_eq!(silent.unwrap().severity, Severity::Critical);
    }

    #[test]
    fn test_active_agent_does_not_trigger_silent() {
        let mut snap = empty_snapshot();
        snap.agents.push(crate::locks::Heartbeat {
            agent_id: "jus4".into(),
            last_heartbeat: now_fixed() - chrono::Duration::minutes(2),
            active_issue_id: Some(42),
            machine_id: "h".into(),
        });
        snap.locks.push(super::super::reader::LockRecord {
            issue_id: 42,
            lock: crate::locks::Lock {
                agent_id: "jus4".into(),
                branch: None,
                claimed_at: now_fixed() - chrono::Duration::minutes(5),
                signed_by: "SHA256:a".into(),
            },
        });
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        assert!(alerts.iter().all(|a| a.kind != "silent_agent"));
    }

    #[test]
    fn test_overdue_issue_emits_warning() {
        let mut snap = empty_snapshot();
        snap.issues.push(make_issue(
            17,
            crate::models::IssueStatus::Open,
            Some(now_fixed() - chrono::Duration::days(3)),
            None,
        ));
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        let overdue = alerts.iter().find(|a| a.kind == "overdue_issue");
        assert!(overdue.is_some());
        assert_eq!(overdue.unwrap().subject_ref, "issue:17");
    }

    #[test]
    fn test_closed_overdue_issue_does_not_alert() {
        let mut snap = empty_snapshot();
        snap.issues.push(make_issue(
            17,
            crate::models::IssueStatus::Closed,
            Some(now_fixed() - chrono::Duration::days(3)),
            None,
        ));
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        assert!(alerts.iter().all(|a| a.kind != "overdue_issue"));
    }

    #[test]
    fn test_orphan_subissue_emits_info() {
        let parent_uuid = Uuid::new_v4();
        let mut parent = make_issue(1, crate::models::IssueStatus::Closed, None, None);
        parent.uuid = parent_uuid;
        let child = make_issue(2, crate::models::IssueStatus::Open, None, Some(parent_uuid));

        let mut snap = empty_snapshot();
        snap.issues.push(parent);
        snap.issues.push(child);
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        let orphan = alerts.iter().find(|a| a.kind == "orphan_subissue");
        assert!(orphan.is_some(), "expected orphan_subissue alert");
        assert_eq!(orphan.unwrap().severity, Severity::Info);
    }

    #[test]
    fn test_open_parent_does_not_trigger_orphan() {
        let parent_uuid = Uuid::new_v4();
        let mut parent = make_issue(1, crate::models::IssueStatus::Open, None, None);
        parent.uuid = parent_uuid;
        let child = make_issue(2, crate::models::IssueStatus::Open, None, Some(parent_uuid));

        let mut snap = empty_snapshot();
        snap.issues.push(parent);
        snap.issues.push(child);
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        assert!(alerts.iter().all(|a| a.kind != "orphan_subissue"));
    }

    #[test]
    fn test_ci_failure_emits_warning() {
        let mut snap = empty_snapshot();
        snap.hub_sha = Some("abc1234".into());
        snap.ci_status = Some(super::super::reader::CiStatus {
            sha: "abc1234".into(),
            state: "failing".into(),
            url: Some("https://ci.example.com/run/42".into()),
        });
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        let ci = alerts
            .iter()
            .find(|a| a.kind == "ci_failure")
            .expect("ci_failure alert expected");
        assert_eq!(ci.severity, Severity::Warning);
        assert_eq!(ci.subject_ref, "commit:abc1234");
        assert!(ci.detail.contains("ci.example.com"));
    }

    #[test]
    fn test_ci_passing_does_not_alert() {
        let mut snap = empty_snapshot();
        snap.hub_sha = Some("abc1234".into());
        snap.ci_status = Some(super::super::reader::CiStatus {
            sha: "abc1234".into(),
            state: "passing".into(),
            url: None,
        });
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        assert!(alerts.iter().all(|a| a.kind != "ci_failure"));
    }

    #[test]
    fn test_signature_invalid_emits_critical() {
        let mut snap = empty_snapshot();
        snap.hub_sha = Some("deadbeef".into());
        snap.signature_state = super::super::reader::SignatureState::Invalid;
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        let sig = alerts
            .iter()
            .find(|a| a.kind == "signature_invalid")
            .expect("signature_invalid alert expected");
        assert_eq!(sig.severity, Severity::Critical);
        assert_eq!(sig.subject_ref, "commit:deadbeef");
    }

    #[test]
    fn test_signature_unknown_or_unsigned_does_not_alert() {
        let mut snap = empty_snapshot();
        snap.signature_state = super::super::reader::SignatureState::Unsigned;
        let alerts = derive_alerts(&base_project(), &snap, now_fixed());
        assert!(alerts.iter().all(|a| a.kind != "signature_invalid"));

        snap.signature_state = super::super::reader::SignatureState::Unknown;
        let alerts2 = derive_alerts(&base_project(), &snap, now_fixed());
        assert!(alerts2.iter().all(|a| a.kind != "signature_invalid"));
    }
}
