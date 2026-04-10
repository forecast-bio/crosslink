use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
pub mod github;

/// Classification of where a signal originated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum SourceKind {
    GitHub,
    Internal,
    CI,
}

/// Classification of the signal event type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum SignalKind {
    LabelAdded,
    StaleIssue,
    CIFailure,
}

/// A maintenance signal detected by a source adapter.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Signal {
    pub source: SourceKind,
    pub kind: SignalKind,
    /// Composite reference: "GH#499:replicate", "GH#499:fix"
    pub reference: String,
    pub title: String,
    pub body: String,
    pub metadata: serde_json::Value,
    pub detected_at: DateTime<Utc>,
}

/// Dedup decision for a signal based on prior dispatch history.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SignalDecision {
    /// Never seen before — dispatch with Sonnet (attempt 1).
    New,
    /// Previous attempt failed — dispatch with Opus (attempt 2).
    Escalate,
    /// Already handled or ineligible — do not dispatch.
    Skip(&'static str),
}

/// A source adapter that polls for maintenance signals.
#[allow(dead_code)]
pub trait Source {
    /// Human-readable name for logging.
    fn name(&self) -> &str;

    /// Poll for new signals. The implementation should use the `SeenSet`
    /// passed by the engine to pre-filter already-handled signals.
    fn poll(&mut self) -> Result<Vec<Signal>>;
}
