---
allowed-tools: Bash(git *), Bash(uuidgen), Bash(ls *), Bash(ln *), Bash(rm *), Bash(tmux *), Bash(cat *), Bash(mkdir *), Bash(which *), Bash(test *), Bash(head *), Bash(grep *), Bash(echo *), Read, Write, Skill
description: Create a worktree and launch a background claude agent in tmux to implement a feature
---

## Context

- Current repo root: !`git rev-parse --show-toplevel`
- Current branch: !`git branch --show-current`
- Existing worktrees: !`git worktree list`
- Working tree status: !`git status --short`
- tmux available: !`which tmux`
- CLAUDE.md head: !`head -3 CLAUDE.md`
- Project root files: !`ls -1`

## Your task

The user provides a feature description (e.g. "add batch retry logic") and optionally additional context, file references, or constraints. You will create an isolated worktree, then launch a background `claude` process in a tmux session to implement the feature autonomously.

### 1. Validate prerequisites

- Confirm `tmux` is installed (`which tmux`). If not, abort with a message telling the user to install it.
- Confirm `claude` CLI is installed (`which claude`). If not, abort.

### 2. Create the worktree via /featree

- Invoke the `/featree` skill with the user's feature description.
- Capture the worktree path and branch name from the output.
- The worktree will be at `<repo-root>/.worktrees/<slug>` (inside the repo, inheriting trust scope).

### 3. Detect project conventions

Before writing the prompt, detect what tools the project uses so the child agent gets appropriate instructions:

- **Test runner**: Check for `justfile` (`just test`), `Makefile`, `package.json` (`npm test`), `pyproject.toml` (`uv run pytest` or `pytest`), `Cargo.toml` (`cargo test`), etc.
- **Linter**: Check for `ruff.toml`, `.eslintrc`, `clippy`, etc.
- **Task runner**: `just`, `make`, `npm run`, `cargo`, etc.
- **CLAUDE.md**: If present, the child agent will read it for full project conventions.

### 4. Prepare the agent prompt

Build a detailed prompt for the child agent. The prompt must be self-contained — the child has no access to this conversation's context. Include:

- The feature description from the user
- Any specific files, modules, or code areas the user mentioned
- Any constraints or requirements the user specified
- Instructions to:
  1. **Read the project's CLAUDE.md** (if it exists) for conventions before starting
  2. Explore relevant code before making changes
  3. Implement the feature fully (no stubs or placeholders)
  4. **Run the project's test suite** to verify changes don't break anything (use the detected test command)
  5. Use `/commit` to commit the work when implementation is complete
  6. Review the diff of all changes and fix any issues found
  7. Use `/commit` again after any fixes
  8. When completely finished, write the word `DONE` to a file called `.kickoff-status` in the worktree root

Write the prompt to `KICKOFF.md` in the worktree root. Ensure it's excluded from git by adding to the main repo's `.git/info/exclude`:

```bash
common_dir=$(git -C <worktree-path> rev-parse --git-common-dir)
grep -qxF 'KICKOFF.md' "$common_dir/info/exclude" || echo "KICKOFF.md" >> "$common_dir/info/exclude"
grep -qxF '.kickoff-status' "$common_dir/info/exclude" || echo ".kickoff-status" >> "$common_dir/info/exclude"
```

### 5. Derive the tmux session name

- Use the feature branch slug as the tmux session name (e.g. `feat-add-batch-retry-logic`).
- Prefix with `feat-` and truncate to 50 characters if needed.
- Replace any characters invalid for tmux session names (periods, colons) with hyphens.

### 6. Launch the tmux session

```bash
tmux new-session -d -s <session-name> -c <worktree-path>
```

Then send the claude command into the session:
- Prefix with `env -u CLAUDECODE` to clear nested-session detection.
- Do NOT use `--dangerously-skip-permissions`. Use `--allowedTools` for auto-approval after user grants trust.
- Use `--` before the positional prompt argument to terminate option parsing.

```bash
tmux send-keys -t <session-name> "env -u CLAUDECODE claude --model opus --allowedTools 'Read,Write,Edit,Glob,Grep,Skill,Task,WebSearch,WebFetch,Bash(git *),Bash(ls *),Bash(mkdir *),Bash(test *),Bash(which *),Bash(touch *),Bash(cat *),Bash(head *),Bash(tail *),Bash(wc *),Bash(diff *),Bash(echo *),Bash(crosslink *),<project-specific-tools>' -- \"\$(cat KICKOFF.md)\"" Enter
```

**Project-specific tool additions** based on detected conventions:
- Python/uv: `Bash(uv *)`
- Node: `Bash(npm *),Bash(npx *)`
- Rust: `Bash(cargo *)`
- Just: `Bash(just *)`
- Make: `Bash(make *)`

### 7. Report to user

```
Feature agent launched.

  Worktree: <path>
  Branch:   feature/<slug>
  Session:  <tmux-session-name>

  Approve trust:   tmux attach -t <tmux-session-name>
  Check status:    /check <tmux-session-name>

  The agent is waiting for trust approval. Attach to the session and approve the prompt to begin.
```

## Constraints

- Never force-push or delete branches.
- Do not push the branch to a remote.
- The child agent prompt must be fully self-contained.
- Leave `KICKOFF.md` in the worktree for reference (git-excluded).
- If a tmux session with the same name already exists, append a short random suffix.
