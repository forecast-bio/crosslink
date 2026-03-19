You are helping the user understand and use crosslink, an issue tracker designed for AI-assisted development. Read the project's CLAUDE.md for the full command reference, then answer the user's question using that context.

## What is Crosslink?

Crosslink is a local-first issue tracker that stores data in SQLite (`.crosslink/issues.db`) and syncs state via a git coordination branch (`crosslink/hub`). It's designed for AI agents working in Claude Code, providing:

- **Issue tracking** that persists across AI sessions
- **Session handoff** so new conversations pick up where the last left off
- **Typed comments** for audit trails (plan, decision, observation, blocker, resolution, result)
- **Multi-agent coordination** via locks, swarms, and kickoff agents
- **Knowledge base** for shared documentation across sessions

## Core Workflow

Every work session follows this pattern:

```bash
# 1. Start session (see what the last session handed off)
crosslink session start

# 2. Create or pick an issue to work on
crosslink quick "Fix login bug" -p high -l bug     # create + track in one step
# OR
crosslink issue list -s open                        # see existing issues
crosslink session work <id>                         # pick one

# 3. Document as you work
crosslink issue comment <id> "Approach: ..." --kind plan
crosslink issue comment <id> "Chose X over Y because ..." --kind decision

# 4. End session with handoff notes
crosslink session end --notes "Fixed the bug, PR ready for review"
```

## Common Tasks

### Creating issues
```bash
crosslink issue create "Title" -p medium              # basic create
crosslink quick "Title" -p high -l bug                 # create + label + start working
crosslink subissue <parent-id> "Child title"           # create under a parent
```

### Querying issues
```bash
crosslink issue list                                   # open issues (default)
crosslink issue list -s all                            # all issues including closed
crosslink issue list -l bug -p high                    # filter by label and priority
crosslink issue search "keyword"                       # full-text search
crosslink issue show <id>                              # full details
crosslink issue tree                                   # hierarchy view
crosslink issue next                                   # suggest what to work on
```

### Issue lifecycle
```bash
crosslink issue close <id>                             # close when done
crosslink issue reopen <id>                            # reopen if needed
crosslink issue delete <id> --force                    # permanent delete
```

### Comments (typed for audit trails)
```bash
crosslink issue comment <id> "text" --kind plan        # what you intend to do
crosslink issue comment <id> "text" --kind decision    # why you chose this approach
crosslink issue comment <id> "text" --kind observation # something you discovered
crosslink issue comment <id> "text" --kind blocker     # what's blocking progress
crosslink issue comment <id> "text" --kind resolution  # how a blocker was resolved
crosslink issue comment <id> "text" --kind result      # what was delivered
```

### Labels and dependencies
```bash
crosslink issue label <id> bug                         # add a label
crosslink issue block <id> <blocker-id>                # mark dependency
crosslink issue blocked                                # show blocked issues
crosslink issue ready                                  # show unblocked issues
```

### Sessions
```bash
crosslink session start                                # begin work, see last handoff
crosslink session work <id>                            # set current focus
crosslink session status                               # check what you're working on
crosslink session action "did X"                       # breadcrumb before compression
crosslink session end --notes "context for next session"
```

### Multi-agent (kickoff)
```bash
crosslink kickoff run <issue-id>                       # launch agent in worktree
crosslink kickoff status                               # check running agents
crosslink kickoff logs <id>                            # view agent output
crosslink kickoff stop <id>                            # stop an agent
crosslink kickoff list                                 # list all worktrees
```

## Issue ID Formats

- `#42` — hub-synced issue with positive display ID
- `L3` — local-only issue (not yet pushed to remote)
- Both formats work in all commands: `crosslink show 42`, `crosslink show L3`

## Priorities

`critical` > `high` > `medium` > `low`

## Global Flags

- `--quiet` / `-q` — minimal output (for scripting)
- `--json` — machine-readable JSON output

## Troubleshooting

```bash
crosslink sync                                         # re-sync from remote
crosslink integrity counters --repair                  # fix counter drift
crosslink integrity hydration --repair                 # re-hydrate SQLite from JSON
crosslink compact                                      # compact event logs
```

## Now Answer the User's Question

Read the project's CLAUDE.md file for any project-specific crosslink configuration, then answer the user's question about crosslink usage.
