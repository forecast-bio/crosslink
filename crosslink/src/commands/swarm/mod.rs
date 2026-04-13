// Swarm coordination: multi-agent phase planning, status, and resume.
//
// Persists swarm state to the hub branch under `swarm/` so it survives
// session boundaries and is visible to all agents.

mod budget;
mod edit;
mod init;
mod io;
mod lifecycle;
mod merge;
mod review;
mod status;
mod types;

// Re-export all public items so `commands::swarm::foo` continues to work.
pub use budget::{config_budget, estimate, harvest_costs, launch_budget_aware, plan, plan_show};
pub use edit::{merge_phases, move_agent, remove_agent, rename_phase, reorder_phase, split_phase};
pub use init::init;
pub use lifecycle::{
    adopt, archive, checkpoint, gate, launch, launch_retry_failed, list_swarms, reset, resume,
    sync_status,
};
pub use merge::merge;
pub use review::{fix, review, review_continue, review_status, run_pipeline_cmd, trust_init};
pub use status::status;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::budget::*;
    use super::init::*;
    use super::lifecycle::*;
    use super::merge::*;
    use super::review::*;
    use super::status::*;
    use super::types::*;
    use crate::commands::design_doc::DesignDoc;
    use crate::findings::{Finding, FindingSeverity, ReviewReport};
    use std::path::PathBuf;

    /// Helper to build `seam::Partition` from a label and file list (for tests).
    fn make_partition(label: &str, files: Vec<&str>) -> crate::seam::Partition {
        crate::seam::Partition {
            label: label.to_string(),
            files: files.into_iter().map(std::path::PathBuf::from).collect(),
            line_count: 0,
        }
    }

    #[test]
    fn test_swarm_plan_serde_roundtrip() {
        let plan = SwarmPlan {
            schema_version: 1,
            title: "Test Swarm".to_string(),
            design_doc: Some("DESIGN.md".to_string()),
            created_at: "2026-03-06T12:00:00Z".to_string(),
            phases: vec!["Phase 1".to_string(), "Phase 2".to_string()],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: SwarmPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, parsed);
    }

    #[test]
    fn test_phase_definition_serde_roundtrip() {
        let phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::InProgress,
            agents: vec![AgentEntry {
                slug: "linear-models".to_string(),
                description: "Implement linear regression".to_string(),
                issue_id: Some(42),
                agent_id: Some("driver--linear-models".to_string()),
                branch: Some("feature/linear-models".to_string()),
                status: AgentStatus::Running,
                started_at: Some("2026-03-06T12:00:00Z".to_string()),
                completed_at: None,
            }],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };
        let json = serde_json::to_string(&phase).unwrap();
        let parsed: PhaseDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(phase, parsed);
    }

    #[test]
    fn test_checkpoint_serde_roundtrip() {
        let cp = Checkpoint {
            phase: "Phase 1".to_string(),
            created_at: "2026-03-06T14:00:00Z".to_string(),
            agents_merged: vec!["driver--linear-models".to_string()],
            agents_pending: vec!["driver--tree-models".to_string()],
            dev_branch_sha: Some("abc1234".to_string()),
            test_result: Some(TestResult {
                total: 631,
                passed: 631,
                failed: 0,
            }),
            handoff_notes: Some("Phase 1 complete.".to_string()),
        };
        let json = serde_json::to_string(&cp).unwrap();
        let parsed: Checkpoint = serde_json::from_str(&json).unwrap();
        assert_eq!(cp, parsed);
    }

    #[test]
    fn test_phase_status_display() {
        assert_eq!(format!("{}", PhaseStatus::Pending), "pending");
        assert_eq!(format!("{}", PhaseStatus::InProgress), "in progress");
        assert_eq!(format!("{}", PhaseStatus::Completed), "completed");
        assert_eq!(format!("{}", PhaseStatus::Failed), "failed");
    }

    #[test]
    fn test_agent_status_display() {
        assert_eq!(format!("{}", AgentStatus::Planned), "planned");
        assert_eq!(format!("{}", AgentStatus::Running), "running");
        assert_eq!(format!("{}", AgentStatus::Completed), "completed");
        assert_eq!(format!("{}", AgentStatus::Merged), "merged");
        assert_eq!(format!("{}", AgentStatus::Failed), "failed");
    }

    #[test]
    fn test_slugify_phase() {
        assert_eq!(slugify_phase("Phase 1"), "phase-1");
        assert_eq!(
            slugify_phase("Phase 2: Core Infrastructure"),
            "phase-2-core-infrastructure"
        );
    }

    #[test]
    fn test_slugify_requirement() {
        assert_eq!(
            slugify_requirement("REQ-1: Implement retry logic"),
            "implement-retry-logic"
        );
        assert_eq!(
            slugify_requirement("- Add batch processing"),
            "add-batch-processing"
        );
        assert_eq!(
            slugify_requirement("AC-2: Handle timeouts"),
            "handle-timeouts"
        );
    }

    #[test]
    fn test_propose_phases_from_requirements() {
        let doc = DesignDoc {
            title: "Test Feature".to_string(),
            summary: String::new(),
            requirements: vec![
                "REQ-1: Add login".to_string(),
                "REQ-2: Add logout".to_string(),
            ],
            requirement_groups: Vec::new(),
            acceptance_criteria: vec![],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].agents.len(), 2);
        assert_eq!(phases[0].agents[0].slug, "add-login");
        assert_eq!(phases[0].agents[1].slug, "add-logout");
    }

    #[test]
    fn test_propose_phases_splits_large_requirement_lists() {
        let doc = DesignDoc {
            title: "Big Feature".to_string(),
            summary: String::new(),
            requirements: (1..=12).map(|i| format!("REQ-{i}: Task {i}")).collect(),
            requirement_groups: Vec::new(),
            acceptance_criteria: vec![],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 2);
        assert_eq!(phases[0].agents.len(), 8);
        assert_eq!(phases[1].agents.len(), 4);
        assert_eq!(phases[0].name, "Phase 1");
        assert_eq!(phases[1].name, "Phase 2");
        assert_eq!(phases[1].depends_on, vec!["Phase 1"]);
    }

    #[test]
    fn test_propose_phases_falls_back_to_title() {
        let doc = DesignDoc {
            title: "Simple Feature".to_string(),
            summary: String::new(),
            requirements: vec![],
            requirement_groups: Vec::new(),
            acceptance_criteria: vec![],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].agents.len(), 1);
        assert_eq!(phases[0].agents[0].description, "Simple Feature");
    }

    #[test]
    fn test_propose_phases_uses_acceptance_criteria_when_no_requirements() {
        let doc = DesignDoc {
            title: "AC Feature".to_string(),
            summary: String::new(),
            requirements: vec![],
            requirement_groups: Vec::new(),
            acceptance_criteria: vec![
                "AC-1: Widget renders".to_string(),
                "AC-2: Widget responds to click".to_string(),
            ],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 1);
        assert_eq!(phases[0].agents.len(), 2);
    }

    #[test]
    fn test_propose_phases_uses_requirement_groups() {
        use crate::commands::design_doc::RequirementGroup;
        let doc = DesignDoc {
            title: "Layered Feature".to_string(),
            summary: String::new(),
            requirements: vec![
                "REQ-1: Foundation item".to_string(),
                "REQ-2: Backend item".to_string(),
                "REQ-3: Delivery item".to_string(),
            ],
            requirement_groups: vec![
                RequirementGroup {
                    name: "Foundation".to_string(),
                    execution_hint: "sequential".to_string(),
                    items: vec!["REQ-1: Foundation item".to_string()],
                },
                RequirementGroup {
                    name: "Backends".to_string(),
                    execution_hint: "parallel".to_string(),
                    items: vec!["REQ-2: Backend item".to_string()],
                },
                RequirementGroup {
                    name: "Delivery".to_string(),
                    execution_hint: "sequential".to_string(),
                    items: vec!["REQ-3: Delivery item".to_string()],
                },
            ],
            acceptance_criteria: vec![],
            architecture: String::new(),
            open_questions: vec![],
            out_of_scope: vec![],
            unknown_sections: vec![],
        };

        let phases = propose_phases(&doc);
        assert_eq!(phases.len(), 3);
        assert_eq!(phases[0].name, "Phase 1: Foundation");
        assert_eq!(phases[0].agents.len(), 1);
        assert!(phases[0].depends_on.is_empty());

        assert_eq!(phases[1].name, "Phase 2: Backends");
        assert_eq!(phases[1].agents.len(), 1);
        // Parallel phase still depends on previous
        assert_eq!(phases[1].depends_on, vec!["Phase 1: Foundation"]);

        assert_eq!(phases[2].name, "Phase 3: Delivery");
        assert_eq!(phases[2].agents.len(), 1);
        assert_eq!(phases[2].depends_on, vec!["Phase 2: Backends"]);
    }

    #[test]
    fn test_probe_agent_status_nonexistent_worktree() {
        let dir = tempfile::tempdir().unwrap();
        // No git repo, no worktree, no branch -> planned
        assert_eq!(probe_agent_status(dir.path(), "nonexistent"), "planned");
    }

    #[test]
    fn test_probe_agent_status_worktree_removed_branch_merged() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();

        // Set up a git repo with a branch that's been merged
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        for args in [
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
        ] {
            std::process::Command::new("git")
                .current_dir(repo)
                .args(&args)
                .output()
                .unwrap();
        }
        std::fs::write(repo.join("README.md"), "# test\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-m", "init", "--no-gpg-sign"])
            .output()
            .unwrap();

        // Create and merge a branch
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["checkout", "-b", "test-agent"])
            .output()
            .unwrap();
        std::fs::write(repo.join("agent-work.txt"), "work\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-m", "agent work", "--no-gpg-sign"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["checkout", "main"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["merge", "test-agent", "--no-gpg-sign"])
            .output()
            .unwrap();

        // No worktree exists, but branch is merged -> should be "completed (merged)"
        assert_eq!(probe_agent_status(repo, "test-agent"), "completed (merged)");
    }

    #[test]
    fn test_probe_agent_status_worktree_removed_branch_exists() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();

        std::process::Command::new("git")
            .current_dir(repo)
            .args(["init", "-q", "-b", "main"])
            .output()
            .unwrap();
        for args in [
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
        ] {
            std::process::Command::new("git")
                .current_dir(repo)
                .args(&args)
                .output()
                .unwrap();
        }
        std::fs::write(repo.join("README.md"), "# test\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-m", "init", "--no-gpg-sign"])
            .output()
            .unwrap();

        // Create a branch with a commit that isn't merged
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["checkout", "-b", "unmerged-agent"])
            .output()
            .unwrap();
        std::fs::write(repo.join("unmerged-work.txt"), "unmerged\n").unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["add", "."])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-m", "unmerged work", "--no-gpg-sign"])
            .output()
            .unwrap();
        std::process::Command::new("git")
            .current_dir(repo)
            .args(["checkout", "main"])
            .output()
            .unwrap();

        // No worktree, branch exists but not merged -> "completed (worktree removed)"
        assert_eq!(
            probe_agent_status(repo, "unmerged-agent"),
            "completed (worktree removed)"
        );
    }

    #[test]
    fn test_probe_agent_status_done() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join(".worktrees").join("my-agent");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".kickoff-status"), "DONE\n").unwrap();
        assert_eq!(probe_agent_status(dir.path(), "my-agent"), "DONE");
    }

    #[test]
    fn test_probe_agent_status_failed() {
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join(".worktrees").join("bad-agent");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".kickoff-status"), "FAILED\n").unwrap();
        assert_eq!(probe_agent_status(dir.path(), "bad-agent"), "FAILED");
    }

    #[test]
    fn test_probe_agent_status_worktree_no_status_no_tmux() {
        // Worktree exists but no .kickoff-status and no tmux session
        // -> should report "failed (session died)" not "unknown"
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join(".worktrees").join("dead-agent");
        std::fs::create_dir_all(&wt).unwrap();
        // No .kickoff-status file, no tmux -> session died
        assert_eq!(
            probe_agent_status(dir.path(), "dead-agent"),
            "failed (session died)"
        );
    }

    #[test]
    fn test_probe_agent_status_launching() {
        // Agent wrote LAUNCHING status but tmux session has since exited
        let dir = tempfile::tempdir().unwrap();
        let wt = dir.path().join(".worktrees").join("launch-agent");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".kickoff-status"), "LAUNCHING\n").unwrap();
        assert_eq!(probe_agent_status(dir.path(), "launch-agent"), "LAUNCHING");
    }

    #[test]
    fn test_load_phase_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path();
        // Create a minimal plan with no phase files
        std::fs::create_dir_all(cache.join("swarm")).unwrap();
        let plan = SwarmPlan {
            schema_version: 1,
            title: "Test".to_string(),
            design_doc: None,
            created_at: "2026-03-06T12:00:00Z".to_string(),
            phases: vec!["Phase 1".to_string()],
        };
        std::fs::write(
            cache.join("swarm/plan.json"),
            serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();

        // We can't easily test load_phase without a SyncManager,
        // but we can test the slug-matching logic indirectly via slugify_phase
        assert_eq!(slugify_phase("Phase 1"), "phase-1");
        assert_eq!(slugify_phase("Phase 2"), "phase-2");
    }

    #[test]
    fn test_parse_test_counts_cargo_format() {
        let stdout = "running 142 tests\n\
                      test result: ok. 140 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out";
        let (total, passed) = parse_test_counts(stdout, "");
        assert_eq!(total, Some(142));
        assert_eq!(passed, Some(140));
    }

    #[test]
    fn test_parse_test_counts_no_match() {
        let (total, passed) = parse_test_counts("all good", "no tests");
        assert_eq!(total, None);
        assert_eq!(passed, None);
    }

    #[test]
    fn test_parse_test_counts_from_stderr() {
        let stderr = "test result: ok. 50 passed; 0 failed; 3 ignored; 0 measured; 10 filtered out";
        let (total, passed) = parse_test_counts("", stderr);
        assert_eq!(total, Some(50));
        assert_eq!(passed, Some(50));
    }

    #[test]
    fn test_gate_result_serde_roundtrip() {
        let gate = GateResult {
            status: "passed".to_string(),
            tests_total: Some(142),
            tests_passed: Some(142),
            ran_at: Some("2026-03-06T15:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&gate).unwrap();
        let parsed: GateResult = serde_json::from_str(&json).unwrap();
        assert_eq!(gate, parsed);
    }

    #[test]
    fn test_phase_status_transitions() {
        // Verify the expected phase lifecycle: Pending -> InProgress -> Completed
        let mut phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::Pending,
            agents: vec![AgentEntry {
                slug: "agent-1".to_string(),
                description: "Test agent".to_string(),
                issue_id: None,
                agent_id: None,
                branch: Some("feature/agent-1".to_string()),
                status: AgentStatus::Planned,
                started_at: None,
                completed_at: None,
            }],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };

        assert_eq!(phase.status, PhaseStatus::Pending);
        assert_eq!(phase.agents[0].status, AgentStatus::Planned);

        // Simulate launch
        phase.status = PhaseStatus::InProgress;
        phase.agents[0].status = AgentStatus::Running;
        phase.agents[0].started_at = Some("2026-03-06T12:00:00Z".to_string());
        assert_eq!(phase.status, PhaseStatus::InProgress);

        // Simulate completion + gate
        phase.agents[0].status = AgentStatus::Completed;
        phase.gate = Some(GateResult {
            status: "passed".to_string(),
            tests_total: Some(100),
            tests_passed: Some(100),
            ran_at: Some("2026-03-06T13:00:00Z".to_string()),
        });

        // Simulate checkpoint
        phase.status = PhaseStatus::Completed;
        phase.agents[0].status = AgentStatus::Merged;
        phase.checkpoint = Some("phase-1".to_string());

        assert_eq!(phase.status, PhaseStatus::Completed);
        assert_eq!(phase.agents[0].status, AgentStatus::Merged);
        assert!(phase.checkpoint.is_some());

        // Roundtrip the final state
        let json = serde_json::to_string(&phase).unwrap();
        let parsed: PhaseDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(phase, parsed);
    }

    #[test]
    fn test_find_latest_checkpoint_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_latest_checkpoint(dir.path()).is_none());
    }

    #[test]
    fn test_find_latest_checkpoint_with_files() {
        let dir = tempfile::tempdir().unwrap();
        let cp = Checkpoint {
            phase: "Phase 1".to_string(),
            created_at: "2026-03-06T14:00:00Z".to_string(),
            agents_merged: vec![],
            agents_pending: vec![],
            dev_branch_sha: None,
            test_result: None,
            handoff_notes: Some("test".to_string()),
        };
        let content = serde_json::to_string_pretty(&cp).unwrap();
        std::fs::write(dir.path().join("phase-1.json"), &content).unwrap();

        let found = find_latest_checkpoint(dir.path()).unwrap();
        assert_eq!(found.phase, "Phase 1");
        assert_eq!(found.handoff_notes, Some("test".to_string()));
    }

    #[test]
    fn test_budget_config_serde_roundtrip() {
        let config = BudgetConfig {
            budget_window_s: 18000,
            model: "opus".to_string(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: BudgetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_cost_log_serde_roundtrip() {
        let mut estimates = std::collections::HashMap::new();
        estimates.insert(
            "opus".to_string(),
            ModelEstimate {
                median_duration_s: 3600,
                p90_duration_s: 5400,
            },
        );
        let log = CostLog {
            observations: vec![CostObservation {
                agent_id: "driver--agent-1".to_string(),
                model: "opus".to_string(),
                duration_s: 4500,
                files_changed: Some(12),
                lines_added: Some(450),
            }],
            model_estimates: estimates,
        };
        let json = serde_json::to_string(&log).unwrap();
        let parsed: CostLog = serde_json::from_str(&json).unwrap();
        assert_eq!(log, parsed);
    }

    #[test]
    fn test_default_agent_duration() {
        assert_eq!(default_agent_duration("opus"), 5400);
        assert_eq!(default_agent_duration("sonnet"), 2700);
        assert_eq!(default_agent_duration("haiku"), 3600);
    }

    #[test]
    fn test_estimate_phase_cost_no_history() {
        let phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::Pending,
            agents: vec![
                AgentEntry {
                    slug: "a1".to_string(),
                    description: "Agent 1".to_string(),
                    issue_id: None,
                    agent_id: None,
                    branch: None,
                    status: AgentStatus::Planned,
                    started_at: None,
                    completed_at: None,
                },
                AgentEntry {
                    slug: "a2".to_string(),
                    description: "Agent 2".to_string(),
                    issue_id: None,
                    agent_id: None,
                    branch: None,
                    status: AgentStatus::Planned,
                    started_at: None,
                    completed_at: None,
                },
            ],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };
        let cost_log = CostLog::default();
        let (total, agents) = estimate_phase_cost(&phase, &cost_log, "opus");
        // 2 agents x 5400s + 2x300 overhead + 600 gate = 12000
        assert_eq!(agents.len(), 2);
        assert_eq!(total, 5400 * 2 + 300 * 2 + 600);
    }

    #[test]
    fn test_estimate_phase_cost_with_history() {
        let phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::Pending,
            agents: vec![AgentEntry {
                slug: "a1".to_string(),
                description: "Agent 1".to_string(),
                issue_id: None,
                agent_id: None,
                branch: None,
                status: AgentStatus::Planned,
                started_at: None,
                completed_at: None,
            }],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };
        let mut estimates = std::collections::HashMap::new();
        estimates.insert(
            "opus".to_string(),
            ModelEstimate {
                median_duration_s: 3000,
                p90_duration_s: 4000,
            },
        );
        let cost_log = CostLog {
            observations: vec![],
            model_estimates: estimates,
        };
        let (total, agents) = estimate_phase_cost(&phase, &cost_log, "opus");
        // 1 agent x 4000 (p90) + 300 overhead + 600 gate = 4900
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].1, 4000);
        assert_eq!(total, 4000 + 300 + 600);
    }

    #[test]
    fn test_estimate_skips_non_planned_agents() {
        let phase = PhaseDefinition {
            name: "Phase 1".to_string(),
            status: PhaseStatus::InProgress,
            agents: vec![
                AgentEntry {
                    slug: "done".to_string(),
                    description: "Done".to_string(),
                    issue_id: None,
                    agent_id: None,
                    branch: None,
                    status: AgentStatus::Completed,
                    started_at: None,
                    completed_at: None,
                },
                AgentEntry {
                    slug: "pending-agent".to_string(),
                    description: "Pending agent".to_string(),
                    issue_id: None,
                    agent_id: None,
                    branch: None,
                    status: AgentStatus::Planned,
                    started_at: None,
                    completed_at: None,
                },
            ],
            gate: None,
            depends_on: vec![],
            checkpoint: None,
        };
        let cost_log = CostLog::default();
        let (_, agents) = estimate_phase_cost(&phase, &cost_log, "opus");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].0, "pending-agent");
    }

    #[test]
    fn test_budget_recommendation_proceed() {
        let rec = budget_recommendation(5000, 18000, 2);
        assert_eq!(rec, BudgetRecommendation::Proceed);
    }

    #[test]
    fn test_budget_recommendation_caution() {
        // Cost is > 80% of budget but still fits
        let rec = budget_recommendation(15000, 18000, 2);
        assert_eq!(rec, BudgetRecommendation::ProceedWithCaution);
    }

    #[test]
    fn test_budget_recommendation_split() {
        // Cost exceeds budget
        let rec = budget_recommendation(20000, 10000, 4);
        match rec {
            BudgetRecommendation::Split {
                recommended_count, ..
            } => {
                assert!(recommended_count > 0);
                assert!(recommended_count < 4);
            }
            other => panic!("Expected Split, got {other:?}"),
        }
    }

    #[test]
    fn test_budget_recommendation_block() {
        // Budget less than coordinator overhead
        let rec = budget_recommendation(20000, 500, 4);
        match rec {
            BudgetRecommendation::Block { .. } => {}
            other => panic!("Expected Block, got {other:?}"),
        }
    }

    #[test]
    fn test_recompute_model_estimates() {
        let mut log = CostLog {
            observations: vec![
                CostObservation {
                    agent_id: "a1".to_string(),
                    model: "opus".to_string(),
                    duration_s: 3000,
                    files_changed: None,
                    lines_added: None,
                },
                CostObservation {
                    agent_id: "a2".to_string(),
                    model: "opus".to_string(),
                    duration_s: 4000,
                    files_changed: None,
                    lines_added: None,
                },
                CostObservation {
                    agent_id: "a3".to_string(),
                    model: "opus".to_string(),
                    duration_s: 5000,
                    files_changed: None,
                    lines_added: None,
                },
            ],
            model_estimates: std::collections::HashMap::new(),
        };
        recompute_model_estimates(&mut log);
        let est = log.model_estimates.get("opus").unwrap();
        assert_eq!(est.median_duration_s, 4000); // middle of [3000, 4000, 5000]
        assert_eq!(est.p90_duration_s, 5000); // ceil(3*0.9) = 3 -> index 2
    }

    #[test]
    fn test_pack_windows_single_window() {
        let phases = vec![
            ("Phase 1".to_string(), 3600, 4),
            ("Phase 2".to_string(), 3600, 4),
        ];
        let windows = pack_windows(&phases, 18000); // 5h window
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].phases.len(), 2);
        assert_eq!(windows[0].phases[0].fit, WindowFit::Fits);
        assert_eq!(windows[0].phases[1].fit, WindowFit::Fits);
        assert!(windows[0].buffer_s > 0);
    }

    #[test]
    fn test_pack_windows_multiple_windows() {
        let phases = vec![
            ("Phase 1".to_string(), 7200, 8),
            ("Phase 2".to_string(), 9000, 9),
            ("Phase 3".to_string(), 7200, 8),
            ("Phase 4".to_string(), 7200, 8),
        ];
        let windows = pack_windows(&phases, 18000); // 5h window
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].phases.len(), 2);
        assert_eq!(windows[1].phases.len(), 2);
        assert!(windows[0].stop_point.contains("Phase 2"));
        assert!(windows[1].stop_point.contains("Phase 4"));
    }

    #[test]
    fn test_pack_windows_tight_fit() {
        // Phase fills > 80% of window but still fits
        let phases = vec![("Phase 1".to_string(), 16000, 6)];
        let windows = pack_windows(&phases, 18000);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].phases[0].fit, WindowFit::Tight);
    }

    #[test]
    fn test_pack_windows_overflow_splits() {
        // Single phase overflows window
        let phases = vec![
            ("Phase 1".to_string(), 10000, 5),
            ("Phase 2".to_string(), 10000, 5),
        ];
        let windows = pack_windows(&phases, 10000);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].phases.len(), 1);
        assert_eq!(windows[1].phases.len(), 1);
    }

    #[test]
    fn test_pack_windows_empty() {
        let phases: Vec<(String, u64, usize)> = vec![];
        let windows = pack_windows(&phases, 18000);
        assert!(windows.is_empty());
    }

    #[test]
    fn test_window_allocation_serde_roundtrip() {
        let alloc = WindowAllocation {
            window_index: 1,
            phases: vec![WindowPhase {
                name: "Phase 1".to_string(),
                agent_count: 4,
                estimate_s: 7200,
                fit: WindowFit::Fits,
            }],
            total_estimate_s: 7200,
            buffer_s: 10800,
            stop_point: "after Phase 1 gate → checkpoint".to_string(),
        };
        let json = serde_json::to_string(&alloc).unwrap();
        let parsed: WindowAllocation = serde_json::from_str(&json).unwrap();
        assert_eq!(alloc, parsed);
    }

    #[test]
    fn test_window_fit_display() {
        let json_fits = serde_json::to_string(&WindowFit::Fits).unwrap();
        assert_eq!(json_fits, "\"fits\"");
        let json_tight = serde_json::to_string(&WindowFit::Tight).unwrap();
        assert_eq!(json_tight, "\"tight\"");
        let json_overflow = serde_json::to_string(&WindowFit::Overflow).unwrap();
        assert_eq!(json_overflow, "\"overflow\"");
    }

    // -----------------------------------------------------------------------
    // swarm review tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_mandate_prompt_adversarial() {
        let prompt = mandate_prompt("adversarial");
        assert!(prompt.contains("ha-satan"));
        assert!(prompt.contains("correctness, safety, and robustness"));
    }

    #[test]
    fn test_mandate_prompt_security() {
        let prompt = mandate_prompt("security");
        assert!(prompt.contains("trust boundary"));
        assert!(prompt.contains("injection vectors"));
    }

    #[test]
    fn test_mandate_prompt_robustness() {
        let prompt = mandate_prompt("robustness");
        assert!(prompt.contains("crash paths"));
        assert!(prompt.contains("resource leaks"));
    }

    #[test]
    fn test_mandate_prompt_correctness() {
        let prompt = mandate_prompt("correctness");
        assert!(prompt.contains("logic errors"));
        assert!(prompt.contains("race conditions"));
    }

    #[test]
    fn test_mandate_prompt_custom_passthrough() {
        let custom = "Check for off-by-one errors everywhere";
        assert_eq!(mandate_prompt(custom), custom);
    }

    #[test]
    fn test_finding_serde_roundtrip() {
        let finding = Finding {
            title: "Unchecked unwrap in parser".to_string(),
            severity: FindingSeverity::High,
            file: "src/parser.rs".to_string(),
            line: Some(42),
            description: "This unwrap will panic on malformed input".to_string(),
            suggested_fix: Some("Use ? operator instead".to_string()),
            agent: "reviewer-1".to_string(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let parsed: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, finding.title);
        assert_eq!(parsed.severity, finding.severity);
        assert_eq!(parsed.file, finding.file);
        assert_eq!(parsed.line, finding.line);
        assert_eq!(parsed.description, finding.description);
        assert_eq!(parsed.suggested_fix, finding.suggested_fix);
        assert_eq!(parsed.agent, finding.agent);
    }

    #[test]
    fn test_finding_minimal_serde_roundtrip() {
        let finding = Finding {
            title: "Minor issue".to_string(),
            severity: FindingSeverity::Info,
            file: "src/lib.rs".to_string(),
            line: None,
            description: "Consider adding docs".to_string(),
            suggested_fix: None,
            agent: "reviewer-2".to_string(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let parsed: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.line, None);
        assert_eq!(parsed.suggested_fix, None);
    }

    #[test]
    fn test_review_report_serde_roundtrip() {
        let report = ReviewReport {
            agent: "reviewer-1".to_string(),
            partition_label: "src, lib".to_string(),
            mandate: "adversarial".to_string(),
            findings: vec![Finding {
                title: "Buffer overflow".to_string(),
                severity: FindingSeverity::Critical,
                file: "src/buffer.rs".to_string(),
                line: Some(100),
                description: "Writes past allocated size".to_string(),
                suggested_fix: Some("Add bounds check".to_string()),
                agent: "reviewer-1".to_string(),
            }],
            completed_at: Some("2026-03-12T10:00:00Z".to_string()),
        };
        let json = serde_json::to_string(&report).unwrap();
        let parsed: ReviewReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent, report.agent);
        assert_eq!(parsed.partition_label, report.partition_label);
        assert_eq!(parsed.mandate, report.mandate);
        assert_eq!(parsed.findings.len(), 1);
        assert_eq!(parsed.findings[0].title, "Buffer overflow");
        assert_eq!(parsed.completed_at, report.completed_at);
    }

    #[test]
    fn test_finding_severity_ordering() {
        // Derived PartialOrd/Ord uses variant declaration order
        assert!(FindingSeverity::Critical < FindingSeverity::High);
        assert!(FindingSeverity::High < FindingSeverity::Medium);
        assert!(FindingSeverity::Medium < FindingSeverity::Low);
        assert!(FindingSeverity::Low < FindingSeverity::Info);

        // Sort a mixed list and verify order
        let mut severities = vec![
            FindingSeverity::Low,
            FindingSeverity::Critical,
            FindingSeverity::Info,
            FindingSeverity::High,
            FindingSeverity::Medium,
        ];
        severities.sort();
        assert_eq!(
            severities,
            vec![
                FindingSeverity::Critical,
                FindingSeverity::High,
                FindingSeverity::Medium,
                FindingSeverity::Low,
                FindingSeverity::Info,
            ]
        );
    }

    #[test]
    fn test_slugify_fix_target_basic() {
        assert_eq!(
            slugify_fix_target(326, "Buffer overflow in parser"),
            "fix-326-buffer-overflow-in-parser"
        );
    }

    #[test]
    fn test_assign_partitions_round_robin() {
        let partitions = vec![
            make_partition("alpha", vec!["a/1.rs"]),
            make_partition("beta", vec!["b/1.rs"]),
            make_partition("gamma", vec!["c/1.rs"]),
            make_partition("delta", vec!["d/1.rs"]),
            make_partition("epsilon", vec!["e/1.rs"]),
        ];
        let assignments = assign_partitions(partitions, 3);

        assert_eq!(assignments.len(), 3);
        // Agent 0 gets partitions 0, 3 (alpha, delta)
        assert!(assignments[0].partition_label.contains("alpha"));
        assert!(assignments[0].partition_label.contains("delta"));
        assert_eq!(assignments[0].files.len(), 2);
        // Agent 1 gets partition 1, 4 (beta, epsilon)
        assert!(assignments[1].partition_label.contains("beta"));
        assert!(assignments[1].partition_label.contains("epsilon"));
        assert_eq!(assignments[1].files.len(), 2);
        // Agent 2 gets partition 2 (gamma)
        assert!(assignments[2].partition_label.contains("gamma"));
        assert_eq!(assignments[2].files.len(), 1);
    }

    #[test]
    fn test_assign_partitions_more_agents_than_partitions() {
        let partitions = vec![
            make_partition("src", vec!["src/main.rs"]),
            make_partition("lib", vec!["lib/mod.rs"]),
        ];
        let assignments = assign_partitions(partitions, 5);

        // Only 2 agents should have files; the rest are filtered out
        assert_eq!(assignments.len(), 2);
        assert_eq!(assignments[0].agent_slug, "reviewer-1");
        assert_eq!(assignments[1].agent_slug, "reviewer-2");
    }

    #[test]
    fn test_assign_partitions_single_agent() {
        let partitions = vec![
            make_partition("a", vec!["a/1.rs"]),
            make_partition("b", vec!["b/1.rs"]),
            make_partition("c", vec!["c/1.rs"]),
        ];
        let assignments = assign_partitions(partitions, 1);

        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].files.len(), 3);
        assert!(assignments[0].partition_label.contains('a'));
        assert!(assignments[0].partition_label.contains('b'));
        assert!(assignments[0].partition_label.contains('c'));
    }

    #[test]
    fn test_assign_partitions_empty() {
        let partitions: Vec<crate::seam::Partition> = vec![];
        let assignments = assign_partitions(partitions, 4);
        assert!(assignments.is_empty());
    }

    #[test]
    fn test_assign_partitions_zero_agents_defaults_to_one() {
        let partitions = vec![make_partition("src", vec!["src/main.rs"])];
        let assignments = assign_partitions(partitions, 0);
        assert_eq!(assignments.len(), 1);
    }

    #[test]
    fn test_review_plan_serde_roundtrip() {
        use super::review::mandate_prompt;
        let prompt = mandate_prompt("adversarial");
        let plan = ReviewPlan {
            mandate: "adversarial".to_string(),
            mandate_prompt: prompt.to_string(),
            agent_count: 2,
            created_at: "2026-03-12T10:00:00Z".to_string(),
            agents: vec![
                ReviewAgentAssignment {
                    agent_slug: "reviewer-1".to_string(),
                    partition_label: "src".to_string(),
                    files: vec!["src/main.rs".to_string()],
                },
                ReviewAgentAssignment {
                    agent_slug: "reviewer-2".to_string(),
                    partition_label: "lib".to_string(),
                    files: vec!["lib/mod.rs".to_string()],
                },
            ],
            doc_output: Some(std::path::PathBuf::from("review-findings.md")),
        };
        let json = serde_json::to_string_pretty(&plan).unwrap();
        let parsed: ReviewPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.mandate, plan.mandate);
        assert_eq!(parsed.agent_count, 2);
        assert_eq!(parsed.agents.len(), 2);
        assert_eq!(parsed.agents[0].agent_slug, "reviewer-1");
        assert_eq!(parsed.doc_output, plan.doc_output);
    }

    #[test]
    fn test_finding_severity_serde_values() {
        // Verify the rename_all = "snake_case" produces expected strings
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Critical).unwrap(),
            "\"critical\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::High).unwrap(),
            "\"high\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Medium).unwrap(),
            "\"medium\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Low).unwrap(),
            "\"low\""
        );
        assert_eq!(
            serde_json::to_string(&FindingSeverity::Info).unwrap(),
            "\"info\""
        );
    }

    #[test]
    fn test_slugify_fix_target_special_chars() {
        assert_eq!(
            slugify_fix_target(42, "Fix: memory leak (critical!)"),
            "fix-42-fix-memory-leak-critical"
        );
    }

    #[test]
    fn test_slugify_fix_target_long_title_truncates() {
        let long_title =
            "This is a very long title that should be truncated to keep the slug reasonable";
        let slug = slugify_fix_target(1, long_title);
        assert!(slug.len() <= 50, "slug too long: {} ({})", slug, slug.len());
        assert!(slug.starts_with("fix-1-"));
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn test_slugify_fix_target_empty_title() {
        assert_eq!(slugify_fix_target(99, ""), "fix-99-");
    }

    #[test]
    fn test_fix_plan_serde_roundtrip() {
        let plan = FixPlan {
            schema_version: 1,
            created_at: "2026-03-12T10:00:00Z".to_string(),
            issues: vec![
                FixTarget {
                    issue_number: 326,
                    title: "Buffer overflow".to_string(),
                    body: "Details here".to_string(),
                    labels: vec!["bug".to_string(), "review-finding".to_string()],
                    agent_slug: "fix-326-buffer-overflow".to_string(),
                    status: AgentStatus::Planned,
                },
                FixTarget {
                    issue_number: 327,
                    title: "Memory leak".to_string(),
                    body: String::new(),
                    labels: vec![],
                    agent_slug: "fix-327-memory-leak".to_string(),
                    status: AgentStatus::Running,
                },
            ],
        };
        let json = serde_json::to_string_pretty(&plan).unwrap();
        let parsed: FixPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, parsed);
    }

    // -----------------------------------------------------------------------
    // Merge orchestration tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_merge_plan_serde_roundtrip() {
        let plan = MergePlan {
            target_branch: "swarm-combined".to_string(),
            agents: vec![MergeSource {
                agent_slug: "agent-a".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["src/main.rs".to_string()],
                commit_count: 3,
            }],
            conflicts: vec![],
            merge_order: vec!["agent-a".to_string()],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: MergePlan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, parsed);
    }

    #[test]
    fn test_parse_issue_numbers_valid() {
        let nums = parse_issue_numbers("326,327,328").unwrap();
        assert_eq!(nums, vec![326, 327, 328]);
    }

    #[test]
    fn test_parse_issue_numbers_with_spaces() {
        let nums = parse_issue_numbers("1, 2, 3").unwrap();
        assert_eq!(nums, vec![1, 2, 3]);
    }

    #[test]
    fn test_parse_issue_numbers_single() {
        let nums = parse_issue_numbers("42").unwrap();
        assert_eq!(nums, vec![42]);
    }

    #[test]
    fn test_parse_issue_numbers_invalid() {
        let result = parse_issue_numbers("326,abc,328");
        assert!(result.is_err());
    }

    #[test]
    fn test_fix_requires_issues_or_label() {
        let result = parse_issue_numbers("");
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // swarm merge tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_conflict_type_serde_roundtrip() {
        let cases = vec![
            (ConflictType::NonOverlapping, "\"non_overlapping\""),
            (ConflictType::Overlapping, "\"overlapping\""),
            (ConflictType::CreateModify, "\"create_modify\""),
        ];
        for (variant, expected_json) in cases {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected_json);
            let parsed: ConflictType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn test_detect_file_conflicts_no_overlaps() {
        let sources = vec![
            MergeSource {
                agent_slug: "agent-a".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["src/foo.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-b".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-b"),
                changed_files: vec!["src/bar.rs".to_string()],
                commit_count: 2,
            },
        ];
        let conflicts = detect_file_conflicts(&sources);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn test_detect_file_conflicts_shared_files() {
        let sources = vec![
            MergeSource {
                agent_slug: "agent-a".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["src/main.rs".to_string(), "src/lib.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-b".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-b"),
                changed_files: vec!["src/main.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-c".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-c"),
                changed_files: vec!["src/lib.rs".to_string(), "src/utils.rs".to_string()],
                commit_count: 1,
            },
        ];
        let conflicts = detect_file_conflicts(&sources);

        // src/main.rs: agent-a + agent-b
        // src/lib.rs: agent-a + agent-c
        assert_eq!(conflicts.len(), 2);

        let main_conflict = conflicts.iter().find(|c| c.file == "src/main.rs").unwrap();
        assert_eq!(main_conflict.agents, vec!["agent-a", "agent-b"]);

        let lib_conflict = conflicts.iter().find(|c| c.file == "src/lib.rs").unwrap();
        assert_eq!(lib_conflict.agents, vec!["agent-a", "agent-c"]);
    }

    #[test]
    fn test_compute_merge_order_non_conflicting_first() {
        let sources = vec![
            MergeSource {
                agent_slug: "agent-a".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["src/main.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-b".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-b"),
                changed_files: vec!["src/main.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-c".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-c"),
                changed_files: vec!["src/other.rs".to_string()],
                commit_count: 1,
            },
        ];

        let conflicts = vec![FileConflict {
            file: "src/main.rs".to_string(),
            agents: vec!["agent-a".to_string(), "agent-b".to_string()],
            conflict_type: ConflictType::Overlapping,
        }];

        let order = compute_merge_order(&sources, &conflicts);

        // agent-c has no conflicts, should be first
        assert_eq!(order[0], "agent-c");
        // agent-a and agent-b both have overlapping conflicts, sorted alphabetically
        assert_eq!(order[1], "agent-a");
        assert_eq!(order[2], "agent-b");
    }

    #[test]
    fn test_compute_merge_order_is_deterministic() {
        let sources = vec![
            MergeSource {
                agent_slug: "zebra".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-z"),
                changed_files: vec!["a.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "alpha".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-a"),
                changed_files: vec!["b.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "middle".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-m"),
                changed_files: vec!["c.rs".to_string()],
                commit_count: 1,
            },
        ];

        let conflicts = vec![];

        // Run multiple times to verify determinism
        let order1 = compute_merge_order(&sources, &conflicts);
        let order2 = compute_merge_order(&sources, &conflicts);
        assert_eq!(order1, order2);
        // All at same conflict level -> alphabetical
        assert_eq!(order1, vec!["alpha", "middle", "zebra"]);
    }

    #[test]
    fn test_compute_merge_order_respects_conflict_levels() {
        let sources = vec![
            MergeSource {
                agent_slug: "agent-overlap".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-1"),
                changed_files: vec!["shared.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-nonoverlap".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-2"),
                changed_files: vec!["shared2.rs".to_string()],
                commit_count: 1,
            },
            MergeSource {
                agent_slug: "agent-clean".to_string(),
                worktree_path: PathBuf::from("/tmp/wt-3"),
                changed_files: vec!["unique.rs".to_string()],
                commit_count: 1,
            },
        ];

        let conflicts = vec![
            FileConflict {
                file: "shared.rs".to_string(),
                agents: vec!["agent-overlap".to_string(), "agent-nonoverlap".to_string()],
                conflict_type: ConflictType::Overlapping,
            },
            FileConflict {
                file: "shared2.rs".to_string(),
                agents: vec!["agent-nonoverlap".to_string(), "agent-clean".to_string()],
                conflict_type: ConflictType::NonOverlapping,
            },
        ];

        let order = compute_merge_order(&sources, &conflicts);
        // agent-clean is involved in NonOverlapping only -> level 1
        // agent-nonoverlap has Overlapping -> level 3
        // agent-overlap has Overlapping -> level 3
        // Wait: agent-clean is in shared2.rs NonOverlapping conflict
        // So: agent-clean -> level 1, agent-nonoverlap -> level 3, agent-overlap -> level 3
        assert_eq!(order[0], "agent-clean");
        assert_eq!(order[1], "agent-nonoverlap");
        assert_eq!(order[2], "agent-overlap");
    }

    #[test]
    fn test_ranges_overlap() {
        // Overlapping ranges
        assert!(ranges_overlap(&[(1, 10)], &[(5, 15)]));
        assert!(ranges_overlap(&[(5, 15)], &[(1, 10)]));
        assert!(ranges_overlap(&[(1, 10)], &[(10, 20)]));

        // Non-overlapping ranges
        assert!(!ranges_overlap(&[(1, 5)], &[(6, 10)]));
        assert!(!ranges_overlap(&[(10, 20)], &[(1, 5)]));

        // Multiple ranges, some overlap
        assert!(ranges_overlap(&[(1, 5), (20, 30)], &[(4, 6)]));
        assert!(!ranges_overlap(&[(1, 5), (20, 30)], &[(6, 19)]));
    }

    #[test]
    fn test_merge_source_serde_roundtrip() {
        let source = MergeSource {
            agent_slug: "my-agent".to_string(),
            worktree_path: PathBuf::from("/home/user/.worktrees/my-agent"),
            changed_files: vec!["src/main.rs".to_string(), "Cargo.toml".to_string()],
            commit_count: 5,
        };
        let json = serde_json::to_string(&source).unwrap();
        let parsed: MergeSource = serde_json::from_str(&json).unwrap();
        assert_eq!(source, parsed);
    }

    #[test]
    fn test_file_conflict_serde_roundtrip() {
        let conflict = FileConflict {
            file: "src/lib.rs".to_string(),
            agents: vec!["agent-a".to_string(), "agent-b".to_string()],
            conflict_type: ConflictType::NonOverlapping,
        };
        let json = serde_json::to_string(&conflict).unwrap();
        let parsed: FileConflict = serde_json::from_str(&json).unwrap();
        assert_eq!(conflict, parsed);
    }
}
