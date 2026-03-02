---
allowed-tools: Bash(crosslink *), Bash(git *), Bash(ls *), Read
description: Full context dump for debugging — use when stuck or disoriented
---

## Context

- Session status: !`crosslink session status`
- Open issues: !`crosslink list -s open`
- Active locks: !`crosslink locks list 2>/dev/null || echo "(no locks)"`
- Current branch: !`git branch --show-current`
- Working tree: !`git status --short`

## Your task

You are stuck, confused, or need to re-orient. This skill dumps all available context so you can diagnose the problem. Run through each section and print the results.

### 1. Project grounding (same as /preflight)

Read core rules:
```
Read .crosslink/rules/global.md
```

Detect languages and read relevant rule files (check for `Cargo.toml`, `package.json`, `tsconfig.json`, `pyproject.toml`, `go.mod`, etc.).

Read project-specific rules:
```
Read .crosslink/rules/project.md
```

Read tracking rules based on current mode:
```bash
crosslink config get tracking_mode
```
Then read `.crosslink/rules/tracking-<mode>.md`.

### 2. Project tree scan

```bash
ls -1
```

Scan the project tree (max depth 3, max 50 entries) to ground yourself on actual paths.

### 3. Dependency versions

Read the primary manifest file to confirm actual dependency versions.

### 4. Session state

```bash
crosslink session status
```

What issue are you working on? What was the last action?

### 5. Active issue details

If working on an issue, get full details:

```bash
crosslink show <issue-id>
```

Review all comments, especially plan and decision comments.

### 6. Related issues and blockers

```bash
crosslink blocked
crosslink ready
```

Are there blocking dependencies? What's unblocked and available?

### 7. Lock state

```bash
crosslink locks list 2>/dev/null
```

Are any issues locked by other agents?

### 8. Recent interventions

Check if there have been recent hook blocks or driver redirects by reviewing recent issue comments.

### 9. Hook configuration

```bash
crosslink config show
```

What tracking mode is active? What commands are blocked/gated?

### 10. Git state

```bash
git status
git log --oneline -5
git diff --stat HEAD
```

What's the current branch state? Any uncommitted changes?

### 11. Print diagnostic summary

```
Audit summary:
  Session:    active / working on #<id>
  Branch:     <branch>
  Tracking:   <mode>
  Languages:  <list>
  Open issues: <count>
  Blocked:    <count>
  Locks:      <count>
  Uncommitted: <count> files changed

Loaded rules: global.md, <lang>.md, tracking-<mode>.md, project.md
```

You are now fully re-oriented. Decide your next action based on this context.
