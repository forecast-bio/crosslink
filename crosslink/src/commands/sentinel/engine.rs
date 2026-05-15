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
        return Ok(run_dry(config, quiet));
    }

    // Poll all configured sources to gather signals
    let all_signals = poll_all_sources(crosslink_dir, config, label_filter);

    process_signal_batch(
        crosslink_dir,
        db,
        writer,
        config,
        &all_signals,
        "oneshot",
        quiet,
    )
}

/// Poll all configured sources and return their combined signals.
/// Applies the optional label filter (exact suffix match).
fn poll_all_sources(
    crosslink_dir: &Path,
    config: &SentinelConfig,
    label_filter: Option<&str>,
) -> Vec<Signal> {
    let mut sources: Vec<Box<dyn Source>> = Vec::new();
    if config.sources.github_labels.enabled {
        sources.push(Box::new(GitHubLabelSource::new(config)));
    }
    if config.sources.github_ci.enabled {
        sources.push(Box::new(super::sources::ci::GitHubCISource::new()));
    }
    if config.sources.internal_hygiene.enabled {
        let hygiene_config = super::sources::internal::InternalHygieneConfig {
            stale_threshold_days: config.sources.internal_hygiene.stale_threshold_days,
        };
        sources.push(Box::new(
            super::sources::internal::InternalHygieneSource::new(crosslink_dir, hygiene_config),
        ));
    }
    if config.sources.maintenance_sweep.enabled {
        let sweep_config = super::sources::maintenance::MaintenanceSweepConfig {
            lint_enabled: config.sources.maintenance_sweep.lint_enabled,
            test_coverage_enabled: config.sources.maintenance_sweep.test_coverage_enabled,
            lint_warning_threshold: config.sources.maintenance_sweep.lint_warning_threshold,
        };
        if let Ok(root) = resolve_repo_root(crosslink_dir) {
            sources.push(Box::new(
                super::sources::maintenance::MaintenanceSweepSource::new(&root, sweep_config),
            ));
        }
    }

    let mut all_signals: Vec<Signal> = Vec::new();
    for source in &mut sources {
        match source.poll() {
            Ok(signals) => all_signals.extend(signals),
            Err(e) => tracing::warn!("source '{}' poll failed: {e}", source.name()),
        }
    }

    if let Some(filter) = label_filter {
        all_signals.retain(|s| {
            s.metadata
                .get("label")
                .and_then(|v| v.as_str())
                .is_some_and(|l| l == filter || l.ends_with(&format!(": {filter}")))
        });
    }

    all_signals
}

/// Process a pre-built batch of signals through the dedup/triage/dispatch pipeline.
///
/// Used by both `run_oneshot` (after polling sources) and `webhook` (for real-time
/// events that arrive between polling cycles).
pub fn process_signal_batch(
    crosslink_dir: &Path,
    db: &Database,
    writer: Option<&SharedWriter>,
    config: &SentinelConfig,
    all_signals: &[Signal],
    mode: &str,
    quiet: bool,
) -> Result<CycleStats> {
    let run_id = Uuid::new_v4().to_string();
    db.insert_sentinel_run(&run_id, mode)?;

    // Load SeenSet
    let seen = SeenSet::load(db)?;

    // Load self-tuning overrides from historical success rates
    let tuning = if config.escalation.enabled {
        super::tuning::TuningOverride::from_history(db, config).unwrap_or_else(|e| {
            tracing::warn!("self-tuning load failed: {e}");
            super::tuning::TuningOverride::none()
        })
    } else {
        super::tuning::TuningOverride::none()
    };
    if tuning.has_overrides() && !quiet {
        println!("  self-tuning: model overrides active based on historical data");
    }

    let mut stats = CycleStats {
        signals_found: all_signals.len() as u32,
        ..Default::default()
    };

    // 4. Triage and dispatch each signal
    for signal in all_signals {
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

        // 5. Triage
        let mut disposition = triage(signal, &decision, config, Some(&tuning));

        // 6. Capacity check: if triage decided to dispatch but we're at capacity,
        //    override to Defer so the signal is retried on the next cycle.
        if matches!(disposition, super::dispatch::Disposition::Dispatch { .. }) {
            let in_flight = db.count_pending_dispatches()?;
            if in_flight >= config.max_concurrent_agents as i64 {
                disposition = super::dispatch::Disposition::Defer {
                    reason: format!(
                        "at capacity: {}/{}",
                        in_flight, config.max_concurrent_agents
                    ),
                };
            }
        }

        match disposition {
            super::dispatch::Disposition::Dispatch {
                description,
                scope,
                attempt,
            } => {
                if !quiet {
                    println!(
                        "  dispatch: {} [{:?}] (attempt {}, model: {}, detected: {})",
                        signal.reference,
                        signal.kind,
                        attempt,
                        scope.model,
                        signal.detected_at.format("%H:%M:%S")
                    );
                }
                // Create crosslink issue for this signal
                let issue_id = create_sentinel_issue(db, writer, signal)?;

                // Spawn agent via kickoff
                let source_str = format!("{:?}", signal.source);
                let label_str = signal
                    .metadata
                    .get("label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");

                match spawn_agent(crosslink_dir, db, writer, &description, issue_id, &scope) {
                    Ok(agent_id) => {
                        db.insert_sentinel_dispatch(&crate::db::sentinel::NewDispatch {
                            run_id: &run_id,
                            signal_ref: &signal.reference,
                            signal_title: &signal.title,
                            source: &source_str,
                            disposition: "dispatch",
                            agent_id: Some(&agent_id),
                            crosslink_issue_id: Some(issue_id),
                            gh_issue_number: gh_number,
                            label: label_str,
                            attempt_number: attempt as i32,
                            model_used: Some(&scope.model),
                        })?;
                        stats.dispatched += 1;
                    }
                    Err(e) => {
                        tracing::error!("agent spawn failed for {}: {e}", signal.reference);
                        let dispatch_id =
                            db.insert_sentinel_dispatch(&crate::db::sentinel::NewDispatch {
                                run_id: &run_id,
                                signal_ref: &signal.reference,
                                signal_title: &signal.title,
                                source: &source_str,
                                disposition: "dispatch",
                                agent_id: None,
                                crosslink_issue_id: Some(issue_id),
                                gh_issue_number: gh_number,
                                label: label_str,
                                attempt_number: attempt as i32,
                                model_used: Some(&scope.model),
                            })?;
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
            super::dispatch::Disposition::Triage { priority, labels } => {
                // Triage-only signals get a crosslink issue with priority + labels
                let issue_id = create_sentinel_issue(db, writer, signal)?;
                let _ = db.update_issue(issue_id, None, None, Some(&priority));
                for l in &labels {
                    if let Some(w) = writer {
                        let _ = w.add_label(db, issue_id, l);
                    } else {
                        let _ = db.add_label(issue_id, l);
                    }
                }
                if !quiet {
                    println!(
                        "  triage: {} (priority: {}, labels: {})",
                        signal.reference,
                        priority,
                        labels.join(", ")
                    );
                }
                stats.skipped += 1;
            }
        }
    }

    // 7. Collect results from previously completed agents
    match collect::collect_completed(db, crosslink_dir, Some(config)) {
        Ok(collect_stats) => stats.collected = collect_stats.collected,
        Err(e) => tracing::warn!("result collection failed: {e}"),
    }

    // 8. Record run stats
    db.complete_sentinel_run(
        &run_id,
        &crate::db::sentinel::RunCounters {
            signals_found: i64::from(stats.signals_found),
            dispatched: i64::from(stats.dispatched),
            collected: i64::from(stats.collected),
            triaged: 0,
            skipped: i64::from(stats.skipped),
            deferred: i64::from(stats.deferred),
        },
    )?;

    if !quiet {
        println!(
            "{} signal(s) found, {} dispatched, {} skipped, {} deferred, {} collected",
            stats.signals_found, stats.dispatched, stats.skipped, stats.deferred, stats.collected,
        );
    }

    Ok(stats)
}

fn run_dry(config: &SentinelConfig, quiet: bool) -> CycleStats {
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
    CycleStats::default()
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
        w.create_issue(db, &signal.title, Some(&description), "medium", None, None)?
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
/// For fix dispatches (`VerifyLevel::Ci`), propagates `GH_TOKEN` so the agent
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
        match std::process::Command::new("gh")
            .args(["auth", "token"])
            .output()
        {
            Ok(output) if output.status.success() => {
                let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !token.is_empty() {
                    std::env::set_var("GH_TOKEN", &token);
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!("gh auth token failed: {}", stderr.trim());
            }
            Err(e) => {
                tracing::warn!("failed to run gh auth token: {e}");
            }
        }
    }

    // Append a strict path-enforcement section so the agent honors AgentScope.allowed_paths
    // even if the prompt template's natural language is ambiguous.
    let scoped_description = format!(
        "{description}\n\n## Path Enforcement (sentinel scope)\n\
         You may ONLY create or modify files under these path prefixes:\n{}\n\
         Modifying files outside these prefixes is a contract violation.",
        scope
            .allowed_paths
            .iter()
            .map(|p| format!("- `{p}`"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let opts = KickoffOpts {
        description: &scoped_description,
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
        permission_mode: None,
    };

    run(crosslink_dir, db, writer, &opts)
}

/// Resolve the repo root from a crosslink directory.
fn resolve_repo_root(crosslink_dir: &Path) -> Result<std::path::PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(crosslink_dir)
        .output()?;
    if !output.status.success() {
        anyhow::bail!("Not in a git repository");
    }
    Ok(std::path::PathBuf::from(
        String::from_utf8_lossy(&output.stdout).trim(),
    ))
}
