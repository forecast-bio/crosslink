use std::path::Path;
use std::time::Duration;

use super::helpers::*;
use super::launch::*;
use super::monitor::*;
use super::plan::*;
use super::prompt::*;
use super::types::*;

#[test]
fn test_slugify_basic() {
    assert_eq!(slugify("add batch retry logic"), "add-batch-retry-logic");
}

#[test]
fn test_slugify_special_chars() {
    assert_eq!(
        slugify("Fix: authentication (timeout) on slow connections!"),
        "fix-authentication-timeout-on-slow-connections"
    );
}

#[test]
fn test_slugify_truncation() {
    let long_desc = "add a very long feature description that definitely exceeds the sixty character limit for branch slugs";
    let slug = slugify(long_desc);
    assert!(slug.len() <= 60, "slug too long: {} chars", slug.len());
    assert!(!slug.ends_with('-'));
}

#[test]
fn test_slugify_leading_trailing_hyphens() {
    assert_eq!(slugify("  hello world  "), "hello-world");
}

#[test]
fn test_parse_duration_hours() {
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
}

#[test]
fn test_parse_duration_minutes() {
    assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
}

#[test]
fn test_parse_duration_seconds() {
    assert_eq!(parse_duration("90s").unwrap(), Duration::from_secs(90));
}

#[test]
fn test_parse_duration_bare_number() {
    assert_eq!(parse_duration("120").unwrap(), Duration::from_secs(120));
}

#[test]
fn test_parse_duration_zero() {
    assert!(parse_duration("0h").is_err());
}

#[test]
fn test_parse_duration_empty() {
    assert!(parse_duration("").is_err());
}

#[test]
fn test_parse_duration_invalid() {
    assert!(parse_duration("abc").is_err());
}

#[test]
fn test_parse_container_mode() {
    assert_eq!(parse_container_mode("none").unwrap(), ContainerMode::None);
    assert_eq!(parse_container_mode("local").unwrap(), ContainerMode::None);
    assert_eq!(
        parse_container_mode("docker").unwrap(),
        ContainerMode::Docker
    );
    assert_eq!(
        parse_container_mode("podman").unwrap(),
        ContainerMode::Podman
    );
    assert_eq!(
        parse_container_mode("Docker").unwrap(),
        ContainerMode::Docker
    );
    assert!(parse_container_mode("kubernetes").is_err());
}

#[test]
fn test_parse_verify_level() {
    assert_eq!(parse_verify_level("local").unwrap(), VerifyLevel::Local);
    assert_eq!(parse_verify_level("ci").unwrap(), VerifyLevel::Ci);
    assert_eq!(
        parse_verify_level("thorough").unwrap(),
        VerifyLevel::Thorough
    );
    assert_eq!(parse_verify_level("CI").unwrap(), VerifyLevel::Ci);
    assert!(parse_verify_level("extreme").is_err());
}

#[test]
fn test_tmux_session_name() {
    assert_eq!(
        tmux_session_name("XZ3j-81jF-add-batch-retry-logic"),
        "XZ3j-81jF-add-batch-retry-logic"
    );
}

#[test]
fn test_tmux_session_name_sanitization() {
    assert_eq!(
        tmux_session_name("XZ3j-81jF-fix.auth:bug"),
        "XZ3j-81jF-fix-auth-bug"
    );
}

#[test]
fn test_tmux_session_name_truncation() {
    let long = "a".repeat(70);
    let name = tmux_session_name(&long);
    assert!(name.len() <= 64);
}

#[test]
fn test_build_prompt_contains_essentials() {
    let conventions = ProjectConventions {
        test_command: Some("cargo test".to_string()),
        lint_commands: vec!["cargo clippy -- -D warnings".to_string()],
        allowed_tools: vec!["Bash(cargo *)".to_string()],
    };
    let opts = KickoffOpts {
        description: "add retry logic",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: None,
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 42, "feature/add-retry-logic", &conventions);

    assert!(prompt.contains("add retry logic"));
    assert!(prompt.contains("#42"));
    assert!(prompt.contains("feature/add-retry-logic"));
    assert!(prompt.contains("cargo test"));
    assert!(prompt.contains("KICKOFF"));
    assert!(prompt.contains("crosslink session"));
}

#[test]
fn test_build_prompt_ci_verification() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "test ci",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Ci,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: None,
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test-ci", &conventions);

    assert!(prompt.contains("CI Verification"));
    assert!(prompt.contains("gh pr create"));
    assert!(!prompt.contains("Adversarial"));
}

#[test]
fn test_build_prompt_thorough_verification() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "test thorough",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Thorough,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: None,
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test-thorough", &conventions);

    assert!(prompt.contains("CI Verification"));
    assert!(prompt.contains("Adversarial Self-Review"));
}

#[test]
fn test_build_allowed_tools_base() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let tools = build_allowed_tools(&conventions, &VerifyLevel::Local);
    assert!(tools.contains("Read"));
    assert!(tools.contains("Bash(crosslink *)"));
    assert!(!tools.contains("Bash(gh *)"));
}

#[test]
fn test_build_allowed_tools_ci() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec!["Bash(cargo *)".to_string()],
    };
    let tools = build_allowed_tools(&conventions, &VerifyLevel::Ci);
    assert!(tools.contains("Bash(gh *)"));
    assert!(tools.contains("Bash(cargo *)"));
}

#[test]
fn test_detect_conventions_rust() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

    let conv = detect_conventions(dir.path());
    assert_eq!(conv.test_command.as_deref(), Some("cargo test"));
    assert!(conv.allowed_tools.contains(&"Bash(cargo *)".to_string()));
}

#[test]
fn test_detect_conventions_node() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();

    let conv = detect_conventions(dir.path());
    assert_eq!(conv.test_command.as_deref(), Some("npm test"));
    assert!(conv.allowed_tools.contains(&"Bash(npm *)".to_string()));
}

#[test]
fn test_rand_suffix_range() {
    let s = rand_suffix();
    assert!(s < 10000);
}

// --- New tests for extracted pure functions ---

#[test]
fn test_slugify_all_special_chars() {
    assert_eq!(slugify("!!!@@@###"), "");
}

#[test]
fn test_slugify_single_word() {
    assert_eq!(slugify("refactor"), "refactor");
}

#[test]
fn test_slugify_unicode() {
    // Rust's is_alphanumeric() includes Unicode letters like é
    assert_eq!(slugify("add café support"), "add-café-support");
}

#[test]
fn test_slugify_consecutive_separators() {
    assert_eq!(slugify("fix -- the -- bug"), "fix-the-bug");
}

#[test]
fn test_slugify_numbers() {
    assert_eq!(slugify("add v2 api endpoint"), "add-v2-api-endpoint");
}

#[test]
fn test_slugify_empty() {
    assert_eq!(slugify(""), "");
}

#[test]
fn test_slugify_truncation_cuts_at_word_boundary() {
    // 61+ chars, should cut at last hyphen before 60
    let desc = "implement-the-very-important-feature-that-does-something-really-great";
    let slug = slugify(desc);
    assert!(slug.len() <= 60);
    assert!(!slug.ends_with('-'));
}

#[test]
fn test_verify_level_name() {
    assert_eq!(verify_level_name(&VerifyLevel::Local), "local");
    assert_eq!(verify_level_name(&VerifyLevel::Ci), "ci");
    assert_eq!(verify_level_name(&VerifyLevel::Thorough), "thorough");
}

#[test]
fn test_build_test_lint_instructions_with_commands() {
    let conv = ProjectConventions {
        test_command: Some("cargo test".to_string()),
        lint_commands: vec![
            "cargo clippy -- -D warnings".to_string(),
            "cargo fmt --check".to_string(),
        ],
        allowed_tools: vec![],
    };
    let section = build_test_lint_instructions(&conv, 42);
    assert!(section.contains("`cargo test`"));
    assert!(section.contains("`cargo clippy -- -D warnings`"));
    assert!(section.contains("`cargo fmt --check`"));
    assert!(section.contains("crosslink comment 42"));
}

#[test]
fn test_build_test_lint_instructions_without_commands() {
    let conv = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let section = build_test_lint_instructions(&conv, 7);
    assert!(section.contains("Run the project's test suite"));
    assert!(section.contains("Run lint and format checks"));
    assert!(section.contains("crosslink comment 7"));
}

#[test]
fn test_build_ci_verification_section_content() {
    let section = build_ci_verification_section();
    assert!(section.contains("CI Verification"));
    assert!(section.contains("gh pr create"));
    assert!(section.contains("gh run list"));
    assert!(section.contains("CI_FAILED"));
    assert!(section.contains("Maximum 5 CI fix-and-retry"));
}

#[test]
fn test_build_adversarial_review_section_content() {
    let section = build_adversarial_review_section();
    assert!(section.contains("Adversarial Self-Review"));
    assert!(section.contains("git diff main...HEAD"));
    assert!(section.contains("unwrap()"));
}

#[test]
fn test_build_final_steps_section_content() {
    let section = build_final_steps_section();
    assert!(section.contains("Self-review checklist"));
    assert!(section.contains("crosslink session end"));
    assert!(section.contains(".kickoff-status"));
    assert!(section.contains("DONE"));
}

#[test]
fn test_missing_exclude_patterns_empty_file() {
    let patterns = missing_exclude_patterns("");
    assert_eq!(
        patterns,
        vec![
            "KICKOFF.md",
            ".kickoff-status",
            ".kickoff-slug",
            ".kickoff-metadata.json",
            "PLAN_KICKOFF.md",
            ".kickoff-plan.json",
            ".kickoff-criteria.json",
            ".kickoff-report.json",
        ]
    );
}

#[test]
fn test_missing_exclude_patterns_one_present() {
    let patterns = missing_exclude_patterns("KICKOFF.md\nsome-other-file\n");
    assert!(patterns.contains(&".kickoff-status"));
    assert!(patterns.contains(&".kickoff-slug"));
    assert!(patterns.contains(&"PLAN_KICKOFF.md"));
    assert!(patterns.contains(&".kickoff-plan.json"));
    assert!(patterns.contains(&".kickoff-criteria.json"));
    assert!(patterns.contains(&".kickoff-report.json"));
    assert!(!patterns.contains(&"KICKOFF.md"));
}

#[test]
fn test_missing_exclude_patterns_all_present() {
    let patterns = missing_exclude_patterns(
        "KICKOFF.md\n.kickoff-status\n.kickoff-slug\n.kickoff-metadata.json\nPLAN_KICKOFF.md\n.kickoff-plan.json\n.kickoff-criteria.json\n.kickoff-report.json\n",
    );
    assert!(patterns.is_empty());
}

#[test]
fn test_missing_exclude_patterns_with_whitespace() {
    let patterns = missing_exclude_patterns(
        "  KICKOFF.md  \n  .kickoff-status  \n  .kickoff-slug  \n  .kickoff-metadata.json  \n  PLAN_KICKOFF.md  \n  .kickoff-plan.json  \n  .kickoff-criteria.json  \n  .kickoff-report.json  \n",
    );
    assert!(patterns.is_empty());
}

#[test]
fn test_build_allowed_tools_thorough() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let tools = build_allowed_tools(&conventions, &VerifyLevel::Thorough);
    assert!(tools.contains("Bash(gh *)"));
    assert!(tools.contains("Bash(sleep *)"));
}

#[test]
fn test_build_allowed_tools_includes_project_tools() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec!["Bash(cargo *)".to_string(), "Bash(npm *)".to_string()],
    };
    let tools = build_allowed_tools(&conventions, &VerifyLevel::Local);
    assert!(tools.contains("Bash(cargo *)"));
    assert!(tools.contains("Bash(npm *)"));
    assert!(!tools.contains("Bash(gh *)"));
}

#[test]
fn test_detect_conventions_python() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pyproject.toml"), "[project]").unwrap();

    let conv = detect_conventions(dir.path());
    assert_eq!(conv.test_command.as_deref(), Some("uv run pytest"));
    assert!(conv.lint_commands.contains(&"ruff check .".to_string()));
    assert!(conv.allowed_tools.contains(&"Bash(python3 *)".to_string()));
}

#[test]
fn test_detect_conventions_go() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example").unwrap();

    let conv = detect_conventions(dir.path());
    assert_eq!(conv.test_command.as_deref(), Some("go test ./..."));
    assert!(conv.lint_commands.contains(&"go vet ./...".to_string()));
    assert!(conv.allowed_tools.contains(&"Bash(go *)".to_string()));
}

#[test]
fn test_detect_conventions_just() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("justfile"), "build:").unwrap();

    let conv = detect_conventions(dir.path());
    assert!(conv.allowed_tools.contains(&"Bash(just *)".to_string()));
}

#[test]
fn test_detect_conventions_make() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Makefile"), "build:").unwrap();

    let conv = detect_conventions(dir.path());
    assert!(conv.allowed_tools.contains(&"Bash(make *)".to_string()));
}

#[test]
fn test_detect_conventions_elixir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mix.exs"),
        r#"defmodule MyApp.MixProject do
  use Mix.Project
  defp deps do
[{:phoenix, "~> 1.7"}, {:credo, "~> 1.7", only: [:dev, :test]}, {:sobelow, "~> 0.13", only: :dev}]
  end
end"#,
    )
    .unwrap();

    let conv = detect_conventions(dir.path());
    assert_eq!(conv.test_command.as_deref(), Some("mix test"));
    assert!(conv
        .lint_commands
        .contains(&"mix format --check-formatted".to_string()));
    assert!(conv
        .lint_commands
        .contains(&"mix credo --strict".to_string()));
    assert!(conv
        .lint_commands
        .contains(&"mix sobelow --config".to_string()));
    assert!(conv.allowed_tools.contains(&"Bash(mix test *)".to_string()));
    assert!(conv
        .allowed_tools
        .contains(&"Bash(mix credo *)".to_string()));
    assert!(conv
        .allowed_tools
        .contains(&"Bash(mix sobelow *)".to_string()));
    assert!(conv
        .allowed_tools
        .contains(&"Bash(mix phx.routes *)".to_string()));
}

#[test]
fn test_detect_conventions_elixir_minimal() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mix.exs"),
        "defmodule MyApp.MixProject do\n  use Mix.Project\nend",
    )
    .unwrap();

    let conv = detect_conventions(dir.path());
    assert_eq!(conv.test_command.as_deref(), Some("mix test"));
    assert!(conv
        .lint_commands
        .contains(&"mix format --check-formatted".to_string()));
    // No credo/sobelow in a minimal mix.exs
    assert!(!conv
        .lint_commands
        .contains(&"mix credo --strict".to_string()));
    assert!(!conv
        .allowed_tools
        .contains(&"mcp__tidewave__get_logs".to_string()));
}

#[test]
fn test_detect_conventions_elixir_with_tidewave() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("mix.exs"),
        r#"defmodule MyApp.MixProject do
  defp deps do
[{:tidewave, "~> 0.1", only: :dev}]
  end
end"#,
    )
    .unwrap();

    let conv = detect_conventions(dir.path());
    assert!(conv
        .allowed_tools
        .contains(&"mcp__tidewave__get_logs".to_string()));
    assert!(conv
        .allowed_tools
        .contains(&"mcp__tidewave__get_docs".to_string()));
    assert!(conv
        .allowed_tools
        .contains(&"mcp__tidewave__project_eval".to_string()));
}

#[test]
fn test_detect_conventions_empty_dir() {
    let dir = tempfile::tempdir().unwrap();

    let conv = detect_conventions(dir.path());
    assert!(conv.test_command.is_none());
    assert!(conv.lint_commands.is_empty());
    assert!(conv.allowed_tools.is_empty());
}

#[test]
fn test_detect_conventions_multi_language() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    std::fs::write(dir.path().join("package.json"), "{}").unwrap();

    let conv = detect_conventions(dir.path());
    // Rust gets priority for test_command
    assert_eq!(conv.test_command.as_deref(), Some("cargo test"));
    // Both toolchains present
    assert!(conv.allowed_tools.contains(&"Bash(cargo *)".to_string()));
    assert!(conv.allowed_tools.contains(&"Bash(npm *)".to_string()));
}

#[test]
fn test_detect_conventions_requirements_txt() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();

    let conv = detect_conventions(dir.path());
    assert_eq!(conv.test_command.as_deref(), Some("uv run pytest"));
    assert!(conv.allowed_tools.contains(&"Bash(uv *)".to_string()));
}

#[test]
fn test_detect_conventions_crosslink_subdir_cargo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("crosslink")).unwrap();
    std::fs::write(dir.path().join("crosslink/Cargo.toml"), "[package]").unwrap();

    let conv = detect_conventions(dir.path());
    assert_eq!(conv.test_command.as_deref(), Some("cargo test"));
}

#[test]
fn test_parse_duration_whitespace() {
    assert_eq!(
        parse_duration("  30m  ").unwrap(),
        Duration::from_secs(1800)
    );
}

#[test]
fn test_parse_duration_large_value() {
    assert_eq!(parse_duration("24h").unwrap(), Duration::from_secs(86400));
}

#[test]
fn test_tmux_session_name_empty() {
    assert_eq!(tmux_session_name(""), "");
}

#[test]
fn test_build_prompt_local_has_no_ci_or_adversarial() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "test local",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: None,
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test-local", &conventions);

    assert!(!prompt.contains("CI Verification"));
    assert!(!prompt.contains("Adversarial Self-Review"));
    assert!(prompt.contains("Final Steps"));
}

#[test]
fn test_build_prompt_contains_blocked_actions() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "test blocked actions",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: None,
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test", &conventions);

    assert!(prompt.contains("Blocked Actions"));
    assert!(prompt.contains("git push"));
    assert!(prompt.contains("git merge"));
    assert!(prompt.contains("git reset"));
}

#[test]
fn test_build_prompt_embeds_issue_id_in_instructions() {
    let conventions = ProjectConventions {
        test_command: Some("cargo test".to_string()),
        lint_commands: vec!["cargo clippy".to_string()],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "test issue refs",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: None,
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 999, "feature/test-refs", &conventions);

    // Issue ID should appear in context header and in session/comment instructions
    assert!(prompt.contains("#999"));
    assert!(prompt.contains("crosslink session work 999"));
    assert!(prompt.contains("crosslink comment 999"));
}

#[test]
fn test_build_prompt_empty_conventions_uses_generic_instructions() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "test generic",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: None,
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test-generic", &conventions);

    // Without specific test/lint commands, prompt should use generic phrasing
    assert!(prompt.contains("Run the project's test suite"));
    assert!(prompt.contains("Run lint and format checks"));
    // Should NOT contain backtick-quoted commands
    assert!(!prompt.contains("`cargo test`"));
}

#[test]
fn test_build_prompt_with_design_doc() {
    let doc = super::super::design_doc::DesignDoc {
        title: "Batch Retry".to_string(),
        summary: "Add retry logic.".to_string(),
        requirements: vec!["REQ-1: Retry 3 times".to_string()],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["AC-1: Tests pass".to_string()],
        architecture: "Middleware pattern".to_string(),
        open_questions: Vec::new(),
        out_of_scope: vec!["Not doing X".to_string()],
        unknown_sections: Vec::new(),
    };
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "batch retry",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: Some(&doc),
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/batch-retry", &conventions);

    assert!(prompt.contains("## Design Specification"));
    assert!(prompt.contains("Add retry logic."));
    assert!(prompt.contains("REQ-1: Retry 3 times"));
    assert!(prompt.contains("AC-1: Tests pass"));
    assert!(prompt.contains("Middleware pattern"));
    assert!(prompt.contains("Not doing X"));
    // No open questions, so no escalation block
    assert!(!prompt.contains("Escalation Required"));
}

#[test]
fn test_build_plan_prompt_contains_essentials() {
    let doc = super::super::design_doc::DesignDoc {
        title: "Batch Retry".to_string(),
        summary: "Add retry logic.".to_string(),
        requirements: vec!["REQ-1: Retry 3 times".to_string()],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["AC-1: Tests pass".to_string()],
        architecture: "Middleware".to_string(),
        open_questions: Vec::new(),
        out_of_scope: Vec::new(),
        unknown_sections: Vec::new(),
    };
    let prompt = build_plan_prompt(&doc, Some(42), None);

    assert!(prompt.contains("KICKOFF PLAN"));
    assert!(prompt.contains("Batch Retry"));
    assert!(prompt.contains("#42"));
    assert!(prompt.contains("Design Specification"));
    assert!(prompt.contains("REQ-1: Retry 3 times"));
    assert!(prompt.contains(".kickoff-plan.json"));
    assert!(prompt.contains("read-only"));
    assert!(prompt.contains("gaps"));
    assert!(prompt.contains("assumptions"));
    assert!(prompt.contains("estimated_subtasks"));
    assert!(prompt.contains("conflicts"));
}

#[test]
fn test_build_plan_prompt_with_open_questions() {
    let doc = super::super::design_doc::DesignDoc {
        title: "Auth".to_string(),
        summary: String::new(),
        requirements: Vec::new(),
        requirement_groups: Vec::new(),
        acceptance_criteria: Vec::new(),
        architecture: String::new(),
        open_questions: vec!["Q1: OAuth or JWT?".to_string()],
        out_of_scope: Vec::new(),
        unknown_sections: Vec::new(),
    };
    let prompt = build_plan_prompt(&doc, None, None);

    assert!(prompt.contains("Escalation Required"));
    assert!(prompt.contains("Q1: OAuth or JWT?"));
    // No issue line when None
    assert!(!prompt.contains("Issue"));
}

#[test]
fn test_build_plan_prompt_without_issue() {
    let doc = super::super::design_doc::DesignDoc {
        title: "Test".to_string(),
        summary: "S".to_string(),
        requirements: Vec::new(),
        requirement_groups: Vec::new(),
        acceptance_criteria: Vec::new(),
        architecture: String::new(),
        open_questions: Vec::new(),
        out_of_scope: Vec::new(),
        unknown_sections: Vec::new(),
    };
    let prompt = build_plan_prompt(&doc, None, None);

    assert!(prompt.contains("KICKOFF PLAN"));
    // No issue line when None
    assert!(!prompt.contains("**Issue**"));
}

#[test]
fn test_build_allowed_tools_plan_is_read_only() {
    let tools = build_allowed_tools_plan();
    assert!(tools.contains("Read"));
    assert!(tools.contains("Glob"));
    assert!(tools.contains("Grep"));
    assert!(!tools.contains("Write"));
    assert!(!tools.contains("Edit"));
}

#[test]
fn test_build_allowed_tools_plan_no_destructive_bash() {
    let tools = build_allowed_tools_plan();
    assert!(!tools.contains("Bash(mkdir"));
    assert!(!tools.contains("Bash(touch"));
    assert!(!tools.contains("Bash(echo"));
    // But read-only bash is allowed
    assert!(tools.contains("Bash(git status"));
    assert!(tools.contains("Bash(ls"));
}

#[test]
fn test_missing_exclude_patterns_includes_plan_files() {
    let patterns = missing_exclude_patterns("");
    assert!(patterns.contains(&"PLAN_KICKOFF.md"));
    assert!(patterns.contains(&".kickoff-plan.json"));
}

#[test]
fn test_build_prompt_with_design_doc_open_questions() {
    let doc = super::super::design_doc::DesignDoc {
        title: "Auth Feature".to_string(),
        summary: "Add auth.".to_string(),
        requirements: vec!["REQ-1: Login".to_string()],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["AC-1: Can log in".to_string()],
        architecture: String::new(),
        open_questions: vec![
            "Q1: OAuth or JWT?".to_string(),
            "Q2: Session duration?".to_string(),
        ],
        out_of_scope: Vec::new(),
        unknown_sections: Vec::new(),
    };
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "auth feature",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: Some(&doc),
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/auth", &conventions);

    assert!(prompt.contains("## Design Specification"));
    assert!(prompt.contains("Escalation Required"));
    assert!(prompt.contains("Q1: OAuth or JWT?"));
    assert!(prompt.contains("Q2: Session duration?"));
    assert!(prompt.contains("crosslink comment"));
}

// --- Round 1: Criteria extraction tests ---

#[test]
fn test_parse_criterion_id_with_prefix() {
    let (id, text) = parse_criterion_id("AC-1: Tests pass");
    assert_eq!(id, "AC-1");
    assert_eq!(text, "Tests pass");
}

#[test]
fn test_parse_criterion_id_without_prefix() {
    let (id, text) = parse_criterion_id("Tests pass");
    assert_eq!(id, "");
    assert_eq!(text, "Tests pass");
}

#[test]
fn test_parse_criterion_id_multidigit() {
    let (id, text) = parse_criterion_id("AC-12: Complex thing");
    assert_eq!(id, "AC-12");
    assert_eq!(text, "Complex thing");
}

#[test]
fn test_parse_criterion_id_lowercase() {
    let (id, text) = parse_criterion_id("ac-3: Lower case");
    assert_eq!(id, "AC-3");
    assert_eq!(text, "Lower case");
}

#[test]
fn test_extract_criteria_all_explicit() {
    let doc = super::super::design_doc::DesignDoc {
        title: String::new(),
        summary: String::new(),
        requirements: vec![],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["AC-1: First".to_string(), "AC-2: Second".to_string()],
        architecture: String::new(),
        open_questions: vec![],
        out_of_scope: vec![],
        unknown_sections: vec![],
    };
    let result = extract_criteria(&doc, "test.md");
    assert_eq!(result.criteria.len(), 2);
    assert_eq!(result.criteria[0].id, "AC-1");
    assert_eq!(result.criteria[0].text, "First");
    assert_eq!(result.criteria[1].id, "AC-2");
    assert_eq!(result.criteria[1].text, "Second");
    assert_eq!(result.source_doc, "test.md");
}

#[test]
fn test_extract_criteria_all_auto() {
    let doc = super::super::design_doc::DesignDoc {
        title: String::new(),
        summary: String::new(),
        requirements: vec![],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["First item".to_string(), "Second item".to_string()],
        architecture: String::new(),
        open_questions: vec![],
        out_of_scope: vec![],
        unknown_sections: vec![],
    };
    let result = extract_criteria(&doc, "test.md");
    assert_eq!(result.criteria[0].id, "AC-1");
    assert_eq!(result.criteria[0].text, "First item");
    assert_eq!(result.criteria[1].id, "AC-2");
    assert_eq!(result.criteria[1].text, "Second item");
    assert_eq!(result.criteria[0].criterion_type, "functional");
}

#[test]
fn test_extract_criteria_mixed_ids_skip_collisions() {
    let doc = super::super::design_doc::DesignDoc {
        title: String::new(),
        summary: String::new(),
        requirements: vec![],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec![
            "AC-1: Explicit first".to_string(),
            "Auto assigned".to_string(),
            "AC-3: Explicit third".to_string(),
            "Another auto".to_string(),
        ],
        architecture: String::new(),
        open_questions: vec![],
        out_of_scope: vec![],
        unknown_sections: vec![],
    };
    let result = extract_criteria(&doc, "design.md");
    assert_eq!(result.criteria[0].id, "AC-1");
    assert_eq!(result.criteria[1].id, "AC-2"); // skips AC-1, takes AC-2
    assert_eq!(result.criteria[2].id, "AC-3");
    assert_eq!(result.criteria[3].id, "AC-4"); // skips AC-3, takes AC-4
}

// --- Round 2: Validation prompt tests ---

#[test]
fn test_build_reporting_section_has_full_schema() {
    let section = build_reporting_section();
    // Phase 3 validation content
    assert!(section.contains("Spec Validation"));
    assert!(section.contains(".kickoff-criteria.json"));
    assert!(section.contains(".kickoff-report.json"));
    assert!(section.contains("pass"));
    assert!(section.contains("fail"));
    assert!(section.contains("partial"));
    assert!(section.contains("evidence"));
    // Phase 4 schema elements
    assert!(section.contains("schema_version"));
    assert!(section.contains("agent_id"));
    assert!(section.contains("phases"));
    assert!(section.contains("commits"));
    assert!(section.contains("files_changed"));
    assert!(section.contains("duration_s"));
}

#[test]
fn test_build_reporting_section_has_validation_instructions() {
    let section = build_reporting_section();
    assert!(section.contains("not_applicable"));
    assert!(section.contains("needs_clarification"));
    assert!(section.contains("Be strict"));
    assert!(section.contains("concrete evidence"));
}

#[test]
fn test_build_prompt_with_criteria_includes_validation() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let doc = super::super::design_doc::DesignDoc {
        title: "Test".to_string(),
        summary: "Summary".to_string(),
        requirements: vec![],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["Users can log in".to_string()],
        architecture: String::new(),
        open_questions: vec![],
        out_of_scope: vec![],
        unknown_sections: vec![],
    };
    let opts = KickoffOpts {
        description: "test feature",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: Some(&doc),
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test", &conventions);
    assert!(prompt.contains("Spec Validation"));
    assert!(prompt.contains(".kickoff-criteria.json"));
    assert!(prompt.contains("schema_version"));
    assert!(prompt.contains("phases"));
}

#[test]
fn test_build_prompt_without_criteria_no_validation() {
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let doc = super::super::design_doc::DesignDoc {
        title: "Test".to_string(),
        summary: "Summary".to_string(),
        requirements: vec![],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec![],
        architecture: String::new(),
        open_questions: vec![],
        out_of_scope: vec![],
        unknown_sections: vec![],
    };
    let opts = KickoffOpts {
        description: "test feature",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: Some(&doc),
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test", &conventions);
    assert!(!prompt.contains("Spec Validation"));
}

#[test]
fn test_build_prompt_validation_ordering() {
    let conventions = ProjectConventions {
        test_command: Some("cargo test".to_string()),
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let doc = super::super::design_doc::DesignDoc {
        title: "Test".to_string(),
        summary: "Summary".to_string(),
        requirements: vec![],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["Criterion one".to_string()],
        architecture: String::new(),
        open_questions: vec![],
        out_of_scope: vec![],
        unknown_sections: vec![],
    };
    let opts = KickoffOpts {
        description: "test feature",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: Some(&doc),
        doc_path: None,
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test", &conventions);
    let test_pos = prompt.find("Run tests").expect("should have test section");
    let validation_pos = prompt
        .find("Spec Validation")
        .expect("should have validation");
    let final_pos = prompt.find("Final Steps").expect("should have final steps");
    assert!(
        test_pos < validation_pos,
        "validation should come after tests"
    );
    assert!(
        validation_pos < final_pos,
        "validation should come before final steps"
    );
}

// --- Round 3: Report command tests ---

fn sample_report() -> KickoffReport {
    KickoffReport {
        validated_at: "2026-03-03T12:00:00Z".to_string(),
        criteria: vec![
            CriterionVerdict {
                id: "AC-1".to_string(),
                verdict: "pass".to_string(),
                evidence: "test_login passes".to_string(),
            },
            CriterionVerdict {
                id: "AC-2".to_string(),
                verdict: "partial".to_string(),
                evidence: "HTTP only, not WebSocket".to_string(),
            },
            CriterionVerdict {
                id: "AC-3".to_string(),
                verdict: "fail".to_string(),
                evidence: "not implemented".to_string(),
            },
        ],
        summary: ReportSummary {
            total: 3,
            pass: 1,
            fail: 1,
            partial: 1,
            not_applicable: 0,
            needs_clarification: 0,
        },
        schema_version: None,
        agent_id: None,
        issue_id: None,
        status: None,
        started_at: None,
        completed_at: None,
        phases: None,
        unresolved_questions: None,
        commits: None,
        files_changed: None,
    }
}

#[test]
fn test_format_report_table_symbols() {
    let report = sample_report();
    let output = format_report_table(&report);
    assert!(output.contains("\u{2713} AC-1"));
    assert!(output.contains("~ AC-2"));
    assert!(output.contains("\u{2717} AC-3"));
}

#[test]
fn test_format_report_table_summary_line() {
    let report = sample_report();
    let output = format_report_table(&report);
    assert!(output.contains("3 criteria: 1 pass, 1 partial, 1 fail"));
}

#[test]
fn test_format_report_markdown_has_table_header() {
    let report = sample_report();
    let output = format_report_markdown(&report);
    assert!(output.contains("| ID | Verdict | Evidence |"));
    assert!(output.contains("|---|---|---|"));
    assert!(output.contains("| AC-1 |"));
}

#[test]
fn test_kickoff_report_deserialization() {
    let report = sample_report();
    let json = serde_json::to_string(&report).unwrap();
    let parsed: KickoffReport = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, report);
}

#[test]
fn test_exclude_patterns_includes_report_files() {
    let patterns = missing_exclude_patterns("");
    assert!(patterns.contains(&".kickoff-criteria.json"));
    assert!(patterns.contains(&".kickoff-report.json"));
}

// --- Round 1 (Phase 4): KickoffReport schema tests ---

#[test]
fn test_kickoff_report_backward_compat() {
    // Old Phase 3 JSON with only validated_at, criteria, summary
    let old_json = r#"{
        "validated_at": "2026-03-03T12:00:00Z",
        "criteria": [
            { "id": "AC-1", "verdict": "pass", "evidence": "test passes" }
        ],
        "summary": {
            "total": 1, "pass": 1, "fail": 0,
            "partial": 0, "not_applicable": 0, "needs_clarification": 0
        }
    }"#;
    let report: KickoffReport = serde_json::from_str(old_json).unwrap();
    assert_eq!(report.criteria.len(), 1);
    assert!(report.schema_version.is_none());
    assert!(report.agent_id.is_none());
    assert!(report.phases.is_none());
    assert!(report.commits.is_none());
    assert!(report.files_changed.is_none());
}

#[test]
fn test_kickoff_report_full_roundtrip() {
    let report = KickoffReport {
        validated_at: "2026-03-03T14:00:00Z".to_string(),
        criteria: vec![CriterionVerdict {
            id: "AC-1".to_string(),
            verdict: "pass".to_string(),
            evidence: "all tests green".to_string(),
        }],
        summary: ReportSummary {
            total: 1,
            pass: 1,
            fail: 0,
            partial: 0,
            not_applicable: 0,
            needs_clarification: 0,
        },
        schema_version: Some(1),
        agent_id: Some("driver--batch-retry".to_string()),
        issue_id: Some(42),
        status: Some("completed".to_string()),
        started_at: Some("2026-03-03T12:00:00Z".to_string()),
        completed_at: Some("2026-03-03T14:00:00Z".to_string()),
        phases: Some(PhaseTimings {
            exploration: Some(PhaseTiming {
                duration_s: 120,
                files_read: Some(34),
                ..Default::default()
            }),
            testing: Some(PhaseTiming {
                duration_s: 90,
                tests_run: Some(146),
                tests_passed: Some(146),
                tests_failed: Some(0),
                ..Default::default()
            }),
            ..Default::default()
        }),
        unresolved_questions: Some(vec!["Max backoff?".to_string()]),
        commits: Some(vec!["abc1234".to_string(), "def5678".to_string()]),
        files_changed: Some(vec!["src/retry.rs".to_string()]),
    };
    let json = serde_json::to_string_pretty(&report).unwrap();
    let parsed: KickoffReport = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, report);
}

#[test]
fn test_phase_timing_partial_fields() {
    let json = r#"{ "duration_s": 60 }"#;
    let timing: PhaseTiming = serde_json::from_str(json).unwrap();
    assert_eq!(timing.duration_s, 60);
    assert!(timing.files_read.is_none());
    assert!(timing.tests_run.is_none());
}

#[test]
fn test_validate_kickoff_report_warnings() {
    let report = sample_report();
    let warnings = validate_kickoff_report(&report);
    assert!(warnings.iter().any(|w| w.contains("schema_version")));
    assert!(warnings.iter().any(|w| w.contains("agent_id")));
}

// --- Round 3 (Phase 4): Report formatting + --all tests ---

#[test]
fn test_format_duration() {
    assert_eq!(format_duration(30), "30s");
    assert_eq!(format_duration(60), "1m");
    assert_eq!(format_duration(90), "1m 30s");
    assert_eq!(format_duration(3600), "1h");
    assert_eq!(format_duration(5400), "1h 30m");
    assert_eq!(format_duration(7200), "2h");
}

#[test]
fn test_format_report_table_with_phases() {
    let mut report = sample_report();
    report.agent_id = Some("driver--batch-retry".to_string());
    report.issue_id = Some(42);
    report.status = Some("completed".to_string());
    report.phases = Some(PhaseTimings {
        exploration: Some(PhaseTiming {
            duration_s: 120,
            files_read: Some(34),
            ..Default::default()
        }),
        testing: Some(PhaseTiming {
            duration_s: 90,
            tests_run: Some(146),
            tests_passed: Some(146),
            tests_failed: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    });
    let output = format_report_table(&report);
    assert!(output.contains("driver--batch-retry"));
    assert!(output.contains("Issue: #42"));
    assert!(output.contains("Phase Timing:"));
    assert!(output.contains("exploration"));
    assert!(output.contains("34 files read"));
    assert!(output.contains("146/146 passed"));
}

#[test]
fn test_format_report_table_without_phases() {
    let report = sample_report();
    let output = format_report_table(&report);
    assert!(!output.contains("Phase Timing:"));
    assert!(output.contains("Acceptance Criteria:"));
}

#[test]
fn test_format_report_markdown_with_metadata() {
    let mut report = sample_report();
    report.agent_id = Some("driver--test".to_string());
    report.issue_id = Some(10);
    report.status = Some("completed".to_string());
    let output = format_report_markdown(&report);
    assert!(output.contains("**Agent**: driver--test"));
    assert!(output.contains("**Issue**: #10"));
    assert!(output.contains("**Status**: completed"));
    assert!(output.contains("| ID | Verdict | Evidence |"));
}

#[test]
fn test_format_report_all_table() {
    let r1 = KickoffReport {
        validated_at: "2026-03-03T12:00:00Z".to_string(),
        criteria: vec![CriterionVerdict {
            id: "AC-1".to_string(),
            verdict: "pass".to_string(),
            evidence: "ok".to_string(),
        }],
        summary: ReportSummary {
            total: 1,
            pass: 1,
            fail: 0,
            partial: 0,
            not_applicable: 0,
            needs_clarification: 0,
        },
        schema_version: Some(1),
        agent_id: Some("driver--alpha".to_string()),
        issue_id: Some(1),
        status: Some("completed".to_string()),
        started_at: None,
        completed_at: None,
        phases: Some(PhaseTimings {
            testing: Some(PhaseTiming {
                duration_s: 60,
                tests_run: Some(50),
                tests_passed: Some(50),
                ..Default::default()
            }),
            ..Default::default()
        }),
        unresolved_questions: None,
        commits: None,
        files_changed: None,
    };
    let r2 = KickoffReport {
        status: Some("failed".to_string()),
        ..r1.clone()
    };
    let reports = vec![("alpha", r1), ("beta", r2)];
    let output = format_report_all_table(&reports);
    assert!(output.contains("2 agents"));
    assert!(output.contains("alpha"));
    assert!(output.contains("beta"));
    assert!(output.contains("1 completed, 1 failed"));
}

#[test]
fn test_preflight_check_passes_when_commands_available() {
    // In the test environment, timeout/tmux/claude may or may not exist.
    // For container mode with a non-existent runtime, it should fail.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hook-config.json"), "{}").unwrap();
    let result = preflight_check(&ContainerMode::Docker, &VerifyLevel::Local, dir.path());
    // Docker may or may not be installed — just verify it doesn't panic.
    let _ = result;
}

#[test]
fn test_preflight_check_missing_command_includes_hint() {
    // Use a container mode referencing a command that almost certainly doesn't exist
    // by checking the error message format when docker/podman is missing.
    // We test the error format rather than specific availability.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hook-config.json"), "{}").unwrap();
    let result = preflight_check(&ContainerMode::Podman, &VerifyLevel::Thorough, dir.path());
    if let Err(e) = result {
        let msg = e.to_string();
        // If podman is missing, the error should mention it with a hint
        if msg.contains("podman") {
            assert!(msg.contains("Pre-flight check failed"));
            assert!(msg.contains("podman"));
        }
        // If gh is also missing, it should appear in the same message
        if msg.contains("GitHub CLI") {
            assert!(msg.contains("gh"));
        }
    }
    // If it passes, both podman and gh are installed — that's fine too.
}

#[test]
fn test_build_agent_command_without_sandbox() {
    let cmd = build_agent_command(
        "timeout",
        3600,
        "opus",
        "Read,Write",
        "KICKOFF.md",
        None,
        Path::new("/tmp/worktree"),
        false,
    );
    assert_eq!(
        cmd,
        "timeout 3600s env -u CLAUDECODE claude --model 'opus' --allowedTools 'Read,Write' -- \"$(cat 'KICKOFF.md')\""
    );
}

#[test]
fn test_build_agent_command_with_sandbox() {
    let cmd = build_agent_command(
        "timeout",
        3600,
        "opus",
        "Read,Write",
        "KICKOFF.md",
        Some("bwrap --bind {{worktree}} /workspace --"),
        Path::new("/tmp/my-worktree"),
        false,
    );
    assert!(cmd.starts_with("timeout 3600s bwrap --bind '/tmp/my-worktree' /workspace --"));
    assert!(cmd.contains("env -u CLAUDECODE claude"));
}

#[test]
fn test_build_agent_command_with_skip_permissions() {
    let cmd = build_agent_command(
        "timeout",
        3600,
        "opus",
        "Read,Write",
        "KICKOFF.md",
        None,
        Path::new("/tmp/worktree"),
        true,
    );
    assert!(
        cmd.contains("--dangerously-skip-permissions"),
        "Should include skip permissions flag"
    );
    assert!(cmd.contains("claude --dangerously-skip-permissions --model 'opus'"));
}

#[test]
fn test_build_agent_command_plan_kickoff() {
    let cmd = build_agent_command(
        "gtimeout",
        1800,
        "sonnet",
        "Read,Glob",
        "PLAN_KICKOFF.md",
        None,
        Path::new("/tmp/worktree"),
        false,
    );
    assert!(cmd.starts_with("gtimeout 1800s"));
    assert!(cmd.contains("$(cat 'PLAN_KICKOFF.md')"));
}

#[test]
fn test_read_sandbox_command_not_configured() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hook-config.json"), "{}").unwrap();
    assert!(read_sandbox_command(dir.path()).is_none());
}

#[test]
fn test_read_sandbox_command_configured() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("hook-config.json"),
        r#"{"sandbox": {"command": "bwrap --bind {{worktree}} /workspace --"}}"#,
    )
    .unwrap();
    let cmd = read_sandbox_command(dir.path());
    assert_eq!(
        cmd.as_deref(),
        Some("bwrap --bind {{worktree}} /workspace --")
    );
}

#[test]
fn test_read_sandbox_command_empty_string_ignored() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("hook-config.json"),
        r#"{"sandbox": {"command": ""}}"#,
    )
    .unwrap();
    assert!(read_sandbox_command(dir.path()).is_none());
}

#[test]
fn test_preflight_check_validates_sandbox_binary() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("hook-config.json"),
        r#"{"sandbox": {"command": "crosslink_nonexistent_sandbox_xyz --isolate --"}}"#,
    )
    .unwrap();
    let result = preflight_check(&ContainerMode::None, &VerifyLevel::Local, dir.path());
    if let Err(e) = result {
        let msg = e.to_string();
        assert!(msg.contains("crosslink_nonexistent_sandbox_xyz"));
        assert!(msg.contains("sandbox.command"));
    }
    // If timeout/tmux/claude are also missing, the sandbox error should still be present
}

#[test]
fn test_command_available_nonexistent() {
    assert!(!command_available("crosslink_nonexistent_binary_xyz"));
}

#[test]
fn test_command_available_real() {
    // `which` should always be available on unix platforms
    assert!(command_available("which"));
}

#[test]
fn test_detect_platform_returns_valid_variant() {
    let platform = detect_platform();
    // On any platform, detect_platform should return a valid variant
    match platform {
        Platform::MacOS | Platform::Windows | Platform::Linux(_) => {}
    }
}

#[test]
fn test_install_hint_timeout_macos() {
    let hint = install_hint("timeout", &Platform::MacOS);
    assert!(hint.contains("brew install coreutils"));
    assert!(hint.contains("gtimeout"));
}

#[test]
fn test_install_hint_timeout_debian() {
    let hint = install_hint("timeout", &Platform::Linux(LinuxDistro::Debian));
    assert!(hint.contains("sudo apt install coreutils"));
}

#[test]
fn test_install_hint_timeout_fedora() {
    let hint = install_hint("timeout", &Platform::Linux(LinuxDistro::Fedora));
    assert!(hint.contains("sudo dnf install coreutils"));
}

#[test]
fn test_install_hint_timeout_arch() {
    let hint = install_hint("timeout", &Platform::Linux(LinuxDistro::Arch));
    assert!(hint.contains("sudo pacman -S coreutils"));
}

#[test]
fn test_install_hint_tmux_macos() {
    let hint = install_hint("tmux", &Platform::MacOS);
    assert!(hint.contains("brew install tmux"));
    assert!(hint.contains("--container docker"));
}

#[test]
fn test_install_hint_tmux_debian() {
    let hint = install_hint("tmux", &Platform::Linux(LinuxDistro::Debian));
    assert!(hint.contains("sudo apt install tmux"));
}

#[test]
fn test_install_hint_tmux_windows() {
    let hint = install_hint("tmux", &Platform::Windows);
    assert!(hint.contains("not available on Windows"));
    assert!(hint.contains("--container docker"));
}

#[test]
fn test_install_hint_claude_macos() {
    let hint = install_hint("claude", &Platform::MacOS);
    assert!(hint.contains("brew install claude-code"));
    assert!(hint.contains("npm install"));
}

#[test]
fn test_install_hint_claude_linux() {
    let hint = install_hint("claude", &Platform::Linux(LinuxDistro::Other));
    assert!(hint.contains("npm install -g @anthropic-ai/claude-code"));
}

#[test]
fn test_install_hint_gh_macos() {
    let hint = install_hint("gh", &Platform::MacOS);
    assert!(hint.contains("brew install gh"));
}

#[test]
fn test_install_hint_gh_debian() {
    let hint = install_hint("gh", &Platform::Linux(LinuxDistro::Debian));
    assert!(hint.contains("sudo apt"));
    assert!(hint.contains("githubcli"));
}

#[test]
fn test_install_hint_gh_windows() {
    let hint = install_hint("gh", &Platform::Windows);
    assert!(hint.contains("winget install"));
}

#[test]
fn test_install_hint_docker_macos() {
    let hint = install_hint("docker", &Platform::MacOS);
    assert!(hint.contains("brew install --cask docker"));
    assert!(hint.contains("--container none"));
}

#[test]
fn test_install_hint_docker_debian() {
    let hint = install_hint("docker", &Platform::Linux(LinuxDistro::Debian));
    assert!(hint.contains("get.docker.com"));
    assert!(hint.contains("usermod"));
}

#[test]
fn test_install_hint_podman_macos() {
    let hint = install_hint("podman", &Platform::MacOS);
    assert!(hint.contains("brew install podman"));
}

#[test]
fn test_install_hint_podman_fedora() {
    let hint = install_hint("podman", &Platform::Linux(LinuxDistro::Fedora));
    assert!(hint.contains("sudo dnf install podman"));
}

#[test]
fn test_install_hint_podman_windows() {
    let hint = install_hint("podman", &Platform::Windows);
    assert!(hint.contains("winget install RedHat.Podman"));
}

#[test]
fn test_install_hint_unknown_command() {
    let hint = install_hint("unknown_tool", &Platform::MacOS);
    assert!(hint.contains("unknown_tool"));
    assert!(hint.contains("package manager"));
}

// --- Tier 1 smoke tests (GH issue #242) ---

#[test]
fn test_kickoff_report_phase3_backward_compat() {
    // Phase 3 report has only validated_at, criteria, summary — no Phase 4 fields.
    // It must deserialize into the current KickoffReport struct.
    let phase3_json = include_str!("../../../test-fixtures/phase3-report.json");
    let report: KickoffReport =
        serde_json::from_str(phase3_json).expect("Phase 3 JSON must deserialize");

    assert_eq!(report.validated_at, "2026-03-01T12:00:00Z");
    assert_eq!(report.criteria.len(), 2);
    assert_eq!(report.criteria[0].id, "AC-1");
    assert_eq!(report.criteria[0].verdict, "pass");
    assert_eq!(report.criteria[1].verdict, "fail");
    assert_eq!(report.summary.total, 2);
    assert_eq!(report.summary.pass, 1);
    assert_eq!(report.summary.fail, 1);

    // Phase 4 fields should all be None (serde defaults)
    assert!(report.schema_version.is_none());
    assert!(report.agent_id.is_none());
    assert!(report.issue_id.is_none());
    assert!(report.status.is_none());
    assert!(report.started_at.is_none());
    assert!(report.completed_at.is_none());
    assert!(report.phases.is_none());
    assert!(report.unresolved_questions.is_none());
    assert!(report.commits.is_none());
    assert!(report.files_changed.is_none());

    // Round-trip: serialize and deserialize again
    let serialized = serde_json::to_string(&report).expect("serialize");
    let roundtrip: KickoffReport =
        serde_json::from_str(&serialized).expect("round-trip deserialize");
    assert_eq!(report, roundtrip);
}

#[test]
fn test_build_prompt_contains_report_json_schema() {
    // When a design doc with acceptance criteria is provided, the prompt
    // must include the KickoffReport JSON schema fields.
    let doc = super::super::design_doc::DesignDoc {
        title: "Test Feature".to_string(),
        summary: String::new(),
        requirements: vec![],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["AC-1: Widget renders".to_string()],
        architecture: String::new(),
        open_questions: vec![],
        out_of_scope: vec![],
        unknown_sections: vec![],
    };
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "test feature",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: Some(&doc),
        doc_path: Some("test.md"),
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/test", &conventions);

    // Must contain the JSON schema field names from KickoffReport
    assert!(prompt.contains("schema_version"));
    assert!(prompt.contains("agent_id"));
    assert!(prompt.contains("issue_id"));
    assert!(prompt.contains("validated_at"));
    assert!(prompt.contains("criteria"));
    assert!(prompt.contains("summary"));
    assert!(prompt.contains(".kickoff-report.json"));
}

#[test]
fn test_build_prompt_contains_validation_section() {
    // When acceptance criteria are present, the prompt must include
    // the spec validation instructions.
    let doc = super::super::design_doc::DesignDoc {
        title: "Validated Feature".to_string(),
        summary: String::new(),
        requirements: vec![],
        requirement_groups: Vec::new(),
        acceptance_criteria: vec!["AC-1: Must work".to_string()],
        architecture: String::new(),
        open_questions: vec![],
        out_of_scope: vec![],
        unknown_sections: vec![],
    };
    let conventions = ProjectConventions {
        test_command: None,
        lint_commands: vec![],
        allowed_tools: vec![],
    };
    let opts = KickoffOpts {
        description: "validated feature",
        issue: None,
        container: ContainerMode::None,
        verify: VerifyLevel::Local,
        model: "opus",
        image: "",
        timeout: Duration::from_secs(3600),
        dry_run: false,
        branch: None,
        quiet: false,
        design_doc: Some(&doc),
        doc_path: Some("test.md"),
        skip_permissions: false,
    };
    let prompt = build_prompt(&opts, 1, "feature/validated", &conventions);

    assert!(prompt.contains("Spec Validation & Reporting"));
    assert!(prompt.contains("Criteria Validation"));
    assert!(prompt.contains(".kickoff-criteria.json"));
    assert!(prompt.contains("pass"));
    assert!(prompt.contains("fail"));
    assert!(prompt.contains("partial"));
    assert!(prompt.contains("not_applicable"));
    assert!(prompt.contains("needs_clarification"));
}

#[test]
fn test_plan_tools_are_read_only() {
    let tools = build_allowed_tools_plan();
    // Plan mode must NOT include write/edit tools
    assert!(
        !tools.contains("Write"),
        "plan tools must not include Write"
    );
    assert!(!tools.contains("Edit"), "plan tools must not include Edit");
    assert!(
        !tools.contains("Bash(git commit"),
        "plan tools must not allow git commit"
    );
    assert!(
        !tools.contains("Bash(git push"),
        "plan tools must not allow git push"
    );
    // Plan mode MUST include read-only tools
    assert!(tools.contains("Read"));
    assert!(tools.contains("Glob"));
    assert!(tools.contains("Grep"));
    assert!(tools.contains("Bash(git log"));
    assert!(tools.contains("Bash(git diff"));
}

#[test]
fn test_watchdog_config_defaults() {
    let cfg = WatchdogConfig::default();
    assert!(cfg.enabled);
    assert_eq!(cfg.staleness_secs, 300);
    assert_eq!(cfg.max_nudges, 5);
    assert_eq!(cfg.check_interval_secs, 120);
    assert_eq!(cfg.grace_period_secs, 300);
}

#[test]
fn test_read_watchdog_config_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = read_watchdog_config(dir.path());
    assert!(cfg.enabled);
    assert_eq!(cfg.staleness_secs, 300);
}

#[test]
fn test_read_watchdog_config_no_watchdog_key() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("hook-config.json"), "{}").unwrap();
    let cfg = read_watchdog_config(dir.path());
    assert!(cfg.enabled);
}

#[test]
fn test_read_watchdog_config_custom_values() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("hook-config.json"),
        r#"{"watchdog": {"enabled": false, "staleness_secs": 600, "max_nudges": 10}}"#,
    )
    .unwrap();
    let cfg = read_watchdog_config(dir.path());
    assert!(!cfg.enabled);
    assert_eq!(cfg.staleness_secs, 600);
    assert_eq!(cfg.max_nudges, 10);
    assert_eq!(cfg.check_interval_secs, 120); // still default
}

#[test]
fn test_build_watchdog_script_contains_key_elements() {
    let cfg = WatchdogConfig {
        enabled: true,
        staleness_secs: 300,
        max_nudges: 3,
        check_interval_secs: 60,
        grace_period_secs: 120,
    };
    let script = build_watchdog_script("feat-my-agent", Path::new("/tmp/wt"), &cfg);
    assert!(script.contains("sleep 120")); // grace period
    assert!(script.contains("sleep 60")); // check interval
    assert!(script.contains(".kickoff-status"));
    assert!(script.contains("feat-my-agent"));
    assert!(script.contains("last-heartbeat"));
    assert!(script.contains("continue working"));
    assert!(script.contains("NUDGES"));
    assert!(script.contains("-gt 300")); // staleness threshold
    assert!(script.contains("-ge 3")); // max nudges
}
