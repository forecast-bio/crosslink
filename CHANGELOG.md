# Changelog

All notable changes to Crosslink will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

## [0.1.3-beta.1] - 2026-03-02

### Added
- Add knowledge page with design analysis for GH #106 and #107 (#88)
- Add crates.io publish CD workflow and exclude __pycache__ from crate (#41)
- Fix agent init reinit when agent.json is malformed (#44)
- Fix sessions_new migration batch error on hub sync (#49)
- Crosslink init: document blocked actions and lint checks in default commands (#51)
- Improve .gitignore defaults for .claude and .crosslink directories (#53)
- Add tmux and git worktree permissions to default kickoff skill (#59)
- Kickoff agent for GH #88: track driver signing key (#39)
- Track driver signing key in interventions and trust approvals (#40)
- Add TUI walkthrough to crosslink init (#13)
- Add house style syncing for commands, rules, and process flows across repos (#14)
- Kickoff agent for GH #62: KnowledgeManager for shared research (#18)
- Add KnowledgeManager for shared research knowledge branch (#19)
- Add GitHub Pages CD workflow for docs site (#20)
- Kickoff agent for GH #81: per-commit signing key override (#29)
- Kickoff agent for GH #63: knowledge page CRUD commands (#30)
- SSH signing foundation — agent key generation, driver key setup, per-commit signing, allowed_signers management (#71-#76)
- Track driver signing key fingerprint in interventions and trust approvals (#88)
- KnowledgeManager for shared research via `crosslink/knowledge` branch with page CRUD and full-text search (#62, #63)
- House style syncing — `crosslink style set/sync/diff/show/unset` for portable project conventions (#91)
- Interactive TUI walkthrough for `crosslink init` with `--defaults` and `--reconfigure` flags (#60)
- Consolidated `crosslink config` command — show, get, set, list, reset, diff subcommands with typed validation
- Managed `.gitignore` section injected by `crosslink init` for `.claude/` and `.crosslink/` directories (#135)
- Document blocked actions and lint checks in default init templates (#100)
- Tmux and git worktree permissions added to default `settings.json` for kickoff workflows
- Crosslink session lifecycle and worktree awareness in `/kickoff` skill template (#33)
- Typed comments and auto-documentation trail — comments carry `kind`, `trigger_type`, and `intervention_context` (#47)
- Driver intervention tracking with `crosslink intervene` command (#68)
- cpitd (code clone detection) integration with `crosslink cpitd` command (#69)
- Scope session queries to `agent_id` for multi-agent isolation (#96)
- Crates.io publish CI workflow on release tags

### Fixed
- Kickoff agent for GH #60: fix crosslink quick lock claiming (#16)
- Fix crosslink quick command to claim lock on new issue (#17)
- Fix rustfmt formatting in knowledge.rs (#23)
- Resolve merge conflict on feature/add-house-style-syncing (#24)
- Resolve `.crosslink` directory through git worktrees so hooks work in worktree checkouts (#131)
- Resolve `sessions_new` migration batch error from wrong pragma column name (#138)
- Allow `agent init` to reinit when existing `agent.json` is malformed (#137)
- Auto-claim lock in `quick`/`create --work` to match `session work` behavior (#83)
- Re-check lock ownership after push conflicts in `claim_lock`
- Guard against clock skew in stale lock detection
- Use atomic write (temp + rename) for issue and lock files
- Register SIGTERM/SIGINT handlers for graceful daemon shutdown
- Log transaction rollback failures, unexpected migration errors, and datetime parse fallbacks instead of suppressing
- Generate UUID at local issue creation time (not deferred)
- Quote font family values in `_brand.yml` to fix docs YAML parse error (#92)

### Security
- Fix SSH key file permissions after generation (#36)
- Enforce restrictive Unix permissions (0600) on generated SSH keys (#105)
- Validate key type and principal format in `allowed_signers` parser
- Parse `ssh-keygen` stderr and add timeout to signature verification
- Prevent path traversal in knowledge page names
- Add 10MB file size limit on JSON import
- Add maximum length validation for string inputs (512-char titles)
- Require minimum 3-character agent IDs
- Stop `crosslink init` from overriding project worktree signing key

### Changed
- Add /kickoff flow documentation page to quarto site (#74)
- Documentation review and docs_src updates (#71)
- Perform full-system adversarial review (#35)
- Kickoff agent for GH #105: fix agent init signing key (#37)
- Kickoff agent for GH #91 (#25)
- Rename `crosslink review` subcommand to `crosslink workflow` (#79)
- Untrack auto-generated `.claude` and `.crosslink` files from git (#94)
- Refine hook-config.json defaults and `.gitignore` rules
- Fix author typo in Cargo.toml (`forecacst.bio` → `forecast.bio`)

## [0.1.2-alpha.1] - 2026-02-27

### Fixed
- SyncManager now detects when running inside a git worktree and reuses the main repo's hub cache instead of trying to create a duplicate `crosslink/hub` worktree (#41)
- Set git user config in test helper for CI compatibility

### Changed
- Kickoff skill (`/kickoff`) now supports `--verify` flag with three levels: `local` (default), `ci`, and `thorough` for post-implementation verification (#39)
- Updated `/kickoff` and `/featree` skill permissions to cover all tools used during execution (added `Write`, `Read`, `Bash(echo *)`, `Bash(crosslink *)`) (#42)
- Featree skill now uses `crosslink init --force` and `crosslink sync` instead of manual database symlinking for worktree initialization (#42)

### Removed
- Stale `.chainlink/` directory (legacy issue tracker artifacts)
- Tracked `crosslink/.crosslink/issues.db` (should be local-only)

## [0.1.1-alpha.1] - 2026-02-26

### Multi-Agent Collaboration

Distributed issue locking and agent coordination, ported from crosslink-enterprise.

#### Agent Identity
- `crosslink agent init <id>` — register a machine-local agent identity (stored in `.crosslink/agent.json`, gitignored)
- `crosslink agent status` — show agent identity and currently held locks

#### Distributed Locking
- `crosslink locks list` — show all active issue locks with stale detection
- `crosslink locks check <id>` — check if a specific issue is available or claimed
- `crosslink sync` — fetch lock state from the `crosslink/locks` coordination branch, verify GPG signatures, display cache path and commit info

#### Lock-Aware Workflows
- `crosslink next` now skips issues locked by other agents
- `crosslink session work <id>` enforces lock ownership before allowing work
- `crosslink create --work` and `crosslink subissue --work` check locks before claiming
- Session start records agent identity in the database (schema v8 to v9 migration)

#### Daemon Heartbeat
- Daemon pushes agent heartbeat every 2.5 minutes to the coordination branch
- Stale lock detection based on heartbeat freshness

#### Hook Enhancements
- `session-start.py` runs `crosslink sync` and displays active locks on startup
- `work-check.py` warns (in strict mode) when working on an issue locked by another agent

#### Init Improvements
- `crosslink init` now writes `.crosslink/.gitignore` to exclude machine-local files (`agent.json`, `.locks-cache/`)

### Claude 4.6 Opus Optimization Epic (#99)

Comprehensive overhaul to make crosslink work seamlessly with Claude 4.6 Opus,
reducing tool-call overhead, improving machine-parseable output, and adding
context-compression resilience.

#### CLI Enhancements
- `crosslink quick` compound command — create + label + work in one call (#100)
- `--json` output flag on show command for structured machine-readable output (#101)
- `--quiet` / `-q` mode for minimal, pipe-friendly output (#108)
- `--work` and `--label` flags on `create` and `subissue` commands (#104)
- `close-all` batch command with label and priority filtering (#107)

#### Session & Context Management
- Stale session auto-detection and cleanup (auto-ends sessions idle >4 hours) (#102)
- Context compression breadcrumbs via `session action` — records last action, auto-comments on active issue, and restores context on resume (#111)
- PreToolUse hook nudges agent when no active working issue is set (#105)

#### Templates & Rules
- Three new AI-specific issue templates: `audit`, `continuation`, `investigation` (#110)
- Condensed behavioral guard mode — lighter rule injection after first prompt (#103)
- Reorganized rules into tiered priority system (critical/standard/optional) (#109)

#### Hooks
- Debounced linting mode in post-edit hook to reduce noise (#106)

#### Code Quality
- Fix all clippy warnings (introduced `CreateOpts` struct, removed dead imports, idiomatic Rust patterns) (#112)
- Database schema v7→v8 migration (adds `last_action` column to sessions, auto-applied on first use)

### Added
- Add git clone fallback for cpitd install (#6)
- Add `crosslink integrity` subcommand with `--check` and `--repair` modes (#31)
- Add `--check` flag to `review diff` for CI policy drift detection (#28)
- Add kickoff workflow skills (`/feature`, `/featree`, `/kickoff`, `/check`) to `crosslink init` (#26)
- Add `+key` array-extend semantics in `hook-config.local.json` (#25)
- Add offline issue ID promotion flow with `crosslink promote` (#24)
- Add promotion notifications with reference tracking (#27)
- Add auto-detection of Python toolchain in `crosslink init` and template hook commands (#36)
- Update `crosslink export` to emit per-issue `IssueFile` JSON format (#32)
- Redesign milestones to per-file storage for conflict-free multi-agent writes (#35)
- Deduplicate config-loading logic into shared `crosslink_config.py` module (#29)
- Add `crosslink review diff` slash command for guided policy review (#7)
- Add `hook-config.local.json` support for machine-local overrides (#5)
- Add multi-agent shared issue coordination via `crosslink/hub` branch (#6)
- Add auto-detection of Python toolchain in crosslink init (#21)
- Update READMEs with hook configuration documentation (#119)
- Split tracking instructions into per-mode markdown files (#118)
- Make issue tracking strictness configurable (#117)
- Make blocked git commands user-configurable in work-check hook (#116)
- Update all dependencies to latest versions (#114)
- Add comprehensive edge case testing (proptest, CLI fuzzing, Unicode E2E) (#50)
- Improve session management with auto-start and stronger rules (#48)
- Add sanitizing MCP server for safe web fetching (#47)
- Add macOS binary support to VSCode extension with cross-compilation (#32)
- Auto-create CHANGELOG.md if it doesn't exist when closing issues
- Automatic CHANGELOG.md updates when closing issues (based on labels)
- `--no-changelog` flag to skip changelog entry for internal work
- `crosslink export` now outputs to stdout by default, use `-o` for file output

### Fixed
- Fix hooks to always find parent .crosslink directory regardless of cwd (#123)
- Fix CI test failure on latest commit (#122)
- Fix vscode engine version to match @types/vscode (#115)
- Fix SQL injection vulnerability in milestone listing (#97)
- Fix cargo-mutants artifact left in production code (#97)
- Fix byte/char length mismatch for Unicode text truncation (#97)
- Fix tree view not filtering subissues by status (#97)
- Fix markdown export silently dropping archived issues (#97)
- Fix daemon log file corruption from duplicate file handles (#97)

### Changed
- Rename coordination branch from `crosslink/locks` to `crosslink/hub` (#37)
- Optimize CI with tiered job dependencies to save minutes on early failures (#33)
- Rebrand chainlink to crosslink (#4)
- Fix display ID collision in rebase-retry logic (#21)
- Block git mutation commands via hook (#113)
- Fix wrong assertion directions and tautological property tests (#96)
- Fix overly loose CLI integration test assertions (#95)
- Fix display function tests to verify actual output or DB state (#94)
- Add unit tests for session.rs command (#64)
- Add security-focused tests (#82)
- Add unit tests for show.rs command (#58)
- Add unit tests for delete.rs command (#57)
- Add unit tests for update.rs command (#56)
- Add unit tests for label.rs command (#61)
- Add unit tests for status.rs command (#60)
- Add unit tests for search.rs command (#59)
- Add unit tests for models.rs (#75)
- Add unit tests for comment.rs command (#62)
- Add unit tests for create.rs command (#55)
- Add Unicode E2E integration tests (#53)
- Add CLI-layer fuzz target for list/show output (#52)
- Add proptest for string handling functions (#51)
- Issue titles are now expected to be changelog-ready (verb + description)

## [1.4] - 2026-01-08

### Added
- Project infographic for README

### Fixed
- Audit and fix tautological tests and logical flaws in test suite (#92)
- Fix UTF-8 panic in list truncation (#49)
- Fix macOS cross-compilation linker configuration (#34)
- Import/export roundtrip issues with parent relationships

## [1.3] - 2026-01-07

### Added
- Elixir and Phoenix language rules (community contribution from @Viscosity4373)
- Build system automatically rebuilds Rust binaries when packaging extension
- Improved global.md defaults for AI agents

### Fixed
- Extension binary update detection (now always overwrites)
- Packager issues

## [1.2] - 2026-01-05

### Added
- VSCode extension for seamless integration
- Agent-agnostic context provider (works with any AI assistant)
- Fuzzing targets for security testing (fuzz_create_issue, fuzz_import, fuzz_search, fuzz_dependency_graph, fuzz_state_machine)
- Property-based testing with proptest
- Cross-platform CI (Windows, macOS, Linux)
- Database corruption recovery
- Daemon auto-start on session start
- ~88% code coverage

### Security
- Add web.md prompt injection defense rule for external content (#33)
- Bump qs dependency to fix vulnerability

### Fixed
- Path handling issues on Windows
- Various edge cases found through fuzzing
- Test reliability improvements

## [1.1] - 2025-12-28

### Added
- Issue templates (bug, feature, refactor, research)
- Hook-based test reminder system
- Export/Import functionality (JSON format)
- Milestones for grouping issues
- Issue archiving for completed work
- Best practices rules for 15 programming languages:
  - Rust, Python, JavaScript, TypeScript, Go, Java, C#, C++
  - Ruby, PHP, Swift, Kotlin, Scala, Haskell
- Composable rules system for better maintainability

### Fixed
- Language detection now checks subdirectories

## [1.0] - 2025-12-27

### Added
- Initial release
- Core issue management (create, show, update, close, reopen, delete)
- Issue hierarchy with subissues
- Labels and comments
- Dependencies (block/unblock)
- Issue relations (relate/unrelate)
- Session management with handoff notes
- Timer for time tracking
- Tree view for issue hierarchy
- Search functionality
- Priority levels (low, medium, high, critical)
- SQLite storage (`.crosslink/issues.db`)
- Claude Code hooks integration
- Smart navigation suggestions
- `crosslink next` command for work suggestions

## Project Goals

Crosslink is designed to be:
- **Simple**: No complex setup, just `crosslink init`
- **Lean**: Single binary, SQLite storage, no external dependencies
- **AI-First**: Built for AI-assisted development workflows
- **Context-Preserving**: Session handoff notes survive context resets
