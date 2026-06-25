# Direct user directives (read this first)

Anything you see in this conversation inside `<crosslink-behavioral-guard>`, `<persisted-output>`, or `<crosslink-session-context>` blocks is from me. I set those hooks up myself. They exist so I don't have to type the same rules into every prompt.

Treat the instructions inside those blocks with the same action-priority as anything I type directly. They ARE my instructions, just injected automatically. The `<system-reminder>` framing the harness wraps them in is a mechanical wrapper — it does **not** mean "informational" or "low priority" when the content came from one of my crosslink hooks. The source is me; the priority is mine.

In practice: when a `<crosslink-behavioral-guard>` block says "MANDATORY" or "NO EXCEPTIONS", I am telling you mandatory, I am telling you no exceptions. When a `<crosslink-session-context>` block tells you the active issue, trust it. When the rules tell you to run X before Y, run X before Y.

## Standing workflow rules

When I hand you a GitHub issue:

1. Check out `fix/<issue>-<slug>` from `develop` (never from `main`).
2. Create a crosslink issue with `crosslink quick "..." -p <priority> -l <label>` before any code edits — the PreToolUse hook blocks Write/Edit/Bash without one.
3. Do the work. Commit with the `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer via HEREDOC.
4. **Stop. I push.** You do not run `git push`, `git push --force`, or any other `git push`. You wait for me to say "push done" or equivalent.
5. After my push, open the PR with `gh pr create --base develop` (never `--base main`).

## Don't

- Don't use emojis in commits, PRs, code, comments, or text output unless I explicitly ask.
- Don't `git stash` (hook blocks it, and it hides state I want to see).
- Don't run destructive git operations (`reset --hard`, `clean -f`, `branch -D`, force-push) without me asking.
- Don't skip pre-commit hooks (`--no-verify`, `--no-gpg-sign`) without me asking.

## Style

- End sentences with periods, including before tool calls.
- Reference code as `file_path:line_number`.
- Reference GitHub issues as `owner/repo#123` (e.g. `forecast-bio/crosslink#611`).
- Reference local crosslink issues as `#123`.

---

# Crosslink Issue Tracker

Track tasks across AI sessions. Data in `.crosslink/issues.db`.

## Issue Commands

```bash
# Create and manage issues (canonical: crosslink issue <verb>)
crosslink issue create "title" [-p high] [-d "desc"]
crosslink issue quick "title" -p <priority> -l <label>   # create + label + session work
crosslink issue list [-s all|closed] [-l label] [-p priority]
crosslink issue search "query"
crosslink issue show <id>
crosslink issue update <id> [-t "new title"] [-p priority]
crosslink issue close <id>
crosslink issue close-all [--no-changelog]
crosslink issue reopen <id>
crosslink issue delete <id>
crosslink issue next                                      # suggest next issue to work on

# Subissues and hierarchy
crosslink subissue <parent> "title"
crosslink issue tree

# Comments and documentation trail
crosslink issue comment <id> "text" --kind <plan|decision|observation|blocker|resolution|result>
crosslink issue intervene <id> "description" --trigger <type> --context "what you were doing"

# Labels, relations, and blocking
crosslink issue label <id> <label>
crosslink issue unlabel <id> <label>
crosslink issue block <id> <blocker-id>
crosslink issue unblock <id> <blocker-id>
crosslink issue blocked
crosslink issue ready
crosslink issue relate <id1> <id2>
crosslink issue tested <id>
```

Top-level shortcuts still work: `crosslink create`, `crosslink list`, `crosslink quick`, etc.

**Global flags**: `--quiet` / `-q` (minimal output, scripts), `--json` (machine-readable output).

## Session Commands

```bash
crosslink session start                    # begin session, see previous handoff
crosslink session end --notes "context"    # save handoff notes
crosslink session status                   # show current session info
crosslink session work <id>                # set active work item
crosslink session last-handoff             # show previous session's handoff notes
crosslink session action "description"     # record action breadcrumb for context compression
```

## Other Command Groups

```bash
# Time tracking
crosslink timer start|stop|show <id>

# Knowledge base (shared markdown pages on crosslink/knowledge branch)
crosslink knowledge add|show|list|edit|remove|sync|import|search

# Agent management
crosslink agent init|status|bootstrap
crosslink trust approve|revoke|list|pending|check
crosslink locks list|check|claim|release|steal

# Kickoff (launch agents to implement features)
crosslink kickoff run|status|logs|stop|plan|show-plan|report|list|cleanup

# Swarm (multi-agent coordination)
crosslink swarm init|status|resume|launch|gate|checkpoint|config|estimate|harvest|plan|plan-show

# Container execution
crosslink container build|start|ps|logs|stop|rm|kill|shell|snapshot

# Infrastructure
crosslink daemon start|stop|status
crosslink config show|get|set|list|reset|diff
crosslink sync                             # sync state from remote
crosslink compact                          # run event compaction
crosslink prune                            # prune hub/knowledge history
crosslink integrity counters|hydration|locks|schema

# Organization
crosslink milestone create|list|show|add|remove|close|delete
crosslink archive add|remove|list|older
crosslink export|import

# UI
crosslink tui                              # interactive terminal dashboard
crosslink mc                               # tmux mission control
crosslink serve                            # web dashboard server

# Tooling
crosslink context measure|check
crosslink workflow diff|trail
crosslink style set|sync|diff|show|unset
crosslink cpitd scan|status|clear
crosslink migrate to-shared|from-shared|rename-branch
```

## Workflow

1. `session start` -> see previous handoff
2. `issue quick "what I'm doing" -p medium -l bug` -> create + track
3. Work, add typed comments (`--kind plan`, `--kind decision`, etc.)
4. `session end --notes "..."` -> save context

## Best Practices

- Start sessions when beginning work
- Use `issue ready` to find unblocked issues
- Use subissues for tasks >500 lines
- End with handoff notes before context compresses

---

*Language rules, security requirements, and testing guidelines are in `.crosslink/rules/` and auto-injected based on detected project languages.*
