# Code Review: Crosslink as a Design-Document-Driven Build System

**Date:** 2026-03-03
**Reviewer:** Claude 
**Scope:** Full codebase review against the long-term vision of design-document-driven autonomous builds

---

## 1. Vision Statement

The goal is a system where:

1. A human hands crosslink a **design document** describing the software to build.
2. Crosslink kicks off one or more autonomous agents to **implement the design**.
3. Agents **test rigorously** and ensure the output meets the spec.
4. Before starting, agents **identify shortfalls** in the document — ambiguities, missing details, contradictions — and report them back for resolution.
5. The human reviews a structured report of what was built, what was validated, and what couldn't be resolved.

---

## 2. Current State Assessment

### 2.1 What Already Works

#### Agent Orchestration — `commands/kickoff.rs` (1170 lines)

The kickoff system is the centerpiece. It provides:

- **Full lifecycle management:** worktree creation → agent identity → prompt generation → tmux/container launch → monitoring → stop.
- **Convention detection:** auto-detects Rust, Node, Python, Go projects and configures test/lint commands accordingly.
- **Three verification tiers:** `local` (run tests), `ci` (push + draft PR + wait for CI), `thorough` (CI + adversarial self-review).
- **Dual execution modes:** local tmux sessions and container-based execution (Docker/Podman).
- **Safety model:** agents can never push to remote, hooks gate commits to require active issues, containers provide process isolation.

| Strength | Detail |
|----------|--------|
| Prompt engineering | `build_prompt()` generates a comprehensive KICKOFF.md with environment info, blocked actions, step-by-step instructions, and verification phases |
| Tool sandboxing | `build_allowed_tools()` constructs a whitelist of exactly which Bash commands the agent may use, based on detected project conventions |
| Monitoring | `kickoff status`, `kickoff logs`, `/check` skill provide multiple ways to inspect running agents |
| Sentinel files | `.kickoff-status` written by agent on completion (DONE / CI_FAILED) — simple, reliable signaling |

#### Multi-Agent Coordination — `sync.rs`, `shared_writer.rs`, `locks.rs`

- **Event-sourced hub branch** (`crosslink/hub`) with deterministic compaction via total ordering keys.
- **Per-agent event logs** — conflict-free concurrent writes (no merge conflicts on append).
- **Lock system** with stale detection (30-min heartbeat timeout), retry-on-push-conflict (3 attempts), and graceful offline degradation.
- **V2 layout** with per-issue lock files eliminates the single-file contention bottleneck of V1.

#### Testing Infrastructure — 255+ test cases

| Layer | Count | Coverage |
|-------|-------|----------|
| Unit tests (`db.rs`) | 77 | Core CRUD, integrity, cascades, SQL injection defense |
| Property-based tests (`db.rs`) | 14 | Roundtrip invariants, idempotency, cycle detection |
| CLI integration tests | 146 | All major commands, security edge cases, complex workflows |
| Fuzz targets | 12 | State machine fuzzing, all subsystems, nightly extended runs |
| CI workflows | 3 | Feature branch (fast), main (full), nightly (fuzz) |

The database layer and CLI are **production-grade tested**. Property-based testing with regression databases and state-machine fuzzing are notably strong.

#### Knowledge Repository — `knowledge.rs`

- Git-backed markdown pages with YAML frontmatter (title, tags, sources, contributors).
- Multi-agent sync with accept-both merge conflict resolution.
- CLI-accessible: `crosslink knowledge add|show|list|edit|remove|search`.
- TUI browser tab for interactive reading.
- Rules system (`.crosslink/rules/`) guides agents to search knowledge before researching externally.

#### Session Management — `commands/session.rs`

- Agent-scoped sessions with handoff notes for cross-session continuity.
- Breadcrumb actions (`session action "..."`) survive context compression.
- Auto-lock-release on session end.
- Hook-injected context at session start (previous handoff, ready issues, knowledge summary).

---

### 2.2 Quantitative Summary

| Metric | Value | Assessment |
|--------|-------|------------|
| Total source lines (Rust) | ~15,000+ | Substantial |
| CLI commands | 70+ (including subcommands) | Comprehensive |
| Test cases | 255+ | Strong for core, gaps in agent layer |
| Fuzz targets | 12 | Excellent |
| CI workflows | 3 (feature, main, nightly) | Good coverage |
| Supported languages (convention detection) | 6 (Rust, Node, Python, Go, Make, Just) | Good |
| Supported container runtimes | 2 (Docker, Podman) | Good |
| Knowledge pages (current) | 6 | Early stage |
| Schema version | 14 | Mature migration system |

---

## 3. Critical Gaps

### 3.1 No Design Document Ingestion Path

**Current:** Kickoff accepts a one-line text description (`crosslink kickoff run "add batch retry logic"`).

**Needed:** Accept a structured design document and parse it into the agent's working context.

**What's missing:**

- No `--doc <path>` flag on `kickoff run` to ingest a design document.
- No document parser that extracts structured sections (requirements, acceptance criteria, architecture, API contracts, open questions).
- No mechanism to break a document into sub-issues for parallel agent work.
- No validation step that checks whether the document has sufficient detail before launching an agent.

**Impact:** Without this, the human must manually translate design documents into text descriptions, losing structure, acceptance criteria, and traceability.

---

### 3.2 No Pre-Flight Document Analysis

**Current:** Kickoff launches immediately. The `--dry-run` flag prints the prompt but performs no analysis.

**Needed:** A pre-flight phase where the agent reads the design document, identifies gaps, and produces a structured report *before writing any code*.

**What's missing:**

- No "analysis only" execution mode.
- No structured output format for document gaps (e.g., "Section 3.2 specifies a REST API but does not define error response schemas").
- No feedback mechanism to route the gap analysis back to the human and block until resolved.
- No distinction between "blocking gaps" (can't proceed) and "advisory gaps" (can proceed with assumptions).

**Impact:** Agents will start building with incomplete information, discover gaps mid-implementation, and either guess wrong or stall. Early gap detection saves significant rework.

---

### 3.3 No Spec Validation Loop

**Current:** Agents run tests, run linters, and optionally self-review for debug code. Verification answers "does it work?" but not "does it match the spec?"

**Needed:** Post-build validation that checks each requirement from the design document and reports pass/fail/partial status.

**What's missing:**

- No extraction of acceptance criteria from design documents into a machine-checkable format.
- No post-build verification phase that maps implementation to requirements.
- No structured result reporting (per-requirement status with evidence).
- No diff between "what was specified" and "what was built."

**Impact:** The human must manually verify spec compliance after every kickoff — the most tedious part of the review process and the part most likely to miss regressions.

---

### 3.4 Agent Orchestration Layer Has No Unit Tests

**Severity: High.** The most critical code for the vision — the agent orchestration, coordination, and execution layers — has **zero unit tests**.

| Module | Lines | Unit Tests | Risk |
|--------|-------|-----------|------|
| `kickoff.rs` | 1,170 | 0 | High — core orchestration |
| `container.rs` | 700+ | 0 | High — container execution |
| `shared_writer.rs` | 1,700+ | 0 | High — multi-agent writes |
| `sync.rs` | 860+ | 0 | High — hub synchronization |
| `signing.rs` | ~200 | 0 | High — cryptographic ops |
| `knowledge.rs` | ~1,800 | 0 | Medium — knowledge management |
| `tui/*` | ~2,000+ | 0 | Medium — interactive dashboard |
| **Total untested** | **~8,400+** | **0** | |

The database layer (`db.rs`) has 77 unit tests + 14 proptests — excellent. But `shared_writer.rs` alone is larger than `db.rs` and has no tests at all.

**Recommendation:** Extract pure functions from side-effectful code. Functions like `build_prompt()`, `build_allowed_tools()`, `detect_conventions()`, slug generation, and lock conflict resolution are all unit-testable without mocking file systems or git.

---

### 3.5 No Structured / Machine-Readable Build Reports

**Current:** Agents communicate via crosslink comments (free-text). Status is a sentinel file containing `DONE` or `CI_FAILED`.

**Needed:** Machine-readable build reports that capture:

- Which tests passed/failed (with output).
- Which spec requirements were verified.
- What the agent couldn't resolve.
- Time spent per phase (exploration, implementation, testing, review).
- Files created/modified with rationale.

**Impact:** Without structured reports, evaluating kickoff results requires reading through git diffs, comment trails, and agent logs manually. This doesn't scale to multiple concurrent agents.

---

### 3.6 Knowledge System Not Wired as First-Class Context

**Current:** Session start hook says "6 pages available, search with CLI." Agents must explicitly run `crosslink knowledge search` and `crosslink knowledge show`.

**Needed:** Relevant knowledge pages auto-injected into agent context based on the issue or design document being worked on.

**What's missing:**

- No MCP resource server for knowledge (agents access pages only via CLI round-trips).
- No auto-injection of relevant pages based on issue tags or design doc references.
- No bulk import tool for design document libraries.
- No structured metadata queries (filter by multiple tags, date ranges, contributors).

---

## 4. Code Quality Findings

### 4.1 Issues to Address

| Finding | Severity | Location | Recommendation |
|---------|----------|----------|----------------|
| `main.rs` is 1,600+ lines with a monolithic match statement | Medium | `main.rs` | Refactor dispatch into module-level handlers; main.rs should only parse args and route |
| Stale locks require manual `locks steal` — no auto-recovery | Medium | `lock_check.rs:81-86` | Add configurable auto-steal after N×stale_timeout with audit trail |
| Lock check → work start has a timing race | Medium | `session.rs:169-170` | Atomic lock-claim-and-start operation to close the window |
| Clock skew warns but doesn't block | Low | `compaction.rs:24` | Consider blocking on skew > configurable threshold for critical operations |
| Push retry exhaustion silently falls back to local-only | Medium | `shared_writer.rs:237-291` | Surface a visible warning when sync fails, so agents don't silently diverge |
| No timeout on container builds | Low | `container.rs:181-236` | Add `--build-timeout` to prevent hung builds from blocking kickoff |

### 4.2 Architecture Strengths

| Pattern | Assessment |
|---------|------------|
| Event sourcing with deterministic compaction | Excellent — enables replay, audit, and conflict-free multi-agent writes |
| SSH-based commit signing with trust chain | Solid — agent keys separate from user keys, trust requires explicit approval |
| Graceful degradation (multi-agent → single-agent → offline) | Good — every operation has a fallback path |
| Convention detection for project-specific tooling | Practical — avoids manual configuration for standard projects |
| Hook-gated safety (no push, gated commit) | Strong — defense in depth against agent mistakes |
| Offline-first local IDs (negative = local) | Clever — enables offline work without ID conflicts |

---

## 5. Suggested Changes — Phased Roadmap

### Phase 1: Testing Foundation

**Goal:** Make the agent orchestration layer trustworthy enough to build on.

**Changes:**

1. **Extract pure functions from `kickoff.rs`:**
   - `build_prompt()` — takes a `KickoffConfig` struct, returns `String`. Testable without filesystem.
   - `build_allowed_tools()` — takes `ProjectConventions`, returns tool list. Testable without filesystem.
   - `detect_conventions()` — takes a list of file existence checks, returns conventions. Mockable.
   - Slug generation / sanitization — pure string transformation.

2. **Add unit tests for extracted functions:**
   - Prompt contains correct issue ID, branch name, blocked commands.
   - Allowed tools include project-specific commands when conventions detected.
   - Convention detection handles mixed projects (Rust + Python).
   - Slug generation handles Unicode, long strings, special characters.

3. **Add unit tests for `shared_writer.rs` lock logic:**
   - Lock claim succeeds when no contention.
   - Lock claim returns `Contended` when another agent holds.
   - Lock release is idempotent.
   - Stale detection uses correct timeout.

4. **Add integration tests for kickoff lifecycle:**
   - `--dry-run` produces valid KICKOFF.md.
   - Worktree creation succeeds and is in correct location.
   - Status command reports correctly for non-existent / running / completed agents.

**Estimated scope:** ~20 new test functions, ~500 lines of test code.

---

### Phase 2: Design Document Support

**Goal:** Kickoff can accept and parse structured design documents.

**Changes:**

1. **Define the design document format:**

   ```markdown
   # Feature: <title>

   ## Summary
   <1-3 sentence overview>

   ## Requirements
   - REQ-1: <requirement description>
   - REQ-2: <requirement description>

   ## Acceptance Criteria
   - [ ] AC-1: <testable criterion>
   - [ ] AC-2: <testable criterion>

   ## Architecture
   <How it should be built — modules, data flow, integration points>

   ## API Contract (optional)
   <Endpoint definitions, request/response schemas>

   ## Open Questions (optional)
   - Q1: <question that needs answering before or during implementation>

   ## Out of Scope
   - <what this feature does NOT include>
   ```

2. **Add `--doc <path>` flag to `kickoff run`:**
   - Parse the document into sections.
   - Inject requirements and acceptance criteria into KICKOFF.md as a structured checklist.
   - If `## Open Questions` section is non-empty, include them in the prompt with instructions to resolve or escalate.

3. **Store design documents as knowledge pages:**
   - `crosslink knowledge add --from-doc <path>` to import a design document.
   - Auto-tag with issue ID for traceability.
   - Agents working on the issue automatically get the design doc in context.

4. **Add `crosslink kickoff plan <doc>` subcommand:**
   - Launches agent in analysis-only mode.
   - Agent reads the doc, explores the codebase, and produces a gap report.
   - No code changes allowed — only comments on the crosslink issue.
   - Outputs structured JSON: `{ "gaps": [...], "assumptions": [...], "estimated_subtasks": [...] }`.

**Estimated scope:** ~800 lines of new Rust code + document format specification.

---

### Phase 3: Spec Validation Loop

**Goal:** Agents verify their work against the original spec, not just "does it compile."

**Changes:**

1. **Extract acceptance criteria at kickoff time:**
   - Parse `## Acceptance Criteria` from the design document.
   - Write criteria to `.kickoff-criteria.json` in the worktree (excluded from git).
   - Include criteria in KICKOFF.md as a numbered checklist.

2. **Add a validation phase to the agent workflow:**
   - After implementation and testing, agent reads `.kickoff-criteria.json`.
   - For each criterion, agent writes a verdict: `pass`, `fail`, `partial`, `not_applicable`, `needs_clarification`.
   - Verdicts written to `.kickoff-report.json`.

3. **Add `crosslink kickoff report <agent-id>` command:**
   - Reads `.kickoff-report.json` from the agent's worktree.
   - Displays human-readable summary.
   - `--json` flag for machine consumption.

4. **Integrate spec validation into verification tiers:**
   - `local`: Run tests + validate criteria locally.
   - `ci`: Tests + criteria + CI pipeline.
   - `thorough`: Tests + criteria + CI + adversarial review + spec compliance check.

**Estimated scope:** ~600 lines of new Rust code + modifications to `build_prompt()`.

---

### Phase 4: Structured Reporting

**Goal:** Machine-readable build reports for evaluating kickoff results at scale.

**Changes:**

1. **Define the kickoff report format:**

   ```json
   {
     "agent_id": "driver--feature-slug",
     "issue_id": 42,
     "status": "completed",
     "phases": {
       "exploration": { "duration_s": 120, "files_read": 34 },
       "planning": { "duration_s": 60, "comments_added": 2 },
       "implementation": {
         "duration_s": 480,
         "files_modified": 8,
         "lines_added": 340,
         "lines_removed": 45
       },
       "testing": {
         "duration_s": 90,
         "tests_run": 146,
         "tests_passed": 146,
         "tests_failed": 0
       },
       "review": { "duration_s": 45, "issues_found": 1, "issues_fixed": 1 }
     },
     "criteria_results": [
       {
         "id": "AC-1",
         "verdict": "pass",
         "evidence": "Test test_batch_retry covers this"
       },
       {
         "id": "AC-2",
         "verdict": "partial",
         "note": "Implemented for HTTP errors only, not timeouts"
       }
     ],
     "unresolved_questions": [
       {
         "question": "Q1 from design doc",
         "resolution": "Assumed X based on existing patterns"
       }
     ],
     "commits": ["abc1234", "def5678"],
     "files_changed": ["src/retry.rs", "src/batch.rs", "tests/retry_test.rs"]
   }
   ```

2. **Instrument the agent prompt to produce this report:**
   - Add a final step to KICKOFF.md: "Write `.kickoff-report.json` with structured results."
   - Template the JSON schema into the prompt so agents know the exact format.

3. **Add `crosslink kickoff report` with formatting options:**
   - Table view (terminal), JSON output, markdown summary.
   - Aggregated view across multiple agents: `crosslink kickoff report --all`.

**Estimated scope:** ~400 lines of new Rust code + prompt modifications.

---

### Phase 5: Knowledge Integration

**Goal:** Design documents and research automatically flow into agent context.

**Changes:**

1. **MCP resource server for knowledge:**
   - Expose knowledge pages as MCP resources.
   - Agents can read pages without CLI round-trips.
   - Auto-refresh when pages are updated by other agents.

2. **Auto-injection based on issue context:**
   - When an issue has a `design-doc:<slug>` label, auto-inject that knowledge page into the agent's session context.
   - The session start hook reads issue labels and injects matching pages.

3. **Bulk import for design document libraries:**
   - `crosslink knowledge import <directory>` — imports all markdown files, auto-tags, preserves directory structure as tag hierarchy.
   - Supports frontmatter or infers metadata from filename/path.

4. **Structured knowledge queries:**
   - `crosslink knowledge search --tag architecture --since 2026-01` — filter by tag and date range.
   - `crosslink knowledge search --contributor <agent-id>` — find what a specific agent learned.

**Estimated scope:** ~1,200 lines of new code (MCP server + CLI enhancements).

---

## 6. Priority Order

| Priority | Phase | Rationale |
|----------|-------|-----------|
| 1 | Phase 1: Testing Foundation | Can't safely extend untested orchestration code |
| 2 | Phase 2: Design Document Support | Core enabler for the entire vision |
| 3 | Phase 3: Spec Validation Loop | Closes the "did it meet the spec?" gap |
| 4 | Phase 4: Structured Reporting | Makes results evaluable at scale |
| 5 | Phase 5: Knowledge Integration | Quality-of-life improvement, not a blocker |

Phases 2 and 3 could be developed in parallel by separate agents once Phase 1 is complete.

---

## 7. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Design doc format is too rigid for real-world documents | Medium | High | Start with a minimal format and iterate; allow freeform sections alongside structured ones |
| Agents produce unreliable spec validation verdicts | High | Medium | Use adversarial self-review: a second pass challenges each verdict with counter-evidence |
| Structured reporting adds too much prompt overhead | Low | Medium | Keep the JSON schema small; only require top-level fields, make details optional |
| MCP knowledge server adds latency | Low | Low | Cache pages locally; only fetch on change |
| Parallel sub-task agents create merge conflicts | Medium | High | Use the existing worktree-per-agent model; final merge is human-reviewed |

---

## 8. Dependencies

| Dependency | Status | Notes |
|------------|--------|-------|
| Kickoff system | Implemented | Core orchestration in place |
| Multi-agent coordination | Implemented | Hub branch, locks, event sourcing all working |
| Convention detection | Implemented | Rust, Node, Python, Go, Make, Just supported |
| Container execution | Implemented | Docker and Podman support |
| Knowledge repository | Implemented | Git-backed, CLI-accessible, TUI browser |
| MCP infrastructure | Partial | Safe-fetch server exists; knowledge server does not |
| Design document parser | Not started | New capability |
| Spec validation engine | Not started | New capability |
| Structured report system | Not started | New capability |

---

## Appendix A: Key Source Files

| Component | File | Lines |
|-----------|------|-------|
| Kickoff orchestration | `crosslink/src/commands/kickoff.rs` | 1,170 |
| Container management | `crosslink/src/commands/container.rs` | 700+ |
| Multi-agent writes | `crosslink/src/shared_writer.rs` | 1,700+ |
| Hub synchronization | `crosslink/src/sync.rs` | 860+ |
| Lock management | `crosslink/src/locks.rs` | 400 |
| Lock enforcement | `crosslink/src/lock_check.rs` | 100 |
| Event system | `crosslink/src/events.rs` | 200 |
| Compaction engine | `crosslink/src/compaction.rs` | 200 |
| Database layer | `crosslink/src/db.rs` | 2,900 |
| Knowledge manager | `crosslink/src/knowledge.rs` | 1,800 |
| Session management | `crosslink/src/commands/session.rs` | 240 |
| Agent identity | `crosslink/src/identity.rs` | 100 |
| CLI entry point | `crosslink/src/main.rs` | 1,600+ |
| Kickoff skill | `crosslink/resources/claude/commands/kickoff.md` | 68 |
| Check skill | `crosslink/resources/claude/commands/check.md` | 141 |
| Session start hook | `crosslink/resources/claude/hooks/session-start.py` | ~200 |
| Dockerfile | `crosslink/resources/container/Dockerfile` | 36 |
| Container entrypoint | `crosslink/resources/container/entrypoint.sh` | 99 |

## Appendix B: Test Inventory

| Test Suite | Count | File |
|------------|-------|------|
| Unit tests (db) | 77 | `crosslink/src/db.rs:1523-2595` |
| Property tests (db) | 14 | `crosslink/src/db.rs:2596-2867` |
| CLI integration | 146 | `crosslink/tests/cli_integration.rs` |
| Tested module | 6 | `crosslink/src/commands/tested.rs:17-113` |
| Fuzz: create_issue | 1 | `crosslink/fuzz/fuzz_targets/fuzz_create_issue.rs` |
| Fuzz: search | 1 | `crosslink/fuzz/fuzz_targets/fuzz_search.rs` |
| Fuzz: import | 1 | `crosslink/fuzz/fuzz_targets/fuzz_import.rs` |
| Fuzz: dependency_graph | 1 | `crosslink/fuzz/fuzz_targets/fuzz_dependency_graph.rs` |
| Fuzz: state_machine | 1 | `crosslink/fuzz/fuzz_targets/fuzz_state_machine.rs` |
| Fuzz: cli_output | 1 | `crosslink/fuzz/fuzz_targets/fuzz_cli_output.rs` |
| Fuzz: comments | 1 | `crosslink/fuzz/fuzz_targets/fuzz_comments.rs` |
| Fuzz: labels | 1 | `crosslink/fuzz/fuzz_targets/fuzz_labels.rs` |
| Fuzz: update_operations | 1 | `crosslink/fuzz/fuzz_targets/fuzz_update_operations.rs` |
| Fuzz: milestones | 1 | `crosslink/fuzz/fuzz_targets/fuzz_milestones.rs` |
| Fuzz: subissues | 1 | `crosslink/fuzz/fuzz_targets/fuzz_subissues.rs` |
| Fuzz: relations | 1 | `crosslink/fuzz/fuzz_targets/fuzz_relations.rs` |
| **Total** | **255+** | |
