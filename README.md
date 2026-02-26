# Crosslink

[![Crates.io](https://img.shields.io/crates/v/crosslink-tracker?style=flat-square)](https://crates.io/crates/crosslink-tracker)
[![Downloads](https://img.shields.io/crates/d/crosslink-tracker?style=flat-square)](https://crates.io/crates/crosslink-tracker)
[![License: MIT](https://img.shields.io/crates/l/crosslink-tracker?style=flat-square)](LICENSE)
![AI Generated](https://img.shields.io/badge/Code-AI_Generated-blue?style=flat-square&logo=probot&logoColor=white)

A simple, lean issue tracker CLI designed for AI-assisted development. Track tasks across sessions with context preservation.

![Crosslink: The Missing Memory Layer for AI-Assisted Development](images/infographic.png)

## Features

- **Local-first**: All data stored in SQLite (`.crosslink/issues.db`)
- **Session management**: Preserve context across Claude/AI sessions with handoff notes
- **Context compression resilience**: Breadcrumb tracking via `session action` survives context resets
- **Quick workflow**: `crosslink quick` creates, labels, and starts work in one command
- **Issue templates**: Built-in templates for bugs, features, audits, investigations, and more
- **Subissues**: Break large tasks into smaller, trackable pieces
- **Dependencies**: Track blocking relationships between issues
- **Related issues**: Link related issues together for context
- **Labels & priorities**: Organize issues with labels and priority levels
- **Milestones**: Group issues into milestones/epics for release planning
- **Time tracking**: Start/stop timers to track time spent on issues
- **Smart recommendations**: `crosslink next` suggests what to work on based on priority and progress
- **Tree view**: Visualize issue hierarchy with `crosslink tree`
- **JSON output**: `--json` flag for structured, machine-readable output
- **Quiet mode**: `--quiet` flag for minimal, pipe-friendly output
- **Batch operations**: `close-all` with label/priority filtering
- **Export/Import**: Backup and restore issues in JSON format
- **Issue archiving**: Archive old closed issues to keep the active list clean
- **Claude Code hooks**: Behavioral guardrails that inject best practices into AI sessions
- **Stale session detection**: Auto-ends sessions idle >4 hours on next startup
- **Multi-agent coordination**: Distributed issue locking via a `crosslink/hub` git branch
- **Agent identity**: Machine-local agent registration for lock ownership tracking
- **Lock-aware workflows**: `next`, `session work`, and `create --work` respect lock state
- **Daemon heartbeat**: Background agent heartbeat for stale lock detection
- **GPG signature verification**: Verify integrity of the shared locks branch
- **Customizable rules**: Override default rules via `.crosslink/rules/` markdown files
- **No sync complexity**: No git hooks, no auto-push, just simple local storage

## Installation

```bash
# Install from crates.io
cargo install crosslink-tracker
```

The binary is named `crosslink` and will be available in your PATH after install.

### Build from Source

```bash
git clone https://github.com/forecast-bio/crosslink.git
cd crosslink/crosslink
cargo install --path .
# The binary will be at ~/.cargo/bin
```

```bash
# Or build without installing

cargo build --release
# The binary will be at target/release/crosslink
```

## Quick Start

```bash
# Initialize in any project
crosslink init

# Start a session when you begin work
crosslink session start

# Create issues
crosslink create "Fix login bug" -p high
crosslink create "Add dark mode" -d "Support light/dark theme toggle"

# Quick create + label + start working in one command
crosslink quick "Fix login bug" -p high -l bug

# Use a template for structured issues
crosslink create "Login fails on refresh" --template bug

# Set what you're working on
crosslink session work 1

# Record breadcrumbs before context compression
crosslink session action "Investigating token refresh logic"

# End session with handoff notes
crosslink session end --notes "Fixed auth bug, dark mode is next"
```

## Commands

### Issue Management

| Command | Description |
|---------|-------------|
| `crosslink create <title>` | Create a new issue |
| `crosslink create <title> -p high` | Create with priority (low/medium/high/critical) |
| `crosslink create <title> -d "desc"` | Create with description |
| `crosslink create <title> --template bug` | Create from template (bug/feature/refactor/research/audit/continuation/investigation) |
| `crosslink create <title> --work -l bug` | Create, label, and start working on it |
| `crosslink quick <title> [-p high] [-l label]` | Shorthand: create + label + set as active work item |
| `crosslink subissue <parent_id> <title>` | Create a subissue under a parent |
| `crosslink subissue <parent_id> <title> -p high` | Subissue with priority |
| `crosslink list` | List open issues |
| `crosslink list -s all` | List all issues |
| `crosslink list -s closed` | List closed issues |
| `crosslink list -l bug` | Filter by label |
| `crosslink list -p high` | Filter by priority |
| `crosslink show <id>` | Show issue details |
| `crosslink show <id> --json` | Show issue details as JSON |
| `crosslink update <id> --title "New"` | Update title |
| `crosslink update <id> -d "desc"` | Update description |
| `crosslink update <id> -p critical` | Update priority |
| `crosslink close <id>` | Close an issue |
| `crosslink close <id> --no-changelog` | Close without adding a changelog entry (e.g. internal scaffolding subissues) |
| `crosslink close-all` | Close all open issues |
| `crosslink close-all --no-changelog` | Close all without changelog entries (e.g. internal scaffolding subissues) |
| `crosslink close-all -l bug` | Close all issues with a specific label |
| `crosslink close-all -p low` | Close all issues with a specific priority |
| `crosslink reopen <id>` | Reopen a closed issue |
| `crosslink delete <id>` | Delete an issue (with confirmation) |
| `crosslink delete <id> -f` | Delete without confirmation |

> **Tip:** Add `--quiet` / `-q` to any command for minimal output (just the ID), useful for scripting and piping.

### Comments & Labels

| Command | Description |
|---------|-------------|
| `crosslink comment <id> "text"` | Add a comment to an issue |
| `crosslink label <id> <label>` | Add a label to an issue |
| `crosslink unlabel <id> <label>` | Remove a label from an issue |

### Dependencies

| Command | Description |
|---------|-------------|
| `crosslink block <id> <blocker_id>` | Mark issue as blocked by another |
| `crosslink unblock <id> <blocker_id>` | Remove blocking relationship |
| `crosslink blocked` | List all blocked issues |
| `crosslink ready` | List issues ready to work on (no blockers) |

### Related Issues

| Command | Description |
|---------|-------------|
| `crosslink relate <id1> <id2>` | Link two related issues together |
| `crosslink unrelate <id1> <id2>` | Remove relationship between issues |

### Milestones

| Command | Description |
|---------|-------------|
| `crosslink milestone create <name>` | Create a new milestone |
| `crosslink milestone create <name> -d "desc"` | Create with description |
| `crosslink milestone list` | List all milestones |
| `crosslink milestone show <id>` | Show milestone details and progress |
| `crosslink milestone add <milestone_id> <issue_id>` | Add an issue to a milestone |
| `crosslink milestone remove <milestone_id> <issue_id>` | Remove an issue from a milestone |
| `crosslink milestone close <id>` | Close a milestone |
| `crosslink milestone reopen <id>` | Reopen a closed milestone |

### Archiving

| Command | Description |
|---------|-------------|
| `crosslink archive <id>` | Archive a closed issue |
| `crosslink unarchive <id>` | Restore an archived issue |
| `crosslink archived` | List all archived issues |
| `crosslink archive-older <days>` | Archive issues closed more than N days ago |

### Export/Import

| Command | Description |
|---------|-------------|
| `crosslink export` | Export all issues to JSON (stdout) |
| `crosslink export -o backup.json` | Export to a file |
| `crosslink import backup.json` | Import issues from JSON file |
| `crosslink import backup.json --merge` | Merge with existing issues |

### Smart Navigation

| Command | Description |
|---------|-------------|
| `crosslink next` | Recommend the next issue to work on (by priority/progress) |
| `crosslink tree` | Show all issues in a tree hierarchy |
| `crosslink tree -s open` | Show only open issues in tree view |

### Time Tracking

| Command | Description |
|---------|-------------|
| `crosslink start <id>` | Start a timer for an issue |
| `crosslink stop` | Stop the current timer |
| `crosslink timer` | Show current timer status |

### Session Management

Sessions preserve context across AI assistant restarts. Stale sessions (idle >4 hours) are auto-ended on the next startup.

| Command | Description |
|---------|-------------|
| `crosslink session start` | Start a session, shows previous handoff notes |
| `crosslink session work <id>` | Set the issue you're currently working on |
| `crosslink session action "..."` | Record a breadcrumb (survives context compression) |
| `crosslink session status` | Show current session info and last action |
| `crosslink session end` | End the current session |
| `crosslink session end --notes "..."` | End with handoff notes for next session |
| `crosslink session last-handoff` | Retrieve handoff notes from the previous session |

### Multi-Agent Coordination

Register agents and coordinate work across multiple AI sessions or machines via distributed locking.

| Command | Description |
|---------|-------------|
| `crosslink agent init <id>` | Register this machine as an agent (stored in `.crosslink/agent.json`) |
| `crosslink agent init <id> -d "desc"` | Register with a description |
| `crosslink agent status` | Show agent identity, machine ID, and held locks |
| `crosslink locks list` | Show all active issue locks (with stale detection) |
| `crosslink locks list --json` | Show locks as JSON |
| `crosslink locks check <id>` | Check if an issue is available or locked by another agent |
| `crosslink sync` | Fetch lock state from coordination branch and verify GPG signatures |

Lock state is stored on a `crosslink/hub` branch and synchronized via git. Agent identity files are machine-local and gitignored.

### Daemon (Optional)

The daemon auto-flushes session state every 30 seconds and pushes agent heartbeats every 2.5 minutes for stale lock detection.

| Command | Description |
|---------|-------------|
| `crosslink daemon start` | Start background daemon |
| `crosslink daemon status` | Check if daemon is running |
| `crosslink daemon stop` | Stop the daemon |

## Workflow Example

```bash
$ crosslink session start
Previous session ended: 2026-01-15 09:00
Handoff notes:
  Working on auth bug. Found issue in token refresh.

Session #5 started.

# Quick create + label + start working in one step
$ crosslink quick "Fix token refresh" -p high -l bug
Created issue #3
Now working on: #3 Fix token refresh

# Record breadcrumbs as you work (survives context compression)
$ crosslink session action "Found root cause in refresh_token()"

$ crosslink comment 3 "Fixed the token refresh issue"
Added comment to issue #3

$ crosslink close 3
Closed issue #3

$ crosslink ready
Ready issues (no blockers):
  #2    medium   Add dark mode

$ crosslink session end --notes "Closed auth bug #3. Dark mode is next."
Session #5 ended.
Handoff notes saved.
```

## Storage

All data is stored locally in `.crosslink/issues.db` (SQLite). No external services, no network requests.

## Claude Code Hooks

Crosslink includes behavioral hooks for [Claude Code](https://claude.com/claude-code) that inject best practice reminders into AI sessions. These hooks help ensure Claude follows coding standards without requiring manual prompting.

> **Requirement:** Python 3.6+ must be installed and available in your PATH for hooks to work. The VS Code extension will warn you if Python is not detected.

### What the Hooks Do

The hooks are located in `.claude/hooks/` and configured in `.claude/settings.json`:

| Hook | Trigger | Purpose |
|------|---------|---------|
| `prompt-guard.py` | Every prompt | Injects language-specific best practices (condensed after first prompt) |
| `post-edit-check.py` | After file edits | Debounced linting reminder to verify changes compile |
| `work-check.py` | Before write/edit | Enforces issue tracking (configurable: strict/normal/relaxed), blocks git mutations, warns on lock conflicts in strict mode |
| `session-start.py` | Session start/resume | Loads context, syncs lock state, shows active locks, detects stale sessions, restores breadcrumbs after context compression |

### Behavioral Guardrails

The hooks enforce these principles:

1. **No Stubs**: Implement complete, working code - no placeholder functions or TODO comments
2. **No Dead Code**: Identify incomplete features and complete them, or remove truly dead code
3. **Full Features**: Implement complete features as requested, don't stop partway
4. **Error Handling**: Proper error handling everywhere, no panics on bad input
5. **Security**: Validate input, use parameterized queries, no command injection

### Large Task Management

When code will exceed 500 lines, the hooks guide Claude to:
1. Create a parent issue for the feature
2. Break it into subissues for trackable components
3. Inform the user about the multi-part implementation
4. Work on one subissue at a time

### Language Detection

The hooks auto-detect the project language(s) and inject relevant best practices:
- **Rust**: Use `?` operator, `clippy`, parameterized SQL, avoid `.unwrap()`
- **Python**: Type hints, proper exceptions, `pathlib`, context managers
- **JavaScript/TypeScript**: `const`/`let`, async/await, strict mode, input validation
- **Go**: Check errors, use `context.Context`, `defer` for cleanup
- **Java, C, C++, C#, Ruby, PHP, Swift, Kotlin, Scala, Zig, Odin, Elixir**: Language-specific best practices

### Configuring Hook Behavior

Hook behavior is controlled by `.crosslink/hook-config.json`:

```json
{
  "tracking_mode": "strict",
  "blocked_git_commands": ["git push", "git commit", "..."],
  "allowed_bash_prefixes": ["crosslink ", "git status", "..."]
}
```

#### Tracking Mode

Controls how aggressively crosslink enforces issue creation before code changes:

| Mode | Hook Behavior | Prompt Language | Best For |
|------|---------------|-----------------|----------|
| `strict` | **Blocks** Write/Edit/Bash without an active issue | ALL CAPS MANDATORY, "ABSOLUTE RULE" | Teams that want every change tracked |
| `normal` | **Reminds** but allows proceeding without an issue | Soft "should" language, gentle nudges | Balanced — tracks most work, doesn't block quick fixes |
| `relaxed` | **No enforcement** — only git mutation blocks apply | Minimal — just mentions crosslink is available | Users who want tracking opt-in only |

Each mode loads its instructions from `.crosslink/rules/tracking-{mode}.md`, so you can edit the wording per-mode.

#### Blocked Git Commands

Git mutation commands (push, commit, merge, rebase, etc.) are **permanently blocked in all modes** — these are human-only operations. You can customize the list in `hook-config.json`.

Read-only git commands (status, diff, log, show, branch) are always allowed.

#### Allowed Bash Prefixes

Commands that bypass the issue-required check (read-only and infrastructure commands). Customize in `hook-config.json`.

### Customizable Rules

Rules are stored in `.crosslink/rules/` as markdown files:

| File | Purpose |
|------|---------|
| `global.md` | Security, correctness, and style rules |
| `tracking-strict.md` | Strict mode: forceful issue tracking instructions |
| `tracking-normal.md` | Normal mode: gentle issue tracking instructions |
| `tracking-relaxed.md` | Relaxed mode: minimal tracking reference |
| `project.md` | Project-specific rules (your custom rules go here) |
| `rust.md`, `python.md`, etc. | Language-specific best practices |

To customize:
1. Edit the appropriate file in `.crosslink/rules/`
2. Changes take effect immediately on the next prompt

To reset rules to defaults:
```bash
crosslink init --force
```

### Installing Hooks in Other Projects

Use `crosslink init` in any project to set up hooks and rules:

```bash
cd /your/project
crosslink init
```

Or copy manually:
```bash
cp -r /path/to/crosslink/.claude /your/project/
cp -r /path/to/crosslink/.crosslink/rules /your/project/.crosslink/
```

## Using Crosslink with Any AI Agent

While crosslink includes native hooks for Claude Code, the context provider script allows integration with **any** AI coding assistant.

### The Context Provider

The context provider (`context-provider.py`) generates intelligent context that can be injected into any AI agent's prompts. It:

- Detects project languages and applies relevant coding rules
- Includes current session state and handoff notes
- Lists open/ready issues
- Shows project structure
- Outputs in XML, Markdown, or JSON formats

**Location:** `.crosslink/integrations/context-provider.py`

### Quick Setup (Shell Aliases)

Add these to your `~/.bashrc`, `~/.zshrc`, or PowerShell profile:

**Bash/Zsh:**
```bash
# Generic AI wrapper - prepends crosslink context to any prompt
ai() {
    local ctx=$(python ~/.crosslink/integrations/context-provider.py 2>/dev/null)
    echo -e "$ctx\n\n---\nUser: $*" | your-llm-cli
}

# Aider with crosslink context
aider-cl() {
    python ~/.crosslink/integrations/context-provider.py --format md > /tmp/crosslink-ctx.md
    aider --read /tmp/crosslink-ctx.md "$@"
}

# Copy context to clipboard for pasting into web UIs
crosslink-ctx() {
    python ~/.crosslink/integrations/context-provider.py --clipboard
}
```

**PowerShell:**
```powershell
# Generic AI wrapper
function ai {
    $ctx = python "$env:USERPROFILE\.crosslink\integrations\context-provider.py" 2>$null
    "$ctx`n`n---`nUser: $($args -join ' ')" | your-llm-cli
}

# Copy context to clipboard
function crosslink-ctx {
    python "$env:USERPROFILE\.crosslink\integrations\context-provider.py" | Set-Clipboard
}
```

### Context Provider Usage

```bash
# Full context (session + issues + rules + structure)
python context-provider.py

# Specific sections only
python context-provider.py --session      # Session state only
python context-provider.py --issues       # Issues only
python context-provider.py --rules        # Coding rules only
python context-provider.py --structure    # Project tree only

# Output formats
python context-provider.py --format xml   # XML tags (default, best for LLMs)
python context-provider.py --format md    # Markdown (human readable)
python context-provider.py --format json  # JSON (programmatic access)

# Integration helpers
python context-provider.py --prepend "fix the auth bug"  # Wrap a prompt
python context-provider.py --clipboard                    # Copy to clipboard
python context-provider.py --env                          # Output as env vars
```

### Agent-Specific Integration

**Cursor:**
```bash
# Generate .cursorrules with crosslink context (run once per session)
python context-provider.py --format md --rules > .cursorrules
```

**Aider:**
```bash
# Method 1: Read file
python context-provider.py --format md > context.md
aider --read context.md

# Method 2: Wrapper script (see aliases above)
aider-cl --model gpt-4
```

**Continue.dev (VS Code):**
Add to `.continue/config.json`:
```json
{
  "contextProviders": [
    {
      "name": "exec",
      "params": {
        "command": "python .crosslink/integrations/context-provider.py --format md"
      }
    }
  ]
}
```

**OpenAI API / Anthropic API direct calls:**
```bash
# Prepend context to your prompt
CONTEXT=$(python context-provider.py)
curl https://api.openai.com/v1/chat/completions \
  -H "Authorization: Bearer $OPENAI_API_KEY" \
  -d "{
    \"model\": \"gpt-4\",
    \"messages\": [{\"role\": \"system\", \"content\": \"$CONTEXT\"},
                   {\"role\": \"user\", \"content\": \"$PROMPT\"}]
  }"
```

**Web UIs (ChatGPT, Claude.ai, etc.):**
```bash
# Copy to clipboard, then paste as first message
python context-provider.py --clipboard
```

### What Gets Injected

The context provider outputs:

```xml
<crosslink-session>
Session #5 active
Working on: #12 Fix authentication bug
Handoff notes: Found issue in token refresh logic
</crosslink-session>

<crosslink-issues>
Ready issues (unblocked):
  #12   high     Fix authentication bug
  #15   medium   Add dark mode toggle
Open issues:
  #18   low      Update documentation
</crosslink-issues>

<coding-rules>
### General Requirements
1. **NO STUBS**: Never write placeholder comments, empty bodies...
2. **ERROR HANDLING**: Proper error handling everywhere...

### Rust Best Practices
- Use `?` operator over `.unwrap()`
- Use `anyhow::Result` for application errors
...
</coding-rules>

<workflow-reminder>
- Use `crosslink session start` at the beginning of work
- Use `crosslink session work <id>` to mark current focus
- Add comments: `crosslink comment <id> "..."`
- End with notes: `crosslink session end --notes "..."`
</workflow-reminder>
```

### Per-Turn vs Static Context

| Approach | Freshness | Setup |
|----------|-----------|-------|
| Shell alias wrapper | Per-turn (fresh every time) | Add alias to shell config |
| `.cursorrules` | Static (regenerate manually) | Run script, paste output |
| Claude Code hooks | Per-turn (automatic) | Built-in, no setup needed |

For the best experience with non-Claude agents, use the shell alias approach to get fresh context on every prompt.

## VS Code Extension

Crosslink is also available as a VS Code extension that bundles the CLI binary:

**Install:** Search "Crosslink Issue Tracker" in VS Code Extensions, or visit the [VS Code Marketplace](https://marketplace.visualstudio.com/items?itemName=forecast-bio.crosslink-issue-tracker).

**Features:**
- Commands accessible from Command Palette (Ctrl+Shift+P)
- Status bar indicator for daemon status
- Auto-starts daemon when `.crosslink` project detected
- Works on Windows, Linux, and macOS

## Development

```bash
# Run tests
cargo test

# Run with clippy
cargo clippy -- -D warnings

# Format code
cargo fmt
```

## License

MIT
