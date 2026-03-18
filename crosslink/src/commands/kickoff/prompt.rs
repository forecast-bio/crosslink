// E-ana tablet — kickoff prompt: prompt building for kickoff agents
use super::helpers::verify_level_name;
use super::types::*;

/// Build the test/lint instruction lines for the prompt.
pub(crate) fn build_test_lint_instructions(
    conventions: &ProjectConventions,
    issue_id: i64,
) -> String {
    let mut section = String::new();

    if let Some(test_cmd) = &conventions.test_command {
        section.push_str(&format!("10. **Run tests**: `{}`\n", test_cmd));
    } else {
        section.push_str("10. **Run the project's test suite** to verify changes\n");
    }

    if !conventions.lint_commands.is_empty() {
        let cmds: Vec<_> = conventions
            .lint_commands
            .iter()
            .map(|c| format!("`{}`", c))
            .collect();
        section.push_str(&format!(
            "11. **Run lint/format checks**: {}\n",
            cmds.join(", ")
        ));
    } else {
        section.push_str("11. **Run lint and format checks** before committing\n");
    }

    section.push_str(&format!(
        r#"12. **Document results**: `crosslink comment {issue_id} "Result: <summary>" --kind result`
13. Use `/commit` to commit the work when implementation is complete
14. Review the diff and fix any issues found
15. Use `/commit` again after any fixes
"#,
        issue_id = issue_id,
    ));

    section
}

/// Build the CI verification section of the prompt.
pub(crate) fn build_ci_verification_section() -> &'static str {
    r#"
### CI Verification

16. **Push and open draft PR**:
    - Push the feature branch: `git push -u origin <branch>`
    - Open a draft PR: `gh pr create --draft --title "<feature title>" --body "Automated PR from kickoff agent"`
    - Record the PR URL for later reference.
17. **Wait for CI to pass**:
    - Poll CI status: `gh run list --branch <branch> --limit 1 --json status,conclusion,databaseId` every 30 seconds.
    - If the run's `status` is `completed` and `conclusion` is `success`, CI has passed. Proceed.
    - If the run's `status` is `completed` and `conclusion` is `failure`:
      - Read the failure logs: `gh run view <run-id> --log-failed`
      - Analyze the failures and fix the issues in the code.
      - Run the local test suite again to verify fixes.
      - Use `/commit` to commit the fixes.
      - Push again: `git push`
      - Wait for the new CI run to complete (repeat this loop).
    - If no CI runs appear after 2 minutes, note this in the status and proceed (the repo may not have CI configured).
    - Maximum 5 CI fix-and-retry cycles. If still failing after 5 attempts, write `CI_FAILED` to `.kickoff-status` and stop.
"#
}

/// Build the adversarial self-review section of the prompt.
pub(crate) fn build_adversarial_review_section() -> &'static str {
    r#"
### Adversarial Self-Review

18. Before marking done, perform a thorough self-review of all changes:
    - All tests pass locally
    - CI is green
    - No unintended file changes (`git diff main...HEAD --stat`)
    - No debug/temporary code left behind (search for debugging macros and unfinished markers)
    - No commented-out code blocks
    - Commit messages are clean and descriptive
    - Changes match the feature description above
    - No new warnings in compiler/linter output
    - Error handling is complete (no unwrap() on fallible operations in non-test code)
    - Public API changes have appropriate documentation
    - Use `/commit` after any fixes from the review.
    - Push again if fixes were made: `git push`
"#
}

/// Build the reporting and validation section of the prompt.
///
/// Instructs the agent to validate acceptance criteria, capture timing and
/// metrics, and write a structured `.kickoff-report.json`.
pub(crate) fn build_reporting_section() -> &'static str {
    r#"
### Spec Validation & Reporting

Before marking the implementation complete, validate every acceptance criterion from
`.kickoff-criteria.json` and produce a structured build report.

#### Criteria Validation

1. **Read the criteria file**: `cat .kickoff-criteria.json`
2. **For each criterion**, evaluate the implementation:
   - **pass**: The criterion is fully satisfied. Cite specific evidence (test name, file:line, behavior observed).
   - **fail**: The criterion is not satisfied. Explain what is missing or broken.
   - **partial**: Partially implemented. Describe what works and what does not.
   - **not_applicable**: The criterion does not apply to this implementation (e.g., environment-specific).
   - **needs_clarification**: The criterion is ambiguous and cannot be evaluated. Explain the ambiguity.
3. **Be strict**: Do NOT mark a criterion as `pass` without citing concrete evidence (a test name, a
   code path, or an observable behavior).
4. If any criterion is `fail`, attempt to fix the implementation before proceeding.
   After fixes, re-evaluate the criteria.

#### Build Metrics

Gather the following data for the report:
- **Phase timing**: Estimate seconds spent on each phase (exploration, planning, implementation, testing, validation, review).
  Use `crosslink session action "Phase: <name>"` breadcrumbs to track transitions.
- **Test results**: Record total tests run, passed, and failed from the test suite output.
- **Files changed**: List files you modified (from `git diff --name-only`).
- **Commits**: List commit SHAs you created (from `git log --oneline`).
- **Unresolved questions**: List any open questions from the design doc that remain unanswered.

#### Write the Report

Create `.kickoff-report.json` with this structure:

```json
{
  "schema_version": 1,
  "agent_id": "<your agent ID>",
  "issue_id": <issue number>,
  "status": "completed|failed|partial",
  "started_at": "ISO-8601 when you started",
  "completed_at": "ISO-8601 now",
  "validated_at": "ISO-8601 now",
  "phases": {
    "exploration": { "duration_s": 120, "files_read": 34 },
    "implementation": { "duration_s": 480, "files_modified": 8, "lines_added": 340, "lines_removed": 45 },
    "testing": { "duration_s": 90, "tests_run": 146, "tests_passed": 146, "tests_failed": 0 },
    "validation": { "duration_s": 30, "criteria_checked": 5 }
  },
  "criteria": [
    { "id": "AC-1", "verdict": "pass", "evidence": "test_upload passes with 100MB" }
  ],
  "summary": {
    "total": 1, "pass": 1, "fail": 0, "partial": 0,
    "not_applicable": 0, "needs_clarification": 0
  },
  "unresolved_questions": [],
  "commits": ["abc1234"],
  "files_changed": ["src/retry.rs"]
}
```

Required fields: `validated_at`, `criteria`, `summary`. All other fields are recommended but optional.
Write this file as the second-to-last step, just before writing `DONE` to `.kickoff-status`.
"#
}

/// Build the final steps section of the prompt.
pub(crate) fn build_final_steps_section() -> &'static str {
    r#"
### Final Steps

**Self-review checklist** (verify each before marking done):
- All tests pass locally
- Linter and formatter checks pass (no warnings or formatting errors)
- No unintended file changes in the diff
- No debug/temporary code left behind
- Commit messages are clean and descriptive
- Changes match the original feature description
- All driver interventions have been logged via `crosslink intervene`

Then:
- **Final sync**: `crosslink sync` — push all comments and state to the coordination hub before ending
- **End session**: `crosslink session end --notes "Completed: <summary of what was delivered, any caveats or follow-ups>"`
- **Write status**: Write the word `DONE` to a file called `.kickoff-status` in the worktree root when completely finished
"#
}

/// Build the KICKOFF.md prompt for the agent.
pub(crate) fn build_prompt(
    opts: &KickoffOpts,
    issue_id: i64,
    branch_name: &str,
    conventions: &ProjectConventions,
) -> String {
    let verify_name = verify_level_name(&opts.verify);

    let mut prompt = format!(
        r#"# KICKOFF: {description}

## Context

- **Issue**: #{issue_id}
- **Branch**: `{branch_name}`
- **Verification level**: {verify_name}

## Feature Description

{description}

## Environment

You are running in a git worktree — an isolated working directory that shares git objects with
the main repo. The `.crosslink/issues.db` is shared across all worktrees via the crosslink/hub
branch. Other agents may be working concurrently in different worktrees. If you need to see the
latest state from other agents, run `crosslink sync`.

## Blocked Actions

The following commands are blocked by project policy and will be rejected. If you need one of
these, ask the user to run it manually:

- `git push`, `git merge`, `git rebase`, `git cherry-pick` — remote/branch operations
- `git reset`, `git checkout .`, `git restore .`, `git clean` — destructive resets
- `git stash`, `git tag`, `git am`, `git apply` — stash/tag/patch operations
- `git branch -d`, `git branch -D`, `git branch -m` — branch deletion/renaming

**Gated** (require active crosslink issue): `git commit`
**Always allowed**: `git status`, `git diff`, `git log`, `git show`, `git branch` (listing)

## Instructions

1. **Verify agent setup**: Run `crosslink agent status` to confirm your agent identity is initialized and the
   database is connected. If it reports no agent, run `crosslink agent init` first. Then run `crosslink sync`
   to pull the latest coordination state from the hub.
2. **Start your crosslink session**: Run `crosslink session start` then `crosslink session work {issue_id}`
3. **Read the project's CLAUDE.md** (if it exists) for conventions before starting
4. Explore relevant code before making changes
5. **Check the knowledge repo** for relevant research before starting:
   `crosslink knowledge search '<relevant terms>'`
   Existing knowledge pages may save you from redundant research.
6. **Document your plan**: `crosslink comment {issue_id} "Plan: <approach, key files, chosen strategy>" --kind plan`
7. Implement the feature fully (no stubs or placeholders)
   - Before each major step: `crosslink session action "Starting <description>..."`
   - **Save research**: If you perform web research, save results for future agents:
     `crosslink knowledge add <slug> --title '<topic>' --tag <category> --source '<url>' --content '<summary>'`
8. **Document decisions**: When choosing between approaches:
   `crosslink comment {issue_id} "Decision: <chose X over Y because Z>" --kind decision`
9. **Document discoveries**: When finding something unexpected:
   `crosslink comment {issue_id} "Found: <observation>" --kind observation`
10. **Sync periodically**: After adding comments or completing major milestones, run `crosslink sync` to push
    your changes to the coordination hub. Other agents and the driver cannot see your comments until you sync.
11. **Log interventions**: If a hook blocks you or a human redirects you, log it immediately:
    `crosslink intervene {issue_id} "Description" --trigger <type> --context "what you were attempting"`
    **Handle blockers visibly**: Document with `crosslink comment {issue_id} "Blocker: <desc>" --kind blocker`
    and resolutions with `crosslink comment {issue_id} "Resolved: <how>" --kind resolution`
"#,
        description = opts.description,
        issue_id = issue_id,
        branch_name = branch_name,
        verify_name = verify_name,
    );

    // Inject design document sections if provided
    if let Some(doc) = opts.design_doc {
        prompt.push_str(&super::super::design_doc::build_design_doc_section(doc));
        if let Some(escalation) = super::super::design_doc::build_open_questions_escalation(doc) {
            prompt.push_str(&escalation);
        }
    }

    prompt.push_str(&build_test_lint_instructions(conventions, issue_id));

    if opts.verify == VerifyLevel::Ci || opts.verify == VerifyLevel::Thorough {
        prompt.push_str(build_ci_verification_section());
    }

    if opts.verify == VerifyLevel::Thorough {
        prompt.push_str(build_adversarial_review_section());
    }

    // Spec validation: only when design doc has acceptance criteria
    if let Some(doc) = opts.design_doc {
        if !doc.acceptance_criteria.is_empty() {
            prompt.push_str(build_reporting_section());
        }
    }

    prompt.push_str(build_final_steps_section());

    prompt
}

/// Build the --allowedTools string for the claude CLI.
pub(crate) fn build_allowed_tools(
    conventions: &ProjectConventions,
    verify: &VerifyLevel,
) -> String {
    let mut tools = vec![
        "Read",
        "Write",
        "Edit",
        "Glob",
        "Grep",
        "Skill",
        "Task",
        "WebSearch",
        "WebFetch",
        "Bash(git *)",
        "Bash(ls *)",
        "Bash(mkdir *)",
        "Bash(test *)",
        "Bash(which *)",
        "Bash(touch *)",
        "Bash(cat *)",
        "Bash(head *)",
        "Bash(tail *)",
        "Bash(wc *)",
        "Bash(diff *)",
        "Bash(echo *)",
        "Bash(crosslink *)",
    ];

    // CI tools
    if *verify == VerifyLevel::Ci || *verify == VerifyLevel::Thorough {
        tools.push("Bash(gh *)");
        tools.push("Bash(sleep *)");
    }

    // Project-specific
    let project_tools: Vec<&str> = conventions
        .allowed_tools
        .iter()
        .map(|s| s.as_str())
        .collect();
    tools.extend(project_tools);

    tools.join(",")
}
