# Crosslink: Explain Like I'm 5

> **Crosslink gives AI assistants a memory that survives between conversations.**

## What is it?

Crosslink is a **to-do list and memory system for AI coding assistants**.

When you use AI tools like Claude to help you code, the AI forgets everything between conversations — and even during long ones when the context window fills up. Crosslink solves this by giving the AI a place to write down what it's working on, what it's learned, and what the next agent should do.

## Before & After

**Without Crosslink:**
> You tell your agent to refactor the auth system. It gets halfway through, the context window fills up, and the session resets. The next agent has no idea what was done, what's left, or why certain decisions were made. You spend 20 minutes re-explaining everything. It redoes work that was already finished. Repeat.

**With Crosslink:**
> You tell your agent to refactor the auth system. The agent creates an issue, breaks it into subissues, and records progress as it goes. The context window fills up — no problem. The next agent reads the handoff notes: "Refactored token refresh (done), session middleware (done), need to update login endpoint next." It picks up exactly where the previous agent left off.

## What can it do?

**Single agent:**

1. **You give an instruction** — The agent creates a task in Crosslink
2. **The agent works on it** — It updates the task with progress and notes
3. **Session ends or context resets** — No problem! The tasks are saved
4. **Next agent starts** — It reads the tasks and picks up where the previous one left off

**Multiple agents:**

1. **You say "build these three features"** — Crosslink launches a separate agent for each one
2. **Each agent works in its own copy of the code** — They coordinate through git so they don't step on each other
3. **You check in when you want** — Agents run in the background and report when they're done

**Design → Build pipeline:**

1. **You describe an idea** — `/design "add batch retry logic"`
2. **Agent interviews you** — Asks questions grounded in what it found in the codebase
3. **Agent writes a design doc** — With requirements, acceptance criteria, and real file references
4. **Agent builds from the design** — `/kickoff --doc` or `swarm init --doc` for larger features

## Try it in 30 seconds

```bash
cargo install crosslink
cd your-project
crosslink init
crosslink session start
crosslink quick "My first task" -p high
crosslink session end --notes "Ready to start working on this next time."
```

## Why should I care?

- **No more repeating yourself** — The AI remembers what you were working on
- **Better handoffs** — Switch between AI sessions without losing context
- **Multiple agents at once** — Work on several features in parallel, safely
- **Design before you build** — Validated specs, not vibes-driven coding
- **Shared knowledge** — Research done by one agent is available to all
- **Automatic changelog** — When tasks are done, they're logged automatically

## One-liner

> Crosslink gives AI assistants a memory that survives between conversations.
