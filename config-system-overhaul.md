---
title: "Config System Overhaul — Unified Registry, Layered Loading, Interactive Editing"
tags: [design-doc]
sources: []
contributors: [maxine--basel]
created: 2026-03-19
updated: 2026-03-19
---


## Design Specification

### Summary

Refactor the config system so that a single registry drives all surfaces (CLI, init, TUI), add proper layered loading that merges team and local config with provenance tracking, make `crosslink config` (no args) launch an interactive ratatui walkthrough with grouped questions and presets, add inline config editing to the TUI config tab with team/local scope awareness, and integrate shell alias setup into the init flow.

### Requirements

- REQ-1: The `REGISTRY` array in `config.rs` must be the single source of truth for all config keys, extended with `group` (workflow/security/infrastructure/agents) and `hot_swappable: bool` fields, consumed by the CLI, init walkthrough, and TUI config tab — eliminating the separate key lists in `config_tab.rs:204-232` and `init.rs:630-646`.
- REQ-2: `read_config()` in `config.rs` must implement layered loading: read `hook-config.json` (team), overlay `hook-config.local.json` (local) if it exists, merge scalars by override and arrays by `+key` extend semantics, and return the merged result with per-key provenance (team, local, or default).
- REQ-3: `crosslink config` with no subcommand must launch an interactive ratatui walkthrough (matching the `crosslink init` interaction style) when in a TTY, presenting all registry keys grouped by section with current values as defaults, and fall back to `config show` behavior in non-TTY environments.
- REQ-4: `crosslink config --preset team` and `crosslink config --preset solo` must apply predefined config profiles non-interactively, suitable for scripted environments and quick setup.
- REQ-5: The `crosslink init` walkthrough must expand from 4 questions to cover all user-facing config keys from the registry, grouped into sections (workflow, security, infrastructure, agents), with the preset selection offered as the first screen.
- REQ-6: The TUI config tab (`config_tab.rs`) must support inline editing: Enter to cycle enum values or toggle booleans, text input for strings, sub-list editor (add/remove) for arrays, with a team/local scope toggle (`t`/`l`) before writing.
- REQ-7: All config display surfaces (`config show`, `config diff`, TUI config tab) must show per-key provenance — distinguishing team, local override, and default values — and highlight where team and local diverge.
- REQ-8: The TUI config tab must show a help pane when a config key is focused, displaying the key's description and valid values from the registry.
- REQ-9: The TUI config tab must visually separate hot-swappable keys (those with `hot_swappable: true` in the registry) from setup-time keys, and mark non-default values with an explicit indicator and a per-key reset shortcut (`r`).
- REQ-10: `crosslink init` must detect the user's shell and offer to install an `xl` alias (or fish abbreviation / PowerShell alias) to the appropriate shell config file, with idempotent insertion and explicit user consent.
- REQ-11: The TUI config tab must display read-only shell alias status (installed/not installed, which file) as a diagnostic line alongside agent identity and database info.

### Acceptance Criteria

- [ ] AC-1: `config.rs` `REGISTRY` contains all 11+ config keys, each with `key`, `config_type`, `description`, `group`, and `hot_swappable` fields. No other file contains a hardcoded list of config key names (validates REQ-1).
- [ ] AC-2: `crosslink config show` displays the merged result of `hook-config.json` + `hook-config.local.json`, with each key annotated as `(team)`, `(local)`, or `(default)` (validates REQ-2, REQ-7).
- [ ] AC-3: When `hook-config.local.json` overrides a team value, `crosslink config show` and `crosslink config diff` display both values with the override highlighted (validates REQ-2, REQ-7).
- [ ] AC-4: `crosslink config` in a TTY opens a ratatui walkthrough showing all config keys grouped by section (workflow, security, infrastructure, agents) with arrow/vim key navigation (validates REQ-3).
- [ ] AC-5: `crosslink config` in a non-TTY prints the same output as `crosslink config show` (validates REQ-3).
- [ ] AC-6: `crosslink config --preset team` sets: tracking_mode=strict, comment_discipline=required, auto_steal_stale_locks=3, kickoff_verification=ci, signing_enforcement=enforced (validates REQ-4).
- [ ] AC-7: `crosslink config --preset solo` sets: tracking_mode=relaxed, comment_discipline=encouraged, auto_steal_stale_locks=false, kickoff_verification=local, signing_enforcement=disabled (validates REQ-4).
- [ ] AC-8: `crosslink init` walkthrough presents config keys in 4 groups (workflow, security, infrastructure, agents) with a preset selection screen before individual questions (validates REQ-5).
- [ ] AC-9: In the TUI config tab, pressing Enter on a boolean key toggles it, pressing Enter on an enum key cycles through valid values, and pressing Enter on a string key opens an inline text input (validates REQ-6).
- [ ] AC-10: In the TUI config tab, pressing Enter on an array key opens a sub-list view with `a` to add, `d` to remove, and `Esc` to return (validates REQ-6).
- [ ] AC-11: Before writing a config change in the TUI, a confirmation prompt shows the change and allows choosing team (`t`) or local (`l`) scope (validates REQ-6).
- [ ] AC-12: In the TUI config tab, each key shows `[team]`, `[local]`, or `[default]` provenance, and keys where local overrides team show both values (validates REQ-7).
- [ ] AC-13: Focusing a config key in the TUI shows its description and valid values in a bottom pane (validates REQ-8).
- [ ] AC-14: The TUI config tab renders hot-swappable keys in a separate visual group above setup-time keys, non-default values display a `*` marker, and pressing `r` on a non-default key resets it with confirmation (validates REQ-9).
- [ ] AC-15: During `crosslink init`, the walkthrough detects the current shell and asks whether to install the `xl` alias. Answering yes appends the correct alias line to the shell config file. Running init again does not duplicate the line (validates REQ-10).
- [ ] AC-16: The TUI config tab displays "xl alias: installed (~/.zshrc)" or "xl alias: not installed" as a read-only diagnostic line in the agent/system info section (validates REQ-11).
- [ ] AC-17: `crosslink config set tracking_mode strict --local` writes to `hook-config.local.json` instead of `hook-config.json` (validates REQ-2, REQ-6).
- [ ] AC-18: `crosslink config set tracking_mode strict` (no scope flag) writes to `hook-config.json` as before — backward compatible (validates REQ-2).

### Architecture

### Extended config registry (`config.rs`)

The `ConfigKey` struct at `config.rs:20-24` gains two fields:

```rust
struct ConfigKey {
    key: &'static str,
    config_type: ConfigType,
    description: &'static str,
    group: ConfigGroup,
    hot_swappable: bool,
}

enum ConfigGroup {
    Workflow,       // tracking_mode, comment_discipline, reminder_drift_threshold
    Security,       // signing_enforcement, auto_steal_stale_locks
    Infrastructure, // tracker_remote, cpitd_auto_install, blocked/gated/allowed arrays
    Agents,         // intervention_tracking, kickoff_verification
}
```

The registry remains a static array in `config.rs:26-87`. Init and TUI import it via `pub use` or a public accessor function.

The hardcoded key list in `config_tab.rs:204-232` is replaced with an iteration over the registry. The `TuiChoices` struct in `init.rs:630-646` is replaced with a generic `HashMap<String, serde_json::Value>` built from registry iteration.

### Layered config loading (`config.rs`)

Replace `read_config()` at `config.rs:106` with:

```rust
pub struct ResolvedConfig {
    pub merged: serde_json::Value,
    pub provenance: HashMap<String, Source>,
}

pub enum Source {
    Default,
    Team,
    Local,
}

pub fn read_config_layered(crosslink_dir: &Path) -> Result<ResolvedConfig>
```

**Merge algorithm:**
1. Start with embedded defaults (`HOOK_CONFIG_JSON` from `init.rs:329`)
2. Overlay `hook-config.json` — scalar keys override, array keys replace
3. Overlay `hook-config.local.json` — scalar keys override, `+key` arrays extend (existing convention)
4. Track which layer set each key

The existing `read_config()` becomes a thin wrapper calling `read_config_layered()` and returning `.merged` for backward compatibility. All display commands (`show`, `diff`, `get`) and the TUI use the full `ResolvedConfig`.

`write_config()` at `config.rs:113` gains a `scope: WriteScope` parameter:

```rust
enum WriteScope { Team, Local }

fn write_config_scoped(crosslink_dir: &Path, key: &str, value: serde_json::Value, scope: WriteScope) -> Result<()>
```

For `Team`, it writes to `hook-config.json`. For `Local`, it writes to `hook-config.local.json`. The CLI gains `--local` flag on `config set`.

### Interactive config walkthrough (`crosslink config` with no args)

New function `interactive_walkthrough()` in `config.rs`, using ratatui + crossterm matching the `crosslink init` pattern (`init.rs:769-1180`).

**Screen flow:**

```
● Preset    ○ Workflow    ○ Security    ○ Infrastructure    ○ Agents    ○ Confirm

  Quick-start presets:

  ❯ Team    — strict tracking, CI verification, signing enforced
    Solo    — relaxed tracking, local verification, no signing
    Custom  — configure each setting individually

  ↑↓ navigate  Enter select  Esc cancel
```

If Custom (or after preset selection), walk through each group. Each group is a screen showing all keys in that group with current values:

```
✓ Preset: Custom
● Workflow    ○ Security    ○ Infrastructure    ○ Agents    ○ Confirm

  tracking_mode           [strict ▾]    How aggressively issue tracking is enforced
  comment_discipline      [encouraged ▾] How strictly typed comments are enforced
  reminder_drift_threshold [3 ▾]         Prompts before re-injecting reminder

  ↑↓ navigate  Enter edit  →/← cycle value  Backspace back  Esc cancel
```

On Confirm, write to `hook-config.json` (team scope — this is a setup flow).

**TTY detection**: Same pattern as `init.rs:1307` — check `io::stdout().is_terminal()`. Non-TTY falls back to `config show`.

**Preset definitions**: Stored as static structs in `config.rs`:

```rust
static PRESET_TEAM: &[(&str, &str)] = &[
    ("tracking_mode", "strict"),
    ("comment_discipline", "required"),
    ("auto_steal_stale_locks", "3"),
    ("kickoff_verification", "ci"),
    ("signing_enforcement", "enforced"),
];

static PRESET_SOLO: &[(&str, &str)] = &[
    ("tracking_mode", "relaxed"),
    ("comment_discipline", "encouraged"),
    ("auto_steal_stale_locks", "false"),
    ("kickoff_verification", "local"),
    ("signing_enforcement", "disabled"),
];
```

### Init walkthrough expansion (`init.rs`)

The current `WalkthroughApp` at `init.rs:769` generates 4 hardcoded questions. Replace with a registry-driven question builder:

1. First screen: preset selection (team/solo/custom) — same as `crosslink config` walkthrough
2. If custom or after preset: iterate `REGISTRY` grouped by `ConfigGroup`, generate one question per non-array key
3. Array keys are not surfaced in init (they're advanced settings better edited via `config set` or TUI)
4. Shell alias question is appended after the registry keys, in the Infrastructure group

The `TuiChoices` struct is replaced by a `HashMap<String, serde_json::Value>` that `apply_tui_choices()` at `init.rs:1243` writes directly to `hook-config.json`.

### TUI config tab editing (`config_tab.rs`)

The config tab gains new view modes alongside the existing `ViewMode` enum at `config_tab.rs:20-25`:

```rust
enum ViewMode {
    Main,           // Existing: read-only dashboard (config_tab.rs:247)
    EventLog,       // Existing: event log browser (config_tab.rs:394)
    EditEnum,       // New: cycling through enum values
    EditString,     // New: inline text input
    EditArray,      // New: sub-list view with add/remove
    ConfirmWrite,   // New: confirmation prompt with scope selection
}
```

**Main view changes** (replacing the render at `config_tab.rs:247-392`):
- Config keys section rebuilt from `REGISTRY` iteration, split into hot-swappable and setup-time groups
- Each key shows: name, value, provenance badge (`[team]`/`[local]`/`[default]`), and `*` for non-default
- Where local overrides team, show both: `tracking_mode: strict [team] → relaxed [local]`
- Bottom pane: focused key's description and valid values (from registry)
- Shell alias diagnostic line in the system info section

**New keybindings (extending `config_tab.rs:464-494`):**
- `Enter` — Edit focused key (opens type-appropriate editor)
- `r` — Reset focused key to default (with confirmation)
- `e` — Event log (unchanged)

**EditEnum mode:**
- Shows all valid values from `ConfigType::Enum` with cursor
- Arrow keys to navigate, Enter to select
- Bool keys use the same mode with `["true", "false"]`

**EditString mode:**
- Inline text input field with cursor
- Enter to confirm, Esc to cancel

**EditArray mode:**
- List of current array items
- `a` — add (opens text input)
- `d` — delete selected item
- Esc — return to main

**ConfirmWrite mode:**
- Shows the change: `tracking_mode: relaxed → strict`
- Scope toggle: `[t] team  [l] local` (default: team)
- Enter to confirm, Esc to cancel
- On confirm: call `write_config_scoped()` and refresh

### Shell alias setup (`init.rs`)

New function `setup_shell_alias()` called during init after config questions, within the Infrastructure group:

1. Detect shell from `$SHELL` env var
2. Map to config file and alias syntax:
   - bash → `~/.bashrc`, `alias xl='crosslink'`
   - zsh → `~/.zshrc`, `alias xl='crosslink'`
   - fish → `~/.config/fish/config.fish`, `abbr -a xl crosslink`
   - PowerShell → `$PROFILE`, `Set-Alias xl crosslink`
3. Check if alias line already exists (idempotent — grep for the exact line)
4. If not present, ask user for consent, append the line
5. Print "Run `source ~/.zshrc` or open a new terminal to activate"

**TUI alias detection** (`config_tab.rs`): New function `detect_alias_status()` checks the shell config file for the alias line and returns installed/not-installed status with file path. Called during `load_config()` at `config_tab.rs:178`, displayed as a diagnostic line.

### `config show` and `config diff` provenance display

**`config show`** (currently `config.rs:168-204`) is updated to use `read_config_layered()` and show provenance:

```
tracking_mode         strict     (team)
intervention_tracking true       (default)
comment_discipline    required   (local — overrides: encouraged)
```

**`config diff`** (currently `config.rs:389-438`) is updated to show team-vs-local divergences alongside default-vs-current:

```
comment_discipline    default: encouraged  team: encouraged  local: required
auto_steal_stale_locks default: false      team: 3           local: —
```

### Backward compatibility

- `crosslink config show/get/set/list/reset/diff` — all unchanged in behavior, enhanced with provenance
- `crosslink config set key value` — writes to team file (unchanged default)
- `crosslink config set key value --local` — new flag, writes to local file
- `crosslink init` — existing 4 questions still work, expanded flow is additive
- `hook-config.json` format — unchanged
- `hook-config.local.json` — already exists as a convention, now properly consumed by Rust CLI

### Files modified

- `crosslink/src/commands/config.rs` — extended registry, layered loading, interactive walkthrough, scoped writes, provenance display
- `crosslink/src/commands/init.rs` — registry-driven question builder, preset selection, shell alias setup, remove hardcoded `TuiChoices`
- `crosslink/src/tui/config_tab.rs` — inline editing modes, help pane, provenance badges, hot-swap grouping, alias status
- `crosslink/src/main.rs` — update `ConfigCommands` for bare-config dispatch, add `--preset` and `--local` flags

### Out of Scope

- Adding new config keys beyond the existing 11 — this refactors how existing keys are surfaced, not what keys exist
- `hook-config.local.json` `+key` array semantics changes — the existing convention is preserved as-is
- Web dashboard config editing — `crosslink serve` can consume the layered config in a future iteration
- Agent-specific config overrides (`agent_overrides` key in hook-config.json) — this is an advanced feature not surfaced in init or TUI
- Config file migration — existing `hook-config.json` files work without changes

### resolved questions

### Q1: Config registry as single source of truth
**Decision: Yes.** `REGISTRY` in `config.rs` extended with `group` and `hot_swappable`. Init and TUI consume it. No more duplicate key lists.

### Q2: `crosslink setup` vs `--reconfigure`
**Decision: `crosslink config` (no args) in a TTY launches the interactive walkthrough.** No new `setup` command. `init --reconfigure` stays for full re-initialization. Non-TTY falls back to `config show`.

### Q3: Layered config loading
**Decision: In scope.** `read_config_layered()` merges team + local with per-key provenance. All display surfaces show provenance. TUI editor supports scope selection. `config set` gains `--local` flag.

### Q4: One doc or split
**Decision: One unified doc.** Registry refactor and layered loading are shared foundations. Shell alias is small. One kickoff can handle it.

### Q5: Shell alias in TUI
**Decision: Read-only status display.** Init handles installation. TUI shows "xl alias: installed/not installed" as a diagnostic line.

