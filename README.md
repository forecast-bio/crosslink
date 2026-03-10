# Crosslink

[![Crates.io](https://img.shields.io/crates/v/crosslink?style=flat-square)](https://crates.io/crates/crosslink)
[![Downloads](https://img.shields.io/crates/d/crosslink?style=flat-square)](https://crates.io/crates/crosslink)
[![License: MIT](https://img.shields.io/crates/l/crosslink?style=flat-square)](LICENSE)
![AI Generated](https://img.shields.io/badge/Code-AI_Generated-blue?style=flat-square&logo=probot&logoColor=white)

**The missing memory layer for AI-assisted development.**

AI coding assistants forget everything between conversations. Crosslink gives them persistent memory — sessions, handoff notes, issue tracking, and breadcrumbs that survive context compression and session restarts.

## Why Crosslink?

Every time an AI assistant's context window fills up or you start a new conversation, the AI loses all context about what it was doing, what's done, and what's next. You end up repeating yourself, re-explaining decisions, and watching the AI redo work.

Crosslink solves this with a local-first issue tracker designed specifically for AI workflows: sessions with handoff notes, breadcrumb tracking that survives context compression, and multi-agent coordination for parallel AI work.

## Quick Start

```bash
# Install
cargo install crosslink

# Initialize in any project (interactive TUI walkthrough)
crosslink init

# Start a session — see what the last AI left you
crosslink session start

# Create + label + start working in one step
crosslink quick "Fix auth token refresh" -p high -l bug

# Record breadcrumbs (survives context compression)
crosslink session action "Found root cause in refresh_token()"

# End with handoff notes for the next session
crosslink session end --notes "Fixed token refresh. Dark mode is next."
```

## Features

### Core Issue Tracking
- **Session memory** — Handoff notes, breadcrumbs, and session state survive restarts
- **Local-first** — All data in SQLite (`.crosslink/issues.db`), no cloud, works offline
- **Smart workflow** — `quick` command, `next` recommendations, `tree` visualization
- **Subissues & dependencies** — Break tasks down, track blocking relationships
- **Time tracking, milestones, archiving** — Full project management in the CLI
- **Templates** — Built-in templates for bugs, features, refactors, and research

### Multi-Agent Orchestration

Crosslink coordinates multiple AI agents working in parallel on the same codebase.

- **Distributed locking** — Agents claim issues via a shared git coordination branch, preventing conflicts
- **Agent identity** — Each agent gets a unique ID and SSH signing key (`crosslink agent init`)
- **`crosslink kickoff`** — Launch background agents in isolated git worktrees (local tmux or container)
  - Design doc-driven: pass `--doc` to generate implementation plans from a design document
  - `kickoff plan` — Read-only gap analysis against the codebase before committing to a build
  - `kickoff report` — Spec validation reports from completed agents
  - Pre-flight checks for required external commands with platform-specific install guidance
- **`crosslink swarm`** — Multi-agent swarm coordination across phased builds
  - `swarm init` — Initialize a swarm plan from a design document
  - `swarm plan` — Plan multi-phase builds across budget windows with cost estimation
  - `swarm launch` — Launch all agents for a phase
  - `swarm gate` — Run the test suite as a phase gate before proceeding
  - `swarm checkpoint` — Record progress after a phase completes
  - `swarm resume` — Reconstruct state and continue after a budget cap or session restart
  - Budget-aware scheduling with configurable window duration and model cost tracking
- **Container execution** — Run agents in isolated Docker containers (`crosslink container`)

### Knowledge Management

Shared documentation synced across agents via a dedicated git branch.

- **`crosslink knowledge`** — CRUD for markdown knowledge pages with YAML frontmatter
- **Full-text search** — `knowledge search` across all pages
- **Bulk import** — `knowledge import` from existing markdown files or design documents
- **Auto-injection** — Relevant knowledge pages injected into agent context automatically
- **Conflict resolution** — Accept-both merge strategy for concurrent knowledge edits

### Behavioral Hooks & Rules

Claude Code hooks that enforce code quality and workflow discipline.

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

Read-only interactive terminal UI built with ratatui:
- **Issues tab** — Tree view, detail view, filtering, and sorting
- **Agents tab** — Active session monitoring with heartbeat status
- **Knowledge tab** — Page browser with syntax highlighting
- **Milestones & Config tabs** — Project overview at a glance
- Mouse support, command palette (`Ctrl-P`), clipboard export, keyboard help (`?`)

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

- [Quick Start](https://forecast-bio.github.io/crosslink/getting-started/quickstart.html) — Get running in under a minute
- [Session Workflow](https://forecast-bio.github.io/crosslink/guides/session-workflow.html) — Deep dive into session management
- [Kickoff: Autonomous Agents](https://forecast-bio.github.io/crosslink/guides/kickoff.html) — Launch background agents in worktrees
- [Swarm Orchestration](https://forecast-bio.github.io/crosslink/guides/swarm.html) — Multi-agent phased builds from design documents
- [Container-Based Agents](https://forecast-bio.github.io/crosslink/guides/container-agents.html) — Run agents in isolated Docker containers
- [Knowledge Management](https://forecast-bio.github.io/crosslink/guides/knowledge.html) — Shared research pages synced via git
- [Terminal Dashboard](https://forecast-bio.github.io/crosslink/guides/tui.html) — Interactive TUI for browsing issues and agents
- [Multi-Agent Coordination](https://forecast-bio.github.io/crosslink/guides/multi-agent.html) — Distributed locking for parallel AI work
- [Claude Code Hooks](https://forecast-bio.github.io/crosslink/guides/hooks.html) — Behavioral guardrails for AI coding
- [Tracking Modes](https://forecast-bio.github.io/crosslink/guides/tracking-modes.html) — Strict, normal, and relaxed enforcement
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

## License

MIT
