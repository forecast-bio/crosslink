# Crosslink

[![Crates.io](https://img.shields.io/crates/v/crosslink?style=flat-square)](https://crates.io/crates/crosslink)
[![Downloads](https://img.shields.io/crates/d/crosslink?style=flat-square)](https://crates.io/crates/crosslink)
[![License: MIT](https://img.shields.io/crates/l/crosslink?style=flat-square)](LICENSE)
![AI Generated](https://img.shields.io/badge/Code-AI_Generated-blue?style=flat-square&logo=probot&logoColor=white)

![Crosslink Banner](images/banner.svg)

**You direct. Your agents remember. Nothing gets lost.**

AI coding agents forget everything between conversations. Crosslink gives them persistent memory — sessions, handoff notes, issue tracking, and breadcrumbs that survive context compression, session restarts, and agent handoffs.

## Why Crosslink?

You tell your agent to fix a bug. It investigates, finds the root cause, starts a fix — then the context window fills up. The agent restarts with zero memory. You explain everything again.

Crosslink eliminates this. Your agents track their own work, leave handoff notes for the next session, and coordinate with other agents on the same repo. You stay in the driver's seat. They handle the bookkeeping.

## Quick Start

**You say:**

> "Fix the auth token refresh bug — high priority"

**Your agent handles the rest:**

```bash
crosslink quick "Fix auth token refresh" -p high -l bug
crosslink session action "Exploring auth module..."
# ... investigates, implements, tests, commits ...
crosslink session end --notes "Fixed token refresh. PR ready for review."
```

Next session, any agent picks up right where this one left off.

<details>
<summary>Manual CLI equivalent</summary>

```bash
cargo install crosslink
crosslink init
crosslink session start
crosslink quick "Fix auth token refresh" -p high -l bug
# ... do the work yourself ...
crosslink session end --notes "Fixed token refresh."
```
</details>

## Features

### Core Issue Tracking
- **Session memory** — Handoff notes, breadcrumbs, and session state survive restarts and context compression
- **Local-first** — All data in SQLite (`.crosslink/issues.db`), no cloud, works offline
- **Smart workflow** — `quick` creates and starts work in one step. `next` recommends what to tackle. `tree` shows the hierarchy
- **Subissues & dependencies** — Break tasks down, track blocking relationships
- **Time tracking, milestones, archiving** — Full project management in the CLI
- **Templates** — Built-in templates for bugs, features, refactors, and research

### Design Document Workflow

Turn ideas into validated, codebase-grounded specs before writing code.

- **`/design`** — Interactive design document authoring through explore → interview → draft → validate loop
- **Codebase-grounded** — Explores real code, asks questions informed by what it finds, references real files
- **Validation** — Requirements, acceptance criteria, and open questions tracked in the document
- **Iteration** — `/design --continue <slug>` to refine an existing design across sessions
- **Feeds into implementation** — Design docs drive `kickoff --doc` and `swarm init --doc`

### Multi-Agent Orchestration

Launch multiple agents and let them coordinate automatically.

- **Distributed locking** — Agents claim issues via a shared git coordination branch, preventing conflicts
- **Agent identity** — Each agent gets a unique ID and SSH signing key (`crosslink agent init`)
- **`crosslink kickoff`** — Launch autonomous agents in isolated git worktrees
  - Agents explore, implement, test, commit, and self-review — fully tracked through crosslink
  - `--verify local|ci|thorough` — Configurable verification levels from local tests to CI + adversarial self-review
  - Design doc-driven: pass `--doc` to generate implementation plans from a design document
  - `kickoff plan` — Read-only gap analysis against the codebase before committing to a build
  - `kickoff report` — Spec validation reports from completed agents
  - `kickoff list` / `kickoff cleanup` — Monitor and manage running agents
- **`crosslink swarm`** — Multi-agent phased builds from design documents
  - `swarm init --doc` — Decompose a design document into phases and work units
  - `swarm launch` / `swarm gate` / `swarm checkpoint` — Execute, gate, and record phase progress
  - `swarm resume` — Reconstruct state and continue after interruption
  - Budget-aware scheduling with cost estimation and configurable window duration
  - Multi-swarm support — manage multiple independent swarms with `swarm create`, `swarm list`, `swarm switch`
  - Plan editing — restructure plans post-init with `move`, `merge`, `split`, `remove`, `reorder`, `rename`
- **Container execution** — Run agents in isolated Docker containers (`crosslink container`)

### Knowledge Management

Research done by one agent is available to all.

- **`crosslink knowledge`** — CRUD for markdown knowledge pages with YAML frontmatter
- **Full-text search** — `knowledge search` across all pages with tag and date filtering
- **Cross-repo querying** — `--from` flag to search issues and knowledge in external repositories
- **Section-based editing** — `--replace-section` and `--append-to-section` for surgical updates
- **Bulk import** — `knowledge import` from existing markdown files or design documents
- **Auto-injection** — Relevant knowledge pages injected into agent context via MCP server
- **Conflict resolution** — Accept-both merge strategy for concurrent knowledge edits

### Behavioral Hooks & Rules

Your agents follow the rules without being told.

- **Issue tracking enforcement** — Hooks block code changes without an active crosslink issue
- **No-stubs policy** — Post-edit hooks detect `TODO`, `FIXME`, `unimplemented!()` stubs
- **Drift detection** — Adaptive reminders when agent behavior drifts from project norms
- **Tracking modes** — Strict, normal, and relaxed enforcement (`crosslink workflow`)
- **Language-aware rules** — 20+ language-specific rule files auto-injected based on project languages
- **House style** — `crosslink style` syncs project conventions from a central git repo
- **Local overrides** — `rules.local/` directory for gitignored per-machine rule customizations

### Terminal Dashboard

```bash
crosslink tui
```

Live interactive terminal UI built with ratatui:
- **Issues tab** — Tree view, detail view, filtering, and sorting
- **Agents tab** — Active session monitoring with heartbeat status
- **Knowledge tab** — Page browser with syntax highlighting
- **Milestones & Config tabs** — Project overview at a glance
- Mouse support, command palette (`Ctrl-P`), clipboard export, keyboard help (`?`)

### Web Dashboard

```bash
crosslink serve
```

Browser-based dashboard for visual project oversight — charts, drag-and-drop, and real-time agent monitoring.

### Maintenance

- **`crosslink prune`** — Squash stale hub and knowledge branch history
- **`crosslink integrity`** — Data integrity checks (counters, hydration, locks, schema, layout)
- **`crosslink compact`** — Manual event compaction

### Other

- **SSH signing** — Agent key generation, per-commit signing, allowed_signers management
- **Driver intervention tracking** — `crosslink intervene` logs human corrections for agent improvement
- **Typed comments** — Comments carry `kind` (plan, decision, observation, blocker, resolution, result)
- **Clock skew detection** — Uses git commit timestamps as witness to detect time drift
- **Context measurement** — `crosslink context` measures and optimizes context injection overhead
- **Configurable git remote** — Use any remote for hub/knowledge branches, not just `origin`
- **Works everywhere** — CLI + VS Code extension + context provider for any AI agent

## Installation

Requires **Rust 1.87+** ([install rustup](https://rustup.rs/)).

```bash
# From crates.io
cargo install crosslink

# From source
git clone https://github.com/forecast-bio/crosslink.git
cd crosslink/crosslink && cargo install --path .
```

Also available as a [VS Code extension](https://marketplace.visualstudio.com/items?itemName=forecast-bio.crosslink-issue-tracker).

## Documentation

**Getting started:**

- [Your First Agent Session](https://forecast-bio.github.io/crosslink/getting-started/quickstart.html) — Get running in under a minute
- [Installation](https://forecast-bio.github.io/crosslink/getting-started/installation.html) — All install methods and requirements

**Guides:**

- [Session Workflow](https://forecast-bio.github.io/crosslink/guides/session-workflow.html) — Persistent memory across conversations
- [Multi-Agent Coordination](https://forecast-bio.github.io/crosslink/guides/multi-agent.html) — Distributed locking for parallel AI work
- [Design Document Workflow](https://forecast-bio.github.io/crosslink/guides/design-workflow.html) — Interactive, codebase-grounded design authoring
- [Kickoff: Autonomous Agents](https://forecast-bio.github.io/crosslink/guides/kickoff.html) — Launch background agents in worktrees
- [Swarm Orchestration](https://forecast-bio.github.io/crosslink/guides/swarm.html) — Multi-agent phased builds from design documents
- [Container-Based Agents](https://forecast-bio.github.io/crosslink/guides/container-agents.html) — Run agents in isolated Docker containers
- [Knowledge Management](https://forecast-bio.github.io/crosslink/guides/knowledge.html) — Shared research pages synced via git
- [Terminal Dashboard](https://forecast-bio.github.io/crosslink/guides/tui.html) — Interactive TUI for issues and agents
- [Web Dashboard](https://forecast-bio.github.io/crosslink/guides/web-dashboard.html) — Browser-based project oversight
- [Behavioral Hooks](https://forecast-bio.github.io/crosslink/guides/hooks.html) — Guardrails for AI coding agents
- [Tracking Modes](https://forecast-bio.github.io/crosslink/guides/tracking-modes.html) — Strict, normal, and relaxed enforcement
- [Maintenance](https://forecast-bio.github.io/crosslink/guides/maintenance.html) — Pruning, health checks, and database compaction

**Reference:**

- [CLI Reference](https://forecast-bio.github.io/crosslink/reference/commands.html) — Full command reference
- [Hook Configuration](https://forecast-bio.github.io/crosslink/reference/hook-config.html) — Customize enforcement behavior
- [Rules Customization](https://forecast-bio.github.io/crosslink/reference/rules.html) — Edit behavioral rules
- [Kickoff Report Schema](https://forecast-bio.github.io/crosslink/reference/kickoff-report.html) — Agent validation report format

## Development

```bash
cargo test          # Run tests
cargo clippy        # Lint
cargo fmt           # Format
```

See also:
- [Architecture Overview](docs/ARCHITECTURE.md)
- [ELI5 Explanation](docs/ELI5.md)
- [API Documentation](docs/api.md)

## License

[MIT](LICENSE)
