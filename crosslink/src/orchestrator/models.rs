//! Domain types for LLM-assisted document decomposition.
//!
//! These types are the raw schema expected from the LLM (Claude CLI) output.
//! [`crate::orchestrator::decompose`] transforms them into the API-facing
//! [`OrchestratorPlan`](crate::server::types::OrchestratorPlan) types.
//!
//! They are also used for on-disk storage in `.crosslink/orchestrator/`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// LLM response schema
// ---------------------------------------------------------------------------

/// A task as returned by the LLM decomposition prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmTask {
    pub title: String,
    pub description: String,
    /// Estimated complexity in agent-hours.
    #[serde(default)]
    pub complexity_hours: f64,
}

/// A stage as returned by the LLM decomposition prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmStage {
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub tasks: Vec<LlmTask>,
    /// Titles or IDs of stages this stage depends on.
    #[serde(default)]
    pub depends_on: Vec<String>,
    /// Suggested number of parallel agents for this stage.
    #[serde(default = "default_agent_count")]
    pub agent_count: usize,
    #[serde(default)]
    pub complexity_hours: f64,
}

/// A phase as returned by the LLM decomposition prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmPhase {
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub stages: Vec<LlmStage>,
    /// Criteria for declaring this phase complete.
    #[serde(default)]
    pub gate_criteria: Vec<String>,
}

/// The top-level LLM response.
///
/// This is the JSON object we instruct the LLM to produce. All fields must
/// tolerate defaults so we can recover from partial or unexpected output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmDecomposeResponse {
    pub phases: Vec<LlmPhase>,
    /// Total estimated agent-hours across all phases.
    #[serde(default)]
    pub estimated_hours: f64,
}

fn default_agent_count() -> usize {
    1
}

// ---------------------------------------------------------------------------
// Plan storage
// ---------------------------------------------------------------------------

/// On-disk representation of a stored plan (`.crosslink/orchestrator/<id>.json`).
///
/// This wraps the API-facing `OrchestratorPlan` with metadata for filesystem
/// storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredPlan {
    /// The plan itself (same shape as the API response).
    pub plan: crate::server::types::OrchestratorPlan,
    /// The raw markdown document that was decomposed.
    pub source_document: String,
    /// When the plan was stored.
    pub stored_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_response_deserializes_minimal() {
        let json = r#"{
            "phases": [
                {
                    "title": "Phase 1",
                    "stages": [
                        {
                            "title": "Stage A",
                            "description": "Do stuff"
                        }
                    ]
                }
            ]
        }"#;
        let resp: LlmDecomposeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.phases.len(), 1);
        assert_eq!(resp.phases[0].stages.len(), 1);
        assert_eq!(resp.phases[0].stages[0].agent_count, 1);
        assert!(resp.phases[0].stages[0].depends_on.is_empty());
    }

    #[test]
    fn llm_response_deserializes_full() {
        let json = r#"{
            "phases": [
                {
                    "title": "Phase 1",
                    "description": "First phase",
                    "stages": [
                        {
                            "title": "Stage A",
                            "description": "First stage",
                            "tasks": [
                                {"title": "Task 1", "description": "Do thing", "complexity_hours": 1.5}
                            ],
                            "depends_on": [],
                            "agent_count": 2,
                            "complexity_hours": 3.0
                        },
                        {
                            "title": "Stage B",
                            "description": "Second stage",
                            "tasks": [],
                            "depends_on": ["Stage A"],
                            "agent_count": 1,
                            "complexity_hours": 2.0
                        }
                    ],
                    "gate_criteria": ["All tests pass"]
                }
            ],
            "estimated_hours": 5.0
        }"#;
        let resp: LlmDecomposeResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.estimated_hours, 5.0);
        assert_eq!(resp.phases[0].stages[0].agent_count, 2);
        assert_eq!(resp.phases[0].stages[1].depends_on, vec!["Stage A"]);
        assert_eq!(
            resp.phases[0].gate_criteria,
            vec!["All tests pass".to_string()]
        );
    }

    #[test]
    fn stored_plan_round_trip() {
        use crate::server::types::OrchestratorPlan;
        let plan = StoredPlan {
            plan: OrchestratorPlan {
                id: "test-plan".to_string(),
                document_slug: "test-doc".to_string(),
                phases: vec![],
                created_at: Utc::now(),
                total_stages: 0,
                estimated_hours: 0.0,
            },
            source_document: "# Test Doc\n\nHello".to_string(),
            stored_at: Utc::now(),
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: StoredPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.plan.id, "test-plan");
        assert_eq!(parsed.source_document, "# Test Doc\n\nHello");
    }
}
