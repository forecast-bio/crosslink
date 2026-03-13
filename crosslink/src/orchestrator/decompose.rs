//! LLM-assisted document decomposition.
//!
//! Accepts a markdown design document, calls the Claude CLI with a structured
//! prompt requesting JSON output, and transforms the result into an
//! [`OrchestratorPlan`](crate::server::types::OrchestratorPlan).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::orchestrator::models::{LlmDecomposeResponse, StoredPlan};
use crate::server::types::{
    OrchestratorPhase, OrchestratorPlan, OrchestratorStage, OrchestratorTask,
};

// ---------------------------------------------------------------------------
// Prompt
// ---------------------------------------------------------------------------

/// Build the system prompt instructing the LLM to decompose a design document.
fn build_system_prompt() -> &'static str {
    concat!(
        "You are a software architecture decomposition engine. ",
        "Your task is to analyze a design document and produce a structured ",
        "execution plan as a JSON object.\n\n",
        "Output ONLY valid JSON — no markdown fences, no commentary, no explanation.\n\n",
        "The JSON schema is:\n",
        "{\n",
        "  \"phases\": [\n",
        "    {\n",
        "      \"title\": \"Phase N: <name>\",\n",
        "      \"description\": \"<what this phase achieves>\",\n",
        "      \"stages\": [\n",
        "        {\n",
        "          \"title\": \"<stage name>\",\n",
        "          \"description\": \"<detailed description of work>\",\n",
        "          \"tasks\": [\n",
        "            {\n",
        "              \"title\": \"<atomic task>\",\n",
        "              \"description\": \"<what to implement>\",\n",
        "              \"complexity_hours\": <number>\n",
        "            }\n",
        "          ],\n",
        "          \"depends_on\": [\"<title of stage this depends on>\"],\n",
        "          \"agent_count\": <suggested parallel agents>,\n",
        "          \"complexity_hours\": <total for this stage>\n",
        "        }\n",
        "      ],\n",
        "      \"gate_criteria\": [\"<criteria for phase completion>\"]\n",
        "    }\n",
        "  ],\n",
        "  \"estimated_hours\": <total across all phases>\n",
        "}\n\n",
        "Rules:\n",
        "- Phases are major sequential milestones\n",
        "- Stages within a phase may be parallelized if they have no mutual dependencies\n",
        "- Tasks are atomic work items within a stage\n",
        "- depends_on references stage titles from the SAME or EARLIER phases\n",
        "- complexity_hours is estimated agent-hours (one agent working)\n",
        "- agent_count is the suggested number of parallel agents for a stage\n",
        "- gate_criteria describe what must be true before advancing to the next phase\n",
        "- Keep stage count reasonable (2-6 per phase)\n",
        "- Every stage must have at least one task\n",
    )
}

/// Build the user prompt containing the document to decompose.
fn build_user_prompt(document: &str) -> String {
    format!(
        "Decompose the following design document into a phased execution plan.\n\n\
         ---BEGIN DOCUMENT---\n\
         {document}\n\
         ---END DOCUMENT---"
    )
}

// ---------------------------------------------------------------------------
// Claude CLI invocation
// ---------------------------------------------------------------------------

/// Call the `claude` CLI to decompose a document.
///
/// This runs `claude -p <prompt> --output-format json` as a subprocess.
/// The Claude CLI must be available on `$PATH`.
///
/// Returns the raw stdout as a string on success.
async fn call_claude_cli(document: &str) -> Result<String> {
    let system_prompt = build_system_prompt();
    let user_prompt = build_user_prompt(document);

    // Combine into a single prompt since `claude -p` takes one prompt argument.
    let full_prompt = format!("{system_prompt}\n\n---\n\n{user_prompt}");

    let output = tokio::process::Command::new("claude")
        .arg("-p")
        .arg(&full_prompt)
        .arg("--output-format")
        .arg("json")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn `claude` CLI — is it installed and on $PATH?")?
        .wait_with_output()
        .await
        .context("Failed to read `claude` CLI output")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`claude` CLI exited with status {}: {}",
            output.status,
            stderr.trim()
        );
    }

    let stdout =
        String::from_utf8(output.stdout).context("`claude` CLI produced non-UTF-8 output")?;

    Ok(stdout)
}

// ---------------------------------------------------------------------------
// Response parsing
// ---------------------------------------------------------------------------

/// Extract JSON from the Claude CLI response.
///
/// The `--output-format json` flag wraps the response in a JSON envelope with
/// a `result` field. We try to parse that envelope first, falling back to
/// direct JSON parsing if the output is raw JSON.
fn extract_json_from_response(raw: &str) -> Result<String> {
    let trimmed = raw.trim();

    // Try the Claude CLI JSON envelope: {"type":"result","result":"<json>", ...}
    if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if let Some(result_text) = envelope.get("result").and_then(|v| v.as_str()) {
            // The result field contains the LLM's text output — extract JSON from it
            return extract_json_block(result_text);
        }
    }

    // Fallback: maybe it's already raw JSON matching our schema
    extract_json_block(trimmed)
}

/// Find and extract a JSON object from text that may contain surrounding prose.
///
/// Looks for the first `{` and last `}` to extract the JSON block.
fn extract_json_block(text: &str) -> Result<String> {
    let trimmed = text.trim();

    // Strip markdown code fences if present
    let cleaned = if trimmed.starts_with("```") {
        let start = trimmed.find('\n').map(|i| i + 1).unwrap_or(0);
        let end = trimmed.rfind("```").unwrap_or(trimmed.len());
        &trimmed[start..end]
    } else {
        trimmed
    };

    // Find the JSON object boundaries
    let start = cleaned
        .find('{')
        .context("LLM response does not contain a JSON object")?;
    let end = cleaned
        .rfind('}')
        .context("LLM response does not contain a closing brace")?;

    if end <= start {
        bail!("Malformed JSON in LLM response: closing brace before opening brace");
    }

    Ok(cleaned[start..=end].to_string())
}

/// Parse the extracted JSON into our LLM response type.
fn parse_llm_response(json_str: &str) -> Result<LlmDecomposeResponse> {
    serde_json::from_str(json_str).context("Failed to parse LLM JSON response into expected schema")
}

// ---------------------------------------------------------------------------
// Transform LLM response → API types
// ---------------------------------------------------------------------------

/// Convert an LLM decomposition response into an API-facing `OrchestratorPlan`.
///
/// Generates stable IDs for each phase/stage/task and computes aggregate
/// statistics.
fn transform_to_plan(response: LlmDecomposeResponse, slug: &str) -> OrchestratorPlan {
    let mut total_stages = 0usize;
    let plan_id = Uuid::new_v4().to_string();

    let phases: Vec<OrchestratorPhase> = response
        .phases
        .into_iter()
        .enumerate()
        .map(|(pi, phase)| {
            let phase_id = format!("{plan_id}-p{pi}");
            let stages: Vec<OrchestratorStage> = phase
                .stages
                .into_iter()
                .enumerate()
                .map(|(si, stage)| {
                    total_stages += 1;
                    let stage_id = format!("{phase_id}-s{si}");
                    let tasks: Vec<OrchestratorTask> = stage
                        .tasks
                        .into_iter()
                        .enumerate()
                        .map(|(ti, task)| OrchestratorTask {
                            id: format!("{stage_id}-t{ti}"),
                            title: task.title,
                            description: task.description,
                            complexity_hours: task.complexity_hours,
                        })
                        .collect();
                    OrchestratorStage {
                        id: stage_id,
                        title: stage.title,
                        description: stage.description,
                        tasks,
                        depends_on: stage.depends_on,
                        agent_count: stage.agent_count,
                        complexity_hours: stage.complexity_hours,
                    }
                })
                .collect();
            OrchestratorPhase {
                id: phase_id,
                title: phase.title,
                description: phase.description,
                stages,
                gate_criteria: phase.gate_criteria,
            }
        })
        .collect();

    OrchestratorPlan {
        id: plan_id,
        document_slug: slug.to_string(),
        phases,
        created_at: Utc::now(),
        total_stages,
        estimated_hours: response.estimated_hours,
    }
}

// ---------------------------------------------------------------------------
// Plan storage
// ---------------------------------------------------------------------------

/// Directory within `.crosslink/` where orchestrator plans are stored.
const PLANS_DIR: &str = "orchestrator";

/// Ensure the orchestrator storage directory exists.
fn ensure_plans_dir(crosslink_dir: &Path) -> Result<PathBuf> {
    let dir = crosslink_dir.join(PLANS_DIR);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create orchestrator directory: {}", dir.display()))?;
    Ok(dir)
}

/// Store a plan on disk and return its file path.
fn store_plan(
    crosslink_dir: &Path,
    plan: &OrchestratorPlan,
    source_document: &str,
) -> Result<PathBuf> {
    let dir = ensure_plans_dir(crosslink_dir)?;
    let file_name = format!("{}.json", plan.id);
    let path = dir.join(&file_name);

    let stored = StoredPlan {
        plan: plan.clone(),
        source_document: source_document.to_string(),
        stored_at: Utc::now(),
    };

    let json =
        serde_json::to_string_pretty(&stored).context("Failed to serialize plan for storage")?;
    std::fs::write(&path, json)
        .with_context(|| format!("Failed to write plan to {}", path.display()))?;

    Ok(path)
}

/// Load a stored plan from disk by its ID.
pub fn load_plan(crosslink_dir: &Path, plan_id: &str) -> Result<StoredPlan> {
    let dir = crosslink_dir.join(PLANS_DIR);
    let path = dir.join(format!("{plan_id}.json"));
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Plan not found: {}", path.display()))?;
    serde_json::from_str(&content).context("Failed to parse stored plan")
}

/// List all stored plan IDs.
pub fn list_plans(crosslink_dir: &Path) -> Result<Vec<String>> {
    let dir = crosslink_dir.join(PLANS_DIR);
    if !dir.exists() {
        return Ok(vec![]);
    }
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&dir).context("Failed to read orchestrator directory")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".json") {
            ids.push(name.trim_end_matches(".json").to_string());
        }
    }
    ids.sort();
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Decompose a design document into an orchestrator plan.
///
/// This is the main entry point called by the HTTP handler. It:
/// 1. Calls the Claude CLI with the document
/// 2. Parses the JSON response
/// 3. Transforms it into an `OrchestratorPlan`
/// 4. Stores the plan on disk
/// 5. Returns the plan
pub async fn decompose_document(
    crosslink_dir: &Path,
    document: &str,
    slug: Option<&str>,
) -> Result<OrchestratorPlan> {
    if document.trim().is_empty() {
        bail!("Document is empty");
    }

    let effective_slug = slug.unwrap_or("untitled");

    // Call the LLM
    let raw_response = call_claude_cli(document).await?;

    // Extract and parse JSON
    let json_str = extract_json_from_response(&raw_response)?;
    let llm_response = parse_llm_response(&json_str)?;

    // Validate: at least one phase with at least one stage
    if llm_response.phases.is_empty() {
        bail!("LLM produced an empty plan with no phases");
    }
    for phase in &llm_response.phases {
        if phase.stages.is_empty() {
            bail!("LLM produced phase '{}' with no stages", phase.title);
        }
    }

    // Transform to API types
    let plan = transform_to_plan(llm_response, effective_slug);

    // Store on disk
    store_plan(crosslink_dir, &plan, document)?;

    Ok(plan)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_user_prompt_contains_document() {
        let prompt = build_user_prompt("# My Design\n\nSome content");
        assert!(prompt.contains("# My Design"));
        assert!(prompt.contains("---BEGIN DOCUMENT---"));
        assert!(prompt.contains("---END DOCUMENT---"));
    }

    #[test]
    fn test_extract_json_block_raw() {
        let input = r#"{"phases": [], "estimated_hours": 0}"#;
        let result = extract_json_block(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_extract_json_block_with_fences() {
        let input = "```json\n{\"phases\": []}\n```";
        let result = extract_json_block(input).unwrap();
        assert_eq!(result, "{\"phases\": []}");
    }

    #[test]
    fn test_extract_json_block_with_surrounding_text() {
        let input = "Here is the plan:\n{\"phases\": []}\nDone.";
        let result = extract_json_block(input).unwrap();
        assert_eq!(result, "{\"phases\": []}");
    }

    #[test]
    fn test_extract_json_block_no_json() {
        let input = "This is just text with no JSON";
        let result = extract_json_block(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_json_from_claude_envelope() {
        let envelope = serde_json::json!({
            "type": "result",
            "result": "{\"phases\": [], \"estimated_hours\": 0}"
        });
        let raw = serde_json::to_string(&envelope).unwrap();
        let result = extract_json_from_response(&raw).unwrap();
        assert_eq!(result, "{\"phases\": [], \"estimated_hours\": 0}");
    }

    #[test]
    fn test_extract_json_from_raw_json() {
        let input = r#"{"phases": [], "estimated_hours": 0}"#;
        let result = extract_json_from_response(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_parse_llm_response_minimal() {
        let json =
            r#"{"phases": [{"title": "P1", "stages": [{"title": "S1", "description": "d"}]}]}"#;
        let resp = parse_llm_response(json).unwrap();
        assert_eq!(resp.phases.len(), 1);
    }

    #[test]
    fn test_transform_to_plan_ids() {
        let response = LlmDecomposeResponse {
            phases: vec![crate::orchestrator::models::LlmPhase {
                title: "Phase 1".to_string(),
                description: "First".to_string(),
                stages: vec![crate::orchestrator::models::LlmStage {
                    title: "Stage A".to_string(),
                    description: "Do A".to_string(),
                    tasks: vec![crate::orchestrator::models::LlmTask {
                        title: "Task 1".to_string(),
                        description: "Impl".to_string(),
                        complexity_hours: 2.0,
                    }],
                    depends_on: vec![],
                    agent_count: 1,
                    complexity_hours: 2.0,
                }],
                gate_criteria: vec!["Tests pass".to_string()],
            }],
            estimated_hours: 2.0,
        };
        let plan = transform_to_plan(response, "test-doc");
        assert_eq!(plan.document_slug, "test-doc");
        assert_eq!(plan.total_stages, 1);
        assert_eq!(plan.estimated_hours, 2.0);
        assert_eq!(plan.phases.len(), 1);
        assert_eq!(plan.phases[0].stages.len(), 1);
        assert_eq!(plan.phases[0].stages[0].tasks.len(), 1);
        // IDs should be nested
        assert!(plan.phases[0].id.contains("-p0"));
        assert!(plan.phases[0].stages[0].id.contains("-s0"));
        assert!(plan.phases[0].stages[0].tasks[0].id.contains("-t0"));
    }

    #[test]
    fn test_transform_to_plan_multiple_phases() {
        let response = LlmDecomposeResponse {
            phases: vec![
                crate::orchestrator::models::LlmPhase {
                    title: "Phase 1".to_string(),
                    description: String::new(),
                    stages: vec![
                        crate::orchestrator::models::LlmStage {
                            title: "S1".to_string(),
                            description: "d".to_string(),
                            tasks: vec![crate::orchestrator::models::LlmTask {
                                title: "T".to_string(),
                                description: "d".to_string(),
                                complexity_hours: 1.0,
                            }],
                            depends_on: vec![],
                            agent_count: 2,
                            complexity_hours: 1.0,
                        },
                        crate::orchestrator::models::LlmStage {
                            title: "S2".to_string(),
                            description: "d".to_string(),
                            tasks: vec![crate::orchestrator::models::LlmTask {
                                title: "T".to_string(),
                                description: "d".to_string(),
                                complexity_hours: 1.0,
                            }],
                            depends_on: vec!["S1".to_string()],
                            agent_count: 1,
                            complexity_hours: 1.0,
                        },
                    ],
                    gate_criteria: vec![],
                },
                crate::orchestrator::models::LlmPhase {
                    title: "Phase 2".to_string(),
                    description: String::new(),
                    stages: vec![crate::orchestrator::models::LlmStage {
                        title: "S3".to_string(),
                        description: "d".to_string(),
                        tasks: vec![crate::orchestrator::models::LlmTask {
                            title: "T".to_string(),
                            description: "d".to_string(),
                            complexity_hours: 3.0,
                        }],
                        depends_on: vec![],
                        agent_count: 3,
                        complexity_hours: 3.0,
                    }],
                    gate_criteria: vec![],
                },
            ],
            estimated_hours: 5.0,
        };
        let plan = transform_to_plan(response, "multi");
        assert_eq!(plan.total_stages, 3);
        assert_eq!(plan.phases.len(), 2);
        assert_eq!(plan.phases[0].stages[1].depends_on, vec!["S1"]);
        assert_eq!(plan.phases[1].stages[0].agent_count, 3);
    }

    #[test]
    fn test_store_and_load_plan() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path();

        let plan = OrchestratorPlan {
            id: "test-plan-123".to_string(),
            document_slug: "my-doc".to_string(),
            phases: vec![],
            created_at: Utc::now(),
            total_stages: 0,
            estimated_hours: 0.0,
        };

        let path = store_plan(crosslink_dir, &plan, "# Hello").unwrap();
        assert!(path.exists());

        let loaded = load_plan(crosslink_dir, "test-plan-123").unwrap();
        assert_eq!(loaded.plan.id, "test-plan-123");
        assert_eq!(loaded.source_document, "# Hello");
    }

    #[test]
    fn test_list_plans_empty() {
        let dir = tempfile::tempdir().unwrap();
        let ids = list_plans(dir.path()).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn test_list_plans_with_stored() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path();

        let plan1 = OrchestratorPlan {
            id: "aaa".to_string(),
            document_slug: "doc1".to_string(),
            phases: vec![],
            created_at: Utc::now(),
            total_stages: 0,
            estimated_hours: 0.0,
        };
        let plan2 = OrchestratorPlan {
            id: "bbb".to_string(),
            document_slug: "doc2".to_string(),
            phases: vec![],
            created_at: Utc::now(),
            total_stages: 0,
            estimated_hours: 0.0,
        };

        store_plan(crosslink_dir, &plan1, "doc 1").unwrap();
        store_plan(crosslink_dir, &plan2, "doc 2").unwrap();

        let ids = list_plans(crosslink_dir).unwrap();
        assert_eq!(ids, vec!["aaa", "bbb"]);
    }

    #[test]
    fn test_load_nonexistent_plan() {
        let dir = tempfile::tempdir().unwrap();
        let result = load_plan(dir.path(), "nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_build_system_prompt_contains_schema() {
        let prompt = build_system_prompt();
        assert!(prompt.contains("phases"));
        assert!(prompt.contains("stages"));
        assert!(prompt.contains("tasks"));
        assert!(prompt.contains("depends_on"));
        assert!(prompt.contains("complexity_hours"));
        assert!(prompt.contains("agent_count"));
        assert!(prompt.contains("gate_criteria"));
        assert!(prompt.contains("estimated_hours"));
    }

    #[test]
    fn test_extract_json_block_malformed_brace_order() {
        // Only a closing brace, no opening brace
        let input = "some text } and { more";
        // `{` at index 16, `}` at index 10 -> end <= start
        let result = extract_json_block(input);
        // find('{') is at 16, rfind('}') is at 10 => end <= start => bail
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Malformed JSON"));
    }

    #[test]
    fn test_extract_json_block_only_opening_brace() {
        let input = "{ no closing brace here";
        // find('{') succeeds, rfind('}') fails
        let result = extract_json_block(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("closing brace"));
    }

    #[test]
    fn test_extract_json_block_empty_string() {
        let result = extract_json_block("");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_json_block_whitespace_only() {
        let result = extract_json_block("   \n\t  ");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_json_block_fences_without_lang() {
        let input = "```\n{\"key\": \"value\"}\n```";
        let result = extract_json_block(input).unwrap();
        assert_eq!(result, "{\"key\": \"value\"}");
    }

    #[test]
    fn test_extract_json_from_response_envelope_non_string_result() {
        // Envelope has "result" but it's a number, not a string
        let envelope = serde_json::json!({
            "type": "result",
            "result": 42
        });
        let raw = serde_json::to_string(&envelope).unwrap();
        // Should fall through to extract_json_block on the full text
        let result = extract_json_from_response(&raw);
        // The raw text is `{"type":"result","result":42}` which is valid JSON
        assert!(result.is_ok());
    }

    #[test]
    fn test_extract_json_from_response_envelope_with_fenced_result() {
        let envelope = serde_json::json!({
            "type": "result",
            "result": "```json\n{\"phases\": [], \"estimated_hours\": 1.0}\n```"
        });
        let raw = serde_json::to_string(&envelope).unwrap();
        let result = extract_json_from_response(&raw).unwrap();
        assert!(result.contains("phases"));
    }

    #[test]
    fn test_parse_llm_response_invalid_json() {
        let result = parse_llm_response("this is not json");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[test]
    fn test_parse_llm_response_wrong_schema() {
        // Valid JSON but wrong schema (missing required "phases" field with stages)
        let result = parse_llm_response(r#"{"foo": "bar"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_llm_response_full() {
        let json = r#"{
            "phases": [{
                "title": "Phase 1",
                "description": "desc",
                "stages": [{
                    "title": "S1",
                    "description": "do stuff",
                    "tasks": [
                        {"title": "T1", "description": "impl", "complexity_hours": 1.5}
                    ],
                    "depends_on": [],
                    "agent_count": 2,
                    "complexity_hours": 3.0
                }],
                "gate_criteria": ["tests pass"]
            }],
            "estimated_hours": 3.0
        }"#;
        let resp = parse_llm_response(json).unwrap();
        assert_eq!(resp.phases.len(), 1);
        assert_eq!(resp.phases[0].stages[0].tasks.len(), 1);
        assert_eq!(resp.phases[0].stages[0].agent_count, 2);
        assert_eq!(resp.estimated_hours, 3.0);
    }

    #[test]
    fn test_transform_to_plan_empty_phases() {
        let response = LlmDecomposeResponse {
            phases: vec![],
            estimated_hours: 0.0,
        };
        let plan = transform_to_plan(response, "empty");
        assert_eq!(plan.total_stages, 0);
        assert_eq!(plan.phases.len(), 0);
        assert_eq!(plan.document_slug, "empty");
    }

    #[test]
    fn test_transform_to_plan_preserves_task_fields() {
        let response = LlmDecomposeResponse {
            phases: vec![crate::orchestrator::models::LlmPhase {
                title: "P".to_string(),
                description: "phase desc".to_string(),
                stages: vec![crate::orchestrator::models::LlmStage {
                    title: "S".to_string(),
                    description: "stage desc".to_string(),
                    tasks: vec![
                        crate::orchestrator::models::LlmTask {
                            title: "Task A".to_string(),
                            description: "Do A".to_string(),
                            complexity_hours: 1.0,
                        },
                        crate::orchestrator::models::LlmTask {
                            title: "Task B".to_string(),
                            description: "Do B".to_string(),
                            complexity_hours: 2.5,
                        },
                    ],
                    depends_on: vec![],
                    agent_count: 1,
                    complexity_hours: 3.5,
                }],
                gate_criteria: vec!["gate".to_string()],
            }],
            estimated_hours: 3.5,
        };
        let plan = transform_to_plan(response, "detail");
        let stage = &plan.phases[0].stages[0];
        assert_eq!(stage.tasks.len(), 2);
        assert_eq!(stage.tasks[0].title, "Task A");
        assert_eq!(stage.tasks[0].description, "Do A");
        assert_eq!(stage.tasks[0].complexity_hours, 1.0);
        assert_eq!(stage.tasks[1].title, "Task B");
        assert_eq!(stage.tasks[1].complexity_hours, 2.5);
        // Check task IDs are sequential
        assert!(stage.tasks[0].id.contains("-t0"));
        assert!(stage.tasks[1].id.contains("-t1"));
        // Phase fields
        assert_eq!(plan.phases[0].title, "P");
        assert_eq!(plan.phases[0].description, "phase desc");
        assert_eq!(plan.phases[0].gate_criteria, vec!["gate"]);
    }

    #[test]
    fn test_store_plan_creates_orchestrator_dir() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path();

        let plan = OrchestratorPlan {
            id: "dir-test".to_string(),
            document_slug: "doc".to_string(),
            phases: vec![],
            created_at: Utc::now(),
            total_stages: 0,
            estimated_hours: 0.0,
        };

        // The orchestrator directory should not exist yet
        assert!(!crosslink_dir.join(PLANS_DIR).exists());

        store_plan(crosslink_dir, &plan, "content").unwrap();

        // Now it should exist
        assert!(crosslink_dir.join(PLANS_DIR).exists());
    }

    #[test]
    fn test_list_plans_ignores_non_json_files() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path();

        // Create the orchestrator directory
        let orch_dir = crosslink_dir.join(PLANS_DIR);
        std::fs::create_dir_all(&orch_dir).unwrap();

        // Write a json file and a non-json file
        std::fs::write(orch_dir.join("plan-1.json"), "{}").unwrap();
        std::fs::write(orch_dir.join("readme.txt"), "hello").unwrap();
        std::fs::write(orch_dir.join("plan-2.json"), "{}").unwrap();

        let ids = list_plans(crosslink_dir).unwrap();
        assert_eq!(ids, vec!["plan-1", "plan-2"]);
    }

    #[test]
    fn test_load_plan_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path();
        let orch_dir = crosslink_dir.join(PLANS_DIR);
        std::fs::create_dir_all(&orch_dir).unwrap();
        std::fs::write(orch_dir.join("bad.json"), "not valid json!").unwrap();

        let result = load_plan(crosslink_dir, "bad");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Failed to parse"));
    }

    #[tokio::test]
    async fn test_decompose_document_empty_document_bails() {
        let dir = tempfile::tempdir().unwrap();
        let result = decompose_document(dir.path(), "", None).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Document is empty"));
    }

    #[tokio::test]
    async fn test_decompose_document_whitespace_only_bails() {
        let dir = tempfile::tempdir().unwrap();
        let result = decompose_document(dir.path(), "   \n\t  ", Some("my-slug")).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Document is empty"));
    }
}
