# Design: Swarm Introspection for Token Budget Awareness and Graceful Phase Breakpoints

**GH Issue:** [#233](https://github.com/forecast-bio/crosslink/issues/233)
**Status:** Draft v1
**Last updated:** 2026-03-06
**Depends on:** [Event-Sourced Coordination](DESIGN-EVENT-SOURCED-COORDINATION.md) (informational), [Container Agents](DESIGN-CONTAINER-AGENTS.md) (informational)

---

## 1. Problem Statement

During a real-world autonomous build (ferrolearn, GH #231), a swarm coordinator launched 33 subagents across 4 phases, consuming two full 5-hour Anthropic usage windows. The coordinator had **zero awareness** of budget constraints. It worked because the usage cap happened to align with a phase boundary. If it had hit mid-phase:

- **Orphaned agents** — running in worktrees with no coordinator to merge them
- **Partial phase state** — some branches merged, others not, no gate run
- **Manual recovery** — user must reconstruct where things left off
- **Wasted tokens** — agents complete work that sits unmergeable

This is worse than context compaction (which auto-continues with a summary). A usage cap is a **hard stop with no automatic recovery**.

### What exists today

| Capability | Status | Location |
|-----------|--------|----------|
| Per-agent KickoffReport with phase timings | Working | `kickoff.rs:556` (KickoffReport struct) |
| `report --all` aggregation across agents | Working | `kickoff.rs:2371` (report_all) |
| Agent heartbeats on hub branch | Working | `sync.rs:692` (push_heartbeat) |
| Lock-based issue claiming | Working | `sync.rs:1034` (claim_lock) |
| Worktree isolation per agent | Working | `kickoff.rs:994` (create_worktree) |
| Session handoff notes | Working | `session.rs`, `session-start.py` |
| Budget/cost tracking | **Missing** | No references in codebase |
| Phase-level coordination | **Missing** | No concept of "phase" above individual agents |
| Checkpoint/resume across sessions | **Missing** | No structured resume capability |

### Design goals

1. **Budget-aware**: The coordinator knows its runway before launching agents
2. **Safe stops**: Phase plans have pre-computed breakpoints aligned to budget limits
3. **Resumable**: When a budget limit forces a stop, a new session can reconstruct state and continue
4. **Incremental**: Each implementation phase delivers standalone value
5. **Backward compatible**: Existing single-agent kickoff workflow is unaffected

---

## 2. Architecture Overview

The swarm introspection system has four layers, each building on the previous:

```
Layer 4: Multi-Window Planning          crosslink swarm plan
Layer 3: Budget Estimation & Throttle   crosslink swarm estimate / swarm launch
Layer 2: Phase Coordination             crosslink swarm phase / swarm gate
Layer 1: Swarm State & Resume           crosslink swarm status / swarm resume
```

All swarm state is persisted to the hub branch under `swarm/` so it survives session boundaries and is visible to all agents.

### Hub branch layout (additive to existing)

```
crosslink/hub:
├── agents/                          # (existing)
├── issues/                          # (existing)
├── swarm/
│   ├── config.json                  # Budget tier, window size, model defaults
│   ├── plan.json                    # Multi-phase plan with cost estimates
│   ├── phases/
│   │   ├── phase-1.json            # Phase definition: agents, deps, status
│   │   ├── phase-2.json
│   │   └── ...
│   ├── checkpoints/
│   │   ├── phase-1-complete.json   # Checkpoint: merged agents, test results, state
│   │   └── phase-2-partial.json    # Partial checkpoint (budget-forced stop)
│   └── history/
│       └── cost-log.json           # Per-agent cost observations for future estimates
```

---

## 3. Implementation Phases

### Phase 1: Swarm State and Resume (standalone value)

**Goal:** A new session can reconstruct exactly where a multi-agent build left off.

**Motivation:** Even without budget awareness, `swarm resume` solves the "where was I?" problem that costs 20+ minutes of archaeology after any interruption (context compaction, usage cap, machine restart).

#### New commands

```bash
# Initialize a swarm plan from a design doc
crosslink swarm init --doc design.md

# Show current swarm state (which agents, what status, what's merged)
crosslink swarm status

# Resume a swarm — reconstruct state and show next steps
crosslink swarm resume
```

#### Data model

**`swarm/plan.json`:**
```json
{
  "schema_version": 1,
  "title": "ferrolearn ML library",
  "design_doc": "DESIGN-FERROLEARN.md",
  "created_at": "2026-03-06T12:00:00Z",
  "phases": ["phase-1", "phase-2", "phase-3", "phase-4"]
}
```

**`swarm/phases/phase-1.json`:**
```json
{
  "name": "Phase 1: Core Infrastructure",
  "status": "completed",
  "agents": [
    {
      "agent_id": "driver--linear-models",
      "issue_id": 42,
      "slug": "linear-models",
      "status": "merged",
      "branch": "feature/linear-models",
      "started_at": "2026-03-06T12:05:00Z",
      "completed_at": "2026-03-06T13:20:00Z"
    },
    {
      "agent_id": "driver--tree-models",
      "issue_id": 43,
      "slug": "tree-models",
      "status": "completed",
      "branch": "feature/tree-models"
    }
  ],
  "gate": {
    "status": "passed",
    "tests_total": 631,
    "tests_passed": 631,
    "ran_at": "2026-03-06T14:00:00Z"
  },
  "depends_on": [],
  "checkpoint": "phase-1-complete"
}
```

**`swarm/checkpoints/phase-1-complete.json`:**
```json
{
  "phase": "phase-1",
  "created_at": "2026-03-06T14:05:00Z",
  "agents_merged": ["driver--linear-models", "driver--tree-models"],
  "agents_pending": [],
  "dev_branch_sha": "abc1234",
  "test_result": { "total": 631, "passed": 631, "failed": 0 },
  "handoff_notes": "Phase 1 complete. All 8 agents merged to dev. 631 tests passing. Ready for Phase 2."
}
```

#### Implementation

1. **`swarm init`**: Parse design doc, create `plan.json` with phase stubs. Uses `kickoff plan` gap analysis to propose agent decomposition per phase. User edits plan before proceeding.

2. **`swarm status`**: Walk `swarm/phases/*.json`, cross-reference with worktree state (`.kickoff-status`, `.kickoff-report.json`), heartbeats, and lock state. Produce a unified view:

   ```
   Swarm: ferrolearn ML library
   Phase 1 (completed): 8/8 agents merged, gate passed (631 tests)
   Phase 2 (in progress):
     ✓ driver--gbm-adaboost       merged     (#48)
     ✓ driver--gmm-agglomerative  merged     (#49)
     ⏸ driver--nmf-kernelpca      completed  (#50, branch ready)
     ● driver--imputers           running    (#51, last heartbeat 3m ago)
     ✗ driver--preprocessors      failed     (#52, see report)
   Phase 3 (pending): 8 agents planned
   Phase 4 (pending): 8 agents planned
   ```

3. **`swarm resume`**: Read latest checkpoint + current phase state. Output structured next-steps that a coordinator (human or agent) can execute:

   ```
   Resume point: Phase 2 (3/5 agents remaining)
   Next actions:
     1. Merge driver--nmf-kernelpca: git merge feature/nmf-kernelpca
     2. Check driver--imputers: crosslink kickoff status imputers
     3. Investigate driver--preprocessors failure: crosslink kickoff report preprocessors
     4. After merges: run gate (cargo test)
     5. If gate passes: checkpoint and start Phase 3
   ```

#### Key design decisions

- **State lives on hub branch**, not in SQLite. This survives machine changes, container restarts, and is visible to all agents.
- **Phases are files, not database rows.** Each phase is a JSON file that can be written atomically. No inter-phase conflicts.
- **Checkpoints are write-once.** A new checkpoint is always a new file. History is preserved.
- **Agent status is derived, not stored.** `swarm status` reads `.kickoff-status` from worktrees and heartbeats from hub in real-time, rather than caching agent status in phase files. Phase files record the *outcome* (merged/failed), not the *current state* (running/polling).

#### Tests

- Unit: `SwarmPlan` / `PhaseState` / `Checkpoint` serde round-trip
- Unit: `swarm_status` derives correct agent states from worktree + heartbeat fixtures
- Integration: `swarm init` → `swarm status` → `swarm resume` lifecycle in temp repo

#### Estimated scope

~800-1200 lines of Rust. New file `commands/swarm.rs` + hub branch writer additions. 2-3 days for an experienced contributor.

---

### Phase 2: Phase Coordination and Gating

**Goal:** Formalize the launch-agents → poll → merge → gate → checkpoint cycle as a repeatable operation.

**Depends on:** Phase 1 (swarm state model)

#### New commands

```bash
# Launch all agents for a phase
crosslink swarm launch phase-2

# Run the gate (build + test) for a phase
crosslink swarm gate phase-2

# Record a checkpoint after a phase completes
crosslink swarm checkpoint phase-2 --notes "All agents merged, 1452 tests passing"
```

#### Behavior

**`swarm launch <phase>`:**
1. Read phase definition from `swarm/phases/<phase>.json`
2. For each agent in the phase: `crosslink kickoff run` with the agent's description and issue
3. Update phase status to `"in_progress"`
4. Print monitoring instructions

**`swarm gate <phase>`:**
1. Verify all agents in the phase have status `merged` or `failed`
2. Run the project's test command (from `ProjectConventions`)
3. Record gate result in the phase file
4. If any agent failed: report which and suggest re-run or skip

**`swarm checkpoint <phase>`:**
1. Verify gate passed (or `--force` to checkpoint without gate)
2. Record merged agents, test results, dev branch SHA, handoff notes
3. Write checkpoint file to `swarm/checkpoints/`
4. Update plan with completed phase

#### Safe-stop checkpoints within a phase

For large phases (8+ agents), the phase definition supports **intra-phase breakpoints**:

```json
{
  "name": "Phase 3",
  "agents": [...],
  "breakpoints": [
    { "after_agents": ["driver--agent-1", "driver--agent-2", "driver--agent-3", "driver--agent-4"],
      "label": "safe-stop-1",
      "action": "gate" }
  ]
}
```

When the coordinator reaches `safe-stop-1`, it runs a partial gate and writes a partial checkpoint. If budget is tight, it can stop here cleanly.

#### Tests

- Unit: Phase state transitions (pending → in_progress → completed/failed)
- Integration: `swarm launch` creates correct worktrees and kickoff commands
- Integration: `swarm gate` runs test command and records result

#### Estimated scope

~600-800 lines. Extends `commands/swarm.rs`. 2 days.

---

### Phase 3: Budget Estimation and Throttling

**Goal:** Before launching a phase, estimate whether it fits in the remaining budget. Throttle agent launches near budget boundaries.

**Depends on:** Phase 2 (phase coordination)

#### New commands

```bash
# Set budget parameters
crosslink swarm config --budget-window 5h --model opus

# Estimate cost for a phase
crosslink swarm estimate phase-3

# Launch with budget awareness
crosslink swarm launch phase-3 --budget-aware
```

#### Cost model

Token cost cannot be measured directly from the CLI (Claude doesn't expose per-session token counts to the spawning process). Instead, we use **wall-clock duration as a proxy**, calibrated by historical observations.

**`swarm/history/cost-log.json`:**
```json
{
  "observations": [
    {
      "agent_id": "driver--linear-models",
      "model": "opus",
      "duration_s": 4500,
      "phase_timings": { "exploration": 120, "implementation": 3600, "testing": 780 },
      "complexity": "medium",
      "files_changed": 12,
      "lines_added": 450
    }
  ],
  "model_estimates": {
    "opus": { "median_duration_s": 3600, "p90_duration_s": 5400 },
    "sonnet": { "median_duration_s": 1800, "p90_duration_s": 3000 }
  }
}
```

When a KickoffReport is written, the cost log is updated with the observation. Over time, estimates improve.

#### Estimation algorithm

```
phase_cost = sum(agent_estimated_duration) + coordinator_overhead
remaining_budget = budget_window - elapsed_time

agent_estimated_duration:
  if historical data for similar complexity: use p90 from cost log
  else: use model default (opus: 90min, sonnet: 45min) × complexity_factor

coordinator_overhead:
  per_agent_merge: 5 minutes
  gate_run: 10 minutes
  total: agents × 5min + 10min

recommendation:
  if phase_cost < remaining_budget × 0.8: PROCEED
  if phase_cost < remaining_budget: PROCEED WITH CAUTION
  if phase_cost > remaining_budget: SPLIT or DEFER
```

#### Throttling

When `--budget-aware` is set and estimated remaining budget is tight:

1. **Warn**: "Budget supports ~5 of 8 agents. Recommend splitting phase."
2. **Suggest split**: "Launch agents 1-5 as Phase 3a, checkpoint, then agents 6-8 as Phase 3b."
3. **Block if unsafe**: If remaining budget < coordinator_overhead (not enough to merge and gate), refuse to launch.

The coordinator is **never** blocked silently. All throttling decisions are printed with reasoning.

#### Tests

- Unit: Cost estimation from historical data
- Unit: Throttle recommendation logic (proceed/caution/split/block)
- Integration: `swarm estimate` output format
- Integration: `swarm launch --budget-aware` respects budget

#### Estimated scope

~500-700 lines. Estimation logic + cost log + config. 2 days.

---

### Phase 4: Multi-Window Planning

**Goal:** Plan a multi-phase build across multiple budget windows, showing natural stop points.

**Depends on:** Phase 3 (budget estimation)

#### New commands

```bash
# Plan a full build with budget constraints
crosslink swarm plan --phases 4 --budget-window 5h

# Show the window plan
crosslink swarm plan show
```

#### Output

```
Estimated total cost: ~2 budget windows

Window 1 (5h):
  Phase 1: 8 agents, est. ~2h (fits)
  Phase 2: 9 agents, est. ~2.5h (fits, tight)
  Buffer: ~30min
  Stop point: after Phase 2 gate → checkpoint

Window 2 (5h):
  Phase 3: 8 agents, est. ~2h (fits)
  Phase 4: 8 agents, est. ~2h (fits)
  Cleanup: ~30min
  Buffer: ~30min
  Stop point: after Phase 4 gate → final checkpoint

Natural safe stops:
  After Phase 1 gate (optional, early exit)
  After Phase 2 gate (REQUIRED — window boundary)
  After Phase 3 gate (optional)
  After Phase 4 gate (REQUIRED — build complete)
```

#### Implementation

This is primarily a planning/display layer on top of Phase 3's estimation. The plan is advisory — the coordinator (human or agent) decides when to actually stop. The plan is stored in `swarm/plan.json` and updated as phases complete with actual vs. estimated costs.

#### Tests

- Unit: Window packing algorithm (bin-pack phases into windows)
- Unit: Plan display formatting
- Integration: `swarm plan` end-to-end

#### Estimated scope

~300-500 lines. Mostly planning logic and display. 1-2 days.

---

## 4. Data Flow

```
                 ┌──────────────┐
                 │  Design Doc  │
                 └──────┬───────┘
                        │ swarm init
                        ▼
              ┌─────────────────────┐
              │    swarm/plan.json   │
              │  (phases, agents)    │
              └─────────┬───────────┘
                        │ swarm launch phase-N
              ┌─────────┼───────────────────────┐
              ▼         ▼                       ▼
         ┌─────────┐ ┌─────────┐          ┌─────────┐
         │ Agent 1 │ │ Agent 2 │   ...    │ Agent N │
         │ (wt)    │ │ (wt)    │          │ (wt)    │
         └────┬────┘ └────┬────┘          └────┬────┘
              │           │                    │
              │  heartbeats, .kickoff-status    │
              ▼           ▼                    ▼
         ┌──────────────────────────────────────────┐
         │           Hub Branch (swarm/)             │
         │  phases/*.json  checkpoints/*.json        │
         │  history/cost-log.json                    │
         └──────────────────────┬───────────────────┘
                                │ swarm status / resume
                                ▼
                    ┌───────────────────────┐
                    │  Coordinator View     │
                    │  (human or agent)     │
                    └───────────────────────┘
```

---

## 5. Out of Scope

These are explicitly deferred:

1. **Token-level metering** — Claude CLI doesn't expose per-session token counts. We use wall-clock duration as a proxy. If the API later exposes usage data, the cost model can be refined without changing the architecture.

2. **Automatic coordinator resume** — `swarm resume` outputs instructions for a human or new agent session. It does not automatically launch a new coordinator. This could be added later with a daemon/watcher.

3. **Cross-machine coordination** — The swarm model assumes a single machine (or shared filesystem). Container-based remote agents (DESIGN-CONTAINER-AGENTS.md) are a prerequisite for multi-machine swarms.

4. **Dependency auto-advance** — When Agent A finishes and unblocks Agent B, there's no automatic notification. This is better solved at the event-sourced coordination layer (DESIGN-EVENT-SOURCED-COORDINATION.md Phase 2).

5. **Lock fairness / deadlock detection** — Covered by the event-sourced coordination design, not this feature.

---

## 6. Acceptance Criteria (per GH #233)

| Criterion | Phase | How |
|-----------|-------|-----|
| Swarm config accepts budget tier / window parameters | 3 | `swarm config --budget-window 5h` |
| Pre-phase estimation compares phase cost against remaining budget | 3 | `swarm estimate phase-N` |
| Multi-window planning shows which phases fit in which windows | 4 | `swarm plan --budget-window 5h` |
| Phase plans include explicit safe-stop checkpoints | 2 | Breakpoints in phase definitions |
| Budget warnings surface when launching agents | 3 | `swarm launch --budget-aware` |
| Agent launch throttling near budget boundaries | 3 | Split/block recommendations |
| `crosslink swarm resume` reconstructs phase state | 1 | `swarm resume` |
| Handoff notes at checkpoints are structured for cold-start | 1 | Checkpoint JSON with handoff_notes |
| Historical cost data feeds back into estimates | 3 | `swarm/history/cost-log.json` |

---

## 7. Migration and Compatibility

- **No breaking changes.** The `swarm` subcommand is entirely additive.
- **Existing kickoff workflow unaffected.** Single-agent `kickoff run` works exactly as before.
- **Opt-in.** Swarm features only activate when `swarm init` has been run.
- **Hub branch additions** are in a new `swarm/` directory, no conflicts with existing layout.

---

## 8. Open Questions

1. **Should `swarm init` auto-decompose a design doc into phases and agents?** Or should it just create the structure and let the user fill in agents? Auto-decomposition requires `kickoff plan`-level analysis, which is expensive. Proposal: `swarm init --doc` creates phases from design doc headings, user refines.

2. **How does the coordinator track elapsed time?** Session start time is available from `crosslink session start`. But if the user takes a break mid-window, elapsed time overcounts. Proposal: track active time via heartbeat intervals, not wall clock.

3. **Should swarm state be in SQLite (like issues) or JSON on hub (like locks)?** Proposal: JSON on hub, for the same reasons locks are there — visibility across agents and machines, no local-only state. But this means swarm operations go through the git-based write path.

4. **Integration with `kickoff plan`?** The gap analysis from `kickoff plan` could feed directly into swarm phase planning. Proposal: `swarm init --doc` internally runs a plan-mode analysis to populate phase definitions.

---

## 9. References

- GH #233: Swarm introspection feature request (this design)
- GH #231: Ferrolearn retrospective (motivating use case)
- GH #113: Event-sourced coordination epic
- GH #110: Container-based agent execution
- DESIGN-EVENT-SOURCED-COORDINATION.md: CRDT coordination for 50+ agents
- DESIGN-CONTAINER-AGENTS.md: Container execution model
