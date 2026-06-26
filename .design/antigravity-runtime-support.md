# Design: Antigravity CLI Runtime Support

**Issue:** L4  
**Status:** Ready for implementation  
**Scope:** `crosslink init`, `crosslink kickoff`, `crosslink workflow`, resource files  
**Goal:** Support Google Antigravity CLI (`agy`) alongside Claude Code (`claude`), so users can switch between Gemini and Claude agents on the same repo without losing crosslink's session tracking, behavioral enforcement, and MCP integrations.

---

## 1. Background and Constraints

Crosslink's Claude Code integration has five layers. This design adds parallel Antigravity equivalents for each:

| Layer | Claude Code | Antigravity CLI | Notes |
|---|---|---|---|
| Project instructions | `CLAUDE.md` | `AGENTS.md` | Prepended to every prompt |
| Config dir | `.claude/` | `.agents/` | Analogous directory tree |
| Hooks (behavioral enforcement) | `.claude/hooks/*.py` via `settings.json` | `.agents/hooks/*.py` via `hooks.json` | Same Python scripts, different invocation config |
| Slash commands | `.claude/commands/*.md` | `.agents/skills/*.md` | Same content, different path |
| MCP servers | `mcpServers` in `.claude/settings.json` | `mcp_config.json` at project root | Same Python servers, different config file |
| Non-interactive launch | `claude --print "$(cat file)"` | `agy -p "$(cat file)"` | Kickoff uses this |
| Permission skip | `--dangerously-skip-permissions` | `--dangerously-skip-permissions` | Identical flag |
| Config dir isolation | `CLAUDE_CONFIG_DIR` env var | Unknown — `--add-dir`? | Open question for kickoff phase |

**Compatibility constraint:** All changes must be strictly additive. Existing `.claude/` behavior is not modified. Users who don't pass `--runtime antigravity` or `--runtime both` see no change.

---

## 2. New CLI Surface

### `crosslink init`

Add a `--runtime` flag:

```
crosslink init [--runtime <claude|antigravity|both>]
```

- **`--runtime claude`** (default, current behavior): write `.claude/` only  
- **`--runtime antigravity`**: write `.agents/` only  
- **`--runtime both`**: write both `.claude/` and `.agents/`

Auto-detection (future): if `agy` is on PATH but `claude` is not, default to `antigravity`. Not in scope for this PR — default stays `claude` for safety.

`--runtime` is stored in `.crosslink/hook-config.json` under a new `"agent_runtime"` key so `--update` and `--force` re-runs respect the original choice without requiring the flag again.

### `crosslink kickoff run`

Add a `--runtime` flag (Phase 2, after init ships):

```
crosslink kickoff run [--runtime <claude|antigravity>] ...
```

Kickoff currently only supports one runtime per launch. `both` is not valid for kickoff.

---

## 3. New Resource Files

### Directory layout

```
resources/
  claude/               (existing — unchanged)
    hooks/*.py
    mcp/*.py
    commands/*.md
    settings.json
  antigravity/          (new)
    hooks.json          # JSON hook config (invokes same .py scripts)
    mcp_config.json     # MCP server declarations
    skills/             # Same content as commands/, different path
      audit.md
      check.md
      commit.md
      ... (mirrored from commands/)
```

The Python hook scripts (`*.py`) are **shared** — not duplicated. Antigravity's `hooks.json` invokes them from `.agents/hooks/`, where they are copied (same content as `.claude/hooks/`). Skills are identical content to commands; only path differs.

### `resources/antigravity/hooks.json`

**Schema confirmed** by examining a live Antigravity CLI wizard-generated hook file. The format is nearly identical to Claude Code's `settings.json` hooks section — same `matcher`, `type: "command"`, `timeout` fields — with two structural differences:

1. **Named top-level objects** with an `enabled` flag. Multiple hook sets coexist as sibling keys. Crosslink uses `"crosslink"` as its key, so merging is trivial: just add/replace that key, preserving all others.
2. **Event names differ** from Claude Code:

| Claude Code event | Antigravity event | Notes |
|---|---|---|
| `UserPromptSubmit` | `PreInvocation` | Fires before each model invocation |
| `SessionStart` | `PreInvocation` | No dedicated session-start event; Python script must be idempotent (check if session already active) |
| `PreToolUse` | `PreToolUse` | Identical, same `matcher` syntax |
| `PostToolUse` | `PostToolUse` | Identical, same `matcher` syntax |
| `Stop` | `Stop` | Identical |
| _(none)_ | `PostInvocation` | Fires after each model response; no current crosslink use |

**Confirmed `hooks.json` format** (from live `~/.gemini/antigravity-cli/hooks.json`):
```json
{
  "hooksetname": {
    "enabled": true|false,
    "PreInvocation": null | [ { "matcher": "...", "hooks": [{ "type": "command", "command": "...", "timeout": N }] } ],
    "PostInvocation": null | [...],
    "PreToolUse":     null | [...],
    "PostToolUse":    null | [...],
    "Stop":           null | [...]
  }
}
```

**Crosslink's `resources/antigravity/hooks.json`** (the actual resource file to ship):

```json
{
  "crosslink": {
    "enabled": true,
    "PreInvocation": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "HOOK=\"$(git rev-parse --show-toplevel 2>/dev/null)/.agents/hooks/prompt-guard.py\"; if [ -f \"$HOOK\" ]; then __PYTHON_PREFIX__ \"$HOOK\"; else exit 0; fi",
            "timeout": 5
          }
        ]
      },
      {
        "hooks": [
          {
            "type": "command",
            "command": "HOOK=\"$(git rev-parse --show-toplevel 2>/dev/null)/.agents/hooks/session-start.py\"; if [ -f \"$HOOK\" ]; then __PYTHON_PREFIX__ \"$HOOK\"; else exit 0; fi",
            "timeout": 10
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "WebFetch|WebSearch",
        "hooks": [
          {
            "type": "command",
            "command": "HOOK=\"$(git rev-parse --show-toplevel 2>/dev/null)/.agents/hooks/pre-web-check.py\"; if [ -f \"$HOOK\" ]; then __PYTHON_PREFIX__ \"$HOOK\"; else exit 0; fi",
            "timeout": 5
          }
        ]
      },
      {
        "matcher": "Write|Edit|Bash",
        "hooks": [
          {
            "type": "command",
            "command": "HOOK=\"$(git rev-parse --show-toplevel 2>/dev/null)/.agents/hooks/work-check.py\"; if [ -f \"$HOOK\" ]; then __PYTHON_PREFIX__ \"$HOOK\"; else exit 0; fi",
            "timeout": 3
          }
        ]
      }
    ],
    "PostToolUse": [
      {
        "matcher": "Write|Edit",
        "hooks": [
          {
            "type": "command",
            "command": "HOOK=\"$(git rev-parse --show-toplevel 2>/dev/null)/.agents/hooks/post-edit-check.py\"; if [ -f \"$HOOK\" ]; then __PYTHON_PREFIX__ \"$HOOK\"; else exit 0; fi",
            "timeout": 5
          }
        ]
      },
      {
        "hooks": [
          {
            "type": "command",
            "command": "HOOK=\"$(git rev-parse --show-toplevel 2>/dev/null)/.agents/hooks/heartbeat.py\"; if [ -f \"$HOOK\" ]; then __PYTHON_PREFIX__ \"$HOOK\"; else exit 0; fi",
            "timeout": 3
          }
        ]
      }
    ],
    "PostInvocation": null,
    "Stop": null
  }
}
```

**Note on `session-start.py` via `PreInvocation`:** Claude Code fires `SessionStart` exactly once per session. Antigravity has no dedicated session-start event, so `session-start.py` runs via `PreInvocation` (before every model turn). The script must be idempotent — check crosslink session state before doing work, skip if already active. This is a **required change to `session-start.py`** (see Section 5a below). The Python script already calls `crosslink session start` which should be a no-op if a session is running; confirm this before shipping.

**Merge strategy for `write_hooks_json_merged()`:** Read existing `.agents/hooks.json` if present, parse as JSON object, upsert the `"crosslink"` key, write back. All other top-level keys are preserved untouched. Parallel to `write_mcp_json_merged()`.

`__PYTHON_PREFIX__` is the same placeholder substitution already used for `settings.json`.

### `resources/antigravity/mcp_config.json`

```json
{
  "mcpServers": {
    "crosslink-safe-fetch": {
      "command": "__PYTHON_PREFIX__",
      "args": [".agents/mcp/safe-fetch-server.py"],
      "type": "stdio"
    },
    "crosslink-knowledge": {
      "command": "__PYTHON_PREFIX__",
      "args": [".agents/mcp/knowledge-server.py"],
      "type": "stdio"
    },
    "crosslink-agent-prompt": {
      "command": "__PYTHON_PREFIX__",
      "args": [".agents/mcp/agent-prompt-server.py"],
      "type": "stdio"
    }
  }
}
```

**Merge strategy:** If `mcp_config.json` already exists (user may have MCP servers configured), merge the `mcpServers` object — add crosslink's keys, preserve user keys, skip crosslink keys that already exist (same pattern as the existing `write_mcp_json_merged`). Use section markers in a `_crosslink_managed` metadata key or a parallel approach.

### `resources/antigravity/skills/`

Symlinked or copied from `resources/claude/commands/` at build time via `build.rs`. Same `.md` content, different destination path (`.agents/skills/` instead of `.claude/commands/`). Build.rs already auto-discovers command files; extend it to also emit `SKILL_FILES` for the antigravity resources.

---

## 4. `AGENTS.md` Handling

Antigravity prepends `AGENTS.md` to every agent prompt (equivalent to `CLAUDE.md`). Crosslink should write a managed section — same marker pattern as `.gitignore`:

```markdown
<!-- crosslink-managed-start -->
This repository uses [crosslink](https://github.com/forecast-bio/crosslink) for AI agent coordination.

**Required workflow:**
- Start every session: `crosslink session start`
- Track work: `crosslink session work <id>`
- End sessions: `crosslink session end --notes "..."`
- Create issues before writing code: `crosslink quick "title" -p <priority>`

Rules and behavioral guards are injected via `.agents/hooks/`. Do not bypass them.
<!-- crosslink-managed-end -->
```

- If `AGENTS.md` doesn't exist: create it with just the managed block  
- If it exists without markers: append the block  
- If it exists with markers: replace the block in-place (idempotent)  
- On `--force`: replace block  

`AGENTS.md` is **tracked in git** (same as `CLAUDE.md`). Add it to the managed files manifest.

---

## 5. `init/mod.rs` Changes

### New type

```rust
/// Which agent runtime(s) to configure during `crosslink init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntime {
    Claude,
    Antigravity,
    Both,
}

impl AgentRuntime {
    pub fn wants_claude(self) -> bool {
        matches!(self, Self::Claude | Self::Both)
    }
    pub fn wants_antigravity(self) -> bool {
        matches!(self, Self::Antigravity | Self::Both)
    }
}

impl std::str::FromStr for AgentRuntime {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "claude" => Ok(Self::Claude),
            "antigravity" | "agy" => Ok(Self::Antigravity),
            "both" => Ok(Self::Both),
            _ => anyhow::bail!("Unknown runtime '{}'. Valid: claude, antigravity, both", s),
        }
    }
}
```

### `InitOpts` extension

```rust
pub struct InitOpts<'a> {
    // ... existing fields ...
    pub runtime: AgentRuntime,  // new, default Claude
}
```

### New embedded resources

```rust
// Antigravity resources
const AGY_HOOKS_JSON: &str = include_str!("../../../resources/antigravity/hooks.json");
const AGY_MCP_CONFIG_JSON: &str = include_str!("../../../resources/antigravity/mcp_config.json");
// Skills auto-discovered by build.rs → SKILL_FILES const
include!(concat!(env!("OUT_DIR"), "/skills_gen.rs"));
```

### `managed_files()` extension

```rust
fn managed_files(python_prefix: &str, runtime: AgentRuntime) -> Vec<(String, String)> {
    let mut files = Vec::new();

    if runtime.wants_claude() {
        // ... existing .claude/ entries (unchanged) ...
    }

    if runtime.wants_antigravity() {
        let hooks_template = AGY_HOOKS_JSON.replace(PYTHON_PREFIX_PLACEHOLDER, python_prefix);
        let mcp_template = AGY_MCP_CONFIG_JSON.replace(PYTHON_PREFIX_PLACEHOLDER, python_prefix);

        // Hook scripts (same Python files, different destination)
        files.push((".agents/hooks/prompt-guard.py".into(), PROMPT_GUARD_PY.into()));
        files.push((".agents/hooks/post-edit-check.py".into(), POST_EDIT_CHECK_PY.into()));
        files.push((".agents/hooks/session-start.py".into(), SESSION_START_PY.into()));
        files.push((".agents/hooks/pre-web-check.py".into(), PRE_WEB_CHECK_PY.into()));
        files.push((".agents/hooks/work-check.py".into(), WORK_CHECK_PY.into()));
        files.push((".agents/hooks/crosslink_config.py".into(), CROSSLINK_CONFIG_PY.into()));

        // Hook config
        files.push((".agents/hooks.json".into(), hooks_template));

        // MCP servers (same Python files, different destination)
        files.push((".agents/mcp/safe-fetch-server.py".into(), SAFE_FETCH_SERVER_PY.into()));
        files.push((".agents/mcp/knowledge-server.py".into(), KNOWLEDGE_SERVER_PY.into()));
        files.push((".agents/mcp/agent-prompt-server.py".into(), AGENT_PROMPT_SERVER_PY.into()));

        // Skills (same content as commands)
        for (filename, content) in SKILL_FILES {
            files.push((format!(".agents/skills/{filename}"), content.to_string()));
        }

        // MCP config
        files.push(("mcp_config.json".into(), mcp_template));
    }

    files
}
```

### `run()` extension (main init flow)

After the existing Claude Code block (`if !claude_exists || force { ... }`), add:

```rust
if runtime.wants_antigravity() && (!agents_dir_exists || force) {
    ui.step_start("Setting up Antigravity CLI hooks");
    // create .agents/hooks/, .agents/mcp/, .agents/skills/
    // write all AGY files
    // merge mcp_config.json
    // write/update AGENTS.md
    ui.step_ok(None);
}
```

### Gitignore additions

Extend `GITIGNORE_MANAGED_SECTION` in `merge.rs`:

```
# .agents/ — auto-generated by crosslink init (not project source)
.agents/hooks/
.agents/skills/
.agents/mcp/

# .agents/ — DO track (if manually configured):
#   mcp_config.json     — Antigravity MCP server config
#   AGENTS.md           — project instructions for Antigravity

# AGENTS.md — auto-generated by crosslink init
# (tracked in git — add to your own gitignore if you want to keep it local)
```

---

## 5a. `session-start.py` via `PreInvocation` — No Change Needed

Antigravity has no `SessionStart` event — `session-start.py` runs via `PreInvocation` (before every model turn). This requires the script to be idempotent.

**Confirmed:** `crosslink session start` when a session is already active prints `"Session #N is already active (started ...)"` and exits cleanly. The hook is safe as-is. No code change required.

---

## 6. `workflow.rs` Changes

Extend drift detection to cover `.agents/` alongside `.claude/`. The `diff()` function currently compares `.claude/hooks/` and `.claude/commands/` against crosslink's templates. Add parallel comparison for `.agents/hooks/` and `.agents/skills/`.

The runtime detection for workflow: check if `.agents/` exists to determine whether to show Antigravity drift. No flag needed — auto-detect from filesystem.

---

## 7. `style.rs` Changes

Add Antigravity path mappings alongside the existing Claude Code ones:

```rust
// existing:
("hooks", "hooks", ".claude/hooks"),
("commands", "commands", ".claude/commands"),
// new:
("agy-hooks", "hooks", ".agents/hooks"),
("skills", "skills", ".agents/skills"),
```

---

## 8. `kickoff/launch.rs` Changes (Phase 2)

**Defer to a follow-up PR.** The init changes are self-contained and immediately useful (users can run Antigravity sessions manually). Kickoff blocks on resolving the `CLAUDE_CONFIG_DIR` isolation question.

When implemented:

```rust
fn build_agy_command(
    timeout_cmd: &str,
    timeout_secs: u64,
    model: &str,
    kickoff_file: &str,
    sandbox_command: Option<&str>,
    worktree_dir: &Path,
    skip_permissions: bool,
) -> String {
    let skip_flag = if skip_permissions { " --dangerously-skip-permissions" } else { "" };
    // agy uses -m for model, -p for non-interactive prompt.
    // No CLAUDE_CONFIG_DIR equivalent needed: agy reads workspace config from
    // .agents/ relative to cwd, and each worktree has its own .agents/ checkout.
    // The tmux session is launched with cwd=worktree_dir, so isolation is automatic.
    let escaped_model = shell_escape_arg(model);
    let escaped_kickoff = shell_escape_arg(kickoff_file);
    let agy_cmd = format!(
        "env -u ANTIGRAVITY agy{skip_flag} -m {escaped_model} -p \"$(cat {escaped_kickoff})\""
    );
    sandbox_command.map_or_else(
        || format!("{timeout_cmd} {timeout_secs}s {agy_cmd}"),
        |cmd| {
            let escaped_worktree = shell_escape_arg(&worktree_dir.to_string_lossy());
            let expanded = cmd.replace("{{worktree}}", &escaped_worktree);
            format!("{timeout_cmd} {timeout_secs}s {expanded} {agy_cmd}")
        },
    )
}
```

Note: **simpler than `build_agent_command`** — no `CLAUDE_CONFIG_DIR` prefix needed since worktree isolation is inherent in `.agents/` being part of the working tree.

The `preflight_check` function gains an `agy` binary check when `runtime == Antigravity`.

---

## 9. `token_usage.rs` Changes

Add Gemini model cost entries. Current table only covers Claude models. Extend with:

```rust
// Gemini 3.x pricing (as of 2026-05, in USD per million tokens)
("gemini-3.1-pro", 3.50, 10.50),
("gemini-3.5-flash", 0.075, 0.30),
("gemini-3-ultra", 15.00, 45.00),
// ... confirm current pricing from Gemini API docs ...
```

This is purely additive and can land in the same PR as init or as a separate small PR.

---

## 10. `main.rs` / CLI Changes

```rust
Commands::Init {
    force,
    // ... existing fields ...
    runtime,   // new: Option<String>, default None → Claude
} => {
    let runtime = runtime
        .as_deref()
        .map(|s| s.parse::<AgentRuntime>())
        .transpose()?
        .unwrap_or(AgentRuntime::Claude);
    let opts = commands::init::InitOpts {
        // ... existing ...
        runtime,
    };
    commands::init::run(&cwd, &opts)
}
```

The `Init` variant in the `Commands` enum needs a new `runtime: Option<String>` field.

---

## 11. Open Questions

| # | Question | Impact | Status |
|---|---|---|---|
| 1 | Exact `hooks.json` schema field names | Blocks `resources/antigravity/hooks.json` | ✅ **Resolved** — confirmed from live file: named top-level objects, `PreInvocation`/`PostInvocation`/`PreToolUse`/`PostToolUse`/`Stop`, same `matcher`/`type`/`command`/`timeout` fields as Claude Code |
| 2 | Does Antigravity read `mcp_config.json` from project root or from `.agents/`? | Affects where to write the file | ✅ **Resolved** — docs confirm: `.agents/` for workspace-scope, `~/.gemini/config/` for global |
| 3 | `CLAUDE_CONFIG_DIR` equivalent for per-worktree config isolation | Blocks kickoff phase | ✅ **Resolved — non-issue.** Antigravity reads workspace config from `.agents/` relative to the working directory. Each worktree has its own `.agents/` checkout, so isolation is automatic. No env var needed. `agy -p "$(cat kickoff)"` launched from the worktree directory picks up the right config natively. |
| 4 | Does `agy` accept `--dangerously-skip-permissions`? | Kickoff launch command | ✅ **Resolved** — confirmed same flag name |
| 5 | Gemini model pricing (exact, current) | Token usage table accuracy | ⏳ **Open** — check Gemini API pricing page before shipping token_usage.rs changes |
| 6 | Is `crosslink session start` a no-op when session is already active? | `session-start.py` idempotency via `PreInvocation` | ✅ **Resolved** — outputs "Session #N is already active" and exits clean. No change to `session-start.py` needed. |

---

## 12. Implementation Plan

### PR 1: `init --runtime antigravity` (this feature)
- [ ] Verify `crosslink session start` idempotency (see §5a); patch `session-start.py` if needed
- [ ] Add `AgentRuntime` enum to `init/mod.rs`
- [ ] Add `--runtime` to `InitOpts` and `main.rs` CLI
- [ ] Create `resources/antigravity/` directory
- [ ] Write `resources/antigravity/hooks.json` (schema confirmed — see §3)
- [ ] Write `resources/antigravity/mcp_config.json`
- [ ] Create `resources/antigravity/skills/` with mirrored command files
- [ ] Extend `build.rs` to emit `SKILL_FILES` const
- [ ] Extend `managed_files()` to accept runtime
- [ ] Extend `run()` to write `.agents/` tree
- [ ] Add `write_agents_md()` function (marker-based merge, like gitignore)
- [ ] Add `write_hooks_json_merged()` function (upserts `"crosslink"` key, preserves all others)
- [ ] Add `write_mcp_config_merged()` function
- [ ] Extend `GITIGNORE_MANAGED_SECTION` with `.agents/` entries
- [ ] Extend `workflow.rs` drift detection
- [ ] Extend `style.rs` path mappings
- [ ] Add integration tests for `--runtime antigravity` and `--runtime both`

### PR 2: Token usage (small, can land with PR 1)
- [ ] Add Gemini model cost table to `token_usage.rs`

### PR 3: `kickoff --runtime antigravity` (after Q3 resolved)
- [ ] Add `build_agy_command()` to `launch.rs`
- [ ] Add runtime detection to `preflight_check()`
- [ ] Add `--runtime` to kickoff CLI

---

## 13. Testing Strategy

New test cases in `init/mod.rs` tests:

```rust
#[test]
fn test_init_antigravity_creates_agents_dir() { ... }

#[test]  
fn test_init_both_creates_both_dirs() { ... }

#[test]
fn test_init_both_no_regression_on_claude() { ... }

#[test]
fn test_agents_md_created_when_missing() { ... }

#[test]
fn test_agents_md_markers_replaced_on_force() { ... }

#[test]
fn test_agents_md_appended_when_no_markers() { ... }
```

Existing Claude Code tests must pass unchanged (no regression).
