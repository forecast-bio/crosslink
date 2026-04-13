use anyhow::Result;
use serde::Serialize;

use crate::db::sentinel::{SentinelDispatch, SentinelRun};
use crate::db::Database;

/// JSON-serializable view of a run with its dispatches (for --detail --json).
#[derive(Serialize)]
struct RunWithDispatches {
    #[serde(flatten)]
    run: SentinelRun,
    dispatches: Vec<SentinelDispatch>,
}

/// Display past sentinel runs and their dispatch outcomes.
pub fn show_history(db: &Database, limit: usize, detail: bool, json: bool) -> Result<()> {
    let runs = db.list_sentinel_runs(limit)?;

    if runs.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No sentinel runs recorded yet.");
        }
        return Ok(());
    }

    if json {
        if detail {
            let with_details: Vec<RunWithDispatches> = runs
                .into_iter()
                .map(|run| {
                    let dispatches = db.list_dispatches_for_run(&run.run_id).unwrap_or_default();
                    RunWithDispatches { run, dispatches }
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&with_details)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&runs)?);
        }
        return Ok(());
    }

    // Table header
    println!(
        "{:<36}  {:<20}  {:>7}  {:>10}  {:>9}  {:>7}  {:>7}",
        "Run", "Started", "Signals", "Dispatched", "Collected", "Skipped", "Deferred"
    );
    println!("{}", "-".repeat(105));

    for run in &runs {
        let started = run
            .started_at
            .get(..19)
            .unwrap_or(&run.started_at)
            .replace('T', " ");
        let run_id_short = run.run_id.get(..12).unwrap_or(&run.run_id);
        println!(
            "{:<36}  {:<20}  {:>7}  {:>10}  {:>9}  {:>7}  {:>7}",
            run_id_short,
            started,
            run.signals_found,
            run.dispatched,
            run.collected,
            run.skipped,
            run.deferred,
        );

        if detail {
            let dispatches = db.list_dispatches_for_run(&run.run_id)?;
            if dispatches.is_empty() {
                println!("    (no dispatches)");
            } else {
                for d in &dispatches {
                    let outcome_icon = match d.outcome.as_str() {
                        "success" => "+",
                        "failure" => "x",
                        "exhausted" => "X",
                        "pending" => ".",
                        "orphaned" => "?",
                        _ => "-",
                    };
                    let agent = d.agent_id.as_deref().unwrap_or("(none)");
                    let model = d.model_used.as_deref().unwrap_or("?");
                    println!(
                        "    [{}] {} {} attempt={} model={} outcome={}",
                        outcome_icon, d.signal_ref, agent, d.attempt_number, model, d.outcome
                    );
                }
            }
            println!();
        }
    }

    Ok(())
}
