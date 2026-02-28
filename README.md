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

# Initialize in any project
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

- **Session memory** — Handoff notes, breadcrumbs, and session state survive restarts
- **Local-first** — All data in SQLite (`.crosslink/issues.db`), no cloud, works offline
- **Smart workflow** — `quick` command, `next` recommendations, `tree` visualization
- **Behavioral hooks** — Claude Code hooks enforce no-stubs, proper error handling, issue tracking
- **Multi-agent** — Distributed issue locking via git for parallel AI work
- **Templates** — Built-in templates for bugs, features, audits, investigations
- **Subissues & dependencies** — Break tasks down, track blocking relationships
- **Time tracking, milestones, archiving** — Full project management in the CLI
- **Works everywhere** — CLI + VS Code extension + context provider for any AI agent

## Installation

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
- [Claude Code Hooks](https://forecast-bio.github.io/crosslink/guides/hooks.html) — Behavioral guardrails for AI coding
- [Multi-Agent Coordination](https://forecast-bio.github.io/crosslink/guides/multi-agent.html) — Distributed locking for parallel AI work
- [Tracking Modes](https://forecast-bio.github.io/crosslink/guides/tracking-modes.html) — Strict, normal, and relaxed enforcement
- [CLI Reference](https://forecast-bio.github.io/crosslink/reference/commands.html) — Full command reference
- [Hook Configuration](https://forecast-bio.github.io/crosslink/reference/hook-config.html) — Customize enforcement behavior
- [Rules Customization](https://forecast-bio.github.io/crosslink/reference/rules.html) — Edit behavioral rules

## Development

```bash
cargo test          # Run tests
cargo clippy        # Lint
cargo fmt           # Format
```

## License

MIT
