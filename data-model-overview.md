---
title: "Data Model Overview"
tags: ["architecture", "onboarding"]
sources: []
contributors: ["maxine--basel"]
created: 2026-03-17
updated: 2026-03-31
---


# Data Model Overview

How crosslink's core entities relate to each other. This is the conceptual model — see `dual-state-architecture-gotchas` for how it maps to storage.

## Entity Relationship

```
Milestone
  └── has many Issues (via milestone_issues)

Issue
  ├── has many Labels (many-to-many via labels table)
  ├── has many Comments (each with kind, author, optional signature)
  ├── has many Subissues (self-referential via parent_id)
  ├── has many Dependencies (blocker/blocked via dependencies table)
  ├── has many Relations (bidirectional via relations table)
  ├── has many Time Entries (start/stop timer)
  └── may have a Lock (one active agent claim at a time)

Session
  ├── has one active Issue (optional, via active_issue_id)
  ├── has handoff notes (free text for next session)
  ├── has last_action breadcrumb
  └── has agent_id (which agent ran this session)

Token Usage
  ├── belongs to Session (optional)
  └── belongs to Agent (by agent_id)

Knowledge Page (on crosslink/knowledge branch)
  ├── has YAML frontmatter (title, tags, sources, contributors)
  └── has markdown body
```

## Issue Lifecycle

```
created (open) -> worked on -> closed -> archived
                     |             |
                     +-- reopened -+
```

- **open**: Default state. Visible in `issue list`.
- **closed**: Done. Hidden from default list, visible with `-s closed` or `-s all`.
- **archived**: Long-term storage. Visible with `-s all` or via `archive list`.

## Comment Kinds

Comments carry a `kind` field for structured audit trails:

| Kind | Purpose | Example |
|------|---------|---------|
| `note` | General comment (default) | "Looking into this now" |
| `plan` | Intended approach | "Will extract auth into separate module" |
| `decision` | Chosen direction with rationale | "Going with retry+backoff over circuit breaker" |
| `observation` | Finding during investigation | "Root cause is in token_refresh.rs:42" |
| `blocker` | Something preventing progress | "Blocked on CI credentials not being set" |
| `resolution` | How a blocker was resolved | "CI creds added to GitHub secrets" |
| `result` | Outcome or measurement | "Latency reduced from 200ms to 45ms" |

## Priority Levels

## Priority Levels

| Priority | When to Use | Availability |
|----------|-------------|--------------|
| `critical` | Blocking other work, production issue, data loss risk | CLI only |
| `high` | Important, should be done soon | CLI + API |
| `medium` | Normal priority (default) | CLI + API |
| `low` | Nice to have, do when time allows | CLI + API |

> **Note**: The web dashboard API accepts `low`, `medium`, and `high` only. The CLI accepts all four including `critical`. If you create a `critical` issue via CLI, it will display correctly everywhere but cannot be set via the API.

## Identity and Attribution

Each entity can be attributed:
- **Issues**: `created_by` field (agent ID)
- **Comments**: `author` field (agent ID) + optional `driver_key_fingerprint` (SSH signature)
- **Sessions**: `agent_id` field
- **Hub events**: Signed with agent's SSH key, verified against `trust/allowed_signers`

## UUIDs vs IDs

Issues, comments, and milestones have both:
- **`id`** (INTEGER): Local auto-increment, used in CLI commands (`crosslink show 42`)
- **`uuid`** (TEXT): Globally unique, used for hub coordination and cross-agent references

When referencing issues in comments or handoff notes, use the numeric ID — it's what CLI commands accept. UUIDs are internal plumbing.
