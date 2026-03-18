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

Restructure the design → plan → run pipeline so that each stage automatically discovers and consumes artifacts from prior stages, pipeline state is tracked per design document, and a single unified entry point (`crosslink kickoff <doc>`) replaces the current three-command manual workflow. This eliminates the path re-entry, stranded plan artifacts, and missing readiness signals identified in GH#376.

### Requirements

- REQ-1: Pipeline state must be tracked per design document via a sidecar file (`.design/<slug>.pipeline.json`) recording plan runs, implementation runs, and current stage, so that any command can answer "where is this design doc in the pipeline?"
- REQ-2: A unified entry point `crosslink kickoff <path-to-design-doc>` must auto-detect the current pipeline stage and advance it — running plan if no plan exists, prompting for run if the plan is clean, or showing blocking gaps if the plan found issues — while `--plan` and `--run` flags force a specific stage.
- REQ-3: Plan output must be copied from the plan worktree to `.design/<slug>.plan.json` upon plan agent completion, making gap analysis results discoverable adjacent to the design doc without requiring knowledge of the plan agent's worktree ID.
- REQ-4: When `kickoff run` finds a `.design/<slug>.plan.json` adjacent to the design doc, it must inject the plan's estimated subtasks and assumptions into the KICKOFF.md agent prompt, giving the implementation agent a head start from the gap analysis.
- REQ-5: `crosslink kickoff status` (no agent argument) must display a pipeline-aware overview of all design documents and their current stage (designed → planned → running → complete), distinct from the existing agent-scoped `kickoff status <agent>` which continues to work unchanged.
- REQ-6: After a plan agent completes, the output must include a clear readiness verdict: either "READY — no blocking gaps" with a runnable command to proceed, or "BLOCKED — N blocking gaps" with a summary and suggestion to iterate on the design doc.
- REQ-7: The `/design` skill's "next steps" output must include a single actionable command (`crosslink kickoff .design/<slug>.md`) instead of separate plan and run commands, and the skill must initialize the pipeline state file upon document creation.

### Acceptance Criteria

- [ ] AC-1: Running `crosslink kickoff .design/foo.md` when no `.design/foo.pipeline.json` exists launches a plan agent and creates the pipeline state file with `stage: "planning"` (validates REQ-1, REQ-2).
- [ ] AC-2: Running `crosslink kickoff .design/foo.md` when a pipeline file exists with a completed plan and zero blocking gaps prints "READY" and prompts for confirmation before launching a run agent (validates REQ-2, REQ-6).
- [ ] AC-3: Running `crosslink kickoff .design/foo.md` when a completed plan has blocking gaps prints "BLOCKED: N blocking gaps" with a summary table and does not offer to launch a run agent (validates REQ-2, REQ-6).
- [ ] AC-4: Running `crosslink kickoff .design/foo.md --plan` always runs a new plan analysis regardless of existing pipeline state (validates REQ-2).
- [ ] AC-5: Running `crosslink kickoff .design/foo.md --run` skips the plan check and launches a run agent directly, matching the current `kickoff run` behavior for users who want to bypass the pipeline (validates REQ-2).
- [ ] AC-6: After a plan agent writes `.kickoff-plan.json` in its worktree, the plan results are also present at `.design/foo.plan.json` — verifiable by `cat .design/foo.plan.json | jq .gaps` (validates REQ-3).
- [ ] AC-7: A `kickoff run` launched via the unified command for a design doc that has `.design/foo.plan.json` produces a KICKOFF.md whose "## Plan Context" section contains the plan's `estimated_subtasks` and `assumptions` (validates REQ-4).
- [ ] AC-8: `crosslink kickoff status` (no args) prints a table with columns DESIGN DOC, STAGE, PLAN, GAPS, RUN, showing one row per `.design/*.md` file that has a `.pipeline.json` sidecar (validates REQ-5).
- [ ] AC-9: `crosslink kickoff status <agent-id>` continues to work exactly as before — showing agent status, worktree, tmux session, timeout, heartbeat (validates REQ-5 non-regression).
- [ ] AC-10: After `/design "some feature"` completes, the printed "next steps" includes exactly one kickoff command: `crosslink kickoff .design/<slug>.md` and a `.design/<slug>.pipeline.json` file exists with `stage: "designed"` (validates REQ-7).
- [ ] AC-11: Running `crosslink kickoff plan .design/foo.md` (old syntax) continues to work, but also creates/updates the pipeline state file — backward compatible (validates REQ-1 non-regression).
- [ ] AC-12: Running `crosslink kickoff run "desc" --doc .design/foo.md` (old syntax) continues to work and also updates the pipeline state file — backward compatible (validates REQ-1 non-regression).
- [ ] AC-13: The `--dry-run` flag on the unified command prints the prompt that would be generated and the pipeline state transition that would occur, without launching any agent or creating any worktree (validates REQ-2).

### Architecture

### Pipeline state file (`.design/<slug>.pipeline.json`)

New sidecar file created alongside each design document. Managed by kickoff commands, never edited by users directly.

```json
{
  "schema_version": 1,
  "design_doc": ".design/kickoff-pipeline-ux.md",
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

This lives in `.design/` rather than `.crosslink/` because it's scoped to a specific design doc and should be discoverable by browsing the design directory. It's gitignored (pipeline state is ephemeral per-machine).

### Unified command dispatch (`crosslink kickoff <doc>`)

New code path in `kickoff.rs` `dispatch()`. When the first positional argument is a path ending in `.md`:

```
crosslink kickoff .design/foo.md
  │
  ├─ No pipeline file? → create it (stage: designed), then run plan
  ├─ stage == designed? → run plan
  ├─ stage == planning? → "Plan still running. Check: kickoff status <agent>"
  ├─ stage == planned?
  │   ├─ blocking_gaps == 0? → print READY, confirm, run
  │   └─ blocking_gaps > 0?  → print BLOCKED + gap summary, suggest /design --continue
  ├─ stage == running? → "Already running. Check: kickoff status <agent>"
  └─ stage == complete? → "Already complete. Use --plan to re-analyze or --run to re-launch."
```

The `--plan` flag forces a plan regardless of stage. The `--run` flag forces a run (also accepts `--verify`, `--model`, `--timeout`, `--container`, `--issue` — same flags as current `kickoff run`).

This is implemented as a new `KickoffCommands::Launch` variant that accepts `PathBuf` + optional flags, sitting alongside the existing `Run` and `Plan` variants which remain for backward compatibility.

### Plan result extraction

The plan agent already writes `.kickoff-plan.json` in its worktree and then writes `DONE` to `.kickoff-status`. Two approaches to get the results to `.design/`:

**Option A — Post-completion copy**: After the plan tmux session exits, a watchdog (the existing `spawn_watchdog` mechanism at `kickoff.rs:3283`) detects completion and copies `.kickoff-plan.json` to `.design/<slug>.plan.json`, then updates the pipeline file.

**Option B — Agent writes both**: The plan prompt (`build_plan_prompt()`) instructs the agent to write `.kickoff-plan.json` locally AND to the design directory path (passed as a variable in the prompt). Simpler but requires the plan worktree to have write access to the main repo's `.design/` directory — which it does, since worktrees share the filesystem.

Option B is simpler and more reliable. The plan prompt at `kickoff.rs:3082` gains one additional instruction:

```
3. Copy `.kickoff-plan.json` to `<repo_root>/.design/<slug>.plan.json`
```

The pipeline state file update happens in the `plan()` function itself (on launch → set stage to `planning`) and via the watchdog (on completion → set stage to `planned`, record gap counts).

### Plan context injection into KICKOFF.md

New function `build_plan_context_section()` in `kickoff.rs`, called from `build_prompt()` when a `.design/<slug>.plan.json` exists:

```markdown
## Plan Context

A prior gap analysis was performed against this design document. Use these findings to guide your implementation:

### Estimated Subtasks
1. <title> (~200 lines, risk: low)
2. ...

### Assumptions
- <about>: <assumption>

### Advisory Notes
- <gap detail>
```

This replaces the current behavior where `build_prompt()` (line 813) only injects the design doc sections. The plan context section is added between the design specification and the test/lint instructions.

### Pipeline status view

New branch in `kickoff status` dispatch: when called with no arguments, scan `.design/*.pipeline.json`, parse each, and render a table:

```
DESIGN DOC                      STAGE      PLAN          GAPS    RUN
kickoff-pipeline-ux.md          planned    done (15m)    0/3     -
adversarial-review-v1.md        running    done (22m)    1/5     agent--abc123 (35m)
dashboard-extraction.md         designed   -             -       -
```

### `/design` skill changes

The skill template (`crosslink/resources/claude/commands/design.md`) changes:

1. After writing `.design/<slug>.md`, create `.design/<slug>.pipeline.json` with `stage: "designed"` and empty `plans`/`runs` arrays.
2. Replace the multi-command "next steps" block with:
   ```
   Next steps:
     - Edit in your editor:  $EDITOR .design/<slug>.md
     - Continue iterating:   /design --continue <slug>
     - Launch pipeline:      crosslink kickoff .design/<slug>.md
   ```

### Backward compatibility

- `crosslink kickoff plan <doc>` continues to work. If a `.design/<slug>.pipeline.json` exists, it updates it. If not, it creates one. The plan worktree and agent launch are unchanged.
- `crosslink kickoff run "desc" --doc <doc>` continues to work. If a pipeline file exists, it updates it. The run worktree, prompt, and agent launch are unchanged.
- `crosslink kickoff status <agent-id>` is unchanged.
- `crosslink kickoff list` is unchanged.

### Files modified

- `crosslink/src/commands/kickoff.rs` — new `Launch` dispatch, pipeline state read/write, plan context injection, pipeline status view, watchdog pipeline update
- `crosslink/src/main.rs` — new `KickoffCommands::Launch` variant with clap definition
- `crosslink/resources/claude/commands/design.md` — updated next-steps output, pipeline file creation
- `.gitignore` — add `*.pipeline.json` pattern

### Files created

- `crosslink/src/commands/pipeline.rs` (optional) — pipeline state types and serialization, extracted from kickoff.rs if the module grows too large

### Out of Scope

- Multi-agent orchestration changes (swarm) — swarm has its own design-doc-to-phase pipeline
- Changes to the plan agent's analysis methodology or gap report schema
- TUI dashboard integration — `crosslink tui` can consume pipeline state in a future iteration
- Container-mode changes — Docker/Podman launch path is unaffected
- Design doc format changes — the markdown structure and parser (`design_doc.rs`) are unchanged

