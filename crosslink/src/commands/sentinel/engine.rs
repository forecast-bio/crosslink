use anyhow::Result;
use std::path::Path;
use uuid::Uuid;

use crate::db::Database;
use crate::shared_writer::SharedWriter;

use super::collect;
use super::config::SentinelConfig;
use super::dispatch::triage;
use super::seen_set::{db_dedup_check, SeenSet};
use super::sources::github::GitHubLabelSource;
use super::sources::{Signal, SignalDecision, Source};

/// Statistics from a single sentinel cycle.
#[derive(Debug, Default)]
pub struct CycleStats {
    pub signals_found: u32,
    pub dispatched: u32,
    pub collected: u32,
    pub skipped: u32,
    pub deferred: u32,
}

/// Run a single sentinel cycle: poll sources, triage, dispatch, collect.
pub fn run_oneshot(
    crosslink_dir: &Path,
    db: &Database,
    writer: Option<&SharedWriter>,
    config: &SentinelConfig,
    dry_run: bool,
    label_filter: Option<&str>,
    quiet: bool,
) -> Result<CycleStats> {
    if !config.enabled {
        if !quiet {
            println!("sentinel is disabled");
        }
        return Ok(CycleStats::default());
    }

    if dry_run {
        return run_dry(config, quiet);
    }

    let run_id = Uuid::new_v4().to_string();
    db.insert_sentinel_run(&run_id, "oneshot")?;

    // 1. Initialize sources
    let mut sources: Vec<Box<dyn Source>> = Vec::new();
    if config.sources.github_labels.enabled {
        match GitHubLabelSource::new(config) {
            Ok(src) => sources.push(Box::new(src)),
            Err(e) => tracing::warn!("failed to initialize github-labels source: {e}"),
        }
    }
    if config.sources.internal_hygiene.enabled {
        let hygiene_config = super::sources::internal::InternalHygieneConfig {
            stale_threshold_days: config.sources.internal_hygiene.stale_threshold_days,
        };
        sources.push(Box::new(
            super::sources::internal::InternalHygieneSource::new(crosslink_dir, hygiene_config),
        ));
    }

    // 2. Load SeenSet
    let seen = SeenSet::load(db)?;

    // 3. Poll all sources
    let mut all_signals: Vec<Signal> = Vec::new();
    for source in &mut sources {
        match source.poll() {
            Ok(signals) => all_signals.extend(signals),
            Err(e) => tracing::warn!("source '{}' poll failed: {e}", source.name()),
        }
    }

    // Apply label filter if specified
    if let Some(filter) = label_filter {
        all_signals.retain(|s| {
            s.metadata
                .get("label")
                .and_then(|v| v.as_str())
                .is_some_and(|l| l.contains(filter))
        });
    }

    let mut stats = CycleStats {
        signals_found: all_signals.len() as u32,
        ..Default::default()
    };

    // 4. Triage and dispatch each signal
    for signal in &all_signals {
        // Layer 2: in-memory dedup
        let decision = seen.evaluate(&signal.reference, config);
        if let SignalDecision::Skip(reason) = &decision {
            if !quiet {
                println!("  skip: {} ({})", signal.reference, reason);
            }
            stats.skipped += 1;
            continue;
        }

        // Layer 3: authoritative DB dedup
        let gh_number = super::seen_set::parse_gh_issue_number(&signal.reference);
        let label_suffix = super::seen_set::parse_signal_label_suffix(&signal.reference);
        if let (Some(num), Some(label)) = (gh_number, label_suffix) {
            let full_label = format!("agent-todo: {label}");
            let db_decision = db_dedup_check(db, num, &full_label, config)?;
            if let SignalDecision::Skip(reason) = &db_decision {
                if !quiet {
                    println!("  skip: {} ({})", signal.reference, reason);
                }
                stats.skipped += 1;
                continue;
            }
            // If DB says Escalate but SeenSet said New, trust DB (it's authoritative)
            // This can happen if SeenSet was loaded before a previous cycle's dispatch was recorded
        }

        // 5. Check capacity
        let in_flight = db.count_pending_dispatches()?;
        if in_flight >= config.max_concurrent_agents as i64 {
            if !quiet {
                println!(
                    "  defer: {} (at capacity: {}/{})",
                    signal.reference, in_flight, config.max_concurrent_agents
                );
            }
            stats.deferred += 1;
            continue;
        }

        // 6. Triage
        let disposition = triage(signal, &decision, config);
        match disposition {
            super::dispatch::Disposition::Dispatch {
                description,
                scope,
                attempt,
            } => {
                if !quiet {
                    println!(
                        "  dispatch: {} (attempt {}, model: {})",
                        signal.reference, attempt, scope.model
                    );
                }
                // Create crosslink issue for this signal
                let issue_id = create_sentinel_issue(db, writer, signal)?;

                // Spawn agent via kickoff
                match spawn_agent(crosslink_dir, db, writer, &description, issue_id, &scope) {
                    Ok(agent_id) => {
                        db.insert_sentinel_dispatch(
                            &run_id,
                            &signal.reference,
                            &signal.title,
                            &format!("{:?}", signal.source),
                            "dispatch",
                            Some(&agent_id),
                            Some(issue_id),
                            gh_number,
                            signal
                                .metadata
                                .get("label")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown"),
                            attempt as i32,
                            Some(&scope.model),
                        )?;
                        stats.dispatched += 1;
                    }
                    Err(e) => {
                        tracing::error!("agent spawn failed for {}: {e}", signal.reference);
                        let dispatch_id = db.insert_sentinel_dispatch(
                            &run_id,
                            &signal.reference,
                            &signal.title,
                            &format!("{:?}", signal.source),
                            "dispatch",
                            None,
                            Some(issue_id),
                            gh_number,
                            signal
                                .metadata
                                .get("label")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown"),
                            attempt as i32,
                            Some(&scope.model),
                        )?;
                        db.update_dispatch_outcome(
                            dispatch_id,
                            "failure",
                            &format!("spawn failed: {e}"),
                        )?;
                    }
                }
            }
            super::dispatch::Disposition::Skip { reason } => {
                if !quiet {
                    println!("  skip: {} ({})", signal.reference, reason);
                }
                stats.skipped += 1;
            }
            super::dispatch::Disposition::Defer { reason } => {
                if !quiet {
                    println!("  defer: {} ({})", signal.reference, reason);
                }
                stats.deferred += 1;
            }
            super::dispatch::Disposition::Triage { .. } => {
                // Triage-only signals just get a crosslink issue, no agent
                let _issue_id = create_sentinel_issue(db, writer, signal)?;
                stats.skipped += 1;
            }
        }
    }

    // 7. Collect results from previously completed agents
    match collect::collect_completed(db, crosslink_dir) {
        Ok(collect_stats) => stats.collected = collect_stats.collected,
        Err(e) => tracing::warn!("result collection failed: {e}"),
    }

    // 8. Record run stats
    db.complete_sentinel_run(
        &run_id,
        stats.signals_found as i64,
        stats.dispatched as i64,
        stats.collected as i64,
        0, // triaged
        stats.skipped as i64,
        stats.deferred as i64,
    )?;

    if !quiet {
        println!(
            "{} signal(s) found, {} dispatched, {} skipped, {} deferred, {} collected",
            stats.signals_found, stats.dispatched, stats.skipped, stats.deferred, stats.collected,
        );
    }

    Ok(stats)
}

fn run_dry(config: &SentinelConfig, quiet: bool) -> Result<CycleStats> {
    if !quiet {
        println!("sentinel dry-run: would poll sources and dispatch agents");
        println!(
            "  sources: github-labels (labels: {:?})",
            config.sources.github_labels.labels
        );
        println!("  max concurrent agents: {}", config.max_concurrent_agents);
        println!("  default model: {}", config.default_agent.model);
        if config.escalation.enabled {
            println!(
                "  escalation: {} after {}m cooldown",
                config.escalation.model, config.escalation.cooldown_minutes
            );
        }
    }
    Ok(CycleStats::default())
}

/// Create a crosslink issue for a sentinel signal.
fn create_sentinel_issue(
    db: &Database,
    writer: Option<&SharedWriter>,
    signal: &Signal,
) -> Result<i64> {
    let description = format!(
        "Sentinel signal: {}\n\n{}",
        signal.reference,
        &signal.body[..signal.body.len().min(2000)]
    );
    let issue_id = if let Some(w) = writer {
        w.create_issue(db, &signal.title, Some(&description), "medium")?
    } else {
        db.create_issue(&signal.title, Some(&description), "medium")?
    };
    // Label the issue
    let label_fn = |label: &str| -> Result<()> {
        if let Some(w) = writer {
            w.add_label(db, issue_id, label)?;
        } else {
            db.add_label(issue_id, label)?;
        }
        Ok(())
    };
    let _ = label_fn("sentinel");
    let _ = label_fn("bug");
    Ok(issue_id)
}

/// Spawn a kickoff agent for a sentinel dispatch.
///
/// For fix dispatches (VerifyLevel::Ci), propagates GH_TOKEN so the agent
/// can push branches and create draft PRs.
fn spawn_agent(
    crosslink_dir: &Path,
    db: &Database,
    writer: Option<&SharedWriter>,
    description: &str,
    issue_id: i64,
    scope: &super::dispatch::AgentScope,
) -> Result<String> {
    use crate::commands::kickoff::{run, ContainerMode, KickoffOpts, VerifyLevel};

    // For Ci verify level, ensure GH_TOKEN is available so the agent can push + create PRs.
    // Read it from `gh auth token` and inject into the process environment.
    if scope.verify == VerifyLevel::Ci && std::env::var("GH_TOKEN").is_err() {
        if let Ok(output) = std::process::Command::new("gh")
            .args(["auth", "token"])
            .output()
        {
            if output.status.success() {
                let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !token.is_empty() {
                    std::env::set_var("GH_TOKEN", &token);
                }
            }
        }
    }

    let opts = KickoffOpts {
        description,
        issue: Some(issue_id),
        container: ContainerMode::None,
        verify: scope.verify.clone(),
        model: &scope.model,
        image: crate::commands::kickoff::DEFAULT_AGENT_IMAGE,
        timeout: scope.timeout,
        dry_run: false,
        branch: None,
        quiet: true,
        design_doc: None,
        doc_path: None,
        skip_permissions: true,
    };

    run(crosslink_dir, db, writer, &opts)
}
