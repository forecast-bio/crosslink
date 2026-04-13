use anyhow::Result;
use std::collections::HashMap;

use crate::db::Database;

use super::config::SentinelConfig;

/// Model override recommendation from historical data.
#[derive(Debug, Clone)]
pub struct TuningOverride {
    /// label -> recommended model (if different from default)
    overrides: HashMap<String, String>,
}

impl TuningOverride {
    /// Analyze dispatch history and recommend model overrides for labels
    /// where the default model (Sonnet) has a success rate below the threshold.
    pub fn from_history(db: &Database, config: &SentinelConfig) -> Result<Self> {
        let metrics = db.get_dispatch_metrics()?;
        let mut overrides = HashMap::new();

        let default_model = &config.default_agent.model;
        let escalation_model = &config.escalation.model;
        let threshold = 40.0; // promote to Opus if Sonnet success rate < 40%

        for m in &metrics {
            // Only look at the default model's performance
            if m.model != *default_model {
                continue;
            }
            let completed = m.total - m.pending;
            if completed < 5 {
                continue; // not enough data to be confident
            }
            if m.success_rate < threshold {
                tracing::info!(
                    "self-tuning: promoting '{}' from {} to {} (success rate {:.0}% < {:.0}%)",
                    m.label,
                    default_model,
                    escalation_model,
                    m.success_rate,
                    threshold
                );
                overrides.insert(m.label.clone(), escalation_model.clone());
            }
        }

        Ok(Self { overrides })
    }

    /// Get the model to use for a given label, or None if no override.
    pub fn model_for_label(&self, label: &str) -> Option<&str> {
        self.overrides.get(label).map(String::as_str)
    }

    /// Check if any overrides are active.
    pub fn has_overrides(&self) -> bool {
        !self.overrides.is_empty()
    }

    /// Empty tuning — no overrides.
    pub fn none() -> Self {
        Self {
            overrides: HashMap::new(),
        }
    }
}
