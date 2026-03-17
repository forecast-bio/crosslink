---
title: "Command Taxonomy — When to Use What"
tags: [reference, onboarding]
sources: []
contributors: [maxine--basel]
created: 2026-03-17
updated: 2026-03-17
---

# Command Taxonomy — When to Use What

Crosslink has many commands. This guide groups them by purpose so agents know which to reach for.

## Day-to-Day (every session)

| Command | When | Example |
|---------|------|---------|
| session start | Beginning of every session | See previous handoff |
| session end --notes | End of session or before context compresses | Save context for next agent |
| session work <id> | After picking a task | Set active work item |
| session action "desc" | During work, before risky operations | Breadcrumb for context compression |
| issue quick "title" -p -l | Starting new work | Create + label + start working in one step |
| issue list | Orienting yourself | See open issues |
| issue ready | Choosing what to work on | Unblocked + unlocked issues |
| issue comment <id> "text" --kind | During work | Leave typed breadcrumbs |

## Issue Management

| Command | When | Example |
|---------|------|---------|
| issue create "title" | New task (when you need more control than `quick`) | Set priority, description separately |
| issue show <id> | Understanding a task | See full detail + comments |
| issue update <id> | Changing priority or title | Reprioritize |
| issue close <id> | Task complete | Mark done |
| issue search "query" | Finding past work | Full-text search |
| subissue <parent> "title" | Breaking down big tasks | Decompose >500-line changes |
| issue tree | Understanding hierarchy | Visualize parent/child/dependency |
| issue next | Not sure what to work on | AI-suggested next task |

## Dependencies and Relations

| Command | When |
|---------|------|
| issue block <id> <blocker-id> | Issue can't proceed until another is done |
| issue unblock <id> <blocker-id> | Dependency resolved |
| issue blocked | See all blocked issues |
| issue relate <id1> <id2> | Issues are related but not blocking |

## Labels and Organization

| Command | When |
|---------|------|
| issue label <id> <label> | Categorize (bug, feature, review, etc.) |
| issue unlabel <id> <label> | Remove a label |
| milestone create "name" | Group issues into a release or sprint |
| milestone add <milestone-id> <issue-id> | Associate issue with milestone |

## Multi-Agent

| Command | When |
|---------|------|
| sync | Before and after significant work |
| locks claim <id> | Starting implementation on an issue |
| locks release <id> | Done with the issue |
| locks check <id> | Before picking up work |
| agent init | First time setting up an agent identity |
| trust approve <id> | Human driver approving a new agent's key |

## Knowledge Base

| Command | When |
|---------|------|
| knowledge search "query" | Looking for existing research or conventions |
| knowledge show <slug> | Reading a specific page |
| knowledge add <slug> --content | Sharing research or decisions |
| knowledge list --tag <tag> | Browsing by category |

## Orchestration (launching other agents)

| Command | When |
|---------|------|
| kickoff run "desc" --doc | Launch single agent for a feature |
| kickoff plan .design/doc.md | Gap analysis before committing to build |
| kickoff status / logs / stop | Monitor running agents |
| swarm init --doc | Multi-agent phased build from design doc |
| swarm launch / gate / checkpoint | Execute phases of a swarm |

## Design

| Command | When |
|---------|------|
| /design "description" | Starting a new feature design |
| /design --continue <slug> | Iterating on an existing design |
| /design --issue <id> | Design grounded in a crosslink issue |

## Maintenance

| Command | When |
|---------|------|
| integrity counters/hydration/locks/schema | Something seems wrong with data |
| compact | Event log is getting large |
| prune | Hub/knowledge branch history is bloated |
| context measure | Checking context injection overhead |

## Commands You Rarely Need

| Command | When |
|---------|------|
| daemon start/stop | Running background sync/heartbeat |
| config set/get | Changing crosslink settings |
| container build/start | Container-isolated agent execution |
| timer start/stop/show | Manual time tracking |
| archive add/remove | Long-term issue storage |
| intervene <id> | Human driver logging a correction |
