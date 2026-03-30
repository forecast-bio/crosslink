use anyhow::{bail, Result};
use chrono::Utc;
use std::path::Path;

use crate::db::Database;
use crate::lock_check::{release_lock_best_effort, try_claim_lock, try_release_lock, ClaimResult};
use crate::utils::format_issue_id;
use crate::SessionCommands;

pub fn run(
    command: SessionCommands,
    db: &Database,
    crosslink_dir: &Path,
    json: bool,
) -> Result<()> {
    match command {
        SessionCommands::Start => start(db, crosslink_dir),
        SessionCommands::End { notes } => end(db, notes.as_deref(), crosslink_dir),
        SessionCommands::Status => status(db, crosslink_dir, json),
        SessionCommands::Work { id } => work(db, id, crosslink_dir),
        SessionCommands::LastHandoff => last_handoff(db, crosslink_dir),
        SessionCommands::Action { text } => action(db, &text, crosslink_dir),
    }
}

/// Load the current `agent_id` from `.crosslink/agent.json` (best-effort).
fn load_agent_id(crosslink_dir: &std::path::Path) -> Option<String> {
    crate::identity::AgentConfig::load(crosslink_dir)
        .ok()
        .flatten()
        .map(|a| a.agent_id)
}

pub fn start(db: &Database, crosslink_dir: &std::path::Path) -> Result<()> {
    let agent_id = load_agent_id(crosslink_dir);

    // Check if there's already an active session for this agent
    if let Some(current) = db.get_current_session_for_agent(agent_id.as_deref())? {
        println!(
            "Session #{} is already active (started {})",
            current.id,
            current.started_at.format("%Y-%m-%d %H:%M")
        );
        return Ok(());
    }

    // Show previous session's handoff notes for this agent
    if let Some(last) = db.get_last_session_for_agent(agent_id.as_deref())? {
        if let Some(ended) = last.ended_at {
            println!("Previous session ended: {}", ended.format("%Y-%m-%d %H:%M"));
        }
        if let Some(notes) = &last.handoff_notes {
            if !notes.is_empty() {
                println!("Handoff notes:");
                for line in notes.lines() {
                    println!("  {line}");
                }
                println!();
            }
        }
    }

    let id = db.start_session_with_agent(agent_id.as_deref())?;
    println!("Session #{id} started.");
    Ok(())
}

pub fn end(db: &Database, notes: Option<&str>, crosslink_dir: &std::path::Path) -> Result<()> {
    let agent_id = load_agent_id(crosslink_dir);
    let Some(session) = db.get_current_session_for_agent(agent_id.as_deref())? else {
        bail!("No active session");
    };

    // Auto-release lock on the active issue in multi-agent mode
    if let Some(issue_id) = session.active_issue_id {
        match try_release_lock(crosslink_dir, issue_id) {
            Ok(true) => println!("Released lock on issue {}", format_issue_id(issue_id)),
            Ok(false) => {}
            Err(e) => tracing::warn!("Could not release lock: {}", e),
        }
    }

    // Write handoff notes as typed comment on active issue for hub sync.
    // Must happen BEFORE end_session so the session is still open if this fails.
    //
    // Strategy: try SharedWriter first (syncs to hub). On failure, fall back to
    // local DB. If both fail, propagate the error so handoff notes are not silently lost (#442).
    if let (Some(notes_text), Some(issue_id)) = (notes, session.active_issue_id) {
        let saved = match crate::shared_writer::SharedWriter::new(crosslink_dir) {
            Ok(Some(w)) => match w.add_comment(db, issue_id, notes_text, "handoff") {
                Ok(_) => true,
                Err(e) => {
                    tracing::warn!(
                        "Handoff notes could not be synced to hub: {}, saving locally",
                        e
                    );
                    false
                }
            },
            _ => false,
        };
        if !saved {
            db.add_comment(issue_id, notes_text, "handoff")?;
        }
    }

    // TEMPORAL COUPLING: end_session MUST be called AFTER the handoff comment
    // above. end_session marks the session as inactive, which prevents later
    // attempts to find the active issue for comment attachment. Moving
    // end_session above the comment block would silently lose handoff notes (#441).
    db.end_session(session.id, notes)?;
    println!("Session #{} ended.", session.id);
    if notes.is_some() {
        println!("Handoff notes saved.");
    }

    Ok(())
}

pub fn status(db: &Database, crosslink_dir: &std::path::Path, json: bool) -> Result<()> {
    let agent_id = load_agent_id(crosslink_dir);
    let Some(session) = db.get_current_session_for_agent(agent_id.as_deref())? else {
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "active": false
                }))?
            );
        } else {
            println!("No active session. Use 'crosslink session start' to begin.");
        }
        return Ok(());
    };

    let duration = Utc::now() - session.started_at;
    let minutes = duration.num_minutes();

    if json {
        let active_issue = session
            .active_issue_id
            .and_then(|id| db.get_issue(id).ok().flatten());
        let mut obj = serde_json::json!({
            "active": true,
            "session_id": session.id,
            "started_at": session.started_at,
            "duration_minutes": minutes,
            "agent_id": session.agent_id,
        });
        if let Some(issue) = active_issue {
            obj["working_on"] = serde_json::json!({
                "id": issue.id,
                "display_id": format_issue_id(issue.id),
                "title": issue.title,
            });
        }
        if let Some(ref action) = session.last_action {
            obj["last_action"] = serde_json::json!(action);
        }
        println!("{}", serde_json::to_string_pretty(&obj)?);
        return Ok(());
    }

    println!(
        "Session #{} (started {})",
        session.id,
        session.started_at.format("%Y-%m-%d %H:%M")
    );

    if let Some(issue_id) = session.active_issue_id {
        if let Some(issue) = db.get_issue(issue_id)? {
            println!("Working on: {} {}", format_issue_id(issue.id), issue.title);
        } else {
            println!(
                "Working on: {} (issue not found)",
                format_issue_id(issue_id)
            );
        }
    } else {
        println!("Working on: (none)");
    }

    if let Some(ref action) = session.last_action {
        println!("Last action: {action}");
    }

    println!("Duration: {minutes} minutes");

    // Session activity summary — shows the value crosslink is providing
    let since = session.started_at.to_rfc3339();
    let issues_created = db.count_issues_since(&since).unwrap_or(0);
    let comments_added = db.count_comments_since(&since).unwrap_or(0);
    if issues_created > 0 || comments_added > 0 {
        let mut parts = Vec::new();
        if issues_created > 0 {
            parts.push(format!(
                "{} issue{} created",
                issues_created,
                if issues_created == 1 { "" } else { "s" }
            ));
        }
        if comments_added > 0 {
            parts.push(format!(
                "{} comment{} recorded",
                comments_added,
                if comments_added == 1 { "" } else { "s" }
            ));
        }
        println!("Activity: {}", parts.join(", "));
    }

    Ok(())
}

pub fn work(db: &Database, issue_id: i64, crosslink_dir: &std::path::Path) -> Result<()> {
    let agent_id = load_agent_id(crosslink_dir);
    let Some(session) = db.get_current_session_for_agent(agent_id.as_deref())? else {
        bail!("No active session. Use 'crosslink session start' first.");
    };

    let Some(issue) = db.get_issue(issue_id)? else {
        bail!("Issue {} not found", format_issue_id(issue_id));
    };

    // Check lock status (handles auto-steal of stale locks if configured)
    crate::lock_check::enforce_lock(crosslink_dir, issue_id, db)?;

    // Atomically claim lock then set session — bail if another agent wins
    let freshly_claimed = match try_claim_lock(crosslink_dir, issue_id, None)? {
        ClaimResult::Claimed => {
            println!("Claimed lock on issue {}", format_issue_id(issue_id));
            true
        }
        ClaimResult::AlreadyHeld | ClaimResult::NotConfigured => false,
        ClaimResult::Contended { winner_agent_id } => {
            bail!(
                "Lock on {} was claimed by '{}' before we could acquire it. \
                 Use 'crosslink locks steal {}' to override.",
                format_issue_id(issue_id),
                winner_agent_id,
                issue_id
            );
        }
    };

    // Only reached if lock claim succeeded (or lock system not configured).
    // If set_session_issue fails after we claimed a lock, release the lock to avoid orphaned locks.
    if let Err(e) = db.set_session_issue(session.id, issue_id) {
        if freshly_claimed {
            release_lock_best_effort(crosslink_dir, issue_id);
        }
        return Err(e);
    }
    println!(
        "Now working on: {} {}",
        format_issue_id(issue.id),
        issue.title
    );
    Ok(())
}

pub fn action(db: &Database, text: &str, crosslink_dir: &std::path::Path) -> Result<()> {
    let agent_id = load_agent_id(crosslink_dir);
    let Some(session) = db.get_current_session_for_agent(agent_id.as_deref())? else {
        bail!("No active session. Use 'crosslink session start' first.");
    };

    db.set_session_action(session.id, text)?;
    println!("Action recorded: {text}");

    // Auto-comment on the active issue if one is set.
    // Use SharedWriter when available so comments sync to the hub (#438).
    if let Some(issue_id) = session.active_issue_id {
        let comment_text = format!("[action] {text}");
        match crate::shared_writer::SharedWriter::new(crosslink_dir) {
            Ok(Some(w)) => {
                if let Err(e) = w.add_comment(db, issue_id, &comment_text, "note") {
                    tracing::warn!("action comment sync failed, saving locally: {}", e);
                    db.add_comment(issue_id, &comment_text, "note")?;
                }
            }
            _ => {
                db.add_comment(issue_id, &comment_text, "note")?;
            }
        }
    }

    Ok(())
}

pub fn last_handoff(db: &Database, crosslink_dir: &std::path::Path) -> Result<()> {
    let agent_id = load_agent_id(crosslink_dir);
    match db.get_last_session_for_agent(agent_id.as_deref())? {
        Some(session) => {
            if let Some(notes) = &session.handoff_notes {
                if !notes.is_empty() {
                    println!("{notes}");
                    return Ok(());
                }
            }
            println!("No previous handoff notes.");
        }
        None => {
            println!("No previous session found.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn setup_test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    // ==================== Start Tests ====================

    #[test]
    fn test_start_session() {
        let (db, _dir) = setup_test_db();

        let result = start(&db, _dir.path());
        assert!(result.is_ok());

        let session = db.get_current_session().unwrap();
        assert!(session.is_some());
    }

    #[test]
    fn test_start_already_active() {
        let (db, _dir) = setup_test_db();

        start(&db, _dir.path()).unwrap();
        let first_session = db.get_current_session().unwrap().unwrap();

        // Starting again should not create new session
        let result = start(&db, _dir.path());
        assert!(result.is_ok());

        let current = db.get_current_session().unwrap().unwrap();
        assert_eq!(current.id, first_session.id);
    }

    // ==================== End Tests ====================

    #[test]
    fn test_end_session() {
        let (db, _dir) = setup_test_db();

        start(&db, _dir.path()).unwrap();
        let result = end(&db, None, _dir.path());
        assert!(result.is_ok());

        let session = db.get_current_session().unwrap();
        assert!(session.is_none());
    }

    #[test]
    fn test_end_session_with_notes() {
        let (db, _dir) = setup_test_db();

        start(&db, _dir.path()).unwrap();
        let result = end(&db, Some("Completed auth feature"), _dir.path());
        assert!(result.is_ok());

        let last = db.get_last_session().unwrap().unwrap();
        assert_eq!(
            last.handoff_notes,
            Some("Completed auth feature".to_string())
        );
    }

    #[test]
    fn test_end_no_active_session() {
        let (db, _dir) = setup_test_db();

        let result = end(&db, None, _dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No active session"));
    }

    // ==================== Status Tests ====================

    #[test]
    fn test_status_no_session() {
        let (db, _dir) = setup_test_db();

        let result = status(&db, _dir.path(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_with_session() {
        let (db, _dir) = setup_test_db();

        start(&db, _dir.path()).unwrap();
        let result = status(&db, _dir.path(), false);
        assert!(result.is_ok());
    }

    #[test]
    fn test_status_with_active_issue() {
        let (db, dir) = setup_test_db();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        start(&db, dir.path()).unwrap();
        work(&db, issue_id, dir.path()).unwrap();

        let result = status(&db, dir.path(), false);
        assert!(result.is_ok());
    }

    // ==================== Work Tests ====================

    #[test]
    fn test_work_sets_active_issue() {
        let (db, dir) = setup_test_db();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();
        start(&db, dir.path()).unwrap();

        let result = work(&db, issue_id, dir.path());
        assert!(result.is_ok());

        let session = db.get_current_session().unwrap().unwrap();
        assert_eq!(session.active_issue_id, Some(issue_id));
    }

    #[test]
    fn test_work_no_session() {
        let (db, dir) = setup_test_db();

        let issue_id = db.create_issue("Test issue", None, "medium").unwrap();

        let result = work(&db, issue_id, dir.path());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No active session"));
    }

    #[test]
    fn test_work_nonexistent_issue() {
        let (db, dir) = setup_test_db();

        start(&db, dir.path()).unwrap();

        let result = work(&db, 99999, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_work_change_active_issue() {
        let (db, dir) = setup_test_db();

        let issue1 = db.create_issue("Issue 1", None, "medium").unwrap();
        let issue2 = db.create_issue("Issue 2", None, "medium").unwrap();
        start(&db, dir.path()).unwrap();

        work(&db, issue1, dir.path()).unwrap();
        let session = db.get_current_session().unwrap().unwrap();
        assert_eq!(session.active_issue_id, Some(issue1));

        work(&db, issue2, dir.path()).unwrap();
        let session = db.get_current_session().unwrap().unwrap();
        assert_eq!(session.active_issue_id, Some(issue2));
    }

    // ==================== Last Handoff Tests ====================

    #[test]
    fn test_last_handoff_no_sessions() {
        let (db, _dir) = setup_test_db();

        let result = last_handoff(&db, _dir.path());
        assert!(result.is_ok());
        // Should handle gracefully when no sessions exist
    }

    #[test]
    fn test_last_handoff_no_notes() {
        let (db, _dir) = setup_test_db();

        start(&db, _dir.path()).unwrap();
        end(&db, None, _dir.path()).unwrap();

        let result = last_handoff(&db, _dir.path());
        assert!(result.is_ok());
        // Should handle gracefully when last session has no notes
    }

    #[test]
    fn test_last_handoff_with_notes() {
        let (db, _dir) = setup_test_db();

        start(&db, _dir.path()).unwrap();
        end(&db, Some("Important handoff notes"), _dir.path()).unwrap();

        let result = last_handoff(&db, _dir.path());
        assert!(result.is_ok());
        // Notes should be retrievable
        let last = db.get_last_session().unwrap().unwrap();
        assert_eq!(
            last.handoff_notes,
            Some("Important handoff notes".to_string())
        );
    }

    // ==================== Full Workflow Tests ====================

    #[test]
    fn test_full_session_workflow() {
        let (db, dir) = setup_test_db();

        // Start session
        start(&db, dir.path()).unwrap();
        assert!(db.get_current_session().unwrap().is_some());

        // Create and work on issue
        let issue_id = db.create_issue("Feature", None, "high").unwrap();
        work(&db, issue_id, dir.path()).unwrap();

        // Check status
        status(&db, dir.path(), false).unwrap();

        // End with notes
        end(&db, Some("Made progress on feature"), dir.path()).unwrap();
        assert!(db.get_current_session().unwrap().is_none());

        // Start new session
        start(&db, dir.path()).unwrap();
        let last = db.get_last_session().unwrap().unwrap();
        assert_eq!(
            last.handoff_notes,
            Some("Made progress on feature".to_string())
        );
    }

    // ==================== Property-Based Tests ====================

    proptest! {
        #[test]
        fn prop_start_end_cycle(iterations in 1usize..5) {
            let (db, _dir) = setup_test_db();

            for _ in 0..iterations {
                start(&db, _dir.path()).unwrap();
                prop_assert!(db.get_current_session().unwrap().is_some());
                end(&db, None, _dir.path()).unwrap();
                prop_assert!(db.get_current_session().unwrap().is_none());
            }
        }

        #[test]
        fn prop_handoff_notes_roundtrip(notes in "[a-zA-Z0-9 ]{0,100}") {
            let (db, _dir) = setup_test_db();

            start(&db, _dir.path()).unwrap();
            end(&db, Some(&notes), _dir.path()).unwrap();

            let last = db.get_last_session().unwrap().unwrap();
            prop_assert_eq!(last.handoff_notes, Some(notes));
        }

        #[test]
        fn prop_work_nonexistent_fails(issue_id in 1000i64..10000) {
            let (db, dir) = setup_test_db();

            start(&db, dir.path()).unwrap();
            let result = work(&db, issue_id, dir.path());
            prop_assert!(result.is_err());
        }
    }
}
