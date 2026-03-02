---
allowed-tools: Bash(git *), Bash(uuidgen), Bash(ls *), Bash(ln *), Bash(rm *), Bash(tmux *), Bash(docker *), Bash(crosslink *), Bash(cat *), Bash(mkdir *), Bash(which *), Bash(test *), Bash(head *), Bash(grep *), Bash(echo *), Read, Write, Skill
description: Create a worktree and launch a background claude agent (container or tmux) to implement a feature
---

## Context

- Current repo root: !`git rev-parse --show-toplevel`
- Current branch: !`git branch --show-current`
- Existing worktrees: !`git worktree list`
- Working tree status: !`git status --short`
- Docker available: !`docker info --format '{{.ID}}' 2>/dev/null || echo "not available"`
- crosslink image: !`docker images crosslink-agent:latest --format '{{.Repository}}:{{.Tag}}' 2>/dev/null || echo "not built"`
- tmux available: !`which tmux`
- gh available: !`which gh`
- CLAUDE.md head: !`head -3 CLAUDE.md`
- Project root files: !`ls -1`

## Your task

The user provides a feature description (e.g. "add batch retry logic") and optionally additional context, file references, or constraints. You will create an isolated worktree, then launch a background `claude` process to implement the feature autonomously.

### Arguments

The user may pass these flags after the feature description:

- `--verify <level>`: Controls post-implementation verification depth.
  - `local` (default): Local tests + self-review checklist only.
  - `ci`: Push branch, open draft PR, wait for CI to pass, fix failures.
  - `thorough`: Everything in `ci` plus a structured adversarial self-review using the `/ad` skill.
- `--backend <mode>`: Controls execution backend.
  - `auto` (default): Use Docker container if Docker is available and the `crosslink-agent` image is built. Fall back to tmux otherwise.
  - `container`: Force Docker container mode. Abort if Docker is unavailable or the image isn't built.
  - `tmux`: Force tmux mode (legacy behavior).
- All other text is the feature description.

**Parsing**: Split ARGUMENTS on whitespace. If `--verify` is found, consume the next token as the level (`local`, `ci`, or `thorough`). If `--backend` is found, consume the next token as the mode (`auto`, `container`, or `tmux`). Everything else (before or after flags) is the feature description. Defaults: `--verify local`, `--backend auto`.

### 1. Determine execution backend and validate prerequisites

Parse the `--backend` flag (default: `auto`).

**If `auto`:**
1. Check Docker: run `docker info` (succeeds = Docker available)
2. Check image: run `docker images crosslink-agent:latest -q` (non-empty output = image built)
3. Check crosslink CLI: run `which crosslink` (must be available for container mode)
4. If all three pass → use **container** mode
5. Otherwise, check `which tmux`. If available → use **tmux** mode
6. If neither is available, abort: "Install Docker (and run `crosslink container build`) or install tmux."

**If `container`:**
- Verify Docker is available (`docker info`). If not → abort: "Docker is not available. Install Docker and ensure the daemon is running."
- Verify image exists (`docker images crosslink-agent:latest -q`). If empty → abort: "Container image not built. Run `crosslink container build` first."
- Verify `crosslink` CLI is available (`which crosslink`). If not → abort.

**If `tmux`:**
- Verify `tmux` is installed (`which tmux`). If not → abort: "tmux is not installed."

**For all modes:**
- Confirm `claude` CLI is installed (`which claude`). If not, abort.
- If `--verify ci` or `--verify thorough`, confirm `gh` is installed (`which gh`). If not, abort.

Print the chosen backend: `"Using <container|tmux> backend."`

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
- The `--verify` level that was selected (so the prompt can reference it)
- **Worktree awareness** (include this context block in the prompt):
  > You are running in a git worktree — an isolated working directory that shares git objects with the main repo. The `.crosslink/issues.db` is shared across all worktrees via the crosslink/hub branch. Other agents may be working concurrently in different worktrees. If you need to see the latest state from other agents, run `crosslink sync`.

- **If container mode**, also include this context block:
  > You are running inside a Docker container with `--dangerously-skip-permissions`. The container is the security boundary — crosslink hooks still enforce git policy (push, merge, reset are blocked). You can spawn sub-agents by running `claude --dangerously-skip-permissions` directly as background processes (no tmux or Docker needed inside the container). Each sub-agent should work in its own git worktree.

- **Blocked actions** (include this context block in the prompt):
  > **Blocked actions**: The following commands are blocked by project policy and will be rejected. If you need one of these, ask the user to run it manually:
  > - `git push`, `git merge`, `git rebase`, `git cherry-pick` — remote/branch operations
  > - `git reset`, `git checkout .`, `git restore .`, `git clean` — destructive resets
  > - `git stash`, `git tag`, `git am`, `git apply` — stash/tag/patch operations
  > - `git branch -d`, `git branch -D`, `git branch -m` — branch deletion/renaming
  >
  > **Gated** (require active crosslink issue): `git commit`
  > **Always allowed**: `git status`, `git diff`, `git log`, `git show`, `git branch` (listing)

- Instructions to:
  1. **Start your crosslink session**: Run `crosslink session start` then `crosslink session work <issue-id>` to register yourself and mark your focus
  2. **Read the project's CLAUDE.md** (if it exists) for conventions before starting
  2b. **Run `/preflight`** to load project rules and grounding context (language rules, tracking mode, project tree, dependency versions)
  3. Explore relevant code before making changes
  3b. **Check the knowledge repo** for relevant research before starting implementation: `crosslink knowledge search '<relevant terms>'`. Existing knowledge pages may save you from redundant research.
  3c. **Save research to the knowledge repo**: If you perform web research during implementation, save the results for future agents: `crosslink knowledge add <slug> --title '<topic>' --tag <category> --source '<url>' --content '<summary>'`. If you discover important codebase patterns or architecture details, document them as knowledge pages with `--tag codebase`.
  4. **Document your plan**: `crosslink comment <issue-id> "Plan: <your approach, key files, chosen strategy>" --kind plan`
  5. Implement the feature fully (no stubs or placeholders). Before each major step, run `crosslink session action "Starting <description>..."` to leave breadcrumbs for context compression recovery
  6. **Document decisions as you go**: When choosing between approaches, run `crosslink comment <issue-id> "Decision: <chose X over Y because Z>" --kind decision`
  7. **Document discoveries**: When finding something unexpected, run `crosslink comment <issue-id> "Found: <observation>" --kind observation`
  7b. **Log interventions**: If a hook blocks you, a human rejects a tool use, or you receive a redirect, log it immediately: `crosslink intervene <issue-id> "Description" --trigger <type> --context "what you were attempting"`
  7c. **Handle blockers visibly**: If something blocks progress, document it with `crosslink comment <issue-id> "Blocker: <description>" --kind blocker` rather than silently failing. If you resolve it, document that too: `crosslink comment <issue-id> "Resolved: <how>" --kind resolution`
  8. **Run the project's test suite** to verify changes don't break anything (use the detected test command)
  8b. **Run lint and format checks** before committing. Use the project's detected tools:
      - Rust: `cargo clippy -- -D warnings` and `cargo fmt --check`
      - Node/TypeScript: `npx eslint .` or `npm run lint` (if configured)
      - Python: `ruff check .` or `uv run ruff check .` (if ruff is available)
      - Go: `go vet ./...` and `gofmt -l .`
      - Other: check for linter/formatter config files and run accordingly
      Fix any issues found before proceeding. Do not commit code with lint warnings or formatting errors.
  9. **Document results**: `crosslink comment <issue-id> "Result: <test summary, what was delivered>" --kind result`
  10. **Run `/review`** for structured self-review before committing (checks stubs, debug leftovers, lint, tests, issue documentation)
  11. Use `/commit` to commit the work when implementation is complete
  12. Review the diff of all changes and fix any issues found
  13. Use `/commit` again after any fixes
  13. **End your session**: Run `crosslink session end --notes "Completed: <summary of what was delivered, any caveats or follow-ups>"`

**Then, conditionally include the following sections based on `--verify` level:**

#### If `--verify ci` or `--verify thorough`:

Add these steps after the diff review:

14. **Push and open draft PR**:
   - Push the feature branch: `git push -u origin <branch>`
   - Open a draft PR: `gh pr create --draft --title "<feature title>" --body "Automated PR from kickoff agent"`
   - Record the PR URL for later reference.

15. **Wait for CI to pass**:
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

#### If `--verify thorough` only:

Add this step after CI passes:

16. **Structured adversarial self-review**:
    - Before marking done, perform a thorough self-review of all changes.
    - Review checklist (go through each item and fix any issues found):
      - [ ] All tests pass locally
      - [ ] CI is green
      - [ ] No unintended file changes in the diff (`git diff main...HEAD --stat`)
      - [ ] No debug/temporary code left behind (search for `dbg!`, `console.log` used for debugging, `println!` debugging, `TODO`, `FIXME`, `HACK`)
      - [ ] No commented-out code blocks
      - [ ] Commit messages are clean and descriptive
      - [ ] Changes match the original feature description from `KICKOFF.md`
      - [ ] No new warnings introduced (check compiler/linter output)
      - [ ] Error handling is complete (no unwrap() on fallible operations in non-test code, no unhandled promise rejections)
      - [ ] Public API changes have appropriate documentation
    - If the `/ad` skill is available, invoke it for a deeper adversarial review.
    - Use `/commit` after any fixes from the review.
    - Push again if fixes were made: `git push`

#### For all `--verify` levels (final steps):

Add the self-review checklist as the final step before writing DONE (even for `--verify local`, but as a lighter check):

**Self-review checklist** (verify each before marking done):
- All tests pass locally
- Linter and formatter checks pass (no warnings or formatting errors)
- No unintended file changes in the diff
- No debug/temporary code left behind
- Commit messages are clean and descriptive
- Changes match the original feature description from `KICKOFF.md`
- All driver interventions have been logged via `crosslink intervene`

Then:
- When completely finished, write the word `DONE` to a file called `.kickoff-status` in the worktree root

Write the prompt to `KICKOFF.md` in the worktree root. Ensure it's excluded from git by adding to the main repo's `.git/info/exclude`:

```bash
common_dir=$(git -C <worktree-path> rev-parse --git-common-dir)
grep -qxF 'KICKOFF.md' "$common_dir/info/exclude" || echo "KICKOFF.md" >> "$common_dir/info/exclude"
grep -qxF '.kickoff-status' "$common_dir/info/exclude" || echo ".kickoff-status" >> "$common_dir/info/exclude"
```

### 5. Launch the agent

#### If container mode:

Run:

```bash
crosslink container start <worktree-path> --issue <issue-id>
```

That's it. `crosslink container start`:
- Reads KICKOFF.md from the worktree automatically
- Sets up all volume mounts (worktree, .git, hub cache, credentials)
- Handles UID remapping, memory limits, git worktree fixups
- Runs `claude --dangerously-skip-permissions --model opus -- <prompt>`
- Writes the container ID to `<worktree>/.crosslink/container-id`

The container name will be `crosslink-task-<slug>` (auto-derived from the worktree directory name).

No tmux, no send-keys, no trust approval needed.

#### If tmux mode:

Derive the tmux session name:
- Use the feature branch slug as the tmux session name (e.g. `feat-add-batch-retry-logic`).
- Prefix with `feat-` and truncate to 50 characters if needed.
- Replace any characters invalid for tmux session names (periods, colons) with hyphens.
- If a tmux session with the same name already exists, append a short random suffix.

Launch the session:

```bash
tmux new-session -d -s <session-name> -c <worktree-path>
```

Then send the claude command into the session:
- Prefix with `env -u CLAUDECODE` to clear nested-session detection.
- Do NOT use `--dangerously-skip-permissions`. Use `--allowedTools` for auto-approval after user grants trust.
- Use `--` before the positional prompt argument to terminate option parsing.

Build the `--allowedTools` string based on the `--verify` level:

**Base tools** (always included):
```
Read,Write,Edit,Glob,Grep,Skill,Task,WebSearch,WebFetch,Bash(git *),Bash(ls *),Bash(mkdir *),Bash(test *),Bash(which *),Bash(touch *),Bash(cat *),Bash(head *),Bash(tail *),Bash(wc *),Bash(diff *),Bash(echo *),Bash(crosslink *)
```

**CI tools** (added when `--verify ci` or `--verify thorough`):
```
Bash(gh *),Bash(sleep *)
```

**Project-specific tools** (based on detected conventions):
- Python/uv: `Bash(uv *)`
- Node: `Bash(npm *),Bash(npx *)`
- Rust: `Bash(cargo *)`
- Just: `Bash(just *)`
- Make: `Bash(make *)`

Concatenate all applicable tool groups into the comma-separated `--allowedTools` value.

```bash
tmux send-keys -t <session-name> "env -u CLAUDECODE claude --model opus --allowedTools '<all-tools>' -- \"\$(cat KICKOFF.md)\"" Enter
```

### 6. Report to user

#### If container mode:

```
Feature agent launched (container mode).

  Worktree:   <path>
  Branch:     feature/<slug>
  Container:  crosslink-task-<slug>
  Verify:     <local|ci|thorough>

  Check status:    /check
  View logs:       crosslink container logs crosslink-task-<slug>
  Shell into:      crosslink container shell crosslink-task-<slug>

  No trust approval needed — the container boundary replaces interactive trust.
```

#### If tmux mode:

```
Feature agent launched (tmux mode).

  Worktree: <path>
  Branch:   feature/<slug>
  Session:  <tmux-session-name>
  Verify:   <local|ci|thorough>

  Approve trust:   tmux attach -t <tmux-session-name>
  Check status:    /check <tmux-session-name>

  The agent is waiting for trust approval. Attach to the session and approve the prompt to begin.
```

If `--verify ci` or `--verify thorough`, also include:
```
  CI verification is enabled. The agent will push to origin and open a draft PR after local tests pass.
```

## Constraints

- Never force-push or delete branches.
- Do not push the branch to a remote from the kickoff skill itself. (The child agent handles pushing when `--verify ci` or `--verify thorough`.)
- The child agent prompt must be fully self-contained.
- Leave `KICKOFF.md` in the worktree for reference (git-excluded).
- If using container mode, ensure the `crosslink-agent` image is built before attempting to start.
- If a tmux session with the same name already exists, append a short random suffix.
