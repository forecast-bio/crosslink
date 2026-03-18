---
title: "Kickoff Pipeline UX — Unified Design-Plan-Run Flow"
tags: [design-doc]
sources: []
contributors: [maxine--basel]
created: 2026-03-18
updated: 2026-03-18
---


## Design Specification

### Summary

Restructure the design → plan → run pipeline into a guided, interactive flow. Add a `crosslink design` CLI command that launches a foreground Claude session for design doc authoring. Add a `crosslink kickoff` interactive wizard (ratatui, matching the `crosslink init` style) that shows design docs with live pipeline state, lets the user pick a source (design doc or quick description), choose plan or run, configure the launch, and fire it off. Track pipeline state per design doc so artifacts flow between stages automatically.

### Requirements

- REQ-1: Pipeline state must be tracked per design document via a sidecar file (`.design/<slug>.pipeline.json`) recording plan runs, implementation runs, current stage, and a SHA-256 hash of the design doc content at plan time for staleness detection.
- REQ-2: `crosslink kickoff` (no arguments) must launch an interactive ratatui wizard — matching the `crosslink init` interaction style (arrow/vim keys, progress dots, confirmation screen) — that walks the user through: source selection → stage selection (plan or run) → configuration → launch.
- REQ-3: The wizard's source selection screen must list all `.design/*.md` files with live pipeline state (checking `.kickoff-status` in agent worktrees via `discover_agents()`), and also offer a free-text "quick feature description" option for users without a design doc.
- REQ-4: The wizard's stage selection screen must offer Plan and Run as explicit, separate choices, showing the current plan status (not yet run / in progress / done with gap counts / stale) to inform the user's decision.
- REQ-5: On confirmation, the wizard must exit the TUI, call the existing `plan()` or `run()` function, and print the tmux session name, worktree path, and attach command — the same output the current `kickoff plan` and `kickoff run` produce.
- REQ-6: Plan output must be copied from the plan worktree to `.design/<slug>.plan.json` upon plan agent completion, making gap analysis results discoverable adjacent to the design doc.
- REQ-7: When `kickoff run` launches for a design doc that has a `.design/<slug>.plan.json`, it must inject the plan's estimated subtasks, assumptions, and advisory gaps into the KICKOFF.md agent prompt as a "## Plan Context" section.
- REQ-8: When the pipeline state file contains a `doc_hash` that differs from the current SHA-256 of the design doc, the wizard must display the plan as "stale" and recommend re-planning before running.
- REQ-9: `crosslink design` must launch a foreground `claude` session with the `/design` skill prompt baked in, accepting the same arguments as `/design` (quoted description, `--issue`, `--continue`). If it detects it is being called from inside Claude Code (via environment variable check), it must print a message directing the user to use `/design` instead, and exit with a non-zero status.
- REQ-10: `crosslink kickoff` must also accept a direct path argument (`crosslink kickoff .design/foo.md`) to skip the source selection screen and go straight to stage selection, and `--plan` / `--run` flags to skip stage selection — preserving the non-interactive CLI flow for scripts and power users.
- REQ-11: The `/design` skill's "next steps" output must include `crosslink kickoff .design/<slug>.md` as the single next command, and must initialize the pipeline state file with `stage: "designed"`.
- REQ-12: `crosslink kickoff plan <doc>` and `crosslink kickoff run "desc" --doc <doc>` (existing syntax) must continue to work and must create/update the pipeline state file — full backward compatibility.

### Acceptance Criteria

- [ ] AC-1: Running `crosslink kickoff` in a TTY with `.design/*.md` files present opens the ratatui wizard showing the design doc list with pipeline status columns (validates REQ-2, REQ-3).
- [ ] AC-2: In the wizard, selecting a design doc and choosing "Plan" launches a plan agent in a tmux session, exits the TUI, and prints the worktree/session/attach information (validates REQ-4, REQ-5).
- [ ] AC-3: In the wizard, selecting a design doc and choosing "Run" launches an implementation agent, exits the TUI, and prints the worktree/session/attach information (validates REQ-4, REQ-5).
- [ ] AC-4: In the wizard, typing a quick feature description (no design doc) and choosing "Run" launches an implementation agent without requiring a `.design/` file (validates REQ-3).
- [ ] AC-5: The wizard's source selection screen shows live agent status — a design doc with a running plan agent shows "planning" with the agent ID, verified by cross-referencing `crosslink kickoff list` output (validates REQ-3).
- [ ] AC-6: After a plan agent writes `.kickoff-plan.json` in its worktree, `.design/<slug>.plan.json` also exists with identical content (validates REQ-6).
- [ ] AC-7: A `kickoff run` launched for a design doc with an existing `.design/<slug>.plan.json` produces a KICKOFF.md containing a "## Plan Context" section with subtask estimates and assumptions from the plan (validates REQ-7).
- [ ] AC-8: Modifying a design doc after plan completion causes the wizard to show "stale" next to the plan status, and the stage selection screen recommends re-planning (validates REQ-8).
- [ ] AC-9: `crosslink design "add batch retry"` in a bare terminal launches an interactive foreground Claude session that produces a `.design/add-batch-retry.md` file (validates REQ-9).
- [ ] AC-10: `crosslink design "feature"` run from within a Claude Code session (where `CLAUDE_CODE=1` or equivalent env var is set) prints "Already inside Claude Code — use /design instead." and exits with code 1 (validates REQ-9).
- [ ] AC-11: `crosslink design --continue foo` resumes iteration on `.design/foo.md` via a foreground Claude session (validates REQ-9).
- [ ] AC-12: `crosslink kickoff .design/foo.md` skips source selection and goes directly to stage selection (validates REQ-10).
- [ ] AC-13: `crosslink kickoff .design/foo.md --plan` launches a plan without any interactive screens (validates REQ-10).
- [ ] AC-14: `crosslink kickoff .design/foo.md --run --verify ci --timeout 2h` launches a run with the specified flags, no interactive screens (validates REQ-10).
- [ ] AC-15: `crosslink kickoff` in a non-TTY environment prints "Non-interactive environment. Use: crosslink kickoff .design/<slug>.md --plan|--run" and exits (validates REQ-2 graceful degradation).
- [ ] AC-16: After `/design "feature"` completes, the next-steps output contains `crosslink kickoff .design/<slug>.md` and `.design/<slug>.pipeline.json` exists with `stage: "designed"` (validates REQ-11).
- [ ] AC-17: `crosslink kickoff plan .design/foo.md` (old syntax) works and creates `.design/foo.pipeline.json` if it didn't exist (validates REQ-12).
- [ ] AC-18: `crosslink kickoff run "desc" --doc .design/foo.md` (old syntax) works and updates the pipeline state file (validates REQ-12).
- [ ] AC-19: `crosslink kickoff status` (no args) prints a table of design docs with pipeline state — one row per `.design/*.pipeline.json` (validates REQ-2 status visibility).

### Architecture

### Pipeline state file (`.design/<slug>.pipeline.json`)

New sidecar file created alongside each design document. Managed by kickoff commands, never edited by users directly.

```json
{
  "schema_version": 1,
  "design_doc": ".design/kickoff-pipeline-ux.md",
  "doc_hash": "sha256:a1b2c3d4...",
  "stage": "planned",
  "plans": [
    {
      "agent_id": "driver--plan-kickoff-pipeline-ux-a3f2",
      "worktree": ".worktrees/plan-kickoff-pipeline-ux-a3f2",
      "started_at": "2026-03-18T18:30:00Z",
      "completed_at": "2026-03-18T18:45:00Z",
      "status": "done",
      "blocking_gaps": 0,
      "advisory_gaps": 3,
      "plan_file": ".design/kickoff-pipeline-ux.plan.json"
    }
  ],
  "runs": [
    {
      "agent_id": "driver--kickoff-pipeline-ux-b1c4",
      "worktree": ".worktrees/kickoff-pipeline-ux-b1c4",
      "issue_id": 42,
      "started_at": "2026-03-18T19:00:00Z",
      "status": "running"
    }
  ]
}
```

**Stage transitions:**
- `designed` — pipeline file created by `/design` or first `kickoff` invocation
- `planning` — plan agent launched
- `planned` — plan agent completed (ready for run or blocked)
- `running` — implementation agent launched
- `complete` — implementation agent finished with DONE status

**Staleness detection:** `doc_hash` is computed as `sha256(design_doc_content)` at plan launch time. On each wizard invocation or `crosslink kickoff <doc>` call, the hash is recomputed and compared. If different, the plan is marked stale in the UI.

The pipeline file lives in `.design/` (scoped to the design doc, discoverable by browsing) and is gitignored (ephemeral per-machine state).

### Interactive wizard (`crosslink kickoff`)

Implemented as a new function `launch_wizard()` in `kickoff.rs`, using the same ratatui + crossterm pattern as `crosslink init` (`commands/init.rs` lines 784-1180). The wizard is a 4-screen flow:

**Screen 1 — Source selection**

Lists all `.design/*.md` files. For each, reads the `.pipeline.json` sidecar (if present) and checks live agent status via `discover_agents()` (existing function at `kickoff.rs:2251`). Displays:

```
● Source    ○ Stage    ○ Configure    ○ Launch

  Design documents:

  ❯ kickoff-pipeline-ux.md        planned ✓   0 blocking
    dashboard-extraction.md        designed     —
    adversarial-review-v1.md       planning ⟳  agent--abc
    external-source-queries.md     running      agent--def ⟳

  — or —

    Quick feature description: _________________________

  ↑↓ navigate  Enter select  Esc cancel
```

If a plan's `doc_hash` doesn't match the current file, the status shows `planned ⚠ stale`.

**Screen 2 — Stage selection**

Shows plan and run as separate panes with current status:

```
✓ Source: kickoff-pipeline-ux.md (7 REQ, 13 AC)
● Stage    ○ Configure    ○ Launch

  ┌─ Plan ─────────────────────────────────┐
  │ Status: done (15m ago)                 │
  │ Gaps: 0 blocking, 3 advisory          │
  │                                        │
  │   Select to re-run gap analysis        │
  └────────────────────────────────────────┘

  ┌─ Run ──────────────────────────────────┐
  │ Status: not started                    │
  │ Plan: ✓ ready (0 blocking gaps)        │
  │                                        │
  │ ❯ Select to launch implementation     │
  └────────────────────────────────────────┘

  ↑↓ navigate  Enter select  Backspace back  Esc cancel
```

If the plan is stale, the Run pane shows `Plan: ⚠ stale (doc modified) — re-plan recommended`.

If the source was a quick description (no design doc), this screen is simplified — just Plan and Run without plan-state context.

**Screen 3 — Configuration**

Different fields per stage, same interaction style as `crosslink init` questions:

For **Plan**: model (opus/sonnet), timeout (default 30m)
For **Run**: verify level (local/ci/thorough), model (opus/sonnet), timeout (default 1h), container (none/docker/podman), issue ID (optional — create new or specify existing)

Each field is a selectable option list or text input, navigated with arrow keys.

**Screen 4 — Confirmation**

```
✓ Source:  kickoff-pipeline-ux.md
✓ Stage:   Run (implementation)
✓ Config:  verify=ci, model=opus, timeout=1h, container=none
● Launch

  Ready to launch?  Enter confirm  Backspace go back  Esc cancel
```

On confirm, the TUI exits (restores terminal), then the function calls the existing `plan()` or `run()` function, which prints the standard launch output (worktree, branch, session, attach command).

**Non-interactive fallback**: If stdin is not a TTY, print a usage hint and exit — matching `init.rs`'s TTY detection pattern. The `--plan` and `--run` flags bypass the wizard entirely for scripted use.

**Data structure:**

```rust
struct WizardChoices {
    source: WizardSource,           // DesignDoc(PathBuf) | QuickDescription(String)
    stage: WizardStage,             // Plan | Run
    plan_config: Option<PlanConfig>,
    run_config: Option<RunConfig>,
}

struct PlanConfig {
    model: String,
    timeout: Duration,
}

struct RunConfig {
    verify: VerifyLevel,
    model: String,
    timeout: Duration,
    container: ContainerMode,
    issue: Option<i64>,
}
```

### `crosslink design` command

New top-level command in `main.rs` `Commands` enum:

```rust
Design {
    /// Feature description or --continue <slug>
    description: Option<String>,
    #[arg(long)]
    issue: Option<i64>,
    #[arg(long, value_name = "SLUG")]
    continue_slug: Option<String>,
}
```

Implementation in a new `commands/design_cmd.rs` (not to be confused with `commands/design_doc.rs` which is the parser):

1. **Claude Code detection**: Check for `CLAUDE_CODE` environment variable (set by the Claude Code CLI). If present, print `"Already inside Claude Code — use /design instead."` and exit with code 1.
2. **Build the prompt**: Read the `/design` skill template from `resources/claude/commands/design.md`. Construct the `ARGUMENTS` line from the command flags (e.g., `"add batch retry"`, `--issue 42`, `--continue foo`).
3. **Launch foreground**: Execute `claude` as a child process (not tmux — foreground, inheriting stdin/stdout/stderr) with the design prompt passed via `--prompt`. Use `std::process::Command::new("claude").stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit())`.
4. **On exit**: The Claude session writes `.design/<slug>.md` and `.design/<slug>.pipeline.json`. The user returns to their terminal.

This reuses the same skill prompt that `/design` uses inside Claude Code, ensuring identical behavior.

### Plan result extraction

The plan prompt (`build_plan_prompt()` at `kickoff.rs:3056`) gains an additional instruction telling the agent to also write the plan JSON to the design directory:

```
3. Write `.kickoff-plan.json` in the current directory (worktree)
4. Copy `.kickoff-plan.json` to <repo_root>/.design/<slug>.plan.json
```

The repo root path and slug are interpolated into the prompt. The plan worktree has filesystem access to the main repo's `.design/` directory since worktrees share the filesystem.

The pipeline state file is updated in two places:
- `plan()` function: on launch, set `stage: "planning"` and record `doc_hash`
- Watchdog (`spawn_watchdog` at `kickoff.rs:3283`): on completion, set `stage: "planned"`, parse `.kickoff-plan.json` to count blocking/advisory gaps, record `completed_at`

### Plan context injection into KICKOFF.md

New function `build_plan_context_section(plan_path: &Path) -> Option<String>` in `kickoff.rs`. Reads and parses the `.design/<slug>.plan.json`, then renders:

```markdown
## Plan Context

A prior gap analysis was performed against this design document. Use these findings to guide your implementation:

### Estimated Subtasks
1. Refactor pipeline state types (~200 lines, risk: low)
2. Add wizard TUI screens (~500 lines, risk: medium)

### Assumptions
- Ratatui version: assumes ratatui 0.30 compatibility (already in Cargo.toml)
- Claude CLI: assumes `claude` accepts `--prompt` flag for foreground sessions

### Advisory Notes
- The existing `build_prompt()` function is already 100 lines; consider extracting plan injection as a separate function
```

Called from `build_prompt()` at `kickoff.rs:887` (after the design doc section injection, before test/lint instructions).

### Pipeline status view (`crosslink kickoff status` with no args)

When `kickoff status` is called without an agent ID, scan `.design/*.pipeline.json`, parse each, cross-reference with `discover_agents()` for live status, and render:

```
DESIGN DOC                      STAGE      PLAN          GAPS    RUN
kickoff-pipeline-ux.md          planned    done (15m)    0/3     —
adversarial-review-v1.md        running    done (22m)    1/5     agent--abc (35m)
dashboard-extraction.md         designed   —             —       —
```

### `/design` skill changes

The skill template (`crosslink/resources/claude/commands/design.md`) is updated:

1. After writing `.design/<slug>.md`, create `.design/<slug>.pipeline.json` with `{ "schema_version": 1, "stage": "designed", "design_doc": ".design/<slug>.md", "doc_hash": "<sha256>", "plans": [], "runs": [] }`.
2. Replace the "next steps" block:
   ```
   Next steps:
     - Edit in your editor:  $EDITOR .design/<slug>.md
     - Continue iterating:   /design --continue <slug>
     - Launch pipeline:      crosslink kickoff .design/<slug>.md
   ```

### Backward compatibility

- `crosslink kickoff plan <doc>` — works unchanged, also creates/updates pipeline state file
- `crosslink kickoff run "desc" --doc <doc>` — works unchanged, also updates pipeline state file
- `crosslink kickoff status <agent-id>` — unchanged
- `crosslink kickoff list` — unchanged
- `crosslink kickoff .design/foo.md --plan` — non-interactive, same as old `kickoff plan`
- `crosslink kickoff .design/foo.md --run` — non-interactive, same as old `kickoff run`

### Files modified

- `crosslink/src/commands/kickoff.rs` — wizard TUI, pipeline state read/write, plan context injection, pipeline status view, watchdog pipeline update, unified dispatch
- `crosslink/src/main.rs` — new `Design` command variant, updated `KickoffCommands` with `Launch` variant
- `crosslink/resources/claude/commands/design.md` — updated next-steps output, pipeline file creation
- `.gitignore` — add `*.pipeline.json` and `*.plan.json` patterns

### Files created

- `crosslink/src/commands/design_cmd.rs` — `crosslink design` command implementation (Claude Code detection, prompt construction, foreground launch)
- `crosslink/src/commands/pipeline.rs` — pipeline state types (`PipelineState`, `PlanRecord`, `RunRecord`), serialization, `doc_hash` computation, staleness check

### Out of Scope

- Multi-agent orchestration changes (swarm) — swarm has its own design-doc-to-phase pipeline
- Changes to the plan agent's analysis methodology or gap report schema
- Full TUI dashboard integration — `crosslink tui` can consume pipeline state in a future iteration
- Container-mode changes — Docker/Podman launch path is unaffected
- Design doc format changes — the markdown structure and parser (`design_doc.rs`) are unchanged
- `crosslink design` as a ratatui TUI — it's a simple foreground Claude session, not an interactive screen

### resolved questions

### Q1: Plan worktrees
**Decision: Keep worktrees.** They work, they're cheap, the watchdog and cleanup already manage lifecycle. No change to plan mode's worktree creation.

### Q2: Interactive setup flow
**Decision: Ratatui wizard matching `crosslink init` style.** Four screens (source → stage → configure → launch). The wizard exits the TUI before launching the agent, printing the standard session/worktree output. Non-interactive fallback for scripts. `--plan` and `--run` flags bypass the wizard entirely.

### Q3: Plan staleness detection
**Decision: SHA-256 content hashing.** Store `sha256(design_doc_content)` as `doc_hash` in the pipeline state file at plan launch time. Recompute and compare on each wizard invocation. If different, mark plan as stale in the UI and recommend re-planning.

