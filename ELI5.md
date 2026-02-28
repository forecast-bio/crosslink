# Crosslink: Explain Like I'm 5

> **Crosslink gives AI assistants a memory that survives between conversations.**

## What is it?

Crosslink is a **to-do list for AI assistants**.

When you use AI tools like Claude to help you code, the AI sometimes forgets what it was doing — especially during long conversations or when you start a new chat.

Crosslink solves this by giving the AI a place to write down:
- What it's working on
- What's done
- What's next
- Important notes for later

## Before & After

**Without Crosslink:**
> You ask Claude to refactor your auth system. It gets halfway through, the context window fills up, and the session resets. New Claude has no idea what was done, what's left, or why certain decisions were made. You spend 20 minutes re-explaining everything. It redoes work that was already finished. Repeat.

**With Crosslink:**
> You ask Claude to refactor your auth system. It creates an issue, breaks it into subissues, and records progress as it goes. The context window fills up — no problem. New Claude reads the handoff notes: "Refactored token refresh (done), session middleware (done), need to update login endpoint next." It picks up exactly where the last session left off.

## How does it work?

1. **You ask the AI to do something** — The AI creates a task in Crosslink
2. **The AI works on it** — It updates the task with progress and notes
3. **Conversation ends or resets** — No problem! The tasks are saved
4. **New conversation starts** — The AI reads the tasks and picks up where it left off

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
- **Automatic changelog** — When tasks are done, they're logged automatically

## One-liner

> Crosslink gives AI assistants a memory that survives between conversations.
