---
description: Answer questions about crosslink usage and commands
argument-hint: [question about crosslink]
---

You are helping the user understand and use crosslink, an issue tracker designed for AI-assisted development. Read the project's CLAUDE.md for the full command reference, then answer the user's question using that context.

## What is Crosslink?

Crosslink is a local-first issue tracker that stores data in SQLite (`.crosslink/issues.db`) and syncs state via a git coordination branch (`crosslink/hub`). It's designed for AI agents working in Claude Code, providing:

- **Issue tracking** that persists across AI sessions
- **Session handoff** so new conversations pick up where the last left off
- **Typed comments** for audit trails (plan, decision, observation, blocker, resolution, result)
- **Multi-agent coordination** via locks, swarms, and kickoff agents
- **Knowledge base** for shared documentation across sessions
- **Signing and trust** for verifying agent identity and commit provenance
- **Design document authoring** with codebase-grounded iteration
- **Container execution** for isolated agent sandboxing

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

## Issue Management

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

### Updating and lifecycle
```bash
crosslink issue update <id> -t "new title" -p high     # update title/priority
crosslink issue close <id>                             # close when done
crosslink issue close-all                              # close all matching filters
crosslink issue reopen <id>                            # reopen if needed
crosslink issue delete <id> --force                    # permanent delete
crosslink issue tested <id>                            # mark tests as run (resets reminder)
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

### Interventions
```bash
crosslink issue intervene <id> "description" --trigger <type> --context "what you were doing"
```

### Labels, relations, and blocking
```bash
crosslink issue label <id> bug                         # add a label
crosslink issue unlabel <id> bug                       # remove a label
crosslink issue block <id> <blocker-id>                # mark dependency
crosslink issue unblock <id> <blocker-id>              # remove blocking relationship
crosslink issue blocked                                # show blocked issues
crosslink issue ready                                  # show unblocked issues
crosslink issue relate <id1> <id2>                     # link related issues
crosslink issue unrelate <id1> <id2>                   # remove a relation
crosslink issue related <id>                           # list related issues
```

## Sessions

```bash
crosslink session start                                # begin work, see last handoff
crosslink session work <id>                            # set current focus
crosslink session status                               # check what you're working on
crosslink session last-handoff                         # show previous session's handoff
crosslink session action "did X"                       # breadcrumb before compression
crosslink session end --notes "context for next session"
```

## Time Tracking

```bash
crosslink timer start <id>                             # start timing work on an issue
crosslink timer stop                                   # stop the current timer
crosslink timer show                                   # show current timer status
```

## Knowledge Base

Shared markdown pages stored on the `crosslink/knowledge` branch:

```bash
crosslink knowledge add "page-slug" --body "content"   # create a page
crosslink knowledge show <slug>                        # display a page
crosslink knowledge list                               # list all pages
crosslink knowledge edit <slug> --body "new content"   # update a page
crosslink knowledge remove <slug>                      # remove a page
crosslink knowledge search "query"                     # search page content
crosslink knowledge sync                               # pull from remote
crosslink knowledge import <path>                      # bulk import markdown files
```

## Kickoff (Agent Launcher)

Launch agents in worktrees or containers to implement features:

```bash
crosslink kickoff run <issue-id>                       # launch agent in worktree
crosslink kickoff launch                               # interactive pipeline wizard
crosslink kickoff status                               # check running agents
crosslink kickoff logs <id>                            # view agent output
crosslink kickoff stop <id>                            # stop an agent
crosslink kickoff list                                 # list all agents across worktrees/tmux/docker
crosslink kickoff cleanup                              # remove completed/stale agents
crosslink kickoff graph                                # show branch topology
```

Design-driven workflow:
```bash
crosslink kickoff plan <design-doc>                    # analyze design doc against codebase
crosslink kickoff show-plan <slug>                     # display a gap report
crosslink kickoff report <id>                          # spec validation report from completed agent
```

## Swarm (Multi-Agent Coordination)

Coordinate multiple agents across phases:

```bash
# Lifecycle
crosslink swarm init <design-doc>                      # initialize from design document
crosslink swarm status                                 # agents, phases, progress, next steps
crosslink swarm resume                                 # reconstruct state and show next steps
crosslink swarm list                                   # list active and archived swarms
crosslink swarm archive                                # archive current swarm
crosslink swarm reset                                  # reset active swarm

# Phase management
crosslink swarm launch                                 # launch all planned agents for a phase
crosslink swarm gate                                   # run test suite as a phase gate
crosslink swarm checkpoint                             # record checkpoint after phase completes
crosslink swarm merge                                  # merge completed agent worktrees into one branch

# Planning and budgeting
crosslink swarm plan                                   # plan multi-phase build across budget windows
crosslink swarm plan-show                              # show current window plan
crosslink swarm config                                 # set budget parameters (window, model)
crosslink swarm estimate                               # estimate wall-clock cost for a phase
crosslink swarm harvest                                # scan completed agents, update cost history

# Review pipeline
crosslink swarm review                                 # launch parallel review agents
crosslink swarm fix                                    # launch parallel fix agents per issue
crosslink swarm pipeline                               # run full review->fix pipeline
crosslink swarm review-status                          # show pipeline status
crosslink swarm review-continue                        # continue paused pipeline

# Plan editing
crosslink swarm adopt                                  # associate external agent with a swarm slot
crosslink swarm move                                   # move agent to different phase
crosslink swarm merge-phases                           # merge two phases into one
crosslink swarm split-phase                            # split a phase after a specific agent
crosslink swarm remove-agent                           # remove agent from plan
crosslink swarm reorder                                # reorder a phase
crosslink swarm rename-phase                           # rename a phase

# Trust
crosslink swarm trust-init                             # initialize trust model config (swarm.toml)
```

## Container Execution

Run agents in isolated Docker containers:

```bash
crosslink container build                              # build agent container image
crosslink container start <worktree>                   # start container for a worktree
crosslink container ps                                 # list running containers
crosslink container logs <id>                          # stream container logs
crosslink container stop <id>                          # stop a container
crosslink container rm <id>                            # remove stopped container
crosslink container kill <id>                          # stop + remove
crosslink container shell <id>                         # open shell inside container
crosslink container snapshot <id>                      # save as cached image
```

## Agent Identity and Trust

```bash
# Agent management
crosslink agent init                                   # initialize agent identity
crosslink agent status                                 # show current identity
crosslink agent prompt <session> "message"             # send prompt to tmux agent
crosslink agent bootstrap                              # bootstrap identity in new repo

# Trust (approve/revoke signing keys)
crosslink trust approve <fingerprint>                  # approve an agent key
crosslink trust revoke <fingerprint>                   # revoke an agent key
crosslink trust list                                   # list trusted signers
crosslink trust pending                                # show keys awaiting approval
crosslink trust check <agent>                          # check trust status
```

## Locks

```bash
crosslink locks list                                   # list active locks
crosslink locks check <id>                             # check if issue is locked
crosslink locks claim <id>                             # claim a lock
crosslink locks release <id>                           # release a lock
crosslink locks steal <id>                             # steal a stale lock
```

## Design Documents

```bash
crosslink design "feature description"                 # start design session
crosslink design --issue <id>                          # pull context from crosslink issue
crosslink design --gh-issue <num>                      # pull context from GitHub issue
crosslink design --continue <slug>                     # resume iteration on existing draft
```

## Milestones

```bash
crosslink milestone create "v1.0"                      # create a milestone
crosslink milestone list                               # list milestones
crosslink milestone show <id>                          # show details
crosslink milestone add <milestone-id> <issue-id>      # add issue to milestone
crosslink milestone remove <milestone-id> <issue-id>   # remove issue
crosslink milestone close <id>                         # close a milestone
crosslink milestone delete <id>                        # delete a milestone
```

## Archive

```bash
crosslink archive add <id>                             # archive a closed issue
crosslink archive remove <id>                          # unarchive (restore to closed)
crosslink archive list                                 # list archived issues
crosslink archive older <days>                         # archive all closed > N days ago
```

## Configuration

```bash
crosslink config                                       # interactive preset walkthrough
crosslink config --preset team                         # apply team preset directly
crosslink config --preset solo                         # apply solo preset directly
crosslink config show                                  # show all config with defaults
crosslink config get <key>                             # get a specific value
crosslink config set <key> <value>                     # set a value
crosslink config list                                  # list all keys with descriptions
crosslink config reset                                 # reset to defaults
crosslink config diff                                  # show differences from defaults
```

## Workflow and Diagnostics

```bash
crosslink workflow diff                                # compare policy files vs embedded defaults
crosslink workflow trail <id>                          # chronological comment trail for an issue
crosslink context measure                              # measure context injection token overhead
crosslink context check                                # verify crosslink files are deployed
crosslink cpitd scan                                   # scan for code clones, create issues
crosslink cpitd status                                 # show open clone issues
crosslink cpitd clear                                  # close all clone issues
```

## Style Syncing

```bash
crosslink style set <repo-url>                         # set house style source
crosslink style sync                                   # pull latest from house style
crosslink style diff                                   # show drift from house style
crosslink style show                                   # show current config
crosslink style unset                                  # remove house style association
```

## Infrastructure

```bash
crosslink init                                         # initialize crosslink in a repo
crosslink sync                                         # sync state from remote
crosslink compact                                      # run event compaction
crosslink prune                                        # prune hub/knowledge history
crosslink export                                       # export issues to JSON/markdown
crosslink import <file>                                # import issues from JSON
crosslink daemon start|stop|status                     # manage background daemon
crosslink migrate to-shared|from-shared|rename-branch  # schema migrations
```

## Integrity and Troubleshooting

```bash
crosslink integrity counters --repair                  # fix counter drift
crosslink integrity hydration --repair                 # re-hydrate SQLite from JSON
crosslink integrity locks                              # check for stale/orphaned locks
crosslink integrity schema                             # verify SQLite schema version
crosslink integrity layout                             # detect mixed V1/V2 hub layout
crosslink integrity sign-backfill                      # sign unsigned entries with human key
```

## UI

```bash
crosslink tui                                          # interactive terminal dashboard
crosslink mc                                           # tmux mission control
crosslink serve                                        # web dashboard server
```

## Issue ID Formats

- `#42` -- hub-synced issue with positive display ID
- `L3` -- local-only issue (not yet pushed to remote)
- Both formats work in all commands: `crosslink show 42`, `crosslink show L3`

## Priorities

`critical` > `high` > `medium` > `low`

## Global Flags

- `--quiet` / `-q` -- minimal output (for scripting)
- `--json` -- machine-readable JSON output
- `--log-level <level>` -- diagnostic output level (error, warn, info, debug, trace)
- `--log-format <format>` -- log format (text, json)

## Now Answer the User's Question

Read the project's CLAUDE.md file for any project-specific crosslink configuration, then answer the user's question about crosslink usage.
