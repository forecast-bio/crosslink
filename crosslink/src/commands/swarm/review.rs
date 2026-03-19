// Swarm review: parallel adversarial review, pipeline orchestration,
// fix planning, and shared helpers.

use anyhow::{bail, Context, Result};
use std::path::Path;

use super::io::*;
use super::types::*;
use crate::findings;
use crate::issue_filing;
use crate::pipeline::{self, Pipeline, PipelineConfig};
use crate::seam;
use crate::sync::SyncManager;
use crate::trust_model;

// Mandate prompt templates
const MANDATE_ADVERSARIAL: &str = "You are the ha-satan, the loyal accuser. \
    Find real problems that would cause failures in production. \
    Ignore style nits, focus on correctness, safety, and robustness.";

const MANDATE_SECURITY: &str = "Review for trust boundary violations, injection vectors, \
    data integrity issues, and unsafe operations.";

const MANDATE_ROBUSTNESS: &str = "Find crash paths, resource leaks, error handling gaps, \
    and unhandled edge cases.";

const MANDATE_CORRECTNESS: &str = "Find logic errors, race conditions, invariant violations, \
    and incorrect algorithm implementations.";

/// Map a mandate name to its prompt text.
pub fn mandate_prompt(mandate: &str) -> &str {
    match mandate {
        "adversarial" => MANDATE_ADVERSARIAL,
        "security" => MANDATE_SECURITY,
        "robustness" => MANDATE_ROBUSTNESS,
        "correctness" => MANDATE_CORRECTNESS,
        _ => mandate, // Custom mandate text passed through as-is
    }
}

/// Assign partitions to agents using round-robin distribution.
pub(super) fn assign_partitions(
    partitions: Vec<seam::Partition>,
    agent_count: usize,
) -> Vec<ReviewAgentAssignment> {
    let agent_count = agent_count.max(1);
    let mut assignments: Vec<ReviewAgentAssignment> = (0..agent_count)
        .map(|i| ReviewAgentAssignment {
            agent_slug: format!("reviewer-{}", i + 1),
            partition_label: String::new(),
            files: Vec::new(),
        })
        .collect();

    for (i, partition) in partitions.into_iter().enumerate() {
        let agent_idx = i % agent_count;
        if !assignments[agent_idx].partition_label.is_empty() {
            assignments[agent_idx].partition_label.push_str(", ");
        }
        assignments[agent_idx]
            .partition_label
            .push_str(&partition.label);
        assignments[agent_idx].files.extend(
            partition
                .files
                .into_iter()
                .map(|f| f.to_string_lossy().to_string()),
        );
    }

    // Filter out agents with no files assigned
    assignments.retain(|a| !a.files.is_empty());
    assignments
}

/// Launch a parallel adversarial review across codebase partitions.
pub fn review(
    crosslink_dir: &Path,
    agent_count: usize,
    mandate: &str,
    doc: Option<&Path>,
    file_issues: bool,
    fix: bool,
) -> Result<()> {
    let repo_root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    // Discover source partitions via seam detection
    let partitions = seam::detect_seams(repo_root, agent_count)?;
    if partitions.is_empty() {
        bail!("No source files found in repo root. Nothing to review.");
    }

    println!(
        "Discovered {} source partition(s) in {}",
        partitions.len(),
        repo_root.display()
    );
    for p in &partitions {
        println!(
            "  {} ({} files, {} lines)",
            p.label,
            p.files.len(),
            p.line_count
        );
    }
    println!();

    // Assign partitions to agents
    let assignments = assign_partitions(partitions, agent_count);
    let prompt_text = mandate_prompt(mandate);
    let now = chrono::Utc::now().to_rfc3339();

    let plan = ReviewPlan {
        mandate: mandate.to_string(),
        mandate_prompt: prompt_text.to_string(),
        agent_count: assignments.len(),
        created_at: now,
        agents: assignments.clone(),
        doc_output: doc.map(|p| p.to_path_buf()),
    };

    // Persist plan to hub branch
    write_hub_json(&sync, "swarm/review-plan.json", &plan)?;
    commit_hub_files(
        &sync,
        &["swarm/review-plan.json"],
        "swarm: store review plan",
    )?;

    // Print summary
    println!("Review plan ({} mandate):", mandate);
    println!("  Prompt: {}", prompt_text);
    println!();
    println!("Agent assignments:");
    for agent in &plan.agents {
        println!(
            "  {} — partitions: [{}] ({} files)",
            agent.agent_slug,
            agent.partition_label,
            agent.files.len()
        );
    }
    println!();

    if let Some(doc_path) = doc {
        println!("Findings will be consolidated to: {}", doc_path.display());
    }

    println!("Plan saved to hub branch at swarm/review-plan.json");

    if file_issues || fix {
        // Run the pipeline for post-review stages
        let config = PipelineConfig {
            agent_count: assignments.len(),
            mandate: mandate.to_string(),
            auto_fix: fix,
            auto_file_issues: file_issues,
            target_branch: "develop".to_string(),
        };
        run_review_pipeline(crosslink_dir, config)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Review pipeline orchestration
// ---------------------------------------------------------------------------

/// Convert consolidated finding groups into the format expected by issue_filing.
fn findings_to_filing(groups: &[findings::FindingGroup]) -> Vec<issue_filing::FindingForFiling> {
    groups
        .iter()
        .map(|g| issue_filing::FindingForFiling {
            title: g.canonical.title.clone(),
            severity: g.effective_severity.to_string(),
            file: g.canonical.file.clone(),
            line: g.canonical.line,
            description: g.canonical.description.clone(),
            suggested_fix: g.canonical.suggested_fix.clone(),
            consensus_count: g.consensus_count,
        })
        .collect()
}

/// Consolidate review findings from agent reports on the hub branch.
fn consolidate_review_findings(crosslink_dir: &Path) -> Result<findings::ConsolidatedReport> {
    let sync = SyncManager::new(crosslink_dir)?;
    let findings_dir = sync.cache_path().join("swarm");
    let reports = findings::parse_reports(&findings_dir)?;
    if reports.is_empty() {
        bail!("No review findings found. Run review agents first.");
    }
    let consolidated = findings::consolidate(reports);

    // Persist consolidated report
    write_hub_json(&sync, "swarm/consolidated-report.json", &consolidated)?;
    let markdown = findings::generate_markdown_report(&consolidated);
    let md_path = sync
        .cache_path()
        .join("swarm")
        .join("consolidated-report.md");
    if let Some(parent) = md_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&md_path, &markdown)?;
    commit_hub_files(
        &sync,
        &[
            "swarm/consolidated-report.json",
            "swarm/consolidated-report.md",
        ],
        "swarm: consolidate review findings",
    )?;

    println!(
        "Consolidated {} findings from {} agents ({} after dedup)",
        consolidated.total_findings, consolidated.agent_count, consolidated.deduplicated_findings,
    );

    Ok(consolidated)
}

/// Apply trust model filtering to consolidated findings.
fn apply_trust_filtering(
    crosslink_dir: &Path,
    report: &findings::ConsolidatedReport,
) -> Vec<findings::FindingGroup> {
    let config = match trust_model::load_trust_config(crosslink_dir) {
        Ok(c) => c,
        Err(_) => return report.groups.clone(),
    };

    // Convert finding groups to tuples for the trust model batch API
    let finding_tuples: Vec<(String, String, String)> = report
        .groups
        .iter()
        .map(|g| {
            (
                g.canonical.title.clone(),
                g.canonical.description.clone(),
                g.effective_severity.to_string(),
            )
        })
        .collect();

    let annotated = trust_model::apply_trust_model(&config, finding_tuples);

    let mut kept = Vec::new();
    let mut by_design_count = 0;
    for (i, (_title, _desc, _sev, result)) in annotated.into_iter().enumerate() {
        let group = &report.groups[i];
        match result {
            trust_model::TriageResult::Valid => kept.push(group.clone()),
            trust_model::TriageResult::ByDesign { reason } => {
                println!("  [by-design] {} — {}", group.canonical.title, reason);
                by_design_count += 1;
            }
            trust_model::TriageResult::Downgraded { reason, .. } => {
                println!("  [downgraded] {} — {}", group.canonical.title, reason);
                kept.push(group.clone());
            }
        }
    }
    if by_design_count > 0 {
        println!("  {} finding(s) triaged as by-design", by_design_count);
    }
    kept
}

/// Drive the review pipeline through its stages.
fn run_review_pipeline(crosslink_dir: &Path, config: PipelineConfig) -> Result<()> {
    let mut pipe = match pipeline::load_pipeline(crosslink_dir)? {
        Some(p) => {
            println!("Resuming existing pipeline at stage: {}", p.current_stage);
            p
        }
        None => Pipeline::new(config),
    };

    loop {
        // Check for human checkpoints using the pipeline API
        if Pipeline::is_checkpoint(pipe.current_stage) {
            println!("\nPipeline paused for human review.");
            println!("Review findings in .crosslink/ or on the hub branch.");
            println!("Run `crosslink swarm review-continue` to proceed.");
            pipeline::save_pipeline(crosslink_dir, &pipe)?;
            return Ok(());
        }

        let stage_result: Result<()> = match pipe.current_stage {
            pipeline::PipelineStage::Partition | pipeline::PipelineStage::Review => {
                // Partitioning and agent launch already handled by review()
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::AwaitReview => {
                println!("Review agents launched. Check progress with `crosslink swarm status`.");
                println!("Run `crosslink swarm review-continue` when agents complete.");
                pipeline::save_pipeline(crosslink_dir, &pipe)?;
                return Ok(());
            }
            pipeline::PipelineStage::Consolidate => {
                let report = consolidate_review_findings(crosslink_dir)?;
                let filtered = apply_trust_filtering(crosslink_dir, &report);
                println!("{} findings after trust model filtering", filtered.len());
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::HumanCheckpoint => {
                // Handled by is_checkpoint check above
                unreachable!()
            }
            pipeline::PipelineStage::FileIssues => {
                if pipe.config.auto_file_issues {
                    let sync = SyncManager::new(crosslink_dir)?;
                    let report: findings::ConsolidatedReport =
                        read_hub_json(&sync, "swarm/consolidated-report.json")?;
                    let filtered = apply_trust_filtering(crosslink_dir, &report);

                    // Deduplicate against existing GitHub issues with the review label
                    let existing_titles = fetch_existing_review_titles();
                    let deduped = findings::cross_reference_issues(&filtered, &existing_titles);
                    if deduped.len() < filtered.len() {
                        println!(
                            "  Skipped {} finding(s) that match existing issues",
                            filtered.len() - deduped.len()
                        );
                    }

                    let for_filing = findings_to_filing(&deduped);
                    issue_filing::file_issues_batch(&for_filing, false)?;
                }
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::Fix => {
                if pipe.config.auto_fix {
                    println!("Launching fix agents...");
                    fix(crosslink_dir, None, Some("review-finding"), 6, false)?;
                }
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::AwaitFix => {
                println!("Fix agents launched. Check progress with `crosslink swarm status`.");
                println!("Run `crosslink swarm review-continue` when agents complete.");
                pipeline::save_pipeline(crosslink_dir, &pipe)?;
                return Ok(());
            }
            pipeline::PipelineStage::Merge | pipeline::PipelineStage::PullRequest => {
                println!(
                    "Stage {}: run `crosslink swarm merge` to combine changes.",
                    pipe.current_stage
                );
                pipe.advance()?;
                Ok(())
            }
            pipeline::PipelineStage::Done => {
                println!("Pipeline complete.");
                break;
            }
            pipeline::PipelineStage::Failed => {
                println!("Pipeline failed.");
                break;
            }
        };

        // On stage failure, mark the pipeline as failed and persist
        if let Err(e) = stage_result {
            pipe.fail(&e.to_string());
            pipeline::save_pipeline(crosslink_dir, &pipe)?;
            return Err(e);
        }

        pipeline::save_pipeline(crosslink_dir, &pipe)?;
    }

    Ok(())
}

/// Fetch titles of existing GitHub issues labeled "review-finding" for deduplication.
fn fetch_existing_review_titles() -> Vec<String> {
    match fetch_issues_by_label("review-finding") {
        Ok(issues) => issues.into_iter().map(|(_, title, _, _)| title).collect(),
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Fetch details for a single GitHub issue via `gh issue view`.
fn fetch_issue_details(number: u64) -> Result<(String, String, Vec<String>)> {
    let output = std::process::Command::new("gh")
        .args([
            "issue",
            "view",
            &number.to_string(),
            "--json",
            "title,body,labels",
        ])
        .output()
        .context("Failed to run gh issue view")?;

    if !output.status.success() {
        bail!(
            "gh issue view {} failed: {}",
            number,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let parsed: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("Failed to parse gh issue view output")?;

    let title = parsed["title"].as_str().unwrap_or_default().to_string();
    let body = parsed["body"].as_str().unwrap_or_default().to_string();
    let labels = parsed["labels"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    Ok((title, body, labels))
}

/// Fetch issues matching a label via `gh issue list`.
pub(super) fn fetch_issues_by_label(label: &str) -> Result<Vec<LabeledIssue>> {
    let output = std::process::Command::new("gh")
        .args([
            "issue",
            "list",
            "--label",
            label,
            "--json",
            "number,title,body,labels",
            "--limit",
            "100",
        ])
        .output()
        .context("Failed to run gh issue list")?;

    if !output.status.success() {
        bail!(
            "gh issue list --label {} failed: {}",
            label,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let parsed: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout).context("Failed to parse gh issue list output")?;

    let mut results = Vec::new();
    for item in parsed {
        let number = item["number"].as_u64().unwrap_or(0);
        if number == 0 {
            continue;
        }
        let title = item["title"].as_str().unwrap_or_default().to_string();
        let body = item["body"].as_str().unwrap_or_default().to_string();
        let labels = item["labels"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v["name"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        results.push((number, title, body, labels));
    }

    Ok(results)
}

/// Create a slug for a fix agent from the issue number and title.
///
/// Example: `slugify_fix_target(326, "Buffer overflow in parser")` -> `"fix-326-buffer-overflow-in-parser"`
pub(super) fn slugify_fix_target(issue_number: u64, title: &str) -> String {
    let slug_part: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    // Truncate slug_part to keep the total slug reasonable
    let max_slug_len: usize = 50;
    let prefix = format!("fix-{}-", issue_number);
    let remaining = max_slug_len.saturating_sub(prefix.len());
    let truncated = if slug_part.len() > remaining {
        // Cut at a word boundary if possible
        match slug_part[..remaining].rfind('-') {
            Some(pos) if pos > 0 => &slug_part[..pos],
            _ => &slug_part[..remaining],
        }
    } else {
        &slug_part
    };

    format!("{}{}", prefix, truncated)
}

/// Parse comma-separated issue numbers from a string.
pub(super) fn parse_issue_numbers(input: &str) -> Result<Vec<u64>> {
    input
        .split(',')
        .map(|s| {
            let trimmed = s.trim();
            trimmed
                .parse::<u64>()
                .with_context(|| format!("Invalid issue number: {:?}", trimmed))
        })
        .collect()
}

/// Build and persist a fix plan for parallel issue resolution.
pub fn fix(
    crosslink_dir: &Path,
    issues: Option<&str>,
    from_label: Option<&str>,
    max_agents: usize,
    budget_aware: bool,
) -> Result<()> {
    // Resolve issues from the provided source
    let issue_data: Vec<(u64, String, String, Vec<String>)> = match (issues, from_label) {
        (Some(ids), _) => {
            let numbers = parse_issue_numbers(ids)?;
            let mut data = Vec::new();
            for num in numbers {
                let (title, body, labels) = fetch_issue_details(num)?;
                data.push((num, title, body, labels));
            }
            data
        }
        (None, Some(label)) => fetch_issues_by_label(label)?,
        (None, None) => {
            bail!(
                "Either --issues or --from-label is required.\n\n\
                 Usage:\n  \
                   crosslink swarm fix --issues 326,327,328\n  \
                   crosslink swarm fix --from-label review-finding"
            );
        }
    };

    if issue_data.is_empty() {
        bail!("No issues found matching the given criteria.");
    }

    // Build fix targets
    let targets: Vec<FixTarget> = issue_data
        .into_iter()
        .map(|(number, title, body, labels)| {
            let agent_slug = slugify_fix_target(number, &title);
            FixTarget {
                issue_number: number,
                title,
                body,
                labels,
                agent_slug,
                status: AgentStatus::Planned,
            }
        })
        .collect();

    let now = chrono::Utc::now().to_rfc3339();
    let plan = FixPlan {
        schema_version: 1,
        created_at: now,
        issues: targets,
    };

    // Persist to hub branch
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    write_hub_json(&sync, "swarm/fix-plan.json", &plan)?;
    commit_hub_files(&sync, &["swarm/fix-plan.json"], "swarm: persist fix plan")?;

    // Print summary
    println!("Fix plan created with {} issue(s):\n", plan.issues.len());
    println!("  {:<8} {:<40} Labels", "Issue", "Agent Slug");
    println!("  {:<8} {:<40} ------", "-----", "----------");
    for target in &plan.issues {
        let labels_str = if target.labels.is_empty() {
            String::from("-")
        } else {
            target.labels.join(", ")
        };
        println!(
            "  #{:<7} {:<40} {}",
            target.issue_number, target.agent_slug, labels_str
        );
    }

    if plan.issues.len() > max_agents {
        println!(
            "\nNote: {} issues exceed max_agents ({}). Some will queue.",
            plan.issues.len(),
            max_agents
        );
    }

    if budget_aware {
        println!("\nBudget checking not yet integrated.");
    }

    println!("\nPlan persisted to hub branch at swarm/fix-plan.json");

    Ok(())
}

// ---------------------------------------------------------------------------
// Pipeline wrappers
// ---------------------------------------------------------------------------

/// Continue a paused pipeline past a human checkpoint.
pub fn review_continue(crosslink_dir: &Path) -> Result<()> {
    let mut pipeline = crate::pipeline::load_pipeline(crosslink_dir)?
        .context("No active pipeline found. Start one with `crosslink swarm review`")?;
    pipeline.confirm_checkpoint()?;
    crate::pipeline::save_pipeline(crosslink_dir, &pipeline)?;
    println!(
        "Pipeline resumed from checkpoint. Current stage: {}",
        pipeline.current_stage
    );
    Ok(())
}

/// Show the current pipeline status.
pub fn review_status(crosslink_dir: &Path) -> Result<()> {
    match crate::pipeline::load_pipeline(crosslink_dir)? {
        Some(pipeline) => println!("{}", pipeline.summary()),
        None => println!("No active pipeline."),
    }
    Ok(())
}

/// Run the standalone pipeline driver (crosslink swarm pipeline).
///
/// This uses [`pipeline::run_pipeline`] which logs each stage transition
/// and pauses at human checkpoints.
pub fn run_pipeline_cmd(
    crosslink_dir: &Path,
    agents: usize,
    mandate: &str,
    target_branch: &str,
    auto_fix: bool,
    auto_file_issues: bool,
) -> Result<()> {
    let config = PipelineConfig {
        agent_count: agents,
        mandate: mandate.to_string(),
        auto_fix,
        auto_file_issues,
        target_branch: target_branch.to_string(),
    };
    pipeline::run_pipeline(crosslink_dir, config)
}

/// Initialize trust model configuration (crosslink swarm trust-init).
pub fn trust_init(crosslink_dir: &Path, model: &str) -> Result<()> {
    trust_model::write_default_config(crosslink_dir, model)?;
    let config = trust_model::generate_default_config(model);
    println!("Trust model configuration written to swarm.toml");
    println!("  Model:       {}", config.trust.model);
    println!("  Description: {}", config.trust.description);
    if !config.ignore.patterns.is_empty() {
        println!("  Ignore patterns: {}", config.ignore.patterns.join(", "));
    }
    if !config.boundaries.external.is_empty() {
        println!(
            "  External boundaries: {}",
            config.boundaries.external.join(", ")
        );
    }
    if !config.boundaries.internal.is_empty() {
        println!(
            "  Internal boundaries: {}",
            config.boundaries.internal.join(", ")
        );
    }
    Ok(())
}
