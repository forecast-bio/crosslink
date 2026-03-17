---
title: "What Is Crosslink — Agent Orientation"
tags: [overview, onboarding]
sources: []
contributors: [maxine--basel]
created: 2026-03-17
updated: 2026-03-17
---

# What Is Crosslink — Agent Orientation

If you're an AI agent starting a session on a crosslink-enabled repo, this page tells you what crosslink is, why it exists, and what you need to know.

## The Problem

AI coding agents lose all context between conversations. When a context window fills up or a session ends, the next agent starts from scratch. This means:
- Repeated investigation of the same bugs
- Lost decisions and rationale
- No coordination between agents working on the same repo
- No audit trail of what was done and why

## What Crosslink Does

Crosslink is a CLI tool that gives agents persistent memory and coordination. It provides:

1. **Session continuity** — Handoff notes, breadcrumbs, and active work items survive session restarts
2. **Issue tracking** — A local-first issue tracker (SQLite + git) purpose-built for agent workflows
3. **Multi-agent coordination** — Distributed locking, agent identity, and event-sourced state sharing via git
4. **Knowledge base** — Shared markdown pages for research, decisions, and conventions (what you're reading now)
5. **Behavioral enforcement** — Hooks and rules that keep agents on track without human reminding

## How It Stores Data

Crosslink maintains two complementary stores:

- **SQLite** (`.crosslink/issues.db`) — Fast local queries for the CLI and dashboard. Sessions, time entries, and config live here exclusively.
- **Git hub branch** (`crosslink/hub`) — Event-sourced JSON files shared across agents via git sync. Issues, comments, labels, milestones, and locks live here for multi-agent coordination.

Hydration flows one way: hub -> SQLite. The CLI reads from SQLite but writes shared state through SharedWriter, which commits to the hub branch and then hydrates locally.

## Your First Actions in Any Session

```bash
crosslink session start          # See previous handoff notes
crosslink issue list             # See what's open
crosslink issue ready            # See what's unblocked
crosslink session work <id>      # Set your active work item
```

## Your Last Actions Before Context Compresses

```bash
crosslink session end --notes "What I did, what's left, what the next agent needs to know"
```

This is the single most important thing you can do for the next session. Good handoff notes prevent the next agent from repeating your work.

## Key Concepts

| Concept | What It Is |
|---------|-----------|
| **Session** | A single agent work period. Has start/end, active issue, handoff notes. |
| **Issue** | A tracked work item with title, priority, status, labels, subissues, dependencies. |
| **Subissue** | A child issue for breaking down large tasks (recommended for >500 lines of change). |
| **Label** | Free-form tag on an issue (bug, feature, review, etc.). |
| **Dependency** | "Issue A blocks Issue B" — tracked and queryable via `issue blocked` / `issue ready`. |
| **Comment** | Typed note on an issue. Kinds: plan, decision, observation, blocker, resolution, result. |
| **Knowledge page** | Shared markdown document. Used for research, conventions, design analysis. What you're reading. |
| **Lock** | Distributed claim on an issue. Prevents two agents from working the same thing. |
| **Agent identity** | Unique ID + SSH key per agent. Used for signing events and audit trail. |
| **Hub branch** | Git branch (`crosslink/hub`) that stores shared state as JSON event files. |

## Where to Learn More

- `crosslink knowledge search <topic>` — Search all knowledge pages
- `crosslink knowledge show <slug>` — Read a specific page
- `crosslink knowledge list --tag conventions` — Project conventions established by the team
- `crosslink knowledge list --tag architecture` — Architecture and design docs
