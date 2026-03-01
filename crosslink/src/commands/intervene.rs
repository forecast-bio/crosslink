use anyhow::{bail, Result};
use std::path::Path;

use crate::db::Database;
use crate::identity::resolve_driver_fingerprint;
use crate::issue_file::validate_trigger_type;
use crate::shared_writer::SharedWriter;
use crate::utils::format_issue_id;

/// Check if intervention tracking is enabled in hook-config.json.
fn is_intervention_tracking_enabled(crosslink_dir: &Path) -> bool {
    let config_path = crosslink_dir.join("hook-config.json");
    let content = match std::fs::read_to_string(&config_path) {
        Ok(c) => c,
        Err(_) => return true, // default: enabled
    };
    let parsed: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return true,
    };
    parsed
        .get("intervention_tracking")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

pub fn run(
    db: &Database,
    writer: Option<&SharedWriter>,
    issue_id: i64,
    description: &str,
    trigger_type: &str,
    context: Option<&str>,
    crosslink_dir: &Path,
) -> Result<()> {
    if !is_intervention_tracking_enabled(crosslink_dir) {
        println!("Intervention tracking is disabled in hook-config.json. Skipping.");
        return Ok(());
    }

    if !validate_trigger_type(trigger_type) {
        bail!(
            "Unknown trigger type '{}'. Valid types: tool_rejected, tool_blocked, redirect, context_provided, manual_action, question_answered",
            trigger_type
        );
    }

    db.require_issue(issue_id)?;

    let driver_fp = resolve_driver_fingerprint(crosslink_dir);
    let driver_fp_ref = driver_fp.as_deref();

    if let Some(w) = writer {
        w.add_intervention_comment(
            db,
            issue_id,
            description,
            trigger_type,
            context,
            driver_fp_ref,
        )?;
    } else {
        db.add_intervention_comment(issue_id, description, trigger_type, context, driver_fp_ref)?;
    }

    if let Some(ref fp) = driver_fp {
        println!(
            "Logged intervention on issue {} [{}] (driver: {})",
            format_issue_id(issue_id),
            trigger_type,
            fp
        );
    } else {
        println!(
            "Logged intervention on issue {} [{}]",
            format_issue_id(issue_id),
            trigger_type
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_intervene_creates_intervention_comment() {
        let (db, dir) = setup_db();
        let id = db.create_issue("Test", None, "medium").unwrap();

        let crosslink_dir = dir.path();
        run(
            &db,
            None,
            id,
            "Blocked: git push",
            "tool_blocked",
            Some("pushing feature branch"),
            crosslink_dir,
        )
        .unwrap();

        let comments = db.get_comments(id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].kind, "intervention");
        assert_eq!(comments[0].trigger_type.as_deref(), Some("tool_blocked"));
        assert_eq!(
            comments[0].intervention_context.as_deref(),
            Some("pushing feature branch")
        );
        assert_eq!(comments[0].content, "Blocked: git push");
    }

    #[test]
    fn test_intervene_without_context() {
        let (db, dir) = setup_db();
        let id = db.create_issue("Test", None, "medium").unwrap();

        run(
            &db,
            None,
            id,
            "Driver redirected approach",
            "redirect",
            None,
            dir.path(),
        )
        .unwrap();

        let comments = db.get_comments(id).unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].trigger_type.as_deref(), Some("redirect"));
        assert!(comments[0].intervention_context.is_none());
    }

    #[test]
    fn test_intervene_invalid_trigger_type() {
        let (db, dir) = setup_db();
        let id = db.create_issue("Test", None, "medium").unwrap();

        let result = run(&db, None, id, "test", "invalid_type", None, dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown trigger type"));
    }

    #[test]
    fn test_intervene_nonexistent_issue() {
        let (db, dir) = setup_db();

        let result = run(&db, None, 99999, "test", "tool_blocked", None, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn test_intervene_disabled_by_config() {
        let (db, dir) = setup_db();
        let id = db.create_issue("Test", None, "medium").unwrap();

        // Write config that disables intervention tracking
        let config = r#"{"tracking_mode":"strict","intervention_tracking":false}"#;
        std::fs::write(dir.path().join("hook-config.json"), config).unwrap();

        run(&db, None, id, "test", "tool_blocked", None, dir.path()).unwrap();

        // No comment should be created
        let comments = db.get_comments(id).unwrap();
        assert!(comments.is_empty());
    }

    #[test]
    fn test_all_trigger_types_accepted() {
        let (db, dir) = setup_db();
        let id = db.create_issue("Test", None, "medium").unwrap();

        for trigger in &[
            "tool_rejected",
            "tool_blocked",
            "redirect",
            "context_provided",
            "manual_action",
            "question_answered",
        ] {
            run(
                &db,
                None,
                id,
                &format!("Test {}", trigger),
                trigger,
                None,
                dir.path(),
            )
            .unwrap();
        }

        let comments = db.get_comments(id).unwrap();
        assert_eq!(comments.len(), 6);
    }
}
