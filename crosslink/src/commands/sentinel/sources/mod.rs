use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod ci;
pub mod github;
pub mod internal;
pub mod maintenance;

/// Classification of where a signal originated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceKind {
    GitHub,
    Internal,
    CI,
}

/// Classification of the signal event type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignalKind {
    LabelAdded,
    StaleIssue,
    CIFailure,
}

/// A maintenance signal detected by a source adapter.
#[derive(Debug, Clone)]
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
pub enum SignalDecision {
    /// Never seen before — dispatch with Sonnet (attempt 1).
    New,
    /// Previous attempt failed — dispatch with Opus (attempt 2).
    Escalate,
    /// Already handled or ineligible — do not dispatch.
    Skip(&'static str),
}

/// A source adapter that polls for maintenance signals.
pub trait Source {
    /// Human-readable name for logging.
    fn name(&self) -> &'static str;

    /// Poll for new signals. The implementation should use the `SeenSet`
    /// passed by the engine to pre-filter already-handled signals.
    fn poll(&mut self) -> Result<Vec<Signal>>;
}
