---
title: "Multi-Agent Coordination — Practical Guide"
tags: ["coordination", "onboarding"]
sources: []
contributors: ["maxine--basel"]
created: 2026-03-17
updated: 2026-03-31
---


# Multi-Agent Coordination — Practical Guide

How agents actually coordinate in crosslink. For the deep CRDT design, see `event-sourced-coordination`. This page covers practical usage.

## The Hub Branch

## The Hub Branch

All multi-agent state lives on the `crosslink/hub` git branch. This branch is never checked out directly — crosslink manages it via a worktree at `.crosslink/.hub-cache`.

```
crosslink/hub branch structure (V2 layout):
├── agents/
│   └── {agent-id}/             # Per-agent event logs
│       └── {event-id}.json
├── checkpoint/
│   ├── state.json              # Compaction watermark and lease state
│   └── skew_warnings.json      # Clock skew violation records
├── heartbeats/
│   └── {agent-id}.json         # Agent liveness heartbeats
├── issues/
│   └── {uuid}/                 # One directory per issue
│       ├── issue.json          # Issue metadata (title, priority, status, etc.)
│       └── comments/           # Comments embedded with their issue
│           └── {comment-uuid}.json
├── locks/
│   └── {issue-id}.json         # Active lock claims (by display ID)
├── locks.json                  # Legacy V1 lock file (read for migration)
├── meta/
│   ├── counters.json           # Next display_id/comment_id/milestone_id allocation
│   ├── milestones/             # Milestone files
│   └── version.json            # Hub layout version marker
└── trust/
    ├── allowed_signers         # Approved agent keys for signature verification
    ├── approvals/              # Trust approval records
    └── keys/
        └── {agent-id}.pub     # Published agent public keys
```

Key V2 changes from V1:
- Issues have their own directories instead of flat files
- Comments are co-located with their parent issue (not in a separate top-level directory)
- Events are partitioned by agent (under `agents/`)
- Heartbeats have their own top-level directory
- Layout version is tracked in `meta/version.json`

## Sync Flow

```bash
crosslink sync
```

This runs: fetch remote hub -> rebase local hub -> push -> hydrate SQLite.

- **Fetch**: Pull latest hub branch state from remote
- **Rebase**: Apply local uncommitted hub changes on top of remote
- **Push**: Send local changes to remote (retries 3x on conflict)
- **Hydrate**: Read hub JSON files and upsert into local SQLite

If push fails (offline, conflict after retries), changes are saved locally. Next sync will retry.

## Locking

Locks prevent two agents from working the same issue simultaneously.

```bash
crosslink locks claim 42     # Write lock file to hub, sync
crosslink locks check 42     # Check if claimed (and by whom)
crosslink locks release 42   # Remove lock file, sync
crosslink locks list          # All active locks
crosslink locks list --stale  # Locks held by agents that haven't heartbeated
```

**Stale lock detection**: If an agent's last heartbeat is older than the configured timeout (default: 30 minutes), its locks are considered stale. Another agent can steal a stale lock:

```bash
crosslink locks steal 42     # Take over a stale lock
```

The original agent will detect the stolen lock on its next operation and stop working on that issue.

## Agent Identity

Each agent has a unique identity created at initialization:

```bash
crosslink agent init          # Generate agent ID + SSH key pair
```

This creates:
- `.crosslink/keys/{agent-id}_ed25519` — Private key (mode 0600)
- Published `trust/keys/{agent-id}.pub` on hub branch

A human driver approves agent keys:
```bash
crosslink trust pending       # See unapproved keys
crosslink trust approve <id>  # Add to allowed_signers
```

## Heartbeats

Agents emit heartbeats during work to signal liveness:
- Written to hub branch during sync
- Used for stale lock detection
- Visible in dashboard agent monitoring

## Conflict Resolution

When two agents modify different issues: no conflict (separate files on hub).

When two agents modify the same issue: the event-sourced model handles this:
1. Both agents append events to the issue's event log
2. On sync, both event streams merge (events have total ordering keys)
3. Hydration replays all events to compute final state
4. Last-writer-wins for simple fields; append-only for comments/labels

When two agents edit the same knowledge page: accept-both merge strategy appends both versions.

## Practical Tips

- **Always sync before starting work** — Gets you the latest lock state and issue updates
- **Sync after significant changes** — Don't let local state drift too far from remote
- **Use locks for implementation work** — Prevents merge headaches
- **Don't lock investigation/review** — Multiple agents can investigate the same issue safely
- **Check `issue blocked`** — Before picking up work, verify nothing is blocking it
- **Use `issue ready`** — Shows issues that are open, unblocked, and unlocked
