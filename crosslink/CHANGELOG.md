# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

## [0.3.0] - 2026-03-05

### Added
- Kickoff CLI command with local and container agent execution
- Design document parser and `--doc` flag for kickoff
- Kickoff plan subcommand for read-only gap analysis
- Spec validation loop and structured build reports for kickoff agents
- Knowledge integration — structured queries, bulk import, MCP server, auto-inject
- `/design` skill for interactive design document authoring
- `/maintain` skill for codebase maintenance
- Configurable auto-steal for stale locks
- Atomic lock claims in `session work`
- `rules.local/` directory for gitignored local rule overrides
- Configurable git remote for hub/knowledge branches
- Clock skew detection using git commit timestamps
- Module dispatch refactor — per-module `run()` functions
- Dry-run integration tests and kickoff unit tests
- Unit tests for shared writer lock operations

### Fixed
- Agent signing chicken-and-egg during init
- TUI agents tab V2 heartbeat reading
- Milestone persistence to coordination branch
- Hub cache hook propagation on init
- Hub sync dirty state and deduplication
- Hub sync push fallback warnings
- Worktree-scoped agent signing config

### Security
- VS Code extension hardening (E1-E3)
- CI/CD fuzz testing improvements (T1-T5)

### Changed
- Restrict proptest CI job to release branches

## [0.1.1-alpha.1] - 2026-02-26

### Added
- Add crosslink kickoff CLI command with local and container agent execution (#2)
- Update injected prompting rules to enforce typed comment discipline (#7)

- **Multi-agent shared issue coordination** — issues can now be shared across agents via a git coordination branch (`crosslink/locks`)
- **`issue_file.rs`** — `IssueFile` serde struct defining the JSON schema for shared issues, including `CommentEntry`, `TimeEntry`, `Counters`, `MilestonesFile`, and `MilestoneEntry`
- **`hydration.rs`** — `hydrate_to_sqlite()` reads all `issues/*.json` from the coordination branch cache and upserts into local SQLite in a single transaction
- **`shared_writer.rs`** — `SharedWriter` handles JSON write → git commit → push with rebase-retry for all write operations in multi-agent mode
- **`commands/migrate.rs`** — `migrate-to-shared` exports local SQLite issues to JSON on the coordination branch; `migrate-from-shared` imports shared JSON back into local SQLite
- **Schema v10 migration** — adds `uuid`, `created_by`, and `author` columns to `issues`, `comments`, and `milestones` tables with unique indexes
- **Hydration insert methods** in `db.rs` — `insert_hydrated_issue()`, `insert_hydrated_comment()`, `insert_hydrated_milestone()`, `clear_shared_data()`, `insert_dependency_raw()`, `insert_relation_raw()`, `insert_label_raw()`, `set_milestone_raw()`
- **Lock claim/release/steal commands** — `crosslink locks claim <id>`, `crosslink locks release <id>`, `crosslink locks steal <id>` for explicit lock management
- **`lock_check.rs`** — `LockStatus` enum and `enforce_lock()` helper; write commands check lock ownership before modifying shared issues
- **`get_writer()` helper** in `main.rs` — constructs `Option<SharedWriter>` (returns `None` in single-agent mode)
- **`parse_issue_id()` utility** — supports regular IDs (`42`) and offline local IDs (`L1` → `-1`)
- **Daemon periodic hydration** — heartbeat cycle now fetches the coordination branch and hydrates SQLite automatically
- **Agent identity in session-start hook** — displays agent identity and coordination sync status on startup

### Changed
- Add Dockerfile and entrypoint for crosslink-agent container image (#3)

- **Write commands accept `Option<&SharedWriter>`** — `create`, `update`, `close/reopen`, `delete`, `comment`, `label/unlabel`, `block/unblock`, `relate/unrelate`, and `session work` route through `SharedWriter` in multi-agent mode, falling back to direct SQLite otherwise
- **`SyncManager` extended** — new methods for shared issue file operations: `push_issue()`, `delete_issue_file()`, `read_counters()`, `write_counters()`, `read_milestones_file()`, `write_milestones_file()`, `cache_path()`
- **Session-start hook** renamed "Lock Sync" to "Coordination Sync" and added "Agent Identity" section
- **`uuid` crate** added as dependency for generating V4 UUIDs
- **Design amendments** added to `.plan/shared-issues-migration.md` — SQLite hydration architecture, single-direction dependency storage, and UUID-primary display ID strategy
