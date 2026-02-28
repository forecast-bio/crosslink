use anyhow::{bail, Result};

use crate::db::Database;
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
pub struct CreateOpts<'a> {
    pub labels: &'a [String],
    pub work: bool,
    pub quiet: bool,
    /// If set, lock enforcement is checked when --work is used.
    pub crosslink_dir: Option<&'a std::path::Path>,
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

        // Template priority is default, user can override
        let priority = if priority != "medium" {
            priority
        } else {
            tmpl.priority
        };

        // Combine template description prefix with user description
        let desc = match (tmpl.description_prefix, description) {
            (Some(prefix), Some(user_desc)) => Some(format!("{}\n\n{}", prefix, user_desc)),
            (Some(prefix), None) => Some(prefix.to_string()),
            (None, user_desc) => user_desc.map(|s| s.to_string()),
        };

        (priority.to_string(), desc, Some(tmpl.label))
    } else {
        (
            priority.to_string(),
            description.map(|s| s.to_string()),
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
        w.create_issue(db, title, final_description.as_deref(), &final_priority)?
    } else {
        db.create_issue(title, final_description.as_deref(), &final_priority)?
    };

    // Auto-add label from template
    if let Some(lbl) = template_label {
        if let Some(w) = writer {
            w.add_label(db, id, lbl)?;
        } else {
            db.add_label(id, lbl)?;
        }
    }

    // Add user-specified labels
    for lbl in opts.labels {
        if let Some(w) = writer {
            w.add_label(db, id, lbl)?;
        } else {
            db.add_label(id, lbl)?;
        }
    }

    if opts.quiet {
        println!("{}", id);
    } else {
        println!("Created issue {}", format_issue_id(id));
        if let Some(tmpl) = template {
            println!("  Applied template: {}", tmpl);
        }
    }

    // Set as active session work item
    if opts.work {
        // Check lock status before allowing work on this issue
        if let Some(dir) = opts.crosslink_dir {
            crate::lock_check::enforce_lock(dir, id)?;

            // Auto-claim lock in multi-agent mode (same as session work)
            if let Ok(Some(agent)) = crate::identity::AgentConfig::load(dir) {
                if let Ok(sync) = crate::sync::SyncManager::new(dir) {
                    if sync.is_initialized() {
                        match sync.claim_lock(&agent, id, None, false) {
                            Ok(true) => {
                                if !opts.quiet {
                                    println!("Auto-claimed lock on issue {}", format_issue_id(id));
                                }
                            }
                            Ok(false) => {} // Already held by us
                            Err(e) => eprintln!("Warning: Could not auto-claim lock: {}", e),
                        }
                    }
                }
            }
        }
        if let Ok(Some(session)) = db.get_current_session() {
            db.set_session_issue(session.id, id)?;
            if !opts.quiet {
                println!("Now working on: {} {}", format_issue_id(id), title);
            }
        } else if !opts.quiet {
            eprintln!("Warning: --work specified but no active session");
        }
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
        w.create_subissue(db, parent_id, title, description, priority)?
    } else {
        db.create_subissue(parent_id, title, description, priority)?
    };

    // Add user-specified labels
    for lbl in opts.labels {
        if let Some(w) = writer {
            w.add_label(db, id, lbl)?;
        } else {
            db.add_label(id, lbl)?;
        }
    }

    if opts.quiet {
        println!("{}", id);
    } else {
        println!(
            "Created subissue {} under {}",
            format_issue_id(id),
            format_issue_id(parent_id)
        );
    }

    // Set as active session work item
    if opts.work {
        // Check lock status before allowing work on this issue
        if let Some(dir) = opts.crosslink_dir {
            crate::lock_check::enforce_lock(dir, id)?;

            // Auto-claim lock in multi-agent mode (same as session work)
            if let Ok(Some(agent)) = crate::identity::AgentConfig::load(dir) {
                if let Ok(sync) = crate::sync::SyncManager::new(dir) {
                    if sync.is_initialized() {
                        match sync.claim_lock(&agent, id, None, false) {
                            Ok(true) => {
                                if !opts.quiet {
                                    println!("Auto-claimed lock on issue {}", format_issue_id(id));
                                }
                            }
                            Ok(false) => {} // Already held by us
                            Err(e) => eprintln!("Warning: Could not auto-claim lock: {}", e),
                        }
                    }
                }
            }
        }
        if let Ok(Some(session)) = db.get_current_session() {
            db.set_session_issue(session.id, id)?;
            if !opts.quiet {
                println!("Now working on: {} {}", format_issue_id(id), title);
            }
        } else if !opts.quiet {
            eprintln!("Warning: --work specified but no active session");
        }
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
}
