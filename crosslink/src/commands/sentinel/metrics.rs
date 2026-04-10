use anyhow::Result;

use crate::db::Database;

/// Display dispatch success rate metrics grouped by model and label.
pub fn show_metrics(db: &Database, json: bool) -> Result<()> {
    let metrics = db.get_dispatch_metrics()?;

    if metrics.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No dispatch data recorded yet.");
        }
        return Ok(());
    }

    if json {
        let json_str = serde_json::to_string_pretty(&metrics)?;
        println!("{json_str}");
        return Ok(());
    }

    println!(
        "{:<30}  {:<14}  {:>5}  {:>4}  {:>4}  {:>4}  {:>7}  {:>8}",
        "Label", "Model", "Total", "Pass", "Fail", "Exh.", "Pending", "Rate"
    );
    println!("{}", "-".repeat(95));

    for m in &metrics {
        let rate_str = if m.total - m.pending > 0 {
            format!("{:.0}%", m.success_rate)
        } else {
            "-".to_string()
        };
        println!(
            "{:<30}  {:<14}  {:>5}  {:>4}  {:>4}  {:>4}  {:>7}  {:>8}",
            truncate(&m.label, 30),
            truncate(&m.model, 14),
            m.total,
            m.successes,
            m.failures,
            m.exhausted,
            m.pending,
            rate_str,
        );
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}
