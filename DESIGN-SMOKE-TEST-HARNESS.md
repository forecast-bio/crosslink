# Design: Adversarial Smoke Test Harness

**Issue:** [GH #350](https://github.com/forecast-bio/crosslink/issues/350)
**Status:** Draft v2
**Last updated:** 2026-03-13

---

## 1. Problem Statement

Crosslink has 186 unit tests and extensive proptest coverage, but no end-to-end smoke tests that exercise the system as a user would: invoking the CLI binary, hitting the HTTP server, running multi-agent coordination, and verifying that the pieces compose correctly. Unit tests mock boundaries; smoke tests prove the boundaries work.

The goal is not just "does it run" — it's "what breaks it." This document takes an adversarial stance: every test section starts with what could go wrong and works backward to the test that would catch it.

### What we're testing

- 30 top-level commands, 115+ subcommands, every flag combination that matters
- 40+ REST API endpoints under `crosslink serve`
- WebSocket subscription, filtering, and backpressure
- Git-backed coordination: event sourcing, compaction, lock contention, push retry
- Multi-agent scenarios: concurrent writers, stale locks, clock skew
- Database migrations across all 15 schema versions
- Import/export roundtrips at boundary sizes
- Platform-specific behavior (Unix permissions, Windows ACLs, clipboard)
- TUI rendering without panics across terminal sizes
- Tooling commands: cpitd clone detection, workflow policy diff/trail, context measurement, house style management
- Design document parsing and validation
- History pruning (`prune`) and manual compaction (`compact`) CLI entry points
- Mission control (`mc`) tmux session lifecycle (opt-in, requires tmux)

### What we're NOT testing

- LLM output quality (kickoff prompt generation) — non-deterministic
- External service availability (GitHub API) — mock at the boundary
- Container runtime internals (Docker/Podman) — test command generation; real runtime tests are `#[ignore]` opt-in
- `crosslink mc` tmux layout rendering — requires tmux; test session creation/teardown only, gate with `#[ignore]`
- Real terminal I/O (raw mode, alternate screen, mouse capture) — `crossterm` library concern

---

## 2. Architecture

```
tests/
├── smoke/
│   ├── harness.rs          # Test harness: temp dirs, binary invocation, server lifecycle
│   ├── cli/
│   │   ├── init.rs         # crosslink init variations
│   │   ├── issue_crud.rs   # create/show/update/close/reopen/delete
│   │   ├── issue_org.rs    # labels, blockers, relations, milestones
│   │   ├── comments.rs     # comment kinds, intervention tracking
│   │   ├── sessions.rs     # start/end/work/handoff
│   │   ├── timer.rs        # start/stop/show
│   │   ├── archive.rs      # archive/unarchive/older
│   │   ├── import_export.rs
│   │   ├── knowledge.rs    # add/edit/search/import
│   │   ├── sync.rs         # sync, migrate, integrity, compact, prune
│   │   ├── config.rs       # get/set/reset/diff
│   │   ├── trust.rs        # approve/revoke/check
│   │   ├── locks.rs        # claim/release/steal/check
│   │   ├── kickoff.rs      # dry-run only (no LLM)
│   │   ├── swarm.rs        # init/status/config (no LLM)
│   │   ├── daemon.rs       # start/stop/status lifecycle
│   │   ├── tui.rs          # headless render tests
│   │   ├── cpitd.rs        # scan/status/clear clone detection
│   │   ├── workflow.rs     # diff/trail policy management
│   │   ├── context.rs      # measure/check context injection
│   │   ├── style.rs        # set/sync/diff/show/unset house style
│   │   ├── design_doc.rs   # design document generation (dry-run)
│   │   └── mc.rs           # mission control (requires tmux, #[ignore])
│   ├── server/
│   │   ├── health.rs       # GET /api/v1/health
│   │   ├── issues_api.rs   # Full CRUD via HTTP
│   │   ├── agents_api.rs   # Agent monitoring endpoints
│   │   ├── ws.rs           # WebSocket connect, subscribe, filter, backpressure
│   │   ├── knowledge_api.rs
│   │   ├── orchestrator_api.rs
│   │   └── sync_api.rs
│   ├── coordination/
│   │   ├── event_append.rs # Event log append, crash recovery
│   │   ├── compaction.rs   # Deterministic reduction, watermarks
│   │   ├── lock_contention.rs  # Concurrent lock claims
│   │   ├── push_retry.rs   # Rebase-retry loops, divergence guard
│   │   └── v1_v2_migration.rs  # Layout version upgrade
│   ├── adversarial/
│   │   ├── boundary.rs     # MAX_* limit testing
│   │   ├── corruption.rs   # Malformed JSON, truncated files, missing dirs
│   │   ├── concurrency.rs  # Parallel CLI invocations, database contention
│   │   ├── clock_skew.rs   # Simulated time drift
│   │   ├── injection.rs    # SQL injection, path traversal, shell metacharacters
│   │   ├── resource.rs     # Memory pressure, disk full, permission denied
│   │   └── migration.rs    # Schema downgrade, version skip, partial migration
│   └── proptest_extended/
│       ├── roundtrip.rs    # export→import roundtrip with arbitrary data
│       └── fuzz_cli.rs     # Random flag combinations
```

### Harness Design

```rust
struct SmokeHarness {
    temp_dir: TempDir,         // Isolated working directory
    crosslink_bin: PathBuf,    // Path to compiled binary
    server_handle: Option<Child>, // Running server process
    server_port: u16,          // Allocated port
    agent_id: String,          // Synthetic agent identity
}

impl SmokeHarness {
    /// Create a fresh crosslink environment.
    /// Runs `crosslink init --defaults --skip-cpitd --skip-signing`
    /// in a temp dir with a bare git repo for hub coordination.
    fn new() -> Self { ... }

    /// Run a CLI command, return (exit_code, stdout, stderr).
    fn run(&self, args: &[&str]) -> CmdResult { ... }

    /// Run and assert success (exit 0, no "Error" in stderr).
    fn run_ok(&self, args: &[&str]) -> CmdResult { ... }

    /// Run and assert failure (non-zero exit).
    fn run_err(&self, args: &[&str]) -> CmdResult { ... }

    /// Start `crosslink serve` on a random port.
    fn start_server(&mut self) -> u16 { ... }

    /// HTTP client pointed at the running server.
    fn client(&self) -> reqwest::Client { ... }

    /// Create a second harness sharing the same remote (for multi-agent tests).
    fn fork_agent(&self, agent_id: &str) -> SmokeHarness { ... }
}
```

### Key Principle: Every Test Cleans Up After Itself

Tests run in isolated `TempDir` instances. No shared state between tests. No reliance on test ordering. Parallel execution by default (`#[tokio::test]` or `cargo nextest`).

---

## 3. Test Categories

### 3.1 CLI Functional Tests — "Does It Do What It Says?"

These verify every command produces correct output and side effects.

#### Issue Lifecycle

```
create → show → update → close → reopen → close → archive → unarchive → delete
```

**What could break:**
- Create with empty title (proptest found this: `title = ""`)
- Create with title at exactly MAX_TITLE_LEN (512 chars) — off-by-one
- Create with title at MAX_TITLE_LEN + 1 — should reject
- Description at exactly 64KB — boundary
- `--priority` with mixed case ("High" vs "high") — should reject
- `--priority` with typo ("hgih") — should reject with suggestion
- Show with negative ID, zero ID, i64::MAX
- Delete without `--force` on a locked issue
- Close an already-closed issue (idempotent or error?)
- Reopen an archived issue (should fail — must unarchive first)
- Create subissue of nonexistent parent
- Create subissue chain deeper than display allows (MAX_DEPTH=32)

**Tests:**
```
test_create_minimal               # Just title
test_create_full                  # All flags: -p, -d, -t, -l, -w, --parent
test_create_boundary_title_512    # Exactly at limit
test_create_boundary_title_513    # One over limit → error
test_create_boundary_desc_64k     # Exactly at limit
test_create_boundary_desc_64k1    # One over → error
test_create_empty_title           # Should succeed or fail? (proptest found edge)
test_create_invalid_priority      # "hgih" → error
test_create_case_sensitive_priority  # "High" → error (not in VALID_PRIORITIES)
test_show_nonexistent             # Issue 99999 → error
test_show_zero                    # Issue 0 → error
test_show_negative                # Issue -1 → L-notation? or error?
test_update_nothing               # No flags → no-op or error?
test_close_already_closed         # Idempotent behavior
test_reopen_archived              # Should fail
test_delete_with_children         # CASCADE or error?
test_delete_with_blockers         # CASCADE on dependencies
test_full_lifecycle               # create→show→update→close→archive→delete
test_subissue_depth_32            # MAX_DEPTH chain
test_subissue_orphan_parent       # Parent doesn't exist
```

#### Comments

**What could break:**
- Comment with content at exactly 1MB — boundary
- Comment with content at 1MB + 1 byte — should reject
- Comment with `--kind` set to invalid value
- Comment with Unicode: RTL text, emoji sequences, zero-width joiners, null bytes
- Comment with SQL injection payload: `'; DROP TABLE comments; --`
- Intervention comment with all trigger types
- Comment on closed issue (should work — comments are metadata)

**Tests:**
```
test_comment_basic                # Simple note
test_comment_kinds_all            # note, plan, decision, observation, blocker, resolution, result, handoff, human
test_comment_boundary_1mb         # Exactly at limit
test_comment_boundary_1mb_plus1   # Over limit → error
test_comment_unicode_rtl          # Right-to-left text
test_comment_unicode_emoji        # Multi-codepoint emoji (👨‍👩‍👧‍👦)
test_comment_unicode_zwj          # Zero-width joiners
test_comment_null_bytes           # Embedded \0
test_comment_sql_injection        # '; DROP TABLE comments; --
test_comment_on_closed_issue      # Should succeed
test_intervene_all_triggers       # All 6 trigger types
test_intervene_invalid_trigger    # Should reject
```

#### Labels & Dependencies

**What could break:**
- Label at exactly 128 chars — boundary
- Label with special characters: spaces, slashes, colons, emoji
- Duplicate label add (should be idempotent via INSERT OR IGNORE)
- Block self (issue blocks itself) — should reject
- Circular dependency: A blocks B, B blocks A — should reject
- Longer cycle: A→B→C→A — should reject (DFS cycle detection)
- Block/unblock nonexistent issue
- `ready` and `blocked` after complex dependency graph manipulation

**Tests:**
```
test_label_boundary_128           # Exactly at limit
test_label_boundary_129           # Over limit → error
test_label_special_chars          # "bug/critical", "p:high", "🔥"
test_label_duplicate_add          # Idempotent
test_label_remove_nonexistent     # Should succeed silently or error?
test_block_self                   # Error: "cannot block itself"
test_block_cycle_2                # A↔B cycle
test_block_cycle_3                # A→B→C→A cycle
test_block_nonexistent            # Error
test_ready_empty                  # No issues → empty list
test_ready_after_unblock          # Unblock → appears in ready
test_blocked_complex_graph        # Diamond dependency pattern
```

#### Sessions

**What could break:**
- Start session when one is already active — should fail or end previous?
- End session with no active session
- Work on nonexistent issue
- Handoff notes with very long text
- `last-handoff` with no previous sessions
- Agent-scoped sessions vs unscoped (agent_id column)
- Concurrent sessions from different agents

**Tests:**
```
test_session_lifecycle            # start → work → end --notes
test_session_double_start         # Start when active → behavior?
test_session_end_no_active        # Error
test_session_work_nonexistent     # Error
test_session_handoff_notes        # Preserved across session boundaries
test_session_last_handoff_empty   # No previous sessions
test_session_action               # Record action text
```

#### Import/Export

**What could break:**
- Export empty database — should produce valid empty JSON `[]`
- Import file at exactly 10MB — boundary
- Import file at 10MB + 1 byte — should reject
- Import file with duplicate UUIDs
- Import malformed JSON (truncated, extra comma, wrong types)
- Import legacy ExportData format vs new IssueFile format
- Roundtrip: export → import → export → diff (should be identical)
- Import with parent references that form cycles
- Import with blocker UUIDs that don't exist in the import set

**Tests:**
```
test_export_empty_db              # Valid empty JSON
test_export_json_format           # Validate JSON schema
test_export_markdown_format       # Validate markdown structure
test_import_boundary_10mb         # At limit
test_import_boundary_10mb_plus1   # Over limit → error
test_import_malformed_json        # Truncated → error
test_import_duplicate_uuids       # How handled?
test_import_legacy_format         # Old ExportData envelope
test_import_export_roundtrip      # export→import→export→diff=∅
test_import_orphan_blockers       # Blocker UUID not in set
test_import_cycle_parents         # Parent chain forms cycle
```

#### Knowledge

**What could break:**
- Slug with path separators: `../../../etc/passwd`
- Slug with spaces, unicode, control characters
- `--from-doc` with a file that doesn't exist
- Edit with `--replace-section` targeting nonexistent section
- Search with regex metacharacters (unescaped `.`, `*`, `(`)
- Import directory with thousands of files
- Concurrent add/edit on same slug

**Tests:**
```
test_knowledge_lifecycle          # add → show → edit → search → remove
test_knowledge_slug_traversal     # "../../../etc/passwd" → sanitized or rejected
test_knowledge_slug_unicode       # Unicode slugs
test_knowledge_from_doc           # Import from design doc
test_knowledge_edit_section       # Replace specific section
test_knowledge_search_regex       # Metacharacters handled
test_knowledge_import_dir         # Bulk import
test_knowledge_import_overwrite   # --overwrite flag
```

#### Config

**What could break:**
- Set a key that doesn't exist
- Set with invalid value type (string where number expected)
- `--add` to a non-array field
- Reset specific key vs reset all
- Config file corruption (malformed TOML/JSON)

**Tests:**
```
test_config_show                  # Displays defaults
test_config_get_set_roundtrip     # Set → get → verify
test_config_invalid_key           # Nonexistent key → error
test_config_array_add_remove      # --add/--remove on arrays
test_config_reset_single          # Reset one key
test_config_reset_all             # Reset everything
test_config_diff                  # Shows only non-default values
test_config_corruption_recovery   # Corrupt config file → graceful degradation
```

#### Sync, Migrate & Integrity

These test the CLI entry points for hub synchronization, layout migration, and data integrity checking. The deeper coordination mechanics (push retry, event ordering) are covered in section 3.4.

**What could break:**
- `sync` with no hub branch initialized — should init or error clearly
- `sync` when already up-to-date — idempotent, fast
- `sync` when offline — should degrade gracefully with clear message
- `migrate to-shared` on empty database — should create empty hub
- `migrate from-shared` with no hub — should error
- `migrate rename-branch` when old branch doesn't exist — should error
- `integrity counters` detects desync — wrong next_display_id
- `integrity hydration --repair` on a clean system — idempotent
- `integrity schema --repair` on current version — no-op
- `integrity locks --repair` releases stale locks

**Tests:**
```
test_sync_basic                   # Init hub → sync → verify cache populated
test_sync_idempotent              # Sync twice → no errors, same state
test_sync_offline                 # No remote → graceful error message
test_migrate_to_shared            # Local db → hub branch created
test_migrate_from_shared          # Hub → local db populated
test_migrate_rename_branch        # Old branch name → new name
test_migrate_rename_no_old        # Old branch doesn't exist → error
test_integrity_counters_clean     # Clean state → no issues found
test_integrity_counters_desync    # Corrupt counter → detected
test_integrity_counters_repair    # --repair → counter recalculated
test_integrity_hydration_clean    # SQLite matches JSON → pass
test_integrity_hydration_repair   # Mismatch → --repair re-hydrates
test_integrity_locks_clean        # No stale locks → pass
test_integrity_locks_repair       # Stale lock → --repair releases it
test_integrity_schema_current     # Current version → pass
test_integrity_schema_repair      # Old version → --repair runs migrations
```

#### Compact & Prune

These are the CLI-level tests for `crosslink compact` and `crosslink prune`. The compaction internals (event reduction, watermarks, determinism) are covered in section 3.4 Coordination Tests.

**What could break:**
- `compact` with no events — should be idempotent
- `compact --force` when another agent holds the lease — should override
- `compact` when compaction is already current (watermark at head) — fast no-op
- `prune --dry-run` — shows what would be pruned without modifying
- `prune --force` — actually prunes, reduces commit count
- `prune --keep-commits 0` — should that be allowed? Minimum should be 1
- `prune --hub-only` and `--knowledge-only` — scope correctly
- `prune` on branches with unpushed local changes — should warn or refuse
- `prune` then `sync` — system still works after history rewrite

**Tests:**
```
test_compact_cli_basic            # crosslink compact → exit 0
test_compact_cli_no_events        # Nothing to compact → exit 0
test_compact_cli_force            # --force overrides stale lease
test_compact_cli_already_current  # Watermark at head → fast no-op
test_prune_dry_run                # --dry-run → shows plan, no modifications
test_prune_force                  # --force → actually squashes history
test_prune_keep_commits           # --keep-commits 3 → preserves 3
test_prune_keep_commits_zero      # --keep-commits 0 → should clamp to 1 or error
test_prune_hub_only               # --hub-only → knowledge branch untouched
test_prune_knowledge_only         # --knowledge-only → hub branch untouched
test_prune_then_sync              # Prune → sync → system still works
test_prune_idempotent             # Prune twice → second is no-op
```

#### CPITD (Clone Detection)

**What could break:**
- Scan on empty directory — should complete with zero findings
- Scan on directory with no source files — should complete gracefully
- `--min-tokens` set to 0 — what happens? Every line is a clone?
- `--min-tokens` set to u32::MAX — should find nothing
- `--ignore` with glob that matches everything — should find nothing
- `--dry-run` should not create issues
- `status` with no prior scan — empty list
- `clear` with no open clone issues — idempotent
- Scan creates issues → `clear` closes them → `status` shows empty

**Tests:**
```
test_cpitd_scan_empty_dir         # No source files → zero findings
test_cpitd_scan_dry_run           # --dry-run → no issues created
test_cpitd_scan_min_tokens_high   # --min-tokens 999999 → nothing found
test_cpitd_scan_ignore_all        # --ignore "**/*" → nothing found
test_cpitd_status_no_scan         # No prior scan → empty
test_cpitd_clear_idempotent       # Clear with nothing to clear → exit 0
test_cpitd_lifecycle               # scan → status (has findings) → clear → status (empty)
```

#### Workflow

**What could break:**
- `diff` on freshly initialized project — no drift expected
- `diff --check` in CI mode — exit 0 when clean, exit 1 when drifted
- `diff --section` with invalid section name — should error or show empty
- `trail` on issue with no comments — empty output
- `trail --kind` with nonexistent kind — empty output, not error
- `trail` on nonexistent issue — error
- `diff` after user has manually edited a policy file (custom marker detection)

**Tests:**
```
test_workflow_diff_clean           # Fresh init → no drift
test_workflow_diff_after_edit      # Modify a policy file → drift detected
test_workflow_diff_check_ci_clean  # --check on clean → exit 0
test_workflow_diff_check_ci_dirty  # --check on dirty → exit 1
test_workflow_diff_section_filter  # --section tracking → only tracking files
test_workflow_diff_section_invalid # --section nonexistent → error or empty
test_workflow_diff_custom_marker   # File with "# crosslink:custom" → not flagged
test_workflow_trail_basic          # Issue with comments → chronological output
test_workflow_trail_kind_filter    # --kind plan,decision → only those kinds
test_workflow_trail_empty          # Issue with no comments → empty
test_workflow_trail_nonexistent    # Nonexistent issue → error
test_workflow_trail_json           # --json → valid JSON array output
```

#### Context

**What could break:**
- `measure` on project with no hooks/rules deployed — should still report zero sizes
- `measure --verbose` — additional detail without crash
- `check` on freshly initialized project — all files present
- `check` after deleting a deployed file — reports missing file
- `measure` token estimate accuracy — at least produces a number > 0

**Tests:**
```
test_context_measure_basic        # Reports section sizes and token estimates
test_context_measure_verbose      # --verbose → additional detail
test_context_measure_no_hooks     # No hooks deployed → reports zero for hooks section
test_context_check_clean          # Fresh init → all files valid
test_context_check_missing_file   # Delete a deployed file → reports missing
test_context_check_corrupt_file   # Corrupt a JSON file → reports invalid
```

#### Style (House Style Management)

**What could break:**
- `set` with invalid URL — should fail with clear error
- `set` with URL that doesn't exist — should fail on fetch
- `sync --dry-run` — shows what would change without writing
- `sync` with no style set — should error
- `diff` with no style set — should error
- `show` with no style set — reports "no house style configured"
- `unset` with no style set — idempotent
- `set` then `unset` then `show` — reports unconfigured
- Style source has conflicting files — `diff` shows them

**Tests:**
```
test_style_show_none              # No style configured → informative message
test_style_set_invalid_url        # Bad URL → error
test_style_lifecycle              # set → show → sync → diff → unset
test_style_sync_dry_run           # --dry-run → shows changes, writes nothing
test_style_sync_no_style          # No style set → error
test_style_diff_no_style          # No style set → error
test_style_unset_idempotent       # Unset when not set → exit 0
test_style_diff_after_local_edit  # Edit a synced file → diff shows drift
```

#### Design Doc

**What could break:**
- Design doc generation relies on LLM — use `--dry-run` or test the parsing/validation only
- Parsing a design doc with missing required sections
- Parsing a design doc with code fence edge cases (nested fences, unclosed fences)
- `validate_design_doc` on well-formed vs malformed input

**Tests:**
```
test_design_doc_parse_valid       # Well-formed design doc → parses without error
test_design_doc_parse_missing_section  # Missing "Problem Statement" → validation error
test_design_doc_parse_nested_fences   # Nested code fences → handled correctly
test_design_doc_parse_unclosed_fence  # Unclosed ``` → graceful error
```

#### Mission Control (`mc`)

Requires tmux. All tests in this section use `#[ignore]` and run only with `cargo test -- --ignored`.

**What could break:**
- `mc` with no tmux installed — should error with "tmux not found"
- `mc` with no active agents — should create empty layout
- `mc --layout` with invalid layout name — should error
- `mc` creates sessions → verify they exist → can be killed

**Tests:**
```
#[ignore] // Requires tmux
test_mc_no_tmux                   # Mock tmux absence → clear error message
#[ignore]
test_mc_empty                     # No agents → creates session with empty layout
#[ignore]
test_mc_layout_tiled              # --layout tiled → tmux session created
#[ignore]
test_mc_layout_invalid            # --layout nonexistent → error
#[ignore]
test_mc_lifecycle                 # Launch → verify tmux session → teardown
```

---

### 3.2 Server API Tests — "Does the HTTP Layer Hold?"

Start `crosslink serve` on a random port in the harness, hit endpoints with `reqwest`.

#### Happy Path CRUD

Mirror every CLI test through the API. Create via POST, verify via GET, update via PATCH, delete via DELETE.

**What could break:**
- Request body exceeding MAX_BODY_SIZE (10MB)
- Missing required fields → 400 or 422?
- Extra unknown fields → silently ignored or rejected?
- Concurrent requests hitting the same issue
- Server started with no `.crosslink/` directory

**Tests:**
```
test_health                       # GET /api/v1/health → 200
test_issues_crud                  # POST → GET → PATCH → DELETE
test_issues_list_filters          # ?status=open&label=bug&priority=high
test_issues_blocked_ready         # GET /issues/blocked, /issues/ready
test_comments_crud                # POST /issues/{id}/comments → GET
test_labels_crud                  # POST /issues/{id}/labels → DELETE
test_blockers_crud                # POST /issues/{id}/block → DELETE
test_sessions_lifecycle           # POST start → GET current → POST end
test_milestones_crud              # POST → GET → close → DELETE
test_knowledge_crud               # POST → GET → search
test_search_global                # GET /search?q=term
test_config_crud                  # GET → PATCH
test_usage_crud                   # POST → GET → summary
test_sync_status                  # GET /sync/status
test_orchestrator_lifecycle       # decompose → execute → poll → stages
```

#### Error Paths

```
test_404_unknown_route            # GET /api/v1/nonexistent → 404
test_405_wrong_method             # PUT /api/v1/health → 405
test_body_too_large               # POST 11MB body → 413
test_missing_required_field       # POST /issues {} → 400/422
test_invalid_json                 # POST garbage → 400
test_issue_not_found              # GET /issues/99999 → 404
test_concurrent_updates           # Two PATCH same issue → last-write-wins?
```

---

### 3.3 WebSocket Tests — "Does Real-Time Actually Work?"

**What could break:**
- Connect without upgrade headers
- Subscribe to nonexistent channel
- Server sends faster than client reads (backpressure, BROADCAST_CAPACITY=256)
- Client disconnects mid-stream
- Multiple clients, some filtered, some not
- Sequence number monotonicity (seq field in WsEnvelope)
- Gap detection when messages are dropped

**Tests:**
```
test_ws_connect                   # Upgrade → 101
test_ws_subscribe_filter          # Subscribe to "issues" channel
test_ws_receive_event             # Create issue via API → receive WsIssueUpdatedEvent
test_ws_backpressure              # Send 300 events rapidly → gap message
test_ws_seq_monotonic             # Verify seq always increases
test_ws_multiple_clients          # 5 clients, different filters
test_ws_client_disconnect         # Disconnect → server continues (no panic)
test_ws_reconnect                 # Disconnect → reconnect → get new events
```

---

### 3.4 Coordination Tests — "Does Multi-Agent Work?"

These are the hardest and most valuable tests. Use `SmokeHarness::fork_agent()` to create two independent agents sharing a remote.

#### Event Sourcing

**What could break:**
- Append to event log during compaction (TOCTOU race)
- Crash mid-append leaves incomplete JSON line — `repair_trailing_line()` must fix
- Events from two agents with identical timestamps — ordering must be deterministic (tiebreak on agent_id, then agent_seq)
- Event with future timestamp (clock skew > 60s) — should warn
- Unsigned events when trust is enabled — should warn
- Event log file doesn't exist yet (first event for new agent)

**Tests:**
```
test_event_append_basic           # Write → read → verify
test_event_append_crash_recovery  # Write partial line → repair → verify
test_event_ordering_same_ts       # Two agents, same timestamp → deterministic order
test_event_ordering_deterministic # Shuffle inputs → same output
test_event_clock_skew             # Future timestamp → SkewWarning
test_event_unsigned_warning       # Unsigned event → UnsignedEventWarning
test_event_first_for_agent        # No existing log → create
test_event_concurrent_append      # Two agents appending simultaneously
```

#### Compaction

**What could break:**
- Two agents try to compact simultaneously — filesystem lock should serialize
- Compaction lock is stale (agent died) — force flag should override after 60s
- Compaction with zero events — should be idempotent
- Compaction after partial previous compaction (checkpoint exists, watermark at midpoint)
- Issue created then updated — compaction merges correctly
- Issue created then deleted — compaction removes it
- Duplicate events (same agent_seq replayed) — idempotent or error?
- Watermark points to event that no longer exists (log truncated)

**Tests:**
```
test_compact_basic                # Events → compact → verify materialized files
test_compact_idempotent           # Compact twice → same result
test_compact_incremental          # Events → compact → more events → compact → correct
test_compact_lock_contention      # Two agents try simultaneously → one wins
test_compact_stale_lock           # Create old lock → force → succeeds
test_compact_zero_events          # No events → no crash
test_compact_partial_checkpoint   # Resume from mid-point watermark
test_compact_create_then_update   # Verify merged state
test_compact_create_then_close    # Verify status change materialized
test_compact_deterministic        # Same events, different order → same output
```

#### Lock Contention

**What could break:**
- Two agents claim the same lock simultaneously — exactly one should win
- Agent dies while holding lock — stale detection via heartbeat
- Lock steal on non-stale lock — should reject
- Lock claim on nonexistent issue
- Lock release by non-holder
- Push retry loop during lock claim — should not double-claim

**Tests:**
```
test_lock_claim_release           # Basic lifecycle
test_lock_claim_conflict          # Two agents, one wins
test_lock_stale_detection         # No heartbeat → detected as stale
test_lock_steal_stale             # Steal stale lock → succeeds
test_lock_steal_fresh             # Steal active lock → fails
test_lock_release_by_non_holder   # Wrong agent releases → fails
test_lock_reentrant               # Same agent claims twice → idempotent?
test_lock_claim_nonexistent       # Lock issue that doesn't exist
```

#### Push Retry & Divergence

**What could break:**
- Push rejected (non-fast-forward) — should rebase and retry up to 3 times
- Rebase creates conflict (two agents modified same file) — should fail cleanly
- Divergence exceeds MAX_DIVERGENCE (10) — should bail rather than loop
- Network failure during push — should report offline gracefully
- Rebase loop: each retry creates new conflict — divergence guard catches this

**Tests:**
```
test_push_retry_success           # Reject → rebase → push → success
test_push_retry_exhausted         # 3 failures → error
test_push_divergence_guard        # >10 commits diverged → bail
test_push_offline_graceful        # Network error → LocalOnly outcome
test_push_conflict_resolution     # Two agents, non-conflicting files → rebase succeeds
test_push_conflict_same_file      # Two agents, same file → rebase conflict → error
```

#### V1 → V2 Migration

**What could break:**
- V1 hub with inline comments → upgrade → comments extracted to standalone files
- Mixed v1/v2 issues in same directory — both should be readable
- `read_all_issue_files` must handle: `issues/{uuid}.json` (v1) AND `issues/{uuid}/issue.json` (v2)
- Migration on already-v2 hub — should be idempotent (return 0)
- Version file missing — assume v1
- Version file corrupt — handle gracefully

**Tests:**
```
test_v1_read                      # Read v1 layout files
test_v2_read                      # Read v2 layout files
test_mixed_v1_v2_read             # Read both in same directory
test_upgrade_v1_to_v2             # Run upgrade → verify comments extracted
test_upgrade_idempotent           # Upgrade v2 hub → 0 migrations, no error
test_upgrade_preserves_data       # All issues, comments, metadata preserved
test_version_file_missing         # No meta/version.json → assume v1
test_version_file_corrupt         # Malformed → handle gracefully
```

---

### 3.5 Adversarial Tests — "What Breaks It?"

This is the torture test section. Every test here is designed to break something.

#### Boundary Attacks

Test every MAX_* constant at exactly the limit, one byte over, and at pathological values.

| Constant | Value | Test at | Test over | Pathological |
|----------|-------|---------|-----------|--------------|
| MAX_TITLE_LEN | 512 | 512 chars | 513 chars | 512 of `\0` |
| MAX_LABEL_LEN | 128 | 128 chars | 129 chars | 128 of `\n` |
| MAX_DESCRIPTION_LEN | 64KB | 65,536 bytes | 65,537 bytes | 64KB of `'` |
| MAX_COMMENT_LEN | 1MB | 1,048,576 bytes | 1,048,577 bytes | 1MB of `%00` |
| MAX_IMPORT_SIZE | 10MB | 10,485,760 bytes | 10,485,761 bytes | 10MB of `{` |
| MAX_BODY_SIZE | 10MB | 10,485,760 bytes | 10,485,761 bytes | Chunked encoding bypass |
| MAX_DEPTH | 32 | 32-deep tree | 33-deep tree | Recursive parent chain |
| MAX_DIVERGENCE | 10 | 10 unpushed | 11 unpushed | Rebase loop |
| MAX_RETRIES | 3 | 3rd attempt succeeds | All 3 fail | Exponential backoff? (no — fixed) |
| BROADCAST_CAPACITY | 256 | 256 queued | 257 queued | Lagged receiver |
| MAX_PARTITION_LINES | 2000 | 2000 line file | 2001 line file | Single 2000-line function |

**Tests:**
```
test_boundary_title_exact         # 512 chars → accepted
test_boundary_title_over          # 513 chars → rejected
test_boundary_title_null          # 512 × \0 → rejected or weird?
test_boundary_desc_exact          # 64KB → accepted
test_boundary_desc_over           # 64KB + 1 → rejected
test_boundary_comment_exact       # 1MB → accepted
test_boundary_comment_over        # 1MB + 1 → rejected
test_boundary_import_exact        # 10MB → accepted
test_boundary_import_over         # 10MB + 1 → rejected
test_boundary_depth_32            # 32-deep subissue chain → renders
test_boundary_depth_33            # 33-deep → truncated or error?
test_boundary_broadcast_overflow  # 257 events → gap message
```

#### Corruption Recovery

Simulate every kind of file corruption crosslink might encounter.

**What could break:**
- SQLite database corrupted (random bytes in middle)
- JSON issue file with trailing garbage
- JSON issue file that's valid JSON but wrong schema (missing fields)
- Empty files where content expected
- Directory where file expected (and vice versa)
- Symlinks in unexpected places
- Permissions removed on database file
- Hub branch with merge conflicts
- Event log with duplicate sequence numbers
- Checkpoint file points to events that don't exist

**Tests:**
```
test_corrupt_sqlite               # Corrupt db → should detect and offer repair
test_corrupt_json_trailing        # Valid JSON + garbage → parse error handled
test_corrupt_json_wrong_schema    # Missing "title" field → skip with warning
test_corrupt_json_empty_file      # 0 bytes → skip with warning
test_corrupt_dir_where_file       # issues/uuid is a dir, not a file → handle
test_corrupt_file_where_dir       # issues/uuid is a file, not dir (v2) → handle
test_corrupt_permissions          # chmod 000 on db → clear error message
test_corrupt_event_log_partial    # Incomplete JSON line → repair_trailing_line
test_corrupt_event_log_binary     # Random bytes → skip corrupt lines
test_corrupt_checkpoint_orphan    # Watermark past end of log → handle
test_corrupt_locks_json           # Malformed locks → handle
test_corrupt_heartbeat_json       # Malformed heartbeat → handle
test_corrupt_config               # Malformed config → use defaults + warn
```

#### Injection Attacks

Verify that user input cannot escape its intended context.

**Input vectors:**
1. Issue title/description (stored in SQLite, displayed in TUI, exported to JSON/markdown)
2. Comment content (stored in SQLite, written to JSON files on hub branch)
3. Label names (stored in SQLite, used in file paths for knowledge)
4. Search queries (interpolated into SQL LIKE)
5. Slug names (used in file paths)
6. Agent IDs (used in file paths, git config)
7. Branch names (passed to git commands)

**Tests:**
```
# SQL injection
test_inject_sql_title             # "'; DROP TABLE issues; --" → stored literally
test_inject_sql_search            # "% OR 1=1 --" → returns nothing extra
test_inject_sql_label             # "'; DELETE FROM labels; --" → stored literally

# Path traversal
test_inject_path_slug             # "../../../etc/passwd" → sanitized
test_inject_path_agent_id         # "../../root" → rejected (alphanumeric only)
test_inject_path_label            # "foo/../../bar" → stored as literal string

# Shell injection
test_inject_shell_branch          # "; rm -rf /" → passed to git safely (no shell)
test_inject_shell_agent_id        # "$(whoami)" → rejected (alphanumeric + hyphens only)

# XSS (if dashboard renders these)
test_inject_xss_title             # "<script>alert(1)</script>" → escaped in JSON
test_inject_xss_comment           # "<img onerror=alert(1)>" → stored literally (API returns JSON)

# Unicode abuse
test_inject_unicode_bidi          # Right-to-left override chars → stored but not executed
test_inject_unicode_homoglyph     # Cyrillic 'а' vs Latin 'a' → distinct labels
test_inject_unicode_null          # Embedded \0 → rejected or truncated
```

#### Concurrency Stress

Run parallel operations and verify no data corruption.

**What could break:**
- Two `crosslink issue create` at the same time — display IDs should not collide
- Create + close on same issue simultaneously — should not corrupt
- Two agents running `crosslink sync` simultaneously — should not deadlock
- Server handling 100 concurrent API requests — Mutex should not starve
- WebSocket broadcast to 50 clients while server processes writes

**Tests:**
```
test_concurrent_create_10         # 10 parallel creates → 10 unique IDs
test_concurrent_create_close      # Create while closing → consistent state
test_concurrent_sync_2_agents     # Two agents sync simultaneously
test_concurrent_api_100           # 100 parallel API requests → all succeed
test_concurrent_ws_50_clients     # 50 WebSocket clients → all receive events
test_concurrent_compact_2         # Two agents compact → one wins, both correct
test_concurrent_lock_claim_race   # N agents race for same lock → exactly 1 wins
```

#### Resource Exhaustion

**What could break:**
- Disk full during write — should not corrupt existing data (atomic writes)
- Database grows very large (10K issues) — queries still fast?
- Event log grows very large (100K events) — compaction still terminates?
- WebSocket memory leak (clients connect but never read)
- Too many open file handles (thousands of issue files)

**Tests:**
```
test_resource_disk_full_write     # Fill temp disk → atomic write fails cleanly
test_resource_10k_issues          # Create 10K issues → list still fast (<5s)
test_resource_100k_events         # 100K events → compaction terminates (<30s)
test_resource_ws_memory           # 100 idle clients → memory bounded
test_resource_many_files          # 1000 issue files → read_all_issue_files handles
```

#### Clock Skew

**What could break:**
- Agent with clock 1 hour ahead creates events — compaction warns
- Agent with clock 1 hour behind creates events — ordering still correct
- System clock jumps backward during session — timer durations go negative?
- Event timestamps aren't monotonic within same agent — should still compact

**Tests:**
```
test_skew_future_event            # timestamp +1h → SkewWarning in checkpoint
test_skew_past_event              # timestamp -1h → still orders correctly
test_skew_timer_clock_jump        # Start timer → mock clock backward → stop → sane duration
test_skew_nonmonotonic_seq        # Agent seq jumps → still processes
```

---

### 3.6 Database Migration Tests — "Does Upgrade Work?"

**What could break:**
- Fresh install → schema v15 directly
- Upgrade from v1 → v15 (apply all 14 migrations)
- Upgrade from v14 → v15 (single migration)
- Database at unknown future version (v16) — should refuse or warn
- Migration fails midway — partial state
- Migration on database with data (not just empty schema)
- The v7 bug: `user_version` was always read as 0, so migration ran repeatedly
- The v14 fix: dropping leftover `sessions_new` table

**Tests:**
```
test_migrate_fresh_v15            # New db → v15 schema
test_migrate_v1_to_v15            # Create v1 db with data → upgrade → verify
test_migrate_v14_to_v15           # Minimal upgrade
test_migrate_future_version       # v16 db → graceful error
test_migrate_with_data            # 100 issues + comments → upgrade → data preserved
test_migrate_idempotent           # Run migrations twice → same result
test_migrate_v7_sessions_bug      # Reproduce the user_version=0 bug → v14 fixes it
```

---

### 3.7 TUI Tests — "Does It Render Without Panicking?"

**What could break:**
- Terminal size 0×0
- Terminal size 1×1 (minimum viable)
- Terminal size 300×100 (very large)
- Resize during render
- Issue with extremely long title (wrapping)
- Issue tree deeper than terminal height
- Empty database (no issues, no sessions)
- PlaceholderTab rendering
- Tab switching with number keys 1-6
- 'q' to quit from any tab
- Command palette behavior

**Tests:**
```
test_tui_render_zero              # 0×0 → no panic
test_tui_render_tiny              # 1×1 → no panic
test_tui_render_large             # 300×100 → no panic
test_tui_render_empty_db          # No data → renders placeholder
test_tui_render_long_title        # 512-char title → wraps correctly
test_tui_render_deep_tree         # 32-deep tree → scrollable
test_tui_tab_switch               # Keys 1-6 → correct tab
test_tui_quit_from_each_tab       # 'q' from each tab → exits
test_tui_placeholder_render       # PlaceholderTab → "Coming soon"
test_tui_command_palette          # ':' → opens palette (if implemented)
```

---

### 3.8 Platform-Specific Tests — "Does It Work Everywhere?"

These are `#[cfg(target_os = "...")]` gated.

#### Unix

```
test_unix_key_permissions         # Generated key has 0o600 permissions
test_unix_keys_dir_permissions    # Keys directory has 0o700
test_unix_clipboard_xclip         # Copy to clipboard via xclip (if available)
test_unix_daemon_signals          # SIGTERM → graceful shutdown
test_unix_readonly_dir            # chmod 444 → clear error on write attempt
```

#### Windows

```
test_win_key_acl                  # icacls sets correct permissions
test_win_clipboard_clip           # clip.exe works
test_win_daemon_tasklist          # Process detection via tasklist
test_win_path_backslash           # Backslash paths handled correctly
```

---

### 3.9 Integration Sanity — "Do the Pieces Compose?"

End-to-end scenarios that exercise multiple subsystems together.

#### Scenario: Full Agent Workflow

```
1. crosslink init --defaults
2. crosslink issue create "Implement feature X" -p high
3. crosslink session start
4. crosslink session work 1
5. crosslink issue comment 1 "Starting implementation" --kind plan
6. crosslink locks claim 1
7. crosslink timer start 1
8. crosslink issue label 1 "in-progress"
9. crosslink issue comment 1 "Done with initial implementation" --kind result
10. crosslink timer stop
11. crosslink locks release 1
12. crosslink issue close 1
13. crosslink session end --notes "Feature X complete, needs review"
14. crosslink session start → verify handoff notes displayed
15. crosslink session last-handoff → verify notes match
```

#### Scenario: Multi-Agent Coordination

```
Agent A:                              Agent B:
1. init, sync                         1. init, sync
2. create "Task A" → ID 1             2. (sync) → sees Task A
3. locks claim 1                      3. create "Task B" → ID 2
4. comment 1 "working"                4. locks claim 2
5. (time passes)                      5. comment 2 "working"
6. locks release 1                    6. block 2 1 (B blocked by A)
7. close 1                            7. (sync) → sees A closed
8. (sync)                             8. unblock 2 1
9. verify: A sees B's work            9. close 2
10. verify: ready shows correct state
```

#### Scenario: Server + CLI Interop

```
1. crosslink init
2. crosslink serve --port 3100 (background)
3. curl POST /api/v1/issues → create issue via API
4. crosslink show 1 → see issue created via API
5. crosslink issue close 1
6. curl GET /api/v1/issues/1 → verify closed via API
7. Verify WebSocket received IssueUpdated events for both operations
```

#### Scenario: Export → Nuke → Import → Verify

```
1. Create 50 issues with labels, comments, blockers, milestones
2. crosslink export -f json -o backup.json
3. Delete the database
4. crosslink import backup.json
5. Verify all 50 issues, all labels, all comments, all blockers, all milestones match
6. crosslink export -f json -o backup2.json
7. diff backup.json backup2.json → should be semantically identical
```

#### Scenario: Integrity Check and Repair

```
1. Create issues, comments, milestones
2. Manually corrupt: delete a label row from SQLite
3. crosslink integrity hydration → detects mismatch
4. crosslink integrity hydration --repair → fixes it
5. crosslink integrity counters → checks consistency
6. crosslink integrity locks → checks for stale locks
7. crosslink integrity schema → verifies version
```

---

## 4. Execution Strategy

### 4.1 Test Tiers

| Tier | Tests | Runtime Target | When to Run |
|------|-------|---------------|-------------|
| **T1: Fast** | CLI happy paths, boundary checks, injection | < 30s | Every commit (pre-push hook) |
| **T2: Medium** | Server API, WebSocket, import/export roundtrip | < 2min | Every PR (CI) |
| **T3: Slow** | Multi-agent coordination, compaction, migration | < 10min | Every PR (CI, parallel jobs) |
| **T4: Torture** | Resource exhaustion, 10K issues, concurrency stress | < 30min | Nightly / release candidate |

### 4.2 Infrastructure

- **Binary compilation**: Build once per test run, share binary path
- **Temp directories**: `tempfile::TempDir` per test, auto-cleanup
- **Port allocation**: Random port per server test (bind to 0, read actual port)
- **Git remote**: Bare repo in temp dir for coordination tests
- **Parallelism**: `cargo nextest` with per-test isolation
- **CI integration**: GitHub Actions matrix: Linux (Ubuntu), macOS, Windows
- **Timeout**: Per-test timeout of 60s (T1-T3), 300s (T4)

### 4.3 Test Data Generators

```rust
/// Generate an issue with specific field sizes for boundary testing.
fn gen_issue(title_len: usize, desc_len: usize) -> (String, String) { ... }

/// Generate N issues with realistic distribution of priorities, labels, and dependencies.
fn gen_issue_set(n: usize) -> Vec<IssueSpec> { ... }

/// Generate a valid IssueFile JSON with N comments for import testing.
fn gen_import_json(issue_count: usize, comments_per: usize) -> String { ... }

/// Generate an event log with N events from M agents with optional clock skew.
fn gen_event_log(agents: usize, events_per: usize, skew_range: Duration) -> Vec<EventEnvelope> { ... }
```

### 4.4 Assertion Helpers

```rust
/// Assert that the CLI output contains expected text.
fn assert_stdout_contains(result: &CmdResult, expected: &str) { ... }

/// Assert that JSON output matches expected structure.
fn assert_json_matches(result: &CmdResult, expected: serde_json::Value) { ... }

/// Assert that the database has exactly N issues with given status.
fn assert_issue_count(harness: &SmokeHarness, status: &str, expected: usize) { ... }

/// Assert that two export files are semantically equivalent (order-independent).
fn assert_exports_equivalent(a: &Path, b: &Path) { ... }
```

---

## 5. Known Risk Areas (Prioritized)

These are the areas most likely to have bugs. Tests here should be written first.

### P0 — Will Lose Data

1. **Atomic write failure**: If `rename()` fails after `write()` succeeds, we have a temp file and the original is gone. Test: fill disk between write and rename.
2. **Compaction race**: If two agents compact simultaneously and the filesystem lock fails (NFS, network drive), checkpoint state could be corrupted. Test: mock filesystem lock failure.
3. **Push retry loop**: If rebase introduces silent data loss (auto-resolved conflict drops changes), the user loses work. Test: create conflicting edits, verify nothing is silently dropped.
4. **V1→V2 migration data loss**: If `migrate_inline_comments_to_v2` crashes mid-migration, some comments are extracted and some aren't, but the version file says v2. Test: simulate crash at each comment.

### P1 — Will Corrupt State

5. **Counter desync**: If `next_display_id` in checkpoint doesn't match actual max ID in issues, new issues get duplicate IDs. Test: corrupt counter, create issue, verify detection.
6. **Hydration mismatch**: If SQLite and hub JSON diverge and nobody runs integrity check, queries return stale data. Test: modify hub JSON directly, verify hydration detects drift.
7. **Session leak**: If `session end` fails (crash, Ctrl+C), the session stays open forever. Next `session start` may fail. Test: kill mid-session, verify recovery.
8. **Orphaned locks**: If agent dies without releasing lock, issue stays locked until stale detection triggers. Test: create lock, kill agent, verify stale detection timing.

### P2 — Will Confuse Users

9. **Silent truncation**: If a field is silently truncated instead of rejected, the user sees garbled data. Test: verify all limits reject rather than truncate.
10. **Inconsistent error messages**: Different commands may report the same error differently. Test: trigger same error from CLI and API, compare messages.
11. **Hidden aliases diverge**: If `crosslink create` (hidden alias) behaves differently from `crosslink issue create`, users get inconsistent results. Test: run both, diff output.
12. **Config state surprise**: If `crosslink config set` writes but the process reading config has a stale cache, behavior diverges from expectation. Test: set config in one process, verify another process reads it.

---

## 6. Proptest Extensions

Beyond the existing proptest regressions, extend coverage for:

### Export/Import Roundtrip (property: `∀ db. import(export(db)) ≡ db`)

```rust
proptest! {
    #[test]
    fn roundtrip_export_import(
        issues in prop::collection::vec(arb_issue(), 0..50),
    ) {
        let harness = SmokeHarness::new();
        for issue in &issues {
            harness.create_issue(issue);
        }
        let export = harness.export_json();
        harness.nuke_db();
        harness.import_json(&export);
        let re_export = harness.export_json();
        assert_exports_equivalent(&export, &re_export);
    }
}
```

### CLI Flag Combinations (property: no panic on any valid flag combo)

```rust
proptest! {
    #[test]
    fn fuzz_issue_list_flags(
        status in prop::option::of(prop_oneof!["open", "closed", "all"]),
        label in prop::option::of("[a-z]{1,10}"),
        priority in prop::option::of(prop_oneof!["low", "medium", "high", "critical"]),
        json in any::<bool>(),
        quiet in any::<bool>(),
    ) {
        let harness = SmokeHarness::new();
        let mut args = vec!["issue", "list"];
        // Build flag combinations...
        let result = harness.run(&args);
        // Must not panic (exit code 0 or well-formed error)
        assert!(result.exit_code == 0 || !result.stderr.contains("panic"));
    }
}
```

### Event Compaction Determinism (property: `∀ events. compact(shuffle(events)) ≡ compact(events)`)

```rust
proptest! {
    #[test]
    fn compaction_order_independent(
        events in prop::collection::vec(arb_event(), 1..100),
    ) {
        let result_a = compact_events(&events);
        let mut shuffled = events.clone();
        shuffled.shuffle(&mut thread_rng());
        let result_b = compact_events(&shuffled);
        assert_eq!(result_a, result_b);
    }
}
```

---

## 7. Failure Injection Framework

For torture tests, we need controlled failure injection. Rather than modifying production code with feature flags, use environment-based injection:

```rust
// In harness:
impl SmokeHarness {
    /// Make the next N writes to a path fail with ENOSPC.
    fn inject_disk_full(&self, path: &Path, count: usize) { ... }

    /// Add latency to git push (simulates slow network).
    fn inject_push_latency(&self, ms: u64) { ... }

    /// Corrupt a file at a specific byte offset.
    fn inject_corruption(&self, path: &Path, offset: usize, byte: u8) { ... }

    /// Kill a background process after N milliseconds.
    fn inject_crash_after(&self, handle: &Child, ms: u64) { ... }

    /// Set system clock offset for child processes (via LD_PRELOAD or similar).
    fn inject_clock_skew(&self, offset: Duration) { ... }
}
```

**Implementation options:**
- **Disk full**: Use a small tmpfs mount (Linux) or quota
- **Push latency**: Git mock script that sleeps before real git
- **Corruption**: Direct byte manipulation via `std::fs::OpenOptions`
- **Crash**: `child.kill()` after timeout
- **Clock skew**: `faketime` library (Linux), or modify event timestamps directly

---

## 8. Success Criteria

The smoke test harness is complete when:

1. **Coverage**: Every CLI subcommand has at least one happy-path and one error-path test
2. **Boundaries**: Every MAX_* constant is tested at, over, and with pathological input
3. **Coordination**: Multi-agent lock contention, event ordering, and compaction determinism are verified
4. **Corruption**: The system recovers gracefully from every file corruption scenario without data loss
5. **Injection**: SQL injection, path traversal, and shell metacharacter attacks are proven harmless
6. **Performance**: 10K-issue database queries complete in <5s; 100K-event compaction in <30s
7. **CI green**: All T1-T3 tests pass on Linux, macOS, and Windows in GitHub Actions
8. **No panics**: No test triggers an unwrap/expect panic in production code paths

---

## 9. Design Decisions (Resolved)

1. **`crosslink issue create ""` (empty title) must fail.** Add validation rejecting empty/whitespace-only titles with a clear error message. Test: `test_create_empty_title` asserts non-zero exit and error text.

2. **`crosslink issue close` on an already-closed issue is idempotent.** No error, no-op. The command should succeed silently (or with a note in `--verbose` mode). Test: `test_close_already_closed` asserts exit 0.

3. **The server must reject unknown JSON fields (strict deserialization).** Use `#[serde(deny_unknown_fields)]` on all request types. This catches typos at the API boundary and prevents silent misconfiguration. Test: `test_unknown_json_field_rejected` sends `{"titl": "..."}` and asserts 400/422.

4. **Container tests require a real container runtime.** Tests that invoke `docker`/`podman` should check for the runtime at test startup and emit a clear skip message: `"Skipping: no container runtime found. Install docker or podman to run container tests."` Use `#[ignore]` + `cargo test -- --ignored` for opt-in. Test the command generation separately (no runtime needed) and the actual execution with runtime present.

5. **Add `--dry-run` to more commands for testability.** Priority targets: `crosslink issue delete`, `crosslink archive older`, `crosslink prune`, `crosslink compact`, `crosslink trust approve/revoke`, and all `crosslink swarm` lifecycle commands. Dry-run should print what would happen without side effects. This enables testing destructive operations safely.

6. **TUI testing in CI uses a three-layer strategy:**
   - **Layer 1: `ratatui::backend::TestBackend`** (already in use) — render every tab at various terminal sizes (0x0, 1x1, 80x24, 300x100), assert no panics, verify specific buffer cells contain expected text.
   - **Layer 2: `insta` snapshot testing** — render to TestBackend, serialize the buffer to a string grid, snapshot it with `insta::assert_snapshot!()`. Visual regressions show up as diffs in PR reviews. Deterministic, no real terminal needed.
   - **Layer 3: Synthetic key event injection** — construct `crossterm::event::KeyEvent` structs (pattern already established in `test_placeholder_key_quit`), feed them through `handle_key()`, assert correct `TabAction` returns and state transitions. Covers tab switching (1-6), quit ('q'), scroll, command palette.
   - **Not tested**: Real TTY escape sequences, alternate screen, mouse capture, raw mode enter/exit. These are `crossterm` library concerns, not ours.
