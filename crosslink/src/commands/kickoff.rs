// E-ana tablet — kickoff command: launch agents to implement features
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::db::Database;
use crate::identity::AgentConfig;
use crate::shared_writer::SharedWriter;

/// Container runtime for agent execution.
#[derive(Debug, Clone, PartialEq)]
pub enum ContainerMode {
    /// Run as a local process (tmux session with claude CLI).
    None,
    /// Run inside a Docker container.
    Docker,
    /// Run inside a Podman container.
    Podman,
}

/// Post-implementation verification level.
#[derive(Debug, Clone, PartialEq)]
pub enum VerifyLevel {
    /// Local tests and self-review checklist only.
    Local,
    /// Push branch, open draft PR, wait for CI.
    Ci,
    /// CI plus structured adversarial self-review.
    Thorough,
}

/// Options for `crosslink kickoff run`.
pub struct KickoffOpts<'a> {
    pub description: &'a str,
    pub issue: Option<i64>,
    pub container: ContainerMode,
    pub verify: VerifyLevel,
    pub model: &'a str,
    pub image: &'a str,
    pub timeout: Duration,
    pub dry_run: bool,
    pub branch: Option<&'a str>,
    pub quiet: bool,
    pub design_doc: Option<&'a super::design_doc::DesignDoc>,
}

/// Parse a container mode string into the enum.
pub fn parse_container_mode(s: &str) -> Result<ContainerMode> {
    match s.to_lowercase().as_str() {
        "none" | "local" => Ok(ContainerMode::None),
        "docker" => Ok(ContainerMode::Docker),
        "podman" => Ok(ContainerMode::Podman),
        _ => bail!(
            "Unknown container runtime '{}'. Use: none, docker, podman",
            s
        ),
    }
}

/// Parse a verification level string into the enum.
pub fn parse_verify_level(s: &str) -> Result<VerifyLevel> {
    match s.to_lowercase().as_str() {
        "local" => Ok(VerifyLevel::Local),
        "ci" => Ok(VerifyLevel::Ci),
        "thorough" => Ok(VerifyLevel::Thorough),
        _ => bail!(
            "Unknown verification level '{}'. Use: local, ci, thorough",
            s
        ),
    }
}

/// Parse a human-readable duration string (e.g. "1h", "30m", "90s") into Duration.
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    if s.is_empty() {
        bail!("Empty duration string");
    }

    let (num_str, unit) = if let Some(n) = s.strip_suffix('h') {
        (n, 'h')
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 'm')
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 's')
    } else {
        // Bare number defaults to seconds
        (s, 's')
    };

    let value: u64 = num_str
        .parse()
        .with_context(|| format!("Invalid duration number: '{}'", num_str))?;

    let secs = match unit {
        'h' => value * 3600,
        'm' => value * 60,
        's' => value,
        _ => unreachable!(),
    };

    if secs == 0 {
        bail!("Duration must be greater than zero");
    }

    Ok(Duration::from_secs(secs))
}

/// Slugify a feature description into a branch-safe name.
pub(crate) fn slugify(description: &str) -> String {
    let slug: String = description
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse multiple hyphens and trim
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen && !result.is_empty() {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }

    // Trim trailing hyphens and truncate
    let trimmed = result.trim_end_matches('-');
    if trimmed.len() > 60 {
        // Cut at the last hyphen before 60 chars to avoid mid-word
        match trimmed[..60].rfind('-') {
            Some(pos) => trimmed[..pos].to_string(),
            None => trimmed[..60].to_string(),
        }
    } else {
        trimmed.to_string()
    }
}

/// Detect project conventions from the repo root.
pub(crate) struct ProjectConventions {
    pub(crate) test_command: Option<String>,
    pub(crate) lint_commands: Vec<String>,
    pub(crate) allowed_tools: Vec<String>,
}

pub(crate) fn detect_conventions(repo_root: &Path) -> ProjectConventions {
    let mut conv = ProjectConventions {
        test_command: None,
        lint_commands: Vec::new(),
        allowed_tools: Vec::new(),
    };

    // Rust
    if repo_root.join("Cargo.toml").is_file() || repo_root.join("crosslink/Cargo.toml").is_file() {
        conv.test_command = Some("cargo test".to_string());
        conv.lint_commands
            .push("cargo clippy -- -D warnings".to_string());
        conv.lint_commands.push("cargo fmt --check".to_string());
        conv.allowed_tools.push("Bash(cargo *)".to_string());
    }

    // Node/TypeScript
    if repo_root.join("package.json").is_file() {
        if conv.test_command.is_none() {
            conv.test_command = Some("npm test".to_string());
        }
        conv.allowed_tools.push("Bash(npm *)".to_string());
        conv.allowed_tools.push("Bash(npx *)".to_string());
    }

    // Python
    if repo_root.join("pyproject.toml").is_file() || repo_root.join("requirements.txt").is_file() {
        if conv.test_command.is_none() {
            conv.test_command = Some("uv run pytest".to_string());
        }
        conv.lint_commands.push("ruff check .".to_string());
        conv.allowed_tools.push("Bash(uv *)".to_string());
        conv.allowed_tools.push("Bash(python3 *)".to_string());
    }

    // Go
    if repo_root.join("go.mod").is_file() {
        if conv.test_command.is_none() {
            conv.test_command = Some("go test ./...".to_string());
        }
        conv.lint_commands.push("go vet ./...".to_string());
        conv.allowed_tools.push("Bash(go *)".to_string());
    }

    // Just
    if repo_root.join("justfile").is_file() || repo_root.join("Justfile").is_file() {
        conv.allowed_tools.push("Bash(just *)".to_string());
    }

    // Make
    if repo_root.join("Makefile").is_file() || repo_root.join("makefile").is_file() {
        conv.allowed_tools.push("Bash(make *)".to_string());
    }

    conv
}

/// Format the verification level as a display string.
pub(crate) fn verify_level_name(level: &VerifyLevel) -> &'static str {
    match level {
        VerifyLevel::Local => "local",
        VerifyLevel::Ci => "ci",
        VerifyLevel::Thorough => "thorough",
    }
}

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
- **End session**: `crosslink session end --notes "Completed: <summary of what was delivered, any caveats or follow-ups>"`
- **Write status**: Write the word `DONE` to a file called `.kickoff-status` in the worktree root when completely finished
"#
}

/// Compute which patterns need adding to a git exclude file.
///
/// Given the existing exclude file content, returns only the patterns
/// from `KICKOFF_EXCLUDE_PATTERNS` that are not already present.
pub(crate) const KICKOFF_EXCLUDE_PATTERNS: &[&str] = &[
    "KICKOFF.md",
    ".kickoff-status",
    "PLAN_KICKOFF.md",
    ".kickoff-plan.json",
];

pub(crate) fn missing_exclude_patterns(existing_content: &str) -> Vec<&'static str> {
    KICKOFF_EXCLUDE_PATTERNS
        .iter()
        .filter(|pattern| !existing_content.lines().any(|l| l.trim() == **pattern))
        .copied()
        .collect()
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

1. **Start your crosslink session**: Run `crosslink session start` then `crosslink session work {issue_id}`
2. **Read the project's CLAUDE.md** (if it exists) for conventions before starting
3. Explore relevant code before making changes
4. **Check the knowledge repo** for relevant research before starting:
   `crosslink knowledge search '<relevant terms>'`
   Existing knowledge pages may save you from redundant research.
5. **Document your plan**: `crosslink comment {issue_id} "Plan: <approach, key files, chosen strategy>" --kind plan`
6. Implement the feature fully (no stubs or placeholders)
   - Before each major step: `crosslink session action "Starting <description>..."`
   - **Save research**: If you perform web research, save results for future agents:
     `crosslink knowledge add <slug> --title '<topic>' --tag <category> --source '<url>' --content '<summary>'`
7. **Document decisions**: When choosing between approaches:
   `crosslink comment {issue_id} "Decision: <chose X over Y because Z>" --kind decision`
8. **Document discoveries**: When finding something unexpected:
   `crosslink comment {issue_id} "Found: <observation>" --kind observation`
9. **Log interventions**: If a hook blocks you or a human redirects you, log it immediately:
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
        prompt.push_str(&super::design_doc::build_design_doc_section(doc));
        if let Some(escalation) = super::design_doc::build_open_questions_escalation(doc) {
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

/// Derive a tmux session name from the branch slug.
pub(crate) fn tmux_session_name(slug: &str) -> String {
    let name = format!("feat-{}", slug);
    let sanitized: String = name
        .chars()
        .map(|c| if c == '.' || c == ':' { '-' } else { c })
        .collect();
    if sanitized.len() > 50 {
        sanitized[..50].to_string()
    } else {
        sanitized
    }
}

/// Check if a tmux session with the given name already exists.
fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if a command is available on PATH.
fn command_available(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get the git repository root.
fn repo_root() -> Result<std::path::PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git rev-parse")?;
    if !output.status.success() {
        bail!("Not inside a git repository");
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(std::path::PathBuf::from(path))
}

/// Create a feature branch and worktree for the agent.
fn create_worktree(
    repo_root: &Path,
    slug: &str,
    base_branch: Option<&str>,
) -> Result<(std::path::PathBuf, String)> {
    let branch_name = format!("feature/{}", slug);
    let worktree_dir = repo_root.join(".worktrees").join(slug);

    if worktree_dir.exists() {
        bail!(
            "Worktree already exists at {}. Remove it first or use --branch to target an existing branch.",
            worktree_dir.display()
        );
    }

    // Determine base ref
    let base = base_branch.unwrap_or("HEAD");

    // Create the worktree with a new branch
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["worktree", "add", "-b", &branch_name])
        .arg(&worktree_dir)
        .arg(base)
        .output()
        .context("Failed to create git worktree")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create worktree: {}", stderr.trim());
    }

    Ok((worktree_dir, branch_name))
}

/// Initialize crosslink and agent identity in the worktree.
fn init_worktree_agent(worktree_dir: &Path, crosslink_dir: &Path, slug: &str) -> Result<String> {
    // Run crosslink init --force in the worktree
    let output = Command::new("crosslink")
        .current_dir(worktree_dir)
        .args(["init", "--force", "--skip-signing", "--defaults"])
        .output()
        .context("Failed to run crosslink init in worktree")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!("Warning: crosslink init in worktree: {}", stderr.trim());
    }

    // Derive agent ID from parent agent or hostname
    let parent_id = AgentConfig::load(crosslink_dir)?
        .map(|c| c.agent_id)
        .unwrap_or_else(|| "driver".to_string());

    let agent_id = format!("{}--{}", parent_id, slug);

    // Initialize agent identity in worktree (skip key gen — inherits from parent)
    let wt_crosslink = worktree_dir.join(".crosslink");
    if wt_crosslink.exists() {
        // Only init if not already configured
        if AgentConfig::load(&wt_crosslink)?.is_none() {
            let _ = super::agent::init(
                &wt_crosslink,
                &agent_id,
                Some(&format!("Kickoff agent for: {}", slug)),
                true, // no-key: inherit parent's key
                false,
            );
        }
    }

    // Sync coordination state
    let output = Command::new("crosslink")
        .current_dir(worktree_dir)
        .args(["sync"])
        .output();

    if let Ok(o) = output {
        if !o.status.success() {
            eprintln!("Warning: crosslink sync in worktree returned non-zero");
        }
    }

    Ok(agent_id)
}

/// Exclude kickoff files from git tracking.
fn exclude_kickoff_files(worktree_dir: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(worktree_dir)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .context("Failed to get git common dir")?;

    let common_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let exclude_path = std::path::PathBuf::from(&common_dir).join("info/exclude");

    // Ensure parent directory exists
    if let Some(parent) = exclude_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    let additions = missing_exclude_patterns(&existing);

    if !additions.is_empty() {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude_path)
            .context("Failed to open git exclude file")?;
        for pattern in additions {
            writeln!(file, "{}", pattern)?;
        }
    }

    Ok(())
}

/// Launch the agent as a local tmux process.
fn launch_local(
    worktree_dir: &Path,
    session_name: &str,
    model: &str,
    allowed_tools: &str,
    timeout: Duration,
) -> Result<()> {
    // Create the tmux session
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            session_name,
            "-c",
            &worktree_dir.to_string_lossy(),
        ])
        .output()
        .context("Failed to create tmux session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create tmux session: {}", stderr.trim());
    }

    // Build the claude command
    let timeout_secs = timeout.as_secs();
    let cmd = format!(
        "timeout {}s env -u CLAUDECODE claude --model {} --allowedTools '{}' -- \"$(cat KICKOFF.md)\"",
        timeout_secs, model, allowed_tools
    );

    // Send the command to the tmux session
    let output = Command::new("tmux")
        .args(["send-keys", "-t", session_name, &cmd, "Enter"])
        .output()
        .context("Failed to send command to tmux session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to send keys to tmux: {}", stderr.trim());
    }

    Ok(())
}

/// Launch the agent in a Docker or Podman container.
fn launch_container(
    runtime: &ContainerMode,
    worktree_dir: &Path,
    image: &str,
    agent_id: &str,
    model: &str,
    allowed_tools: &str,
    timeout: Duration,
) -> Result<String> {
    let runtime_cmd = match runtime {
        ContainerMode::Docker => "docker",
        ContainerMode::Podman => "podman",
        ContainerMode::None => unreachable!(),
    };

    // Check runtime is available
    if !command_available(runtime_cmd) {
        bail!(
            "{} is not installed. Install it or use --container none for local mode.",
            runtime_cmd
        );
    }

    let timeout_secs = timeout.as_secs();
    let container_name = format!("crosslink-agent-{}", agent_id);

    // Resolve host auth path for credential mounting
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    let host_auth = format!("{}/.claude", home);

    // Get host UID/GID for remapping
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "1000".to_string());
    let gid = Command::new("id")
        .arg("-g")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "1000".to_string());

    let mut args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        container_name.clone(),
        // Mount the worktree as workspace
        "-v".to_string(),
        format!("{}:/workspaces/repo", worktree_dir.to_string_lossy()),
        // Mount credentials read-only
        "-v".to_string(),
        format!("{}:/host-auth:ro", host_auth),
        // Environment
        "-e".to_string(),
        format!("AGENT_ID={}", agent_id),
        "-e".to_string(),
        format!("HOST_UID={}", uid),
        "-e".to_string(),
        format!("HOST_GID={}", gid),
    ];

    // Image and command
    args.push(image.to_string());
    args.push("bash".to_string());
    args.push("-c".to_string());
    args.push(format!(
        "cd /workspaces/repo && timeout {}s claude --model {} --allowedTools '{}' -- \"$(cat KICKOFF.md)\"",
        timeout_secs, model, allowed_tools
    ));

    let output = Command::new(runtime_cmd)
        .args(&args)
        .output()
        .with_context(|| format!("Failed to launch {} container", runtime_cmd))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{} container launch failed: {}", runtime_cmd, stderr.trim());
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(container_id)
}

/// Main entry point: `crosslink kickoff run`.
pub fn run(
    crosslink_dir: &Path,
    db: &Database,
    writer: Option<&SharedWriter>,
    opts: &KickoffOpts,
) -> Result<()> {
    // 1. Validate prerequisites (skip for dry-run — no agent is launched)
    if !opts.dry_run {
        if opts.container == ContainerMode::None && !command_available("tmux") {
            bail!("tmux is not installed. Install tmux or use --container docker.");
        }
        if opts.container == ContainerMode::None && !command_available("claude") {
            bail!("claude CLI is not installed. Install it from https://claude.ai/install.sh");
        }
        if (opts.verify == VerifyLevel::Ci || opts.verify == VerifyLevel::Thorough)
            && !command_available("gh")
        {
            bail!("GitHub CLI (gh) is required for --verify ci/thorough. Install from https://cli.github.com");
        }
    }

    let root = repo_root()?;
    let slug = slugify(opts.description);

    // 2. Create or find the issue
    let issue_id = if let Some(id) = opts.issue {
        // Verify the issue exists
        if db.get_issue(id)?.is_none() {
            bail!("Issue #{} not found", id);
        }
        id
    } else {
        // Create a new issue directly
        let id = if let Some(w) = writer {
            w.create_issue(
                db,
                opts.description,
                Some("Created by crosslink kickoff"),
                "medium",
            )?
        } else {
            db.create_issue(
                opts.description,
                Some("Created by crosslink kickoff"),
                "medium",
            )?
        };
        // Add the feature label
        if let Some(w) = writer {
            let _ = w.add_label(db, id, "feature");
        } else {
            let _ = db.add_label(id, "feature");
        }
        if !opts.quiet {
            println!("Created issue #{}", id);
        }
        id
    };

    // 3. Create worktree and feature branch (or use existing branch)
    let (worktree_dir, branch_name) = if let Some(br) = opts.branch {
        // Use existing branch — check if worktree exists
        let wt_slug = br.strip_prefix("feature/").unwrap_or(br);
        let worktree_dir = root.join(".worktrees").join(wt_slug);
        if !worktree_dir.exists() {
            create_worktree(&root, wt_slug, None)?
        } else {
            (worktree_dir, br.to_string())
        }
    } else {
        create_worktree(&root, &slug, None)?
    };

    // 4. Detect project conventions
    let conventions = detect_conventions(&root);

    // 5. Build the prompt
    let prompt = build_prompt(opts, issue_id, &branch_name, &conventions);

    // 6. Write KICKOFF.md to worktree
    std::fs::write(worktree_dir.join("KICKOFF.md"), &prompt)
        .context("Failed to write KICKOFF.md")?;

    // 7. Exclude kickoff files from git
    exclude_kickoff_files(&worktree_dir)?;

    // Dry run: print prompt and exit (skip agent init — no launch needed)
    if opts.dry_run {
        let parent_id = AgentConfig::load(crosslink_dir)?
            .map(|c| c.agent_id)
            .unwrap_or_else(|| "driver".to_string());
        let agent_id = format!("{}--{}", parent_id, slug);
        println!("{}", prompt);
        println!("---");
        println!("Worktree: {}", worktree_dir.display());
        println!("Branch:   {}", branch_name);
        println!("Agent:    {}", agent_id);
        return Ok(());
    }

    // 8. Initialize crosslink + agent in worktree (only for real launches)
    let agent_id = init_worktree_agent(&worktree_dir, crosslink_dir, &slug)?;

    // 9. Launch the agent
    let allowed_tools = build_allowed_tools(&conventions, &opts.verify);

    match &opts.container {
        ContainerMode::None => {
            let mut session_name = tmux_session_name(&slug);
            if tmux_session_exists(&session_name) {
                // Append random suffix
                let suffix: u32 = rand_suffix();
                session_name =
                    format!("{}-{}", &session_name[..session_name.len().min(44)], suffix);
            }

            launch_local(
                &worktree_dir,
                &session_name,
                opts.model,
                &allowed_tools,
                opts.timeout,
            )?;

            // 10. Report
            if !opts.quiet {
                println!("Feature agent launched.");
                println!();
                println!("  Worktree: {}", worktree_dir.display());
                println!("  Branch:   {}", branch_name);
                println!("  Issue:    #{}", issue_id);
                println!("  Agent:    {}", agent_id);
                println!("  Session:  {}", session_name);
                println!("  Verify:   {:?}", opts.verify);
                println!();
                println!("  Approve trust:  tmux attach -t {}", session_name);
                println!("  Check status:   crosslink kickoff status {}", agent_id);
                if opts.verify == VerifyLevel::Ci || opts.verify == VerifyLevel::Thorough {
                    println!();
                    println!("  CI verification is enabled. The agent will push and open a draft PR after local tests pass.");
                }
            } else {
                println!("{}", session_name);
            }
        }
        mode @ (ContainerMode::Docker | ContainerMode::Podman) => {
            let container_id = launch_container(
                mode,
                &worktree_dir,
                opts.image,
                &agent_id,
                opts.model,
                &allowed_tools,
                opts.timeout,
            )?;

            if !opts.quiet {
                let runtime = if *mode == ContainerMode::Docker {
                    "docker"
                } else {
                    "podman"
                };
                println!("Feature agent launched in container.");
                println!();
                println!("  Worktree:    {}", worktree_dir.display());
                println!("  Branch:      {}", branch_name);
                println!("  Issue:       #{}", issue_id);
                println!("  Agent:       {}", agent_id);
                println!(
                    "  Container:   {}",
                    &container_id[..12.min(container_id.len())]
                );
                println!("  Verify:      {:?}", opts.verify);
                println!();
                println!(
                    "  View logs:   {} logs -f {}",
                    runtime,
                    &container_id[..12.min(container_id.len())]
                );
                println!("  Check status: crosslink kickoff status {}", agent_id);
            } else {
                println!("{}", container_id);
            }
        }
    }

    Ok(())
}

/// Generate a small random numeric suffix (no external crate needed).
fn rand_suffix() -> u32 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    seed % 10000
}

/// `crosslink kickoff status <agent>`
pub fn status(crosslink_dir: &Path, agent: &str) -> Result<()> {
    // Check for .kickoff-status in any matching worktree
    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    // Try to find the worktree by agent ID or branch slug
    let slug = agent
        .strip_prefix("feature/")
        .or_else(|| agent.strip_prefix("feat-"))
        .unwrap_or(agent);

    // Also try splitting on -- (agent IDs are parent--slug)
    let wt_slug = slug.rsplit("--").next().unwrap_or(slug);

    let worktree_dir = root.join(".worktrees").join(wt_slug);

    if !worktree_dir.exists() {
        // Try scanning all worktrees
        let worktrees_dir = root.join(".worktrees");
        if worktrees_dir.is_dir() {
            println!("Available worktrees:");
            for entry in std::fs::read_dir(&worktrees_dir)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    let name = entry.file_name();
                    let status_file = entry.path().join(".kickoff-status");
                    let status = if status_file.exists() {
                        std::fs::read_to_string(&status_file)
                            .unwrap_or_default()
                            .trim()
                            .to_string()
                    } else {
                        "running".to_string()
                    };
                    println!("  {} — {}", name.to_string_lossy(), status);
                }
            }
        } else {
            println!("No worktrees found.");
        }
        return Ok(());
    }

    // Check .kickoff-status
    let status_file = worktree_dir.join(".kickoff-status");
    let agent_status = if status_file.exists() {
        std::fs::read_to_string(&status_file)
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        "running (no status file yet)".to_string()
    };

    println!("Agent:     {}", agent);
    println!("Worktree:  {}", worktree_dir.display());
    println!("Status:    {}", agent_status);

    // Check tmux session
    let session_name = tmux_session_name(wt_slug);
    if tmux_session_exists(&session_name) {
        println!("tmux:      active ({})", session_name);
    } else {
        println!("tmux:      no active session");
    }

    // Check heartbeat on hub if available
    if let Ok(sync) = crate::sync::SyncManager::new(crosslink_dir) {
        let cache = sync.cache_path();
        // Try both agent ID formats
        for candidate in &[agent.to_string(), format!("driver--{}", wt_slug)] {
            let heartbeat_path = cache.join("agents").join(candidate).join("heartbeat.json");
            if heartbeat_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&heartbeat_path) {
                    if let Ok(hb) = serde_json::from_str::<serde_json::Value>(&content) {
                        if let Some(ts) = hb.get("timestamp").and_then(|v| v.as_str()) {
                            println!("Heartbeat: {}", ts);
                        }
                    }
                }
                break;
            }
        }
    }

    Ok(())
}

/// `crosslink kickoff logs <agent>`
pub fn logs(crosslink_dir: &Path, agent: &str, lines: usize) -> Result<()> {
    // Read the agent's event log from the hub branch
    if let Ok(sync) = crate::sync::SyncManager::new(crosslink_dir) {
        let _ = sync.init_cache();
        let _ = sync.fetch();
        let cache = sync.cache_path();

        // Find agent directory
        let slug = agent.rsplit("--").next().unwrap_or(agent);
        let agents_dir = cache.join("agents");

        let mut found = false;
        if agents_dir.is_dir() {
            for entry in std::fs::read_dir(&agents_dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().to_string();
                if name == agent || name.ends_with(&format!("--{}", slug)) {
                    found = true;
                    println!("Agent: {}", name);

                    // Show heartbeat
                    let hb_path = entry.path().join("heartbeat.json");
                    if hb_path.exists() {
                        let content = std::fs::read_to_string(&hb_path)?;
                        println!("Heartbeat: {}", content.trim());
                    }

                    // Show event log (if CBOR events exist)
                    let events_path = entry.path().join("events.log");
                    if events_path.exists() {
                        let metadata = std::fs::metadata(&events_path)?;
                        println!("Events log: {} bytes", metadata.len());
                    } else {
                        println!("Events log: (none)");
                    }

                    println!();
                    break;
                }
            }
        }

        if !found {
            println!("No agent '{}' found on hub branch.", agent);
            println!("Available agents:");
            if agents_dir.is_dir() {
                for entry in std::fs::read_dir(&agents_dir)? {
                    let entry = entry?;
                    println!("  {}", entry.file_name().to_string_lossy());
                }
            }
        }
    } else {
        bail!("Could not access hub branch. Run 'crosslink sync' first.");
    }

    // Also check local worktree for recent git log
    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;
    let slug = agent.rsplit("--").next().unwrap_or(agent);
    let worktree_dir = root.join(".worktrees").join(slug);

    if worktree_dir.exists() {
        println!("Recent commits in worktree:");
        let output = Command::new("git")
            .current_dir(&worktree_dir)
            .args([
                "log",
                "--oneline",
                &format!("-{}", lines),
                "--format=%h %s (%cr)",
            ])
            .output();

        if let Ok(o) = output {
            if o.status.success() {
                print!("{}", String::from_utf8_lossy(&o.stdout));
            }
        }
    }

    // Suppress unused variable warning
    let _ = lines;

    Ok(())
}

/// `crosslink kickoff stop <agent>`
pub fn stop(_crosslink_dir: &Path, agent: &str, force: bool) -> Result<()> {
    let slug = agent
        .strip_prefix("feature/")
        .or_else(|| agent.strip_prefix("feat-"))
        .unwrap_or(agent);
    let wt_slug = slug.rsplit("--").next().unwrap_or(slug);

    // Try to stop tmux session (local mode)
    let session_name = tmux_session_name(wt_slug);
    if tmux_session_exists(&session_name) {
        let signal = if force { "kill-session" } else { "send-keys" };

        if force {
            let output = Command::new("tmux")
                .args(["kill-session", "-t", &session_name])
                .output()
                .context("Failed to kill tmux session")?;
            if output.status.success() {
                println!("Killed tmux session: {}", session_name);
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("Warning: failed to kill session: {}", stderr.trim());
            }
        } else {
            // Send Ctrl-C gracefully
            let output = Command::new("tmux")
                .args(["send-keys", "-t", &session_name, "C-c", ""])
                .output()
                .context("Failed to send interrupt to tmux session")?;
            if output.status.success() {
                println!("Sent interrupt to tmux session: {}", session_name);
                println!("Use --force to kill immediately.");
            }
        }
        let _ = signal; // consumed in branch logic above
        return Ok(());
    }

    // Try to stop container (docker/podman)
    let container_name = format!("crosslink-agent-{}", agent);
    for runtime in &["docker", "podman"] {
        if command_available(runtime) {
            let stop_cmd = if force { "kill" } else { "stop" };
            let output = Command::new(runtime)
                .args([stop_cmd, &container_name])
                .output();

            if let Ok(o) = output {
                if o.status.success() {
                    println!("Stopped {} container: {}", runtime, container_name);
                    return Ok(());
                }
            }
        }
    }

    bail!(
        "No running agent found for '{}'. Checked tmux session '{}' and container '{}'.",
        agent,
        session_name,
        container_name
    );
}

/// Build the allowed tools string for plan mode (read-only analysis).
pub(crate) fn build_allowed_tools_plan() -> String {
    let tools = vec![
        "Read",
        "Glob",
        "Grep",
        "WebSearch",
        "WebFetch",
        "Bash(git status *)",
        "Bash(git log *)",
        "Bash(git diff *)",
        "Bash(git show *)",
        "Bash(git branch *)",
        "Bash(ls *)",
        "Bash(cat *)",
        "Bash(head *)",
        "Bash(tail *)",
        "Bash(wc *)",
        "Bash(crosslink *)",
    ];
    tools.join(",")
}

/// Build the prompt for plan mode — read-only gap analysis.
pub(crate) fn build_plan_prompt(
    doc: &super::design_doc::DesignDoc,
    issue_id: Option<i64>,
) -> String {
    let issue_line = match issue_id {
        Some(id) => format!("- **Issue**: #{}\n", id),
        None => String::new(),
    };

    let mut prompt = format!(
        r#"# KICKOFF PLAN: Gap Analysis — {}

## Context

{}- **Mode**: Read-only analysis (no code changes)

"#,
        doc.title, issue_line,
    );

    prompt.push_str(&super::design_doc::build_design_doc_section(doc));

    if let Some(escalation) = super::design_doc::build_open_questions_escalation(doc) {
        prompt.push_str(&escalation);
    }

    prompt.push_str(
        r#"
## Analysis Instructions

You are in **read-only analysis mode**. Do NOT write or edit any code files. Your task is to
analyze the design document above against the existing codebase and produce a structured gap report.

### Steps

1. **Explore the codebase** — find files, patterns, and existing implementations relevant to
   each requirement in the design document.
2. **Assess each requirement** — for each one, determine:
   - Is it feasible with the current codebase?
   - What existing code supports or conflicts with it?
   - What information is missing?
3. **Address open questions** — attempt to answer each from codebase context (existing patterns,
   conventions, prior art).
4. **Identify conflicts** — flag any existing code that contradicts or complicates requirements.
5. **Estimate subtasks** — break the implementation into estimated subtasks with scope and risk.
6. **Write the gap report** — produce `.kickoff-plan.json` in the current directory.

### Output Format

Write a JSON file `.kickoff-plan.json` with exactly this structure:

```json
{
  "gaps": [
    {
      "section": "Requirements|Acceptance Criteria|Architecture|...",
      "item": "REQ-1 or null",
      "severity": "blocking|advisory",
      "detail": "description of the gap"
    }
  ],
  "assumptions": [
    {
      "about": "what this assumption relates to",
      "assumption": "what we're assuming"
    }
  ],
  "estimated_subtasks": [
    {
      "title": "subtask title",
      "scope": "~200 lines",
      "risk": "low|medium|high"
    }
  ],
  "conflicts": [
    {
      "file": "src/path/to/file.rs",
      "detail": "description of the conflict"
    }
  ]
}
```

### Final Steps

1. Write `.kickoff-plan.json` (valid JSON only)
2. Write the word `DONE` to `.kickoff-status`
"#,
    );

    prompt
}

/// Options for `crosslink kickoff plan`.
pub struct PlanOpts<'a> {
    pub doc: &'a super::design_doc::DesignDoc,
    pub model: &'a str,
    pub timeout: Duration,
    pub dry_run: bool,
    pub issue: Option<i64>,
    pub quiet: bool,
}

/// Main entry point: `crosslink kickoff plan`.
pub fn plan(crosslink_dir: &Path, db: &Database, opts: &PlanOpts) -> Result<()> {
    // 1. Validate prerequisites (skip for dry-run)
    if !opts.dry_run {
        if !command_available("tmux") {
            bail!("tmux is not installed. Install tmux to use kickoff plan.");
        }
        if !command_available("claude") {
            bail!("claude CLI is not installed. Install it from https://claude.ai/install.sh");
        }
    }

    let root = repo_root()?;
    let title_slug = if opts.doc.title.is_empty() {
        "analysis".to_string()
    } else {
        slugify(&opts.doc.title)
    };
    let slug = format!("plan-{}", title_slug);

    // 2. Create or find issue (optional for plan mode)
    let issue_id = if let Some(id) = opts.issue {
        if db.get_issue(id)?.is_none() {
            bail!("Issue #{} not found", id);
        }
        Some(id)
    } else {
        None
    };

    // 3. Create worktree
    let (worktree_dir, branch_name) = create_worktree(&root, &slug, None)?;

    // 4. Build prompt
    let prompt = build_plan_prompt(opts.doc, issue_id);

    // 5. Write PLAN_KICKOFF.md
    std::fs::write(worktree_dir.join("PLAN_KICKOFF.md"), &prompt)
        .context("Failed to write PLAN_KICKOFF.md")?;

    // 6. Exclude files from git
    exclude_kickoff_files(&worktree_dir)?;

    // Dry run: print and exit
    if opts.dry_run {
        let parent_id = AgentConfig::load(crosslink_dir)?
            .map(|c| c.agent_id)
            .unwrap_or_else(|| "driver".to_string());
        let agent_id = format!("{}--{}", parent_id, slug);
        println!("{}", prompt);
        println!("---");
        println!("Worktree: {}", worktree_dir.display());
        println!("Branch:   {}", branch_name);
        println!("Agent:    {}", agent_id);
        return Ok(());
    }

    // 7. Init worktree agent
    let agent_id = init_worktree_agent(&worktree_dir, crosslink_dir, &slug)?;

    // 8. Launch with read-only tools
    let allowed_tools = build_allowed_tools_plan();
    let mut session_name = tmux_session_name(&slug);
    if tmux_session_exists(&session_name) {
        let suffix = rand_suffix();
        session_name = format!("{}-{}", &session_name[..session_name.len().min(44)], suffix);
    }

    // Plan mode reads PLAN_KICKOFF.md instead of KICKOFF.md
    let timeout_secs = opts.timeout.as_secs();
    let cmd = format!(
        "timeout {}s env -u CLAUDECODE claude --model {} --allowedTools '{}' -- \"$(cat PLAN_KICKOFF.md)\"",
        timeout_secs, opts.model, allowed_tools
    );

    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session_name,
            "-c",
            &worktree_dir.to_string_lossy(),
        ])
        .output()
        .context("Failed to create tmux session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to create tmux session: {}", stderr.trim());
    }

    let output = Command::new("tmux")
        .args(["send-keys", "-t", &session_name, &cmd, "Enter"])
        .output()
        .context("Failed to send command to tmux session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to send keys to tmux: {}", stderr.trim());
    }

    // 9. Report
    if !opts.quiet {
        println!("Plan analysis agent launched (read-only mode).");
        println!();
        println!("  Worktree: {}", worktree_dir.display());
        println!("  Branch:   {}", branch_name);
        if let Some(id) = issue_id {
            println!("  Issue:    #{}", id);
        }
        println!("  Agent:    {}", agent_id);
        println!("  Session:  {}", session_name);
        println!();
        println!("  Approve trust:  tmux attach -t {}", session_name);
        println!("  Check status:   crosslink kickoff status {}", agent_id);
        println!("  View report:    crosslink kickoff show-plan {}", agent_id);
    } else {
        println!("{}", session_name);
    }

    Ok(())
}

/// Display a gap report from a previous plan analysis.
pub fn show_plan(crosslink_dir: &Path, agent: &str) -> Result<()> {
    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    let slug = agent
        .strip_prefix("feature/")
        .or_else(|| agent.strip_prefix("feat-"))
        .unwrap_or(agent);
    let wt_slug = slug.rsplit("--").next().unwrap_or(slug);

    let worktree_dir = root.join(".worktrees").join(wt_slug);
    if !worktree_dir.exists() {
        bail!(
            "No worktree found for '{}'. Checked: {}",
            agent,
            worktree_dir.display()
        );
    }

    let plan_file = worktree_dir.join(".kickoff-plan.json");
    if !plan_file.exists() {
        // Check status
        let status_file = worktree_dir.join(".kickoff-status");
        let status = if status_file.exists() {
            std::fs::read_to_string(&status_file)
                .unwrap_or_default()
                .trim()
                .to_string()
        } else {
            "still running".to_string()
        };
        bail!(
            "No gap report found yet for '{}'. Agent status: {}",
            agent,
            status
        );
    }

    let content =
        std::fs::read_to_string(&plan_file).context("Failed to read .kickoff-plan.json")?;

    // Pretty-print the JSON
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
        println!(
            "{}",
            serde_json::to_string_pretty(&parsed).unwrap_or(content)
        );
    } else {
        // Not valid JSON — print raw
        print!("{}", content);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            tmux_session_name("add-batch-retry-logic"),
            "feat-add-batch-retry-logic"
        );
    }

    #[test]
    fn test_tmux_session_name_sanitization() {
        assert_eq!(tmux_session_name("fix.auth:bug"), "feat-fix-auth-bug");
    }

    #[test]
    fn test_tmux_session_name_truncation() {
        let long = "a".repeat(60);
        let name = tmux_session_name(&long);
        assert!(name.len() <= 50);
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
                "PLAN_KICKOFF.md",
                ".kickoff-plan.json"
            ]
        );
    }

    #[test]
    fn test_missing_exclude_patterns_one_present() {
        let patterns = missing_exclude_patterns("KICKOFF.md\nsome-other-file\n");
        assert!(patterns.contains(&".kickoff-status"));
        assert!(patterns.contains(&"PLAN_KICKOFF.md"));
        assert!(patterns.contains(&".kickoff-plan.json"));
        assert!(!patterns.contains(&"KICKOFF.md"));
    }

    #[test]
    fn test_missing_exclude_patterns_both_present() {
        let patterns = missing_exclude_patterns(
            "KICKOFF.md\n.kickoff-status\nPLAN_KICKOFF.md\n.kickoff-plan.json\n",
        );
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_missing_exclude_patterns_with_whitespace() {
        let patterns = missing_exclude_patterns(
            "  KICKOFF.md  \n  .kickoff-status  \n  PLAN_KICKOFF.md  \n  .kickoff-plan.json  \n",
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
        assert_eq!(tmux_session_name(""), "feat-");
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
            acceptance_criteria: vec!["AC-1: Tests pass".to_string()],
            architecture: "Middleware".to_string(),
            open_questions: Vec::new(),
            out_of_scope: Vec::new(),
            unknown_sections: Vec::new(),
        };
        let prompt = build_plan_prompt(&doc, Some(42));

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
            acceptance_criteria: Vec::new(),
            architecture: String::new(),
            open_questions: vec!["Q1: OAuth or JWT?".to_string()],
            out_of_scope: Vec::new(),
            unknown_sections: Vec::new(),
        };
        let prompt = build_plan_prompt(&doc, None);

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
            acceptance_criteria: Vec::new(),
            architecture: String::new(),
            open_questions: Vec::new(),
            out_of_scope: Vec::new(),
            unknown_sections: Vec::new(),
        };
        let prompt = build_plan_prompt(&doc, None);

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
        };
        let prompt = build_prompt(&opts, 1, "feature/auth", &conventions);

        assert!(prompt.contains("## Design Specification"));
        assert!(prompt.contains("Escalation Required"));
        assert!(prompt.contains("Q1: OAuth or JWT?"));
        assert!(prompt.contains("Q2: Session duration?"));
        assert!(prompt.contains("crosslink comment"));
    }
}
