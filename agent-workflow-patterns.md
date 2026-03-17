---
title: "Agent Workflow Patterns"
tags: [workflow, onboarding]
sources: []
contributors: [maxine--basel]
created: 2026-03-17
updated: 2026-03-17
---

# Agent Workflow Patterns

Practical patterns for how agents should use crosslink throughout a work session. These are conventions established through experience, not just documentation of commands.

## Pattern 1: The Standard Session

The most common workflow for a single agent working on a task.

```
session start -> see handoff -> pick/create issue -> work -> comment -> session end
```

```bash
crosslink session start                    # Read previous handoff notes
crosslink issue ready                      # Find unblocked work
crosslink session work <id>                # Claim the issue

# ... do the work ...
crosslink issue comment <id> "Found root cause in auth.rs:142 — token expiry check uses wrong clock" --kind observation
crosslink issue comment <id> "Decided to fix by switching to monotonic clock instead of wall clock" --kind decision

# ... implement, test, commit ...
crosslink session end --notes "Fixed token expiry. PR #42 open. Tests pass locally, CI running. Remaining: update docs for new clock behavior."
```

**Key habits:**
- Always start with `session start` to see what the previous agent left
- Use typed comments (`--kind plan`, `--kind decision`, `--kind blocker`) to leave a trail
- End with detailed handoff notes — this is the #1 thing that helps the next session

## Pattern 2: Breaking Down Large Work

When a task is too big for one session (>500 lines of change), decompose into subissues.

```bash
crosslink issue create "Refactor auth module" -p high -d "Auth module is 2000 lines, needs decomposition"
# Returns issue #50

crosslink subissue 50 "Extract token validation into auth/tokens.rs"
crosslink subissue 50 "Extract session management into auth/sessions.rs"  
crosslink subissue 50 "Add unit tests for extracted modules"

crosslink issue tree   # Visualize the hierarchy
```

Each subissue can be worked independently, possibly by different agents. The parent issue tracks overall completion.

## Pattern 3: Design-First Feature Work

For non-trivial features, start with a design document before writing code.

```
/design "feature description" -> iterate on open questions -> kickoff or swarm
```

1. `/design "Add batch retry logic for sync"` — Explores codebase, asks questions, drafts design doc
2. Answer open questions, iterate with `/design --continue <slug>`
3. `crosslink kickoff run "batch retry" --doc .design/batch-retry-logic.md` — Launch agent with design doc
4. Or `crosslink swarm init --doc .design/batch-retry-logic.md` for multi-agent phased execution

## Pattern 4: Multi-Agent Coordination

When multiple agents work the same repo simultaneously.

**Locking** prevents conflicts:
```bash
crosslink locks check <id>     # Is this issue claimed?
crosslink locks claim <id>     # Claim it for yourself
crosslink locks release <id>   # Done, release for others
```

**Sync** shares state:
```bash
crosslink sync                 # Pull latest from hub, push local changes
```

**Knowledge** shares research:
```bash
crosslink knowledge add "api-rate-limits" --tag research --content "Found that the API rate limits at 100 req/s per key..."
# Other agents can now: crosslink knowledge search "rate limits"
```

## Pattern 5: Investigation and Research

When you're exploring a problem space rather than implementing.

```bash
crosslink issue quick "Investigate flaky CI test_sync_recovery" -p medium -l investigation

# ... research ...
crosslink issue comment <id> "The test assumes network is available but CI runners have intermittent DNS" --kind observation
crosslink issue comment <id> "Three options: mock DNS, retry with backoff, or skip in CI" --kind plan
crosslink issue comment <id> "Going with retry+backoff — least invasive, matches production behavior" --kind decision
crosslink issue comment <id> "Root cause: test_sync_recovery uses real git remote. Fixed by adding 3-retry with 1s backoff" --kind resolution

crosslink session end --notes "Flaky test fixed. Root cause was DNS in CI. Added retry logic. See comments on #<id> for full analysis."
```

The typed comments create a decision trail that future agents can reference.

## Pattern 6: Adversarial Review

When reviewing code or design for issues.

```bash
crosslink issue quick "Adversarial review of sync module" -p high -l review

# ... review ...
# Create subissues for each finding:
crosslink subissue <id> "Silent error in shared_writer.rs:275" -p high
crosslink subissue <id> "Missing transaction guard in create.rs:137" -p high
crosslink subissue <id> "No test for offline sync recovery" -p medium

crosslink issue tree  # See all findings organized under the review issue
```

## Anti-Patterns to Avoid

- **Starting work without `session start`** — You miss handoff notes from the previous session
- **Ending without handoff notes** — The next agent starts from scratch
- **Working without an active issue** — Hooks may block your changes; also loses audit trail
- **Giant issues** — Break down anything >500 lines into subissues
- **Untyped comments** — Always use `--kind` so the comment trail is scannable
- **Forgetting to sync** — Run `crosslink sync` before and after significant work in multi-agent setups
