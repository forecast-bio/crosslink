---
title: Shared Issues on Git Coordination Branch Migration Plan
tags: [planning, architecture]
sources:
  - url: .plan/shared-issues-migration.md
    title: 
    accessed_at: 2026-03-11
contributors: [maxine--basel--gh-287-and-gh-291]
created: 2026-03-11
updated: 2026-03-11
---

# Shared Issues on Git Coordination Branch

Migration from local SQLite `issues.db` to git-mergeable JSON files on the
`crosslink/locks` orphan branch, enabling multi-agent issue coordination with
conflict-free merges.

## Design Principles

1. **One file per issue** — git merges of changes to different files are always clean
2. **Locks guarantee exclusive writes** — no two agents mutate the same issue file
3. **Local SQLite becomes a read cache** — rebuilt from JSON on fetch, preserves fast queries
4. **Graceful degradation** — single-agent mode (no agent.json) keeps working with local-only SQLite
5. **Sessions stay local** — they're machine-specific state, not shared

## Branch Layout

```
crosslink/locks branch (renamed conceptually to "coordination branch"):
  locks.json                    # existing — issue lock assignments
  heartbeats/{agent_id}.json    # existing — agent liveness
  trust/keyring.json            # existing — GPG trust
  issues/{uuid}.json            # NEW — one file per issue
  meta/
    counters.json               # NEW — next display_id, next comment_id
    milestones.json             # NEW — milestone definitions
    labels.json                 # NEW — label registry (optional, for discovery)
```

## Issue File Format

Each issue is a self-contained JSON file at `issues/{uuid}.json`:

```json
{
  "uuid": "a1b2c3d4-e5f6-7890-abcd-ef1234567890",
  "display_id": 42,
  "title": "Fix auth timeout",
  "description": "Users see 504 errors after 30s",
  "status": "open",
  "priority": "critical",
  "parent_uuid": null,
  "created_by": "worker-1",
  "created_at": "2026-02-25T14:30:00Z",
  "updated_at": "2026-02-25T15:00:00Z",
  "closed_at": null,
  "labels": ["bug", "auth"],
  "comments": [
    {
      "id": 1,
      "author": "worker-1",
      "content": "Reproduced on staging",
      "created_at": "2026-02-25T15:10:00Z"
    }
  ],
  "blockers": ["f1e2d3c4-..."],
  "related": ["e9f0a1b2-..."],
  "milestone_uuid": null,
  "time_entries": [
    {
      "id": 1,
      "started_at": "2026-02-25T15:00:00Z",
      "ended_at": "2026-02-25T16:00:00Z",
      "duration_seconds": 3600
    }
  ]
}
```

Key decisions:
- **UUIDs as identity**, display_ids as human-friendly aliases
- **All relationships use UUIDs**, not display_ids
- **Comments are inline** — they're always read with their issue, and the lock
  holder is the only writer, so no conflict
- **Labels are inline** — no separate join table needed
- **Time entries are inline** — scoped to one issue
- **Dependencies stored single-direction** — only `blockers` array on the blocked
  issue; reverse direction derived during SQLite hydration (see Amendment 2 below)

## Counters File

```json
{
  "next_display_id": 43,
  "next_comment_id": 157
}
```

- Atomically incremented in each commit that creates an issue or comment
- On push conflict (non-fast-forward): pull --rebase, re-read counter, re-assign IDs
- This is the **only shared mutable state** beyond locks.json, and it's a single small file

## Milestones File

```json
{
  "milestones": {
    "m-uuid-1": {
      "uuid": "m-uuid-1",
      "display_id": 1,
      "name": "v1.0",
      "description": "Initial release",
      "status": "open",
      "created_at": "2026-02-25T10:00:00Z",
      "closed_at": null
    }
  }
}
```

- Issue-to-milestone association lives in the issue file (`milestone_uuid` field)
- Milestone creation/modification is infrequent and typically done by a single coordinator

## Conflict-Free Guarantee

The invariant:
> Every mutation to `issues/{uuid}.json` requires holding the lock on that UUID.
> Locks are exclusive. Therefore no two agents ever modify the same file in
> the same push window.

This means:
- **Different issues modified** → different files → git auto-merges on rebase
- **Same issue modified** → impossible, lock prevents it
- **New issues created** → new files with unique UUIDs → no conflict
- **Counter conflicts** → handled by rebase retry (same pattern as heartbeat push)

## Implementation Phases

---

### Phase 1: Issue JSON Store (core read/write layer)

**New module: `crosslink/src/issue_store.rs`**

Responsibilities:
- Define `IssueFile` struct (the JSON schema above) with serde derives
- `read_issue(cache_dir, uuid)` → deserialize one issue file
- `write_issue(cache_dir, issue_file)` → serialize and write
- `list_issue_files(cache_dir)` → glob `issues/*.json`, return Vec<IssueFile>
- `delete_issue_file(cache_dir, uuid)` → remove file
- `read_counters(cache_dir)` / `increment_counter(cache_dir, field)` → counter management
- `read_milestones(cache_dir)` / `write_milestones(cache_dir)` → milestone CRUD
- UUID generation (uuid crate v4)
- Display ID ↔ UUID index (built in-memory from file scan)

**New module: `crosslink/src/issue_index.rs`**

Responsibilities:
- `IssueIndex` struct: HashMap<i64, Uuid> (display_id → uuid), HashMap<Uuid, IssueFile>
- Build from scanning all issue files
- Query methods: `by_display_id()`, `by_uuid()`, `by_status()`, `by_label()`, `by_priority()`
- Dependency graph traversal: `is_blocked()`, `blockers_of()`, `would_create_cycle()`
- Search: title/description/comment substring matching
- Ready issues: open + no open blockers
- Tree building: parent-child traversal

This replaces all the SQL queries in db.rs with in-memory operations over the
deserialized issue files. The index is rebuilt on every `fetch` — the dataset is
small enough (hundreds to low thousands of issues) that this is instantaneous.

**Tests:**
- Round-trip serialization for every field combination
- Property tests: create N random issues, serialize, rebuild index, verify queries
- Cycle detection on dependency graphs
- Counter increment + conflict simulation

---

### Phase 2: Extend SyncManager for Issue Operations

**Modify: `crosslink/src/sync.rs`**

Add to SyncManager:
- `read_issue(uuid)` → load single issue file from cache
- `write_issue(issue_file)` → write file, stage, commit
- `delete_issue(uuid)` → remove file, stage, commit
- `read_all_issues()` → load all issue files
- `read_counters()` / `write_counters()` → counter operations
- `read_milestones()` / `write_milestones()` → milestone operations
- `push_issues()` → push to remote with rebase-retry on conflict
- `rebuild_index()` → returns `IssueIndex` from current cache state
- `claim_and_write(uuid, agent, issue_file)` → atomic lock-claim + issue-write in one commit

The commit + push flow:
```
1. Stage changed files (issues/{uuid}.json, counters.json if changed)
2. Commit with message: "{agent_id}: {action} #{display_id} {title}"
3. Push to origin/crosslink/locks
4. On rejection: pull --rebase, re-read counters, re-assign if needed, retry push
5. Max 3 retries, then fail with clear error
```

**Extend `init_cache()`:**
- Create `issues/` and `meta/` directories on first init
- Write initial `counters.json` with `{"next_display_id": 1, "next_comment_id": 1}`

**Tests:**
- Write + read roundtrip in tempdir (no git needed)
- Multiple writes to different files don't conflict
- Counter increment simulation

---

### Phase 3: Dual-Mode Database Adapter

**New module: `crosslink/src/store.rs`**

A trait-based adapter that presents a uniform interface regardless of backend:

```rust
pub trait IssueStore {
    fn create_issue(&mut self, title: &str, desc: Option<&str>, priority: &str) -> Result<i64>;
    fn get_issue(&self, display_id: i64) -> Result<Option<Issue>>;
    fn list_issues(&self, status: Option<&str>, label: Option<&str>, priority: Option<&str>) -> Result<Vec<Issue>>;
    fn update_issue(&mut self, display_id: i64, title: Option<&str>, desc: Option<&str>, priority: Option<&str>) -> Result<bool>;
    fn close_issue(&mut self, display_id: i64) -> Result<bool>;
    fn reopen_issue(&mut self, display_id: i64) -> Result<bool>;
    fn delete_issue(&mut self, display_id: i64) -> Result<bool>;
    fn add_label(&mut self, display_id: i64, label: &str) -> Result<bool>;
    fn remove_label(&mut self, display_id: i64, label: &str) -> Result<bool>;
    fn get_labels(&self, display_id: i64) -> Result<Vec<String>>;
    fn add_comment(&mut self, display_id: i64, content: &str) -> Result<i64>;
    fn get_comments(&self, display_id: i64) -> Result<Vec<Comment>>;
    fn add_dependency(&mut self, blocked_id: i64, blocker_id: i64) -> Result<bool>;
    fn remove_dependency(&mut self, blocked_id: i64, blocker_id: i64) -> Result<bool>;
    fn list_ready_issues(&self) -> Result<Vec<Issue>>;
    fn list_blocked_issues(&self) -> Result<Vec<Issue>>;
    fn search_issues(&self, query: &str) -> Result<Vec<Issue>>;
    // ... milestone, relation, time tracking, archive methods
}
```

Two implementations:
- `SqliteStore` — wraps the existing `Database`, delegates all calls. Zero behavior change.
- `SharedStore` — wraps `SyncManager` + `IssueIndex`. Each write operation:
  1. Checks/acquires lock
  2. Modifies the in-memory index + writes JSON file
  3. Commits to the coordination branch
  4. Pushes (with retry)

**Mode selection** (in main.rs):
```rust
let store: Box<dyn IssueStore> = if AgentConfig::load(&crosslink_dir)?.is_some() {
    // Multi-agent mode: use shared store on coordination branch
    Box::new(SharedStore::new(&crosslink_dir)?)
} else {
    // Single-agent mode: use local SQLite (existing behavior)
    Box::new(SqliteStore::new(db))
};
```

This preserves full backward compatibility. If there's no `agent.json`, behavior
is identical to today.

**Sessions stay in SQLite regardless** — they're machine-local state (which agent
started when, what they're working on). The `Session` model gets no changes.

**Tests:**
- Run the full existing test suite against `SqliteStore` → must pass unchanged
- Mirror every test against `SharedStore` in a tempdir with a git repo
- Property tests: random operation sequences produce same results on both backends

---

### Phase 4: Wire Commands to Store Trait

**Modify all command files** to accept `&dyn IssueStore` instead of `&Database`:

The mechanical change is:
1. Every command function signature changes from `db: &Database` to `store: &dyn IssueStore`
2. All `db.foo()` calls become `store.foo()` calls
3. `main.rs` constructs the appropriate store and passes it through

Commands affected:
- `create.rs` — `store.create_issue()`, `store.add_label()`, lock enforcement stays
- `show.rs` — `store.get_issue()`, `store.get_labels()`, `store.get_comments()`, etc.
- `list.rs` — `store.list_issues()`
- `update.rs` — `store.update_issue()`
- `delete.rs` — `store.delete_issue()`
- `comment.rs` — `store.add_comment()`
- `label.rs` — `store.add_label()`, `store.remove_label()`
- `deps.rs` — `store.add_dependency()`, `store.remove_dependency()`, etc.
- `search.rs` — `store.search_issues()`
- `next.rs` — `store.list_ready_issues()`
- `tree.rs` — `store.list_issues()`, `store.get_subissues()`
- `milestone.rs` — all milestone methods
- `relate.rs` — all relation methods
- `timer.rs` — all time tracking methods
- `archive.rs` — archive/unarchive methods
- `export.rs` / `import.rs` — bulk operations
- `session.rs` — stays on `Database` directly for session ops; uses `store` for issue lookups
- `tested.rs` — `store.add_label()`

**main.rs changes:**
- Construct store based on agent config presence
- Pass `&dyn IssueStore` (or `&mut dyn IssueStore` for writes) to each command
- Keep `Database` for session-only operations

**Tests:**
- All existing command tests must pass (they use `setup_test_db()` → SqliteStore)
- Add parallel test suite using SharedStore

---

### Phase 5: Lock Claim/Release Commands

**New commands** (the missing write side from the original commit):

`crosslink locks claim <display_id> [--branch <name>]`:
1. Resolve display_id → uuid
2. Fetch latest locks
3. Check if already locked (fail if locked by other, succeed if locked by self)
4. Write lock entry to `locks.json`
5. Commit and push

`crosslink locks release <display_id>`:
1. Fetch latest locks
2. Verify this agent holds the lock (fail otherwise)
3. Remove lock entry from `locks.json`
4. Commit and push

`crosslink locks steal <display_id>` (for stale lock recovery):
1. Fetch latest locks
2. Verify lock is stale
3. Replace lock entry with this agent
4. Commit and push

**Auto-claim integration:**
- `session work <id>` → auto-claims lock if in multi-agent mode
- `session end` / `close` → auto-releases lock
- `create --work` → auto-claims after creation

---

### Phase 6: Migration Tool

`crosslink migrate-to-shared`:
1. Verify agent config exists
2. Init coordination branch cache
3. Read all issues from local SQLite
4. For each issue: generate UUID, write `issues/{uuid}.json`
5. Write `counters.json` with next IDs
6. Write `milestones.json`
7. Commit all files
8. Push to remote
9. Print summary

`crosslink migrate-from-shared` (reverse):
1. Fetch coordination branch
2. Read all issue files
3. Insert into local SQLite (creating fresh DB if needed)
4. Print summary

---

### Phase 7: Daemon & Hook Updates

**Daemon:**
- Add periodic `fetch` cycle (every N heartbeat cycles) to keep local cache fresh
- After fetch, rebuild index for faster command execution

**Hooks:**
- `session-start.py`: Already runs `crosslink sync` — now also shows shared issue count
- `work-check.py`: Lock warnings already in place — now locks are actually enforceable

---

## Files Changed Summary

### New files:
- `crosslink/src/issue_store.rs` — JSON issue file read/write
- `crosslink/src/issue_index.rs` — in-memory query index
- `crosslink/src/store.rs` — `IssueStore` trait + `SqliteStore` + `SharedStore`
- `crosslink/src/commands/migrate.rs` — migration commands

### Modified files:
- `crosslink/src/sync.rs` — issue/counter/milestone operations on coordination branch
- `crosslink/src/main.rs` — store construction, new commands
- `crosslink/src/commands/*.rs` — all commands: `&Database` → `&dyn IssueStore`
- `crosslink/src/commands/locks_cmd.rs` — claim/release/steal commands
- `crosslink/src/daemon.rs` — periodic fetch cycle
- `crosslink/src/lock_check.rs` — auto-claim on `session work`
- `crosslink/Cargo.toml` — add `uuid` crate

### Unchanged:
- `crosslink/src/db.rs` — kept as-is, wrapped by `SqliteStore`
- `crosslink/src/models.rs` — kept as-is, used by both backends
- `crosslink/src/locks.rs` — kept as-is
- `crosslink/src/identity.rs` — kept as-is

## Risk Mitigations

1. **Data loss during migration** — migration tool is additive (writes JSON from SQLite),
   never deletes the SQLite file. Both can coexist.

2. **Performance regression** — the index rebuild on fetch is O(n) where n is issue count.
   For <10,000 issues this is <100ms. If it becomes a problem, add a local SQLite cache
   that's rebuilt from JSON (Phase 3 already supports this via the trait).

3. **Network dependency** — SharedStore falls back to last-fetched cache state when offline.
   All reads work. Writes are committed locally and pushed when connectivity returns.

4. **Counter conflicts under high concurrency** — bounded retries (3 attempts) with
   exponential backoff. In practice, issue creation is infrequent enough that this
   almost never happens.

5. **Backward compatibility** — no `agent.json` = SqliteStore = identical to today.
   The migration is opt-in per-machine.

## Open Questions

1. **Should the coordination branch be renamed?** `crosslink/locks` is historical.
   `crosslink/coordination` or `crosslink/shared` better reflects the expanded scope.

2. **Should sessions be shared?** Currently local-only. Some teams might want to see
   what other agents are working on. Could add an optional `sessions/` directory on
   the coordination branch.

3. **Should there be a "leader" agent concept?** A designated agent that handles
   milestone management and other low-frequency shared mutations to avoid even the
   small conflict surface on `milestones.json`.

4. **Import/export format** — should `crosslink export` emit the new JSON format
   or keep the current format? Both?

---

# Design Amendments

The following three amendments refine the original plan based on architectural
review. They simplify Phases 1-4 by replacing the in-memory `IssueIndex` and
`IssueStore` trait with SQLite hydration and a write-only `SharedWriter`.

---

## Amendment 1: SQLite Hydration (replaces in-memory IssueIndex)

### Decision

JSON on the coordination branch is the source of truth. Local SQLite is always
the read path, hydrated from JSON on every `crosslink sync`.

### Why

- Eliminates `IssueStore` trait, `SqliteStore`, `SharedStore`, and `IssueIndex`
- Existing SQL queries in `db.rs` work unchanged against hydrated SQLite
- Read-only commands (`show`, `list`, `search`, `tree`, `blocked`, `ready`) need zero changes

### Architecture

**Fetch flow:**
```
Remote git  →  git fetch/rebase  →  .locks-cache/issues/*.json
                                          ↓
                                   hydrate_to_sqlite()
                                          ↓
                                   .crosslink/issues.db (local SQLite)
                                          ↓
                                   All reads via db.*
```

**Write flow:**
```
Command  →  SharedWriter  →  write JSON to .locks-cache/
                          →  git add + commit + push (retry on conflict)
                          →  insert into local SQLite immediately
```

### New modules

- **`hydration.rs`** — `hydrate_to_sqlite(cache_dir, db)`: reads all
  `issues/*.json`, runs `DELETE + INSERT` in a single transaction
- **`shared_writer.rs`** — `SharedWriter` struct wrapping `SyncManager` +
  `AgentConfig`. Handles JSON write → git push → SQLite update. Returns `None`
  in single-agent mode (no `agent.json`)
- **`issue_file.rs`** — `IssueFile` serde struct (the JSON schema)

### Command signature change

Instead of replacing `&Database` with `&dyn IssueStore`:

```rust
// Before
pub fn run(db: &Database, ...) -> Result<()>

// After — write commands only
pub fn run(db: &Database, writer: Option<&SharedWriter>, ...) -> Result<()> {
    let id = if let Some(w) = writer {
        w.create_issue(db, title, desc, priority)?
    } else {
        db.create_issue(title, desc, priority)?  // unchanged path
    };
}
```

Read-only commands unchanged. Write commands get `Option<&SharedWriter>`.

### Schema migration (v10)

```sql
ALTER TABLE issues ADD COLUMN uuid TEXT;
CREATE UNIQUE INDEX idx_issues_uuid ON issues(uuid);
ALTER TABLE issues ADD COLUMN created_by TEXT;
ALTER TABLE comments ADD COLUMN uuid TEXT;
ALTER TABLE comments ADD COLUMN author TEXT;
ALTER TABLE milestones ADD COLUMN uuid TEXT;
CREATE UNIQUE INDEX idx_milestones_uuid ON milestones(uuid);
```

`uuid` is nullable — in single-agent mode it stays NULL, everything works as before.

### What this replaces from the original plan

| Removed | Reason |
|---------|--------|
| `IssueStore` trait | SQLite is always the read path |
| `SqliteStore` wrapper | `Database` used directly |
| `SharedStore` wrapper | Replaced by `SharedWriter` (write-only) |
| `IssueIndex` in-memory query engine | SQLite handles all queries |
| `issue_index.rs` | Not needed |
| `store.rs` | Not needed |
| Phase 3 (Dual-Mode Adapter) | Eliminated entirely |
| Phase 4 (Wire all commands to trait) | Simplified to adding `Option<&SharedWriter>` |

---

## Amendment 2: Cross-Issue Dependencies (single-direction storage)

### Problem

Original plan stores dependencies bidirectionally: `blockers` AND `blocking`
arrays on each issue JSON. But if Agent A wants to block issue X on issue Y,
and Y is locked by Agent B, Agent A can't write to Y's file.

### Decision

**Store `blockers` only on the blocked issue.** The reverse direction
(`blocking`) is derived during SQLite hydration.

### Why this works

- Agent A only writes to issue X's file (which A locks):
  `"blockers": ["uuid-of-Y"]`
- Agent A never touches Y's file
- During `hydrate_to_sqlite()`, all `blockers` arrays are scanned and inserted
  into the `dependencies(blocker_id, blocked_id)` table — both directions
  available via SQL
- Existing queries (`get_blockers`, `get_blocking`, `list_blocked_issues`,
  `list_ready_issues`) work unchanged against the hydrated table
- Cycle detection via `would_create_cycle()` DFS works unchanged against SQLite

### JSON format (amended)

```json
{
  "uuid": "a1b2c3d4-...",
  "display_id": 42,
  "blockers": ["uuid-of-17"],
  "related": ["uuid-of-99"]
}
```

Removed: `"blocking"` array (was bidirectional, required cross-lock writes).

### Write flow for `crosslink block 42 17`

1. Look up UUIDs for #42 and #17 from SQLite
2. Verify Agent holds lock on #42 (do NOT need lock on #17)
3. Read `issues/uuid-42.json`, append `"uuid-17"` to `blockers`
4. Run cycle detection against SQLite (`would_create_cycle`)
5. Write JSON, commit, push
6. Insert into SQLite `dependencies` table

### Edge cases

- **Dangling blocker UUID** (blocker deleted by another agent): hydration
  silently skips the unknown UUID. Stale reference in JSON is harmless.
- **Race on cycle creation** (A adds X→Y, B adds Y→X simultaneously): after
  rebase-retry, `SharedWriter` re-hydrates and re-validates. If cycle detected
  post-rebase, operation fails with error.
- **`unblock 42 17`**: only modifies #42's file (removes uuid-17 from
  `blockers`). No lock on #17 needed.
- **Relations**: same single-direction strategy. Hydration inserts both
  directions into `relations` table.

### Alternatives rejected

| Option | Why rejected |
|--------|-------------|
| Separate `meta/dependencies.json` | Single shared file = contention bottleneck |
| Message queue for pending deps | Overengineered, requires async consumer |
| Dependencies table on coordination branch | Same contention as shared file |

---

## Amendment 3: Display ID Strategy (UUIDs primary, stable IDs on push)

### Problem

| Approach | Flaw |
|----------|------|
| UUIDs-only, local counter reconciled on fetch | IDs change between syncs — `#5` becomes `#23` |
| Per-agent namespace (1000-1999, 2000-2999) | Gaps in numbering, namespace exhaustion |
| Shared counter with rebase-retry | Viable but needs offline handling |

### Decision

**UUIDs as primary identity + stable display IDs assigned from shared counter
on first push.** Once assigned, a display ID never changes.

### Why this is most scalable

- **Stable**: `#42` stays `#42` forever — users, handoff notes, commit messages
  all reference it reliably
- **UUIDs for internals**: `blockers`, `parent_uuid`, `related`, `milestone_uuid`
  all use UUIDs — immune to display ID assignment
- **Offline-capable**: agents create issues offline with temporary local IDs,
  resolved on next sync
- **Low contention**: issue creation is infrequent (dozens/day), rebase-retry
  adds <1s latency in rare conflicts

### Counter claim flow

```
1. Generate UUID v4
2. Fetch latest from remote (best-effort)
3. Read meta/counters.json: { "next_display_id": 42 }
4. Claim display_id = 42, write next_display_id = 43
5. Write issues/{uuid}.json with display_id: 42
6. git add + commit + push
7. Push rejected? → pull --rebase, re-read counter, reassign, retry (max 3)
```

### Offline (temporary local IDs)

Issues created offline get `display_id: null` in JSON and negative IDs in
SQLite (`-1`, `-2`, ...). Users see these as `L1`, `L2`.

On next successful sync:
1. Read `counters.json`, claim N sequential IDs
2. Rewrite JSON files with real display IDs
3. Commit + push
4. Re-hydrate SQLite (negative IDs → positive)
5. Print: `"Issue L1 has been assigned display ID #50"`

**Parsing in CLI:**
```rust
fn parse_issue_id(s: &str) -> Result<i64> {
    if let Some(n) = s.strip_prefix('L') {
        Ok(-(n.parse::<i64>()?))  // L1 → -1
    } else {
        Ok(s.parse()?)            // 42 → 42
    }
}
```

### Counter recovery

If `counters.json` is corrupted or missing: scan all `issues/*.json` for max
`display_id`, set `next_display_id = max + 1`.

---

## Amended Implementation Phases

These replace the original Phases 1-4:

| Phase | Description | Modules |
|-------|-------------|---------|
| 1 | `IssueFile` serde struct + `hydration.rs` + schema v10 | `issue_file.rs`, `hydration.rs`, `db.rs` |
| 2 | `SharedWriter` + counter management + push-with-retry | `shared_writer.rs` |
| 3 | Integrate hydration into `SyncManager` + `crosslink sync` | `sync.rs` |
| 4 | Wire write commands to `SharedWriter` (incremental) | `commands/*.rs`, `main.rs` |
| 5 | Lock claim/release commands (unchanged) | `commands/locks_cmd.rs` |
| 6 | Migration tool + offline→online ID promotion | `commands/migrate.rs` |
| 7 | Daemon periodic hydration + CLI UX polish | `daemon.rs` |

## Amended Files Changed Summary

### New files:
- `crosslink/src/issue_file.rs` — `IssueFile` serde struct (JSON schema)
- `crosslink/src/hydration.rs` — JSON → SQLite hydration
- `crosslink/src/shared_writer.rs` — write-path coordination for multi-agent mode
- `crosslink/src/commands/migrate.rs` — migration commands

### Modified files:
- `crosslink/src/db.rs` — schema v10 migration (uuid columns), `insert_hydrated_issue()`, `clear_shared_data()`, `insert_dependency_raw()`
- `crosslink/src/sync.rs` — issue/counter file operations, hydration integration
- `crosslink/src/main.rs` — construct `SharedWriter`, pass to write commands, `parse_issue_id()`
- `crosslink/src/commands/*.rs` — write commands get `Option<&SharedWriter>` parameter
- `crosslink/src/commands/locks_cmd.rs` — claim/release/steal commands
- `crosslink/src/daemon.rs` — periodic fetch + hydration cycle
- `crosslink/Cargo.toml` — add `uuid` crate

### Unchanged:
- `crosslink/src/models.rs` — kept as-is, used by both modes
- `crosslink/src/locks.rs` — kept as-is
- `crosslink/src/identity.rs` — kept as-is
