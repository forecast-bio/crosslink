# Changelog

All notable changes to Crosslink will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

## [Unreleased]

### Added
- Design doc for GH issue #367 (#203)
- Integrate pre-flight check improvements for kickoff command: consolidate the inline prerequisite checks in run() and plan() with a unified preflight_check function that adds resolve_timeout_command() for macOS gtimeout support, PreflightResult struct that threads timeout_cmd through to launch_local() and plan(), and detailed platform-specific install instructions. Keep existing tests intact. Reference the worktree version at .worktrees/kickoff-pre-flight-check-for-required-external-commands-e-g/crosslink/src/commands/kickoff.rs for the target implementation. (#151)
- TUI: fix issues tab scroll and improve startup/tab-switch latency. Two bugs: (1) Issues tab scroll is broken - selection cursor moves past visible area but list doesn't scroll to follow, viewport never advances. Fix so selected entry is always visible. (2) Startup and tab-switch latency - noticeable delay on launch and switching to Agents tab from synchronous data loading. Introduce TUI-local cache that populates display immediately with stale/empty state, then updates asynchronously once real data arrives. See GitHub issue #240. (#150)
- Add agent prompting norms for saving research to knowledge repo (#80)
- Phase 4.1: GitHub PAT storage + token management endpoint (GH #429) (#701)
- Dashboard: detect + remedy tracked clones without crosslink init / agent config (#710)
- Dashboard: make alerts clickable to expand with actions (#708)
- crosslink dashboard discover: filesystem autodiscovery of crosslink-enabled repos (GH #429 followup) (#705)
- Write DESIGN-CROSSLINK-OPS.md (GH #429 followup) (#688)
- Phase 5.3: dashboard polish — theme toggle + audible alerts + preferences page (GH #429) (#704)
- Phase 5.2: dashboard webhook alerting — outbound Slack/Discord/generic JSON on alert fire (GH #429) (#703)
- Phase 5.1: dashboard export — projects.csv + alerts.csv endpoints + UI download buttons (GH #429) (#702)
- Phase 4.1: GitHub PAT storage + token management endpoint (GH #429) (#701)

#### Dashboard — multi-project control panel ([GH-429])

- `crosslink dashboard serve` — new SCADA-style panel replacing
  `crosslink serve`'s single-project focus. Aggregates every tracked
  project into one view with live tiles, alerts, and full CLI parity
  for writes.
- `crosslink dashboard track <path>` / `untrack` / `list` — point the
  panel at user's existing project workspaces (no private clones).
- 5-second poll loop per project: git fetch → hub snapshot → counters +
  derived alerts → SQLite persistence → WebSocket fanout.
- Alerts surface: `stale_lock`, `silent_agent`, `overdue_issue`,
  `orphan_subissue`, `unreachable_project` — open/resolve reconciled
  against derived set on every tick.
- Write surface (shell out through `run_cli` primitive; every
  invocation audited to the `actions` table with actor + verb +
  outcome):
  - Issues: close, reopen, comment, block, unblock, relate, label,
    unlabel
  - Milestones: create (with optional description), add issue,
    remove issue, close
  - Locks: claim (with optional branch), release, steal
  - Agents: send control request (kill / pause / resume / reprioritise)
    via the git-native protocol from design doc §9
- Git-native agent control protocol:
  - `crosslink agent request <target> <kind>` writes a signed JSON
    under `agents/<target>/requests/` on the hub branch.
  - `crosslink agent poll-requests` (run automatically every
    `crosslink sync`) translates incoming requests into local
    control flags under `.crosslink/agent-flags/` and writes an
    ack back to the hub so the dashboard shows outcome status.
  - `crosslink agent requests` lists pending/acked requests for
    triage.
- Per-user SQLite index at `~/.crosslink/dashboard.db` with 7 tables
  (projects, project_state, alerts, pty_sessions, actions, activity,
  config).
- React + Vite frontend embedded via `rust-embed`, bundled with the
  binary. New IA: project grid (default), per-project detail (issues
  with inline label chips + comment/label/block/relate drawers,
  agents, locks with Release + Steal controls, milestones with
  create form), /alerts page grouped by severity.
- Deprecates `crosslink serve` — prints a warning pointing users at
  `crosslink dashboard serve`.

`crosslink serve`'s legacy JSON log format is preserved under the
new subcommand for log-scraping continuity.

## [0.5.2] - 2026-03-19

### Added

#### External Source Querying
- `crosslink issue --from` and `crosslink knowledge --from` flags for querying external repositories ([CL-206], [GH-428])
- External repository resolution with GitHub API integration

#### Init & Onboarding
- Greenfield scaffold with interactive design questions and CLAUDE.md template generation ([CL-369])
- `crosslink-guide` Claude skill for in-session feature discoverability ([GH-404])
- Implementation rigor guardrail with auto-discovered rule files ([GH-366])

#### Swarm Enhancements
- Multi-swarm UUID data model and swarm lifecycle commands (`swarm create`, `swarm list`, `swarm switch`) ([CL-371])
- Swarm plan editing commands: `move`, `merge`, `split`, `remove`, `reorder`, `rename` ([CL-373])
- Explicit layer/phase header support in `swarm init` design doc parsing ([CL-373])

#### CLI Enhancements
- `--json` support for `swarm status`, `session status`, `issue tree`, `blocked`, `ready`, and `next` commands ([CL-375], [CL-377])
- Local time displayed alongside UTC timestamps in TUI issue detail ([GH-402])
- Token-budget-aware behavioral guard reinjection for long sessions ([CL-384])

#### Testing
- Concurrency, coordination, and lifecycle smoke test suites ([GH-364])
- Dashboard unit tests (App, color utilities, general utils) with Vitest ([GH-364])
- VS Code extension platform detection tests ([GH-364])

#### Build & Infrastructure
- Integrity layout check and centralized hub file path constants ([GH-428])
- Auto-discover rule files and command files from resources directories in `build.rs` ([CL-387])

### Fixed
- Signing: human key always signs hub commits; agent keys only for identity (#718)
- Fix piped shell commands in skill templates that fail permission checks (#254)
- Fix hub cache recovery loop caused by tracked .hub-write-lock file (#634)
- Fix auth token refresh (#146)
- Alert lock actions: surface holder + disambiguate Release vs Steal (#716)
- Dashboard InitBanner doesn't refresh on init success + track-all init fails on subset of repos (#715)
- Clone target should be $HOME/<repo>, not $HOME/<owner>/<repo> (#714)
- Resolve crosslink binary via PATH first, not current_exe (#713)
- InitBanner error 'spawn crosslink init' hides the real underlying cause (#712)
- Change default clone_root from ~/crosslink-tracked to $HOME (#711)
- Alerts page mutations don't invalidate alerts query; orphan_subissue close appears to no-op (#709)
- Dashboard polish: token refresh UX + issue ordering + remote-repo hub data missing (#707)
- Dashboard: project detail page blank on click + gh CLI auth fallback (#706)
- Fix crosslink init: deploy agent-prompt-server.py (GH#554) (#677)
- Fix kickoff env propagation: CLAUDE_CONFIG_DIR not reaching tmux agent (GH#555) (#676)
- Fix dashboard auth: wire API client to bearer token (GH#556) (#675)

#### Hub & Sync
- V1/V2 hub layout coexistence — resolve inconsistent write paths and cache corruption ([GH-428])
- Prevent hydration data loss and resolve `--parent` cache lookup failure ([GH-427])
- Preserve local-only close events during sync fetch ([GH-430])
- Preserve session work state across hydration cycles ([GH-443])
- Prevent hub cache corruption and ensure agent issue mutations sync correctly ([CL-372])

#### CLI & Display
- Render local issues as `Ln` instead of `#-n` across all commands ([GH-424])
- Resolve worktree paths relative to main repo root, not pwd ([GH-425])
- Recognize local issue `L-` prefix in `work-check` hook ([CL-370])
- Detect Claude Code sub-agent worktrees in `is_agent_context` ([CL-420])
- Resolve main repo root in `kickoff show-plan` for worktree support ([CL-374])

#### Reliability
- Make `delete_issue` atomic — restore files on commit failure ([GH-427])
- Migrate `eprintln!` to `tracing` in sync/hydration paths to prevent TUI screen corruption ([GH-447])
- Replace `unwrap`/`expect` with proper error handling in `build.rs` and sync paths ([CL-206])
- Make `detect_hostname` test non-flaky in parallel test suites

#### Adversarial Review Fixes
- Apply all Group A correctness fixes from adversarial review ([CL-364])
- Resolve clippy errors across Group A fixes ([CL-364])
- Replace lazy `INTENTIONAL` suppressions with `eprintln!` warnings ([CL-L6])
- Add `INTENTIONAL` comments to deliberate error suppression patterns ([CL-419])

### Changed
- Release v0.6.0 — version bump, changelog, docs, tests (#258)
- Add VHS tape and screenshot scripts for docs visuals (#131)
- Manual test container-based agent execution (#73)
- Configure GitHub rulesets for release/* branches and create RELEASING.md (#170)
- Repo cleanup: migrate design docs to knowledge, move docs, remove root scripts (#171)
- Configure GitHub rulesets for release/* branches and create RELEASING.md (#170)
- Repo cleanup: migrate design docs to knowledge, move docs, remove root scripts (#171)
- Configure GitHub rulesets for release/* branches and create RELEASING.md (#170)
- Repo cleanup: migrate design docs to knowledge, move docs, remove root scripts (#171)
- Configure GitHub rulesets for release/* branches and create RELEASING.md (#170)
- Repo cleanup: migrate design docs to knowledge, move docs, remove root scripts (#171)
- P1.11: agent request protocol — hub-branch agents/<id>/requests/*.json + agent-side polling lib + kill/pause/resume/reprioritise verbs + audit log (#700)
- P1.10: lock controls — claim, release, steal (#699)
- P1.9: write surface — milestones + relations + blockers (#698)
- P1.8: write surface — issues + labels (close, reopen, comment, update, label, unlabel) (#697)
- P1.7: alert UI — banner, sidebar rail, /alerts page, desktop notifications (#696)
- P1.6: alert engine — derived alerts (stale_lock, silent_agent, overdue_issue, ci_failure, unreachable, signature_invalid, orphan_subissue, hub_diverged, hub_parse_error, untrusted_signer, pending_request, compaction_lag) (#695)
- P1.5: WebSocket live updates — push change notifications, frontend refetch-on-signal (#694)
- P1.4: project grid (read-only frontend) — tile component, fetch projects, show counts (#693)
- P1.3: REST API — projects + activity endpoints, bearer auth (#692)
- P1.2: poll loop + single-project index — git-fetch every 5s, populate projects/project_state/activity (#691)
- P1.1: scaffolding — crosslink dashboard subcommand + serve deprecation + empty SPA + SQLite bootstrap (#690)
- Manual QA for PR #553 sentinel run + webhook tests (#678)
- Manual QA for PR #553 sentinel run + webhook tests (#678)

#### Codebase Decomposition
- Decompose 6 god files into focused submodules — `shared_writer.rs`, `kickoff.rs`, `db.rs`, `sync.rs`, `knowledge.rs`, `commands/knowledge.rs` ([CL-413])
- Split `kickoff.rs` into 10 submodules (conventions, container, plan, prompt, report, status, watchdog, worktree, wizard, cleanup) ([CL-413])
- Split `commands/knowledge.rs` into `mod.rs` + `operations.rs` ([CL-413])
- Properly wire all swarm paths through `SwarmContext`, remove `allow(dead_code)` annotations

#### Observability
- Migrate logging from `eprintln!` to `tracing` crate across sync, hydration, and daemon modules ([GH-364], [GH-447])
- Route tracing output to stderr to avoid polluting structured CLI output

## [0.5.1] - 2026-03-15

### Added

#### Swarm Review System
- `swarm review` command for parallel adversarial codebase exploration ([GH-342])
- `swarm fix` command for parallel issue-to-agent fix execution ([GH-345])
- `swarm merge` subcommand for combining parallel agent worktree changes ([GH-346])
- End-to-end `swarm review --fix` pipeline orchestrator ([GH-348])
- Finding consolidation and deduplication for swarm reviews ([GH-343])
- Seam detection and codebase auto-partitioning module ([GH-341])
- Trust model configuration for swarm review triage ([GH-347])
- Automatic GitHub issue creation from swarm review findings ([GH-344])

#### Testing
- Adversarial smoke test harness with 134 tests ([GH-354])
- Test coverage boosted to 92.73% ([GH-355])

#### Language Support
- First-class Elixir support in kickoff conventions, context detection, hooks, and command docs ([GH-352])

#### CLI Enhancements
- `--skip-permissions` flag for `kickoff run` ([GH-357])
- Git commit hash included in version string ([GH-339])

#### Build Tooling
- `justfile` with `render-docs` pipeline: SVG generation, quarto render via staging dir, collision detection for manually-maintained docs, asset lint for broken images/styles/scripts/links ([CL-192], [CL-193])

### Fixed
- Missing banner and wordmark SVGs in docs site deployment — banner ref escaped the `docs/` boundary, wordmark not copied by Quarto ([CL-192])
- Swarm launch failure — write `.kickoff-status` sentinel on launch and treat missing status as failed ([GH-359])
- V2 comment hydration bug found during smoke testing ([GH-354])
- Swarm gate worktree bug ([GH-355])
- Update resource templates and hooks to use canonical `crosslink issue` syntax ([GH-338])
- 9 Windows compatibility issues across codebase ([GH-337])
- Windows clipboard support via `clip.exe` ([GH-325])

### Changed
- Remove all `#[allow(dead_code)]` annotations and wire in unused code
- Update CLAUDE.md to document canonical CLI syntax, `--quiet`, and `--json` flags ([GH-338])
- Docs CI workflow now uses `just render-docs` with asset verification gate

## [0.5.0] - 2026-03-11

### Added

#### Web Dashboard (`crosslink serve`)
- `crosslink serve` subcommand with axum HTTP server scaffold ([GH-290])
- React Vite dashboard scaffold with TypeScript, TailwindCSS 4, and shadcn/ui components
- Agent monitoring REST endpoints and real-time WebSocket updates with filesystem watcher
- Agent list page with AgentCard component and agent detail drilldown with HeartbeatTimeline and LockList
- Issues CRUD REST endpoints, issue list and detail views, session management UI
- Label manager, dependency editor, and bulk issue operations
- Sessions, milestones, knowledge, search, sync, and config REST API endpoints
- Knowledge browser, milestones, and command palette pages
- Sync dashboard, config editor, and lock visualization
- Usage graphs and cost breakdown components with token usage collection and storage
- DAG and Gantt visualization for orchestrator execution
- Execution controls and live monitoring components
- Document import, stage editor, and LLM-assisted document decomposition orchestrator
- DAG execution engine with topological sort and executor lifecycle management
- Appearance settings page and orchestrator endpoint wiring

#### CLI Enhancements
- `crosslink prune` command for hub/knowledge history pruning ([GH-297])
- `crosslink kickoff cleanup` command for pruning stale worktrees and tmux sessions ([GH-298])
- `crosslink kickoff list` command with worktree, tmux, and Docker discovery
- Refactor CLI into `issue`/`timer`/`migrate` subcommand groups ([CL-157])
- Watchdog sidecar to nudge idle kickoff agents

#### Knowledge Management
- `--replace-section` and `--append-to-section` flags for `knowledge edit` command ([GH-264])

#### TUI Improvements
- Startup sync, periodic background sync, and manual `r` keybinding for refresh ([CL-169])

#### CI
- Fix CI concurrency groups and repo cleanup ([GH-287], [GH-291])

### Fixed
- Address 14 findings from adversarial codebase review
- Restore view state on issue detail back navigation and clamp scroll bounds ([GH-293])
- Add agent init verification and sync steps to kickoff instructions ([GH-289])
- `ssh-keygen` verify checks both stdout and stderr, allow unsigned hub writes ([GH-299], [GH-301])
- Replace `unwrap()` calls with `ok_or_else()` for strict clippy CI compliance
- Resolve duplicate Agent type and stale field names in dashboard types
- Fix clippy warnings in adversarial review fixes

### Changed
- Move `.crosslink/` ignores to inner `.gitignore` ([CL-175])

## [0.4.0] - 2026-03-10

### Added

#### Swarm Orchestration (`crosslink swarm`)
- `crosslink swarm init/status/resume` commands for multi-agent swarm lifecycle (Phase 1, [GH-233])
- `crosslink swarm launch/gate/checkpoint` commands for coordinated agent execution (Phase 2, [GH-233])
- Swarm budget estimation and throttling (Phase 3, [GH-233])
- Swarm multi-window planning (Phase 4, [GH-233])

#### Mission Control
- `crosslink mission-control` command for monitoring active agents in a unified dashboard

#### Agent Hooks & Liveness
- Agent-aware hooks with git flag bypass fix ([GH-164], [GH-226])
- Hook-based heartbeats for kickoff agent liveness detection
- Custom sandbox wrapper support for agent isolation (alternative to Docker)

#### Kickoff & Preflight
- Unified preflight check with macOS `gtimeout` support for kickoff command
- Platform-specific remediation hints for preflight dependency checks ([GH-260])

#### TUI Improvements
- Async data loading for TUI agents and config tabs ([GH-254])
- Table scroll-to-follow across all tabs ([GH-240])

#### Knowledge & Search
- Word-level fuzzy matching for knowledge search ([GH-263])

#### CI
- Tiered smoke tests for CI pipeline ([GH-242])
- Restrict fuzz tests to release branches and PRs targeting main

### Fixed
- Mission-control pane liveness and auto-attach robustness
- Load `knowledge.md` rules into Claude prompt via `prompt-guard.py`
- Publish parent SSH key under kickoff agent ID after creation ([GH-261])
- Degrade gracefully when `gpg.ssh.allowedSignersFile` is not configured ([GH-262])
- Replace `unwrap()` calls with proper error handling for strict clippy compliance
- Restore deleted tests and update preflight test signatures
- Swarm coherence fixes across all 4 phases ([GH-233])
- Skip headings inside code fences in design doc parser ([GH-248])
- Simplify drift reminder to fixed 3-turn interval
- Pre-flight check for required external commands in kickoff

### Changed
- Clean up feature worktrees and tmux sessions (#180)
- README updated with multi-agent orchestration, swarm, kickoff, knowledge, TUI, and hooks features

## [0.3.0] - 2026-03-05

### Added

#### Kickoff & Agent Orchestration
- `crosslink kickoff` CLI command with local and container agent execution ([GH-175])
- Design document parser and `--doc` flag for kickoff, importing design docs to knowledge ([GH-215], [GH-216])
- `crosslink kickoff plan` subcommand for read-only gap analysis
- Spec validation loop for structured validation of kickoff agents ([GH-217])
- Structured machine-readable build reports for kickoff agents ([GH-219])

#### Knowledge Integration
- Structured queries, bulk import, MCP server, and auto-inject for knowledge branch ([GH-221])
- `--from-doc` flag on `knowledge add` for design doc import

#### CLI & Workflow
- `/design` skill for interactive design document authoring ([GH-225])
- `/maintain` skill for codebase maintenance ([GH-205])
- Configurable auto-steal for stale locks ([GH-223])
- Atomic lock claims — bail on contended lock claims in `session work` to close timing race ([GH-224])
- `rules.local/` directory for gitignored local rule overrides ([GH-234])
- Configurable git remote for hub/knowledge branches via `tracker_remote` setting ([GH-235])
- VHS tape files and screenshot scripts for docs visuals ([GH-227])

#### Code Quality
- Module dispatch refactor — extract monolithic dispatch from `main.rs` into module-level `run()` functions ([GH-222])
- Extract pure functions from kickoff module for testability ([GH-209])
- Dry-run integration tests and `build_prompt` unit tests for kickoff ([GH-214])
- Unit tests for shared writer lock claim, release, and contention ([GH-208])
- Clock skew detection using git commit timestamps as witness ([GH-173])

### Fixed
- Agent signing chicken-and-egg during init — defer key publish and use unsigned bootstrap commits ([GH-237])
- TUI agents tab not forming agent list — read V2 heartbeats and refresh data on tab focus ([GH-232])
- Milestone add/remove now persists to coordination branch ([GH-174])
- Hub cache hooks — propagate `.claude/hooks` into hub cache worktree on init ([GH-213])
- Hub sync dirty state — deduplicate hub cache issues during hydration, prevent vicious sync loop ([GH-210])
- Hub sync push warnings — surface visible warnings when push falls back to local-only ([GH-206])
- Use `--worktree` scope for agent signing config in linked worktrees ([GH-167])
- Skip worktree agent init and tmux/claude prerequisite checks in dry-run mode

### Security
- VS Code extension security hardening (E1-E3) ([GH-169], [GH-175])
- CI/CD fuzz testing improvements (T1-T5) ([GH-168])

### Changed
- Restrict proptest CI job to release branches and PRs to main to reduce CI minutes ([GH-228])

## [0.2.0] - 2026-03-03

First stable release. Promotes all changes from v0.1.3-beta.1 plus major new features.

### Added

#### Terminal UI (`crosslink tui`)
- `crosslink tui` command — read-only terminal dashboard for browsing crosslink data ([GH-152])
- Issues tab with tree view, detail view, filtering, and sorting
- Agents tab with active session monitoring
- Knowledge tab with page browser and vivid syntax highlighting
- Milestones tab and Config tab
- Mouse support, command palette (`Ctrl-P`), and clipboard export
- Keyboard navigation with help overlay (`?`)

#### Container-Based Agent Execution
- `crosslink container` command for isolated agent environments ([GH-110])
- Container bootstrap command for setting up agent containers
- Updated `/kickoff` and `/check` skills to support container backend

#### Context Management
- `crosslink context` command — context injection optimization with skills, adaptive reminders, and measurement

#### Hub Layout V2
- Compact command, `--defer-id`, lock timeout, V2 stale detection (Phase 5) ([GH-113])
- Event-based lock confirmation protocol (Phase 3)
- Container bootstrap command (Phase 4)
- SharedWriter updated for v2 hub layout paths ([GH-132])

#### Knowledge Management
- Accept-both conflict resolution for knowledge branch sync/push
- Knowledge management prompting norms for agents
- KnowledgeManager for shared research via `crosslink/knowledge` branch with page CRUD and full-text search ([GH-62], [GH-63])

#### Init Experience
- Ratatui-based setup wizard with styled progress output for `crosslink init`
- Confirmation screen, taller viewport, and smoother progress output
- Inline TUI rendering with clean Esc cancel
- Interactive TUI walkthrough with `--defaults` and `--reconfigure` flags ([GH-60])
- Managed `.gitignore` section injected by `crosslink init`
- Document blocked actions and lint checks in default init templates

#### SSH Signing
- SSH signing foundation — agent key generation, driver key setup, per-commit signing, allowed_signers management ([GH-71]-[GH-76])
- Track driver signing key fingerprint in interventions and trust approvals

#### Other
- House style syncing — `crosslink style set/sync/diff/show/unset` for portable project conventions ([GH-91])
- Consolidated `crosslink config` command — show, get, set, list, reset, diff with typed validation
- Typed comments and auto-documentation trail — comments carry `kind`, `trigger_type`, and `intervention_context`
- Driver intervention tracking with `crosslink intervene` command
- cpitd (code clone detection) integration with `crosslink cpitd` command
- Scope session queries to `agent_id` for multi-agent isolation
- Crates.io publish CI workflow on release tags
- GitHub Pages CD workflow for docs site

### Fixed
- Address TUI adversarial review findings (C1, C2, H2-H4, M3-M7)
- Resolve `.crosslink` directory through git worktrees so hooks work in worktree checkouts ([GH-131])
- Make hooks resilient to invocation from non-project directories
- Resolve container startup failures found during manual testing
- Resolve `sessions_new` migration batch error from wrong pragma column name ([GH-138])
- Allow `agent init` to reinit when existing `agent.json` is malformed ([GH-137])
- Auto-claim lock in `quick`/`create --work` to match `session work` behavior
- Configure fallback git identity in hub cache for CI environments
- Re-check lock ownership after push conflicts in `claim_lock`
- Guard against clock skew in stale lock detection
- Use atomic write (temp + rename) for issue and lock files
- Register SIGTERM/SIGINT handlers for graceful daemon shutdown
- Generate UUID at local issue creation time (not deferred)

### Security
- Enforce restrictive Unix permissions (0600) on generated SSH keys ([GH-105])
- Validate key type and principal format in `allowed_signers` parser
- Parse `ssh-keygen` stderr and add timeout to signature verification
- Prevent path traversal in knowledge page names
- Add 10MB file size limit on JSON import
- Add maximum length validation for string inputs (512-char titles)
- Require minimum 3-character agent IDs
- Stop `crosslink init` from overriding project worktree signing key

### Changed
- Restructure CI/CD workflows for git flow branching model (develop/feature/release/hotfix)
- Add hotfix/release branch triggers and PRs-to-main to CI workflow
- Decouple publish.yml from ci.yml, rely on branch protection
- Rename `crosslink review` subcommand to `crosslink workflow`
- Untrack auto-generated `.claude` and `.crosslink` files from git
- Comprehensive documentation review and quarto site updates
- Full-system adversarial review

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
