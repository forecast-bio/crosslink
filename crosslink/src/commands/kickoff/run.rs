// E-ana tablet — kickoff run: main entry point for `crosslink kickoff run`
use anyhow::{bail, Context, Result};
use std::path::Path;

use crate::db::Database;
use crate::identity::AgentConfig;
use crate::shared_writer::SharedWriter;

use super::helpers::*;
use super::launch::*;
use super::prompt::*;
use super::types::*;

/// Main entry point: `crosslink kickoff run`.
pub fn run(
    crosslink_dir: &Path,
    db: &Database,
    writer: Option<&SharedWriter>,
    opts: &KickoffOpts,
) -> Result<()> {
    // 1. Pre-flight: validate all required external commands are present
    let preflight = if !opts.dry_run {
        Some(preflight_check(
            &opts.container,
            &opts.verify,
            crosslink_dir,
        )?)
    } else {
        None
    };

    let root = repo_root()?;
    let base_slug = slugify(opts.description);
    let slug = if base_slug.is_empty() {
        rand_hex_suffix()
    } else {
        format!("{}-{}", base_slug, rand_hex_suffix())
    };

    // 2. Create or find the issue
    let issue_id = if let Some(id) = opts.issue {
        // Verify the issue exists
        if db.get_issue(id)?.is_none() {
            bail!("Issue #{} not found", id);
        }
        id
    } else {
        // Create a new issue directly
        let id = if let Some(w) = writer {
            w.create_issue(
                db,
                opts.description,
                Some("Created by crosslink kickoff"),
                "medium",
            )?
        } else {
            db.create_issue(
                opts.description,
                Some("Created by crosslink kickoff"),
                "medium",
            )?
        };
        let label_err = if let Some(w) = writer {
            w.add_label(db, id, "feature").err()
        } else {
            db.add_label(id, "feature").err()
        };
        if let Some(e) = label_err {
            tracing::warn!("could not label issue #{id} with 'feature': {e}");
        }
        if !opts.quiet {
            println!("Created issue #{}", id);
        }
        id
    };

    // 3. Create worktree and feature branch (or use existing branch)
    let (worktree_dir, branch_name) = if let Some(br) = opts.branch {
        // Use existing branch — check if worktree exists
        let wt_slug = br.strip_prefix("feature/").unwrap_or(br);
        let worktree_dir = root.join(".worktrees").join(wt_slug);
        if !worktree_dir.exists() {
            create_worktree(&root, wt_slug, None)?
        } else {
            (worktree_dir, br.to_string())
        }
    } else {
        create_worktree(&root, &slug, None)?
    };

    // Write slug sentinel so other commands can identify this worktree
    std::fs::write(worktree_dir.join(".kickoff-slug"), &slug)
        .context("Failed to write .kickoff-slug sentinel")?;

    // 4. Detect project conventions
    let conventions = detect_conventions(&root);

    // 5. Build the prompt
    let prompt = build_prompt(opts, issue_id, &branch_name, &conventions);

    // 6. Write KICKOFF.md to worktree
    std::fs::write(worktree_dir.join("KICKOFF.md"), &prompt)
        .context("Failed to write KICKOFF.md")?;

    // 6b. Extract and write criteria if design doc has acceptance criteria
    if let Some(doc) = opts.design_doc {
        if !doc.acceptance_criteria.is_empty() {
            let source = opts.doc_path.unwrap_or("unknown");
            let criteria_file = extract_criteria(doc, source);
            let json = serde_json::to_string_pretty(&criteria_file)
                .context("Failed to serialize criteria")?;
            std::fs::write(worktree_dir.join(".kickoff-criteria.json"), &json)
                .context("Failed to write .kickoff-criteria.json")?;
        }
    }

    // 6c. Write launch metadata (timeout + start time) for status tracking
    {
        let metadata = KickoffMetadata {
            started_at: chrono::Utc::now().to_rfc3339(),
            timeout_secs: opts.timeout.as_secs(),
        };
        let json = serde_json::to_string_pretty(&metadata)
            .context("Failed to serialize kickoff metadata")?;
        std::fs::write(worktree_dir.join(".kickoff-metadata.json"), &json)
            .context("Failed to write .kickoff-metadata.json")?;
    }

    // 7. Exclude kickoff files from git
    exclude_kickoff_files(&worktree_dir)?;

    // Dry run: print prompt and exit (skip agent init — no launch needed)
    if opts.dry_run {
        let parent_id = AgentConfig::load(crosslink_dir)?
            .map(|c| c.agent_id)
            .unwrap_or_else(|| "driver".to_string());
        let agent_id = format!("{}--{}", parent_id, slug);
        println!("{}", prompt);
        println!("---");
        println!("Worktree: {}", worktree_dir.display());
        println!("Branch:   {}", branch_name);
        println!("Agent:    {}", agent_id);
        return Ok(());
    }

    // 8. Initialize crosslink + agent in worktree (only for real launches)
    let agent_id = init_worktree_agent(&worktree_dir, crosslink_dir, &slug)?;

    // preflight is guaranteed Some after the dry-run early return above
    let preflight = preflight.context("preflight check was skipped unexpectedly")?;

    // 9. Launch the agent
    let allowed_tools = build_allowed_tools(&conventions, &opts.verify);

    match &opts.container {
        ContainerMode::None => {
            let mut session_name = tmux_session_name(&slug);
            if tmux_session_exists(&session_name) {
                // Append random suffix
                let suffix: u32 = rand_suffix();
                session_name =
                    format!("{}-{}", &session_name[..session_name.len().min(44)], suffix);
            }

            launch_local(
                &worktree_dir,
                &session_name,
                opts.model,
                &allowed_tools,
                opts.timeout,
                preflight.timeout_cmd,
                preflight.sandbox_command.as_deref(),
                crosslink_dir,
                opts.skip_permissions,
            )?;

            // 10. Report
            if !opts.quiet {
                println!("Feature agent launched.");
                println!();
                println!("  Worktree: {}", worktree_dir.display());
                println!("  Branch:   {}", branch_name);
                println!("  Issue:    #{}", issue_id);
                println!("  Agent:    {}", agent_id);
                println!("  Session:  {}", session_name);
                println!("  Verify:   {:?}", opts.verify);
                println!();
                println!("  Approve trust:  tmux attach -t {}", session_name);
                println!("  Check status:   crosslink kickoff status {}", agent_id);
                if opts.verify == VerifyLevel::Ci || opts.verify == VerifyLevel::Thorough {
                    println!();
                    println!("  CI verification is enabled. The agent will push and open a draft PR after local tests pass.");
                }
            } else {
                println!("{}", session_name);
            }
        }
        mode @ (ContainerMode::Docker | ContainerMode::Podman) => {
            let container_id = launch_container(
                mode,
                &worktree_dir,
                opts.image,
                &agent_id,
                opts.model,
                &allowed_tools,
                opts.timeout,
            )?;

            if !opts.quiet {
                let runtime = if *mode == ContainerMode::Docker {
                    "docker"
                } else {
                    "podman"
                };
                println!("Feature agent launched in container.");
                println!();
                println!("  Worktree:    {}", worktree_dir.display());
                println!("  Branch:      {}", branch_name);
                println!("  Issue:       #{}", issue_id);
                println!("  Agent:       {}", agent_id);
                println!(
                    "  Container:   {}",
                    &container_id[..12.min(container_id.len())]
                );
                println!("  Verify:      {:?}", opts.verify);
                println!();
                println!(
                    "  View logs:   {} logs -f {}",
                    runtime,
                    &container_id[..12.min(container_id.len())]
                );
                println!("  Check status: crosslink kickoff status {}", agent_id);
            } else {
                println!("{}", container_id);
            }
        }
    }

    Ok(())
}
