use anyhow::{bail, Result};

use crate::db::Database;
use crate::lock_check::{release_lock_best_effort, try_claim_lock, ClaimResult};
use crate::shared_writer::SharedWriter;
use crate::utils::format_issue_id;

const VALID_PRIORITIES: [&str; 4] = ["low", "medium", "high", "critical"];

/// Built-in issue templates
pub struct Template {
    pub name: &'static str,
    pub priority: &'static str,
    pub label: &'static str,
    pub description_prefix: Option<&'static str>,
}

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "bug",
        priority: "high",
        label: "bug",
        description_prefix: Some("Steps to reproduce:\n1. \n\nExpected: \nActual: "),
    },
    Template {
        name: "feature",
        priority: "medium",
        label: "feature",
        description_prefix: Some("Goal: \n\nAcceptance criteria:\n- "),
    },
    Template {
        name: "refactor",
        priority: "low",
        label: "refactor",
        description_prefix: Some("Current state: \n\nDesired state: \n\nReason: "),
    },
    Template {
        name: "research",
        priority: "low",
        label: "research",
        description_prefix: Some("Question: \n\nContext: \n\nFindings: "),
    },
    Template {
        name: "audit",
        priority: "high",
        label: "audit",
        description_prefix: Some("Scope: \n\nFiles to review: \n\nFindings: \n\nSeverity: "),
    },
    Template {
        name: "continuation",
        priority: "high",
        label: "continuation",
        description_prefix: Some("Previous session: \n\nCompleted: \n\nRemaining: \n\nBlockers: "),
    },
    Template {
        name: "investigation",
        priority: "medium",
        label: "investigation",
        description_prefix: Some(
            "Symptom: \n\nReproduction: \n\nHypotheses: \n\nRoot cause: \n\nFix: ",
        ),
    },
];

pub fn get_template(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

pub fn list_templates() -> Vec<&'static str> {
    TEMPLATES.iter().map(|t| t.name).collect()
}

pub fn validate_priority(priority: &str) -> bool {
    VALID_PRIORITIES.contains(&priority)
}

/// Options shared by create and subissue commands.
/// Auto-claim lock in multi-agent mode and set the session work item.
/// Returns Ok(()) on success or propagates errors from lock enforcement.
/// Releases the lock if session update fails (avoids orphaned locks).
fn auto_claim_and_set_work(
    db: &Database,
    id: i64,
    title: &str,
    crosslink_dir: Option<&std::path::Path>,
    quiet: bool,
) -> Result<()> {
    let mut freshly_claimed = false;

    if let Some(dir) = crosslink_dir {
        crate::lock_check::enforce_lock(dir, id, db)?;

        match try_claim_lock(dir, id, None) {
            Ok(ClaimResult::Claimed) => {
                freshly_claimed = true;
                if !quiet {
                    println!("Auto-claimed lock on issue {}", format_issue_id(id));
                }
            }
            Ok(ClaimResult::AlreadyHeld | ClaimResult::NotConfigured) => {}
            Ok(ClaimResult::Contended { winner_agent_id }) => {
                tracing::warn!(
                    "Lock on {} won by '{}'",
                    format_issue_id(id),
                    winner_agent_id
                );
            }
            Err(e) => tracing::warn!("Could not auto-claim lock: {}", e),
        }
    }

    let agent_id = crosslink_dir.and_then(|dir| {
        crate::identity::AgentConfig::load(dir)
            .ok()
            .flatten()
            .map(|a| a.agent_id)
    });
    if let Ok(Some(session)) = db.get_current_session_for_agent(agent_id.as_deref()) {
        if let Err(e) = db.set_session_issue(session.id, id) {
            if freshly_claimed {
                if let Some(dir) = crosslink_dir {
                    release_lock_best_effort(dir, id);
                }
            }
            return Err(e);
        }
        // Write sentinel file for fast hook checks (#522)
        if let Some(dir) = crosslink_dir {
            crate::commands::session::write_active_issue_sentinel(dir, id);
        }
        if !quiet {
            println!("Now working on: {} {}", format_issue_id(id), title);
        }
    } else if !quiet {
        tracing::warn!("--work specified but no active session");
    }

    Ok(())
}

pub struct CreateOpts<'a> {
    pub labels: &'a [String],
    pub work: bool,
    pub quiet: bool,
    /// If set, lock enforcement is checked when --work is used.
    pub crosslink_dir: Option<&'a std::path::Path>,
    /// Skip compaction after creation (batch mode — display ID assigned on next compaction).
    pub defer_id: bool,
}

pub fn run(
    db: &Database,
    writer: Option<&SharedWriter>,
    title: &str,
    description: Option<&str>,
    priority: &str,
    template: Option<&str>,
    opts: &CreateOpts<'_>,
) -> Result<()> {
    // Apply template if specified
    let (final_priority, final_description, template_label) = if let Some(tmpl_name) = template {
        let tmpl = get_template(tmpl_name).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown template '{}'. Available: {}",
                tmpl_name,
                list_templates().join(", ")
            )
        })?;

        // Template priority is the default; user can override with any non-default value.
        // NOTE: This uses the CLI default ("medium") as a sentinel to detect "user didn't
        // specify priority". An explicit `--priority medium` is indistinguishable from the
        // default and will be overridden by the template's priority. To fix this fully,
        // the CLI would need `Option<String>` for priority (#449).
        let priority = if priority == "medium" {
            tmpl.priority
        } else {
            priority
        };

        // Combine template description prefix with user description
        let desc = match (tmpl.description_prefix, description) {
            (Some(prefix), Some(user_desc)) => Some(format!("{prefix}\n\n{user_desc}")),
            (Some(prefix), None) => Some(prefix.to_string()),
            (None, user_desc) => user_desc.map(ToString::to_string),
        };

        (priority.to_string(), desc, Some(tmpl.label))
    } else {
        (
            priority.to_string(),
            description.map(ToString::to_string),
            None,
        )
    };

    if !validate_priority(&final_priority) {
        bail!(
            "Invalid priority '{}'. Must be one of: {}",
            final_priority,
            VALID_PRIORITIES.join(", ")
        );
    }

    let id = if let Some(w) = writer {
        let id = w.create_issue(db, title, final_description.as_deref(), &final_priority)?;

        // Auto-add label from template
        if let Some(lbl) = template_label {
            w.add_label(db, id, lbl)?;
        }

        // Add user-specified labels
        for lbl in opts.labels {
            w.add_label(db, id, lbl)?;
        }

        id
    } else {
        // Wrap create + labels in a transaction so a label failure
        // doesn't leave an issue without its labels.
        db.transaction(|| {
            let id = db.create_issue(title, final_description.as_deref(), &final_priority)?;

            // Auto-add label from template
            if let Some(lbl) = template_label {
                db.add_label(id, lbl)?;
            }

            // Add user-specified labels
            for lbl in opts.labels {
                db.add_label(id, lbl)?;
            }

            Ok(id)
        })?
    };

    if opts.defer_id && !opts.quiet {
        println!(
            "Created issue {} (display ID deferred — assigned on next compaction)",
            format_issue_id(id)
        );
    } else if opts.quiet {
        println!("{id}");
    } else {
        println!("Created issue {}", format_issue_id(id));
        if let Some(tmpl) = template {
            println!("  Applied template: {tmpl}");
        }
    }

    // Set as active session work item
    if opts.work {
        auto_claim_and_set_work(db, id, title, opts.crosslink_dir, opts.quiet)?;
    }

    Ok(())
}

pub fn run_subissue(
    db: &Database,
    writer: Option<&SharedWriter>,
    parent_id: i64,
    title: &str,
    description: Option<&str>,
    priority: &str,
    opts: &CreateOpts<'_>,
) -> Result<()> {
    if !validate_priority(priority) {
        bail!(
            "Invalid priority '{}'. Must be one of: {}",
            priority,
            VALID_PRIORITIES.join(", ")
        );
    }

    // Verify parent exists
    let parent = db.get_issue(parent_id)?;
    if parent.is_none() {
        bail!("Parent issue {} not found", format_issue_id(parent_id));
    }

    let id = if let Some(w) = writer {
        let id = w.create_subissue(db, parent_id, title, description, priority)?;

        // Add user-specified labels
        for lbl in opts.labels {
            w.add_label(db, id, lbl)?;
        }

        id
    } else {
        // Wrap create + labels in a transaction so a label failure
        // doesn't leave a subissue without its labels.
        db.transaction(|| {
            let id = db.create_subissue(parent_id, title, description, priority)?;

            // Add user-specified labels
            for lbl in opts.labels {
                db.add_label(id, lbl)?;
            }

            Ok(id)
        })?
    };

    if opts.quiet {
        println!("{id}");
    } else {
        println!(
            "Created subissue {} under {}",
            format_issue_id(id),
            format_issue_id(parent_id)
        );
    }

    // Set as active session work item
    if opts.work {
        auto_claim_and_set_work(db, id, title, opts.crosslink_dir, opts.quiet)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ==================== Unit Tests ====================

    #[test]
    fn test_validate_priority_valid() {
        assert!(validate_priority("low"));
        assert!(validate_priority("medium"));
        assert!(validate_priority("high"));
        assert!(validate_priority("critical"));
    }

    #[test]
    fn test_validate_priority_invalid() {
        assert!(!validate_priority(""));
        assert!(!validate_priority("urgent"));
        assert!(!validate_priority("LOW")); // Case sensitive
        assert!(!validate_priority("MEDIUM"));
        assert!(!validate_priority("High"));
        assert!(!validate_priority("CRITICAL"));
        assert!(!validate_priority(" medium"));
        assert!(!validate_priority("medium "));
        assert!(!validate_priority("medium\n"));
    }

    #[test]
    fn test_validate_priority_malicious() {
        // Security: ensure no injection vectors
        assert!(!validate_priority("'; DROP TABLE issues; --"));
        assert!(!validate_priority("high\0medium"));
        assert!(!validate_priority("medium; DELETE FROM issues"));
        assert!(!validate_priority("<script>alert('xss')</script>"));
    }

    #[test]
    fn test_get_template_exists() {
        let bug = get_template("bug");
        assert!(bug.is_some());
        let template = bug.unwrap();
        assert_eq!(template.name, "bug");
        assert_eq!(template.priority, "high");
        assert_eq!(template.label, "bug");
        assert!(template.description_prefix.is_some());
    }

    #[test]
    fn test_get_template_not_found() {
        assert!(get_template("nonexistent").is_none());
        assert!(get_template("").is_none());
        assert!(get_template("Bug").is_none()); // Case sensitive
        assert!(get_template("BUG").is_none());
    }

    #[test]
    fn test_list_templates() {
        let templates = list_templates();
        assert!(templates.contains(&"bug"));
        assert!(templates.contains(&"feature"));
        assert!(templates.contains(&"refactor"));
        assert!(templates.contains(&"research"));
        assert!(templates.contains(&"audit"));
        assert!(templates.contains(&"continuation"));
        assert!(templates.contains(&"investigation"));
        assert_eq!(templates.len(), 7);
    }

    #[test]
    fn test_template_fields() {
        // Verify all templates have required fields
        for template in TEMPLATES {
            assert!(!template.name.is_empty());
            assert!(validate_priority(template.priority));
            assert!(!template.label.is_empty());
        }
    }

    #[test]
    fn test_template_bug_description_prefix() {
        let template = get_template("bug").unwrap();
        let prefix = template.description_prefix.unwrap();
        assert!(prefix.contains("Steps to reproduce"));
        assert!(prefix.contains("Expected"));
        assert!(prefix.contains("Actual"));
    }

    #[test]
    fn test_template_feature_description_prefix() {
        let template = get_template("feature").unwrap();
        let prefix = template.description_prefix.unwrap();
        assert!(prefix.contains("Goal"));
        assert!(prefix.contains("Acceptance criteria"));
    }

    // ==================== Property-Based Tests ====================

    proptest! {
        #[test]
        fn prop_invalid_priorities_never_validate(
            priority in "[a-zA-Z]{1,20}"
                .prop_filter("Exclude valid priorities", |s| {
                    !["low", "medium", "high", "critical"].contains(&s.as_str())
                })
        ) {
            prop_assert!(!validate_priority(&priority));
        }

        #[test]
        fn prop_unknown_template_returns_none(name in "[a-zA-Z]{5,20}"
            .prop_filter("Exclude known templates", |s| {
                !["bug", "feature", "refactor", "research", "audit", "continuation", "investigation"].contains(&s.as_str())
            })
        ) {
            prop_assert!(get_template(&name).is_none());
        }
    }

    // ==================== Integration Tests (#450) ====================

    fn setup_test_db() -> (crate::db::Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = crate::db::Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_run_creates_issue() {
        let (db, _dir) = setup_test_db();
        let opts = CreateOpts {
            labels: &[],
            work: false,
            quiet: false,
            crosslink_dir: None,
            defer_id: false,
        };
        run(&db, None, "Test issue", None, "medium", None, &opts).unwrap();
        let issues = db.list_issues(Some("all"), None, None).unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].title, "Test issue");
    }

    #[test]
    fn test_run_with_template_applies_label() {
        let (db, _dir) = setup_test_db();
        let opts = CreateOpts {
            labels: &[],
            work: false,
            quiet: false,
            crosslink_dir: None,
            defer_id: false,
        };
        run(&db, None, "A bug", None, "medium", Some("bug"), &opts).unwrap();
        let issues = db.list_issues(Some("all"), None, None).unwrap();
        assert_eq!(issues.len(), 1);
        let labels = db.get_labels(issues[0].id).unwrap();
        assert!(labels.contains(&"bug".to_string()));
    }

    #[test]
    fn test_run_with_user_labels() {
        let (db, _dir) = setup_test_db();
        let labels = vec!["urgent".to_string(), "backend".to_string()];
        let opts = CreateOpts {
            labels: &labels,
            work: false,
            quiet: false,
            crosslink_dir: None,
            defer_id: false,
        };
        run(&db, None, "Labeled issue", None, "high", None, &opts).unwrap();
        let issues = db.list_issues(Some("all"), None, None).unwrap();
        let issue_labels = db.get_labels(issues[0].id).unwrap();
        assert_eq!(issue_labels.len(), 2);
        assert!(issue_labels.contains(&"urgent".to_string()));
        assert!(issue_labels.contains(&"backend".to_string()));
    }

    #[test]
    fn test_run_subissue_creates_child() {
        let (db, _dir) = setup_test_db();
        let parent_id = db.create_issue("Parent", None, "high").unwrap();
        let opts = CreateOpts {
            labels: &[],
            work: false,
            quiet: false,
            crosslink_dir: None,
            defer_id: false,
        };
        run_subissue(&db, None, parent_id, "Child task", None, "medium", &opts).unwrap();
        let subs = db.get_subissues(parent_id).unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].title, "Child task");
    }

    #[test]
    fn test_run_invalid_priority_fails() {
        let (db, _dir) = setup_test_db();
        let opts = CreateOpts {
            labels: &[],
            work: false,
            quiet: false,
            crosslink_dir: None,
            defer_id: false,
        };
        let result = run(&db, None, "Bad priority", None, "urgent", None, &opts);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid priority"));
    }
}
