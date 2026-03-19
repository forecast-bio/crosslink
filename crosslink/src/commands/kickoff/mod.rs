// E-ana tablet — kickoff command: launch agents to implement features
mod cleanup;
mod helpers;
mod launch;
mod monitor;
mod plan;
mod prompt;
mod run;
mod types;

#[cfg(test)]
mod tests;

// Re-export public types used by external callers (swarm, main, etc.)
pub use types::{ContainerMode, KickoffOpts, KickoffReport, PlanOpts, ReportFormat, VerifyLevel};

// Re-export parse functions (used by dispatch and swarm)
pub use types::{parse_container_mode, parse_duration, parse_verify_level};

// Re-export public command functions (used from main.rs dispatch)
pub use cleanup::cleanup;
pub use monitor::{list, logs, report, report_all, status, stop};
pub use plan::{plan, show_plan};
pub use run::run;

// Re-export crate-internal items used by other modules within this crate
pub(crate) use helpers::{
    command_available, detect_conventions, format_duration, slugify, tmux_session_name,
};

use anyhow::{Context, Result};
use std::path::Path;

use crate::db::Database;
use crate::shared_writer::SharedWriter;
use crate::KickoffCommands;

pub fn dispatch(
    command: KickoffCommands,
    crosslink_dir: &Path,
    db: &Database,
    writer: Option<&SharedWriter>,
    quiet: bool,
    json: bool,
) -> Result<()> {
    match command {
        KickoffCommands::Run {
            description,
            issue,
            container,
            verify,
            model,
            image,
            timeout,
            dry_run,
            branch,
            doc,
            skip_permissions,
        } => {
            let parsed_doc = if let Some(ref path) = doc {
                let content = std::fs::read_to_string(path)
                    .with_context(|| format!("Failed to read design doc: {}", path.display()))?;
                let d = super::design_doc::parse_design_doc(&content);
                for warning in super::design_doc::validate_design_doc(&d) {
                    tracing::warn!("{}", warning);
                }
                Some(d)
            } else {
                None
            };
            let opts = KickoffOpts {
                description: &description,
                issue,
                container: parse_container_mode(&container)?,
                verify: parse_verify_level(&verify)?,
                model: &model,
                image: &image,
                timeout: parse_duration(&timeout)?,
                dry_run,
                branch: branch.as_deref(),
                quiet,
                design_doc: parsed_doc.as_ref(),
                doc_path: doc.as_ref().map(|p| p.to_str().unwrap_or("unknown")),
                skip_permissions,
            };
            run(crosslink_dir, db, writer, &opts)
        }
        KickoffCommands::Status { agent } => status(crosslink_dir, &agent),
        KickoffCommands::Logs { agent, lines } => logs(crosslink_dir, &agent, lines),
        KickoffCommands::Stop { agent, force } => stop(crosslink_dir, &agent, force),
        KickoffCommands::Plan {
            doc,
            issue,
            model,
            timeout,
            dry_run,
        } => {
            let content = std::fs::read_to_string(&doc)
                .with_context(|| format!("Failed to read design doc: {}", doc.display()))?;
            let design_doc = super::design_doc::parse_design_doc(&content);
            for warning in super::design_doc::validate_design_doc(&design_doc) {
                tracing::warn!("{}", warning);
            }
            let plan_opts = PlanOpts {
                doc: &design_doc,
                model: &model,
                timeout: parse_duration(&timeout)?,
                dry_run,
                issue,
                quiet,
            };
            plan(crosslink_dir, db, &plan_opts)
        }
        KickoffCommands::ShowPlan { agent } => show_plan(crosslink_dir, &agent),
        KickoffCommands::Report {
            agent,
            json: report_json,
            markdown,
            all,
        } => {
            let format = if report_json {
                ReportFormat::Json
            } else if markdown {
                ReportFormat::Markdown
            } else {
                ReportFormat::Table
            };
            if all {
                report_all(crosslink_dir, format)
            } else {
                let agent =
                    agent.ok_or_else(|| anyhow::anyhow!("Agent ID required (or use --all)"))?;
                report(crosslink_dir, &agent, format)
            }
        }
        KickoffCommands::List { status } => list(crosslink_dir, &status, json, quiet),
        KickoffCommands::Cleanup {
            dry_run,
            force,
            keep,
            json: cleanup_json,
        } => cleanup(crosslink_dir, dry_run, force, keep, cleanup_json),
    }
}
