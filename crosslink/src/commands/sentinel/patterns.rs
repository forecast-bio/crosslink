use anyhow::Result;

use crate::db::Database;

/// A detected pattern from dispatch history.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Pattern {
    pub kind: String,
    pub description: String,
    pub signal_refs: Vec<String>,
    pub count: i64,
    pub severity: String,
}

/// Analyze dispatch history for recurring patterns and hotspots.
pub fn detect_patterns(db: &Database, json: bool) -> Result<()> {
    let mut patterns: Vec<Pattern> = Vec::new();

    patterns.extend(find_repeat_failures(db)?);
    patterns.extend(find_label_success_imbalance(db)?);
    patterns.extend(find_escalation_heavy_signals(db)?);

    if patterns.is_empty() {
        if json {
            println!("[]");
        } else {
            println!("No patterns detected yet. Need more dispatch history.");
        }
        return Ok(());
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&patterns)?);
        return Ok(());
    }

    for pattern in &patterns {
        let icon = match pattern.severity.as_str() {
            "high" => "!!",
            "medium" => " !",
            _ => "  ",
        };
        println!(
            "[{}] {} ({}x): {}",
            icon, pattern.kind, pattern.count, pattern.description
        );
        if !pattern.signal_refs.is_empty() {
            let refs = pattern
                .signal_refs
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            println!("     signals: {refs}");
        }
    }

    Ok(())
}

/// Signals that have failed 2+ times (both Sonnet and Opus exhausted multiple times).
fn find_repeat_failures(db: &Database) -> Result<Vec<Pattern>> {
    let mut stmt = db.conn.prepare(
        "SELECT signal_ref, COUNT(*) as fail_count
         FROM sentinel_dispatches
         WHERE outcome IN ('failure', 'exhausted')
         GROUP BY signal_ref
         HAVING fail_count >= 2
         ORDER BY fail_count DESC
         LIMIT 10",
    )?;

    let rows: Vec<(String, i64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .filter_map(Result::ok)
        .collect();

    if rows.is_empty() {
        return Ok(Vec::new());
    }

    Ok(vec![Pattern {
        kind: "repeat-failures".to_string(),
        description: format!(
            "{} signal(s) have failed multiple times despite escalation",
            rows.len()
        ),
        signal_refs: rows.iter().map(|(r, _)| r.clone()).collect(),
        count: rows.iter().map(|(_, c)| c).sum(),
        severity: "high".to_string(),
    }])
}

/// Labels where the success rate is significantly below average.
fn find_label_success_imbalance(db: &Database) -> Result<Vec<Pattern>> {
    let metrics = db.get_dispatch_metrics()?;

    let mut patterns = Vec::new();
    for m in &metrics {
        let completed = m.total - m.pending;
        if completed < 3 {
            continue; // not enough data
        }
        if m.success_rate < 30.0 {
            patterns.push(Pattern {
                kind: "low-success-rate".to_string(),
                description: format!(
                    "'{}' with {} has only {:.0}% success rate ({}/{} completed)",
                    m.label, m.model, m.success_rate, m.successes, completed
                ),
                signal_refs: Vec::new(),
                count: completed,
                severity: if m.success_rate < 10.0 {
                    "high".to_string()
                } else {
                    "medium".to_string()
                },
            });
        }
    }

    Ok(patterns)
}

/// Signals that always escalate from Sonnet to Opus (Sonnet never succeeds).
fn find_escalation_heavy_signals(db: &Database) -> Result<Vec<Pattern>> {
    let mut stmt = db.conn.prepare(
        "SELECT label,
                SUM(CASE WHEN attempt_number = 1 AND outcome = 'failure' THEN 1 ELSE 0 END) as sonnet_fails,
                SUM(CASE WHEN attempt_number = 2 THEN 1 ELSE 0 END) as opus_attempts,
                COUNT(*) as total
         FROM sentinel_dispatches
         WHERE disposition = 'dispatch'
         GROUP BY label
         HAVING total >= 4 AND sonnet_fails > opus_attempts * 0.8",
    )?;

    let rows: Vec<(String, i64, i64, i64)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .filter_map(Result::ok)
        .collect();

    let mut patterns = Vec::new();
    for (label, sonnet_fails, opus_attempts, _total) in &rows {
        patterns.push(Pattern {
            kind: "escalation-heavy".to_string(),
            description: format!(
                "'{label}': Sonnet failed {sonnet_fails}x, escalated to Opus {opus_attempts}x — consider defaulting to Opus"
            ),
            signal_refs: Vec::new(),
            count: *sonnet_fails,
            severity: "medium".to_string(),
        });
    }

    Ok(patterns)
}
