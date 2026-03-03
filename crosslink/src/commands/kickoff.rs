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
fn slugify(description: &str) -> String {
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
struct ProjectConventions {
    test_command: Option<String>,
    lint_commands: Vec<String>,
    allowed_tools: Vec<String>,
}

fn detect_conventions(repo_root: &Path) -> ProjectConventions {
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

/// Build the KICKOFF.md prompt for the agent.
fn build_prompt(
    opts: &KickoffOpts,
    issue_id: i64,
    branch_name: &str,
    conventions: &ProjectConventions,
) -> String {
    let verify_name = match opts.verify {
        VerifyLevel::Local => "local",
        VerifyLevel::Ci => "ci",
        VerifyLevel::Thorough => "thorough",
    };

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

    // Test/lint instructions
    if let Some(test_cmd) = &conventions.test_command {
        prompt.push_str(&format!("10. **Run tests**: `{}`\n", test_cmd));
    } else {
        prompt.push_str("10. **Run the project's test suite** to verify changes\n");
    }

    if !conventions.lint_commands.is_empty() {
        let cmds: Vec<_> = conventions
            .lint_commands
            .iter()
            .map(|c| format!("`{}`", c))
            .collect();
        prompt.push_str(&format!(
            "11. **Run lint/format checks**: {}\n",
            cmds.join(", ")
        ));
    } else {
        prompt.push_str("11. **Run lint and format checks** before committing\n");
    }

    prompt.push_str(&format!(
        r#"12. **Document results**: `crosslink comment {issue_id} "Result: <summary>" --kind result`
13. Use `/commit` to commit the work when implementation is complete
14. Review the diff and fix any issues found
15. Use `/commit` again after any fixes
"#,
        issue_id = issue_id,
    ));

    // CI/thorough verification steps
    if opts.verify == VerifyLevel::Ci || opts.verify == VerifyLevel::Thorough {
        prompt.push_str(
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
"#,
        );
    }

    if opts.verify == VerifyLevel::Thorough {
        prompt.push_str(
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
"#,
        );
    }

    // Final steps for all verify levels
    prompt.push_str(
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
"#,
    );

    prompt
}

/// Build the --allowedTools string for the claude CLI.
fn build_allowed_tools(conventions: &ProjectConventions, verify: &VerifyLevel) -> String {
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
fn tmux_session_name(slug: &str) -> String {
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

    let mut additions = Vec::new();
    for pattern in &["KICKOFF.md", ".kickoff-status"] {
        if !existing.lines().any(|l| l.trim() == *pattern) {
            additions.push(*pattern);
        }
    }

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
    // 1. Validate prerequisites
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

    // 4. Initialize crosslink + agent in worktree
    let agent_id = init_worktree_agent(&worktree_dir, crosslink_dir, &slug)?;

    // 5. Detect project conventions
    let conventions = detect_conventions(&root);

    // 6. Build the prompt
    let prompt = build_prompt(opts, issue_id, &branch_name, &conventions);

    // 7. Write KICKOFF.md to worktree
    std::fs::write(worktree_dir.join("KICKOFF.md"), &prompt)
        .context("Failed to write KICKOFF.md")?;

    // 8. Exclude kickoff files from git
    exclude_kickoff_files(&worktree_dir)?;

    // Dry run: print prompt and exit
    if opts.dry_run {
        println!("{}", prompt);
        println!("---");
        println!("Worktree: {}", worktree_dir.display());
        println!("Branch:   {}", branch_name);
        println!("Agent:    {}", agent_id);
        return Ok(());
    }

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
}
