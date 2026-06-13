# Feature: Issue Scheduling Fields (scheduled_at / due_at)

## Summary
Add optional `scheduled_at` and `due_at` DateTime fields to issues, enabling task scheduling workflows. `scheduled_at` marks when an issue becomes actionable; `due_at` is the hard deadline. The `crosslink next` command filters not-yet-scheduled issues, boosts overdue issues, and warns about approaching deadlines. Both fields propagate through the full pipeline: CLI, IssueFile JSON, CompactIssue, SQLite schema, and hydration.

## Requirements
- REQ-1: Add `scheduled_at: Option<DateTime<Utc>>` and `due_at: Option<DateTime<Utc>>` to Issue (models.rs:5-15), IssueFile (issue_file.rs:13-45), CompactIssue (checkpoint.rs:63-87), HydratedIssue (db/hydration.rs:8-20), and SQLite schema (db/core.rs, SCHEMA_VERSION 15 -> 16).
- REQ-2: Add `--scheduled` and `--due` CLI flags to the `Create` (main.rs:465-489), `Quick` (main.rs:492-510), and `Update` (main.rs:557-570) commands. Accept `YYYY-MM-DD` or full RFC 3339 datetime input.
- REQ-3: Add `--no-scheduled` and `--no-due` CLI flags to `Update` for clearing dates. Use clap `conflicts_with` to prevent `--due` + `--no-due`.
- REQ-4: `crosslink next` (commands/next.rs:49-157) must filter out issues whose `scheduled_at` is in the future (not yet actionable), boost overdue issues (`due_at < now`) by +100, and print a warning when `due_at` is within 1 day.
- REQ-5: `crosslink show` (commands/show.rs) must display `Scheduled` and `Due` fields when set, formatted as `YYYY-MM-DD`.
- REQ-6: Date parsing convention: `YYYY-MM-DD` for `--scheduled` parses to `T00:00:00Z` (start of day); for `--due` parses to `T23:59:59Z` (end of day). Full RFC 3339 input is parsed directly.
- REQ-7: Subissue constraint: reject `--scheduled` or `--due` when `--parent` is also set, with error: "Scheduling dates apply to parent issues, not subissues."
- REQ-8: Full backward compatibility via `#[serde(default)]` — existing issues, event logs, and checkpoint state without these fields must continue to work.
- REQ-9: SharedWriter::update_issue (shared_writer/mutations.rs:163-206) must support setting/clearing scheduling fields alongside title/description/priority updates.
- REQ-10: Hydration (hydration.rs:240-252) must pass scheduling fields from IssueFile through HydratedIssue into SQLite.

## Acceptance Criteria
- [ ] AC-1: `crosslink create "title" --scheduled 2026-03-20 --due 2026-03-25` creates an issue with `scheduled_at` stored as `2026-03-20T00:00:00Z` and `due_at` as `2026-03-25T23:59:59Z`. (REQ-2, REQ-6)
- [ ] AC-2: `crosslink quick "title" -p high --due 2026-03-25` creates an issue with due date set and scheduled_at as None. (REQ-2, REQ-6)
- [ ] AC-3: `crosslink update <id> --scheduled 2026-03-20` sets scheduled_at without changing due_at. (REQ-2, REQ-9)
- [ ] AC-4: `crosslink update <id> --no-due` clears due_at. `--due X` and `--no-due` simultaneously is rejected by clap. (REQ-3)
- [ ] AC-5: `crosslink show <id>` on an issue with both dates prints `Scheduled: 2026-03-20` and `Due: 2026-03-25`. (REQ-5)
- [ ] AC-6: `crosslink show <id>` on an issue with no scheduling dates does not print Scheduled/Due lines. (REQ-5, REQ-8)
- [ ] AC-7: `crosslink next` with an issue whose `scheduled_at` is tomorrow excludes it. (REQ-4)
- [ ] AC-8: `crosslink next` with a `medium` overdue issue and `medium` non-overdue issue ranks the overdue one higher (score 300 vs 200). (REQ-4)
- [ ] AC-9: `crosslink next` with an issue due in 6 hours prints `Due in 6 hours` alongside the recommendation. (REQ-4)
- [ ] AC-10: Scheduling dates survive a full round-trip: create -> SQLite -> show — dates match. (REQ-1, REQ-10)
- [ ] AC-11: Deserializing an existing Issue JSON without scheduling fields succeeds with both defaulting to None. (REQ-8)
- [ ] AC-12: `crosslink create "subtask" --parent 1 --due 2026-03-25` is rejected with error. (REQ-7)
- [ ] AC-13: `crosslink next` with no-date issues includes them (dateless issues always eligible). (REQ-4, REQ-8)
- [ ] AC-14: `crosslink create "title" --due 2026-03-20T14:00:00Z` stores exactly `2026-03-20T14:00:00Z`, not adjusted. (REQ-6)
- [ ] AC-15: Schema migration v15->v16 adds `scheduled_at TEXT` and `due_at TEXT` columns to `issues` table. (REQ-1)

## Architecture

### Data Model Changes (Layer 1-4)

All four issue representations gain the same two optional fields with `#[serde(default, skip_serializing_if = "Option::is_none")]`:

- **Issue** struct in `crosslink/src/models.rs:5-15` — add after `closed_at`
- **IssueFile** in `crosslink/src/issue_file.rs:13-45` — add after `closed_at` (line 29)
- **CompactIssue** in `crosslink/src/checkpoint.rs:63-87` — add after `closed_at` (line 78)
- **HydratedIssue** in `crosslink/src/db/hydration.rs:8-20` — add `scheduled_at: Option<&'a str>` and `due_at: Option<&'a str>` after `closed_at`

### Database Schema (Layer 5)

Increment `SCHEMA_VERSION` from 15 to 16 in `crosslink/src/db/core.rs:5`. Add migration:
```sql
ALTER TABLE issues ADD COLUMN scheduled_at TEXT;
ALTER TABLE issues ADD COLUMN due_at TEXT;
```

Update `insert_hydrated_issue` in `crosslink/src/db/hydration.rs:55-62` to include the two new columns in the INSERT statement.

Update `create_issue_with_parent` in `crosslink/src/db/issues.rs:33-62` to accept and insert optional scheduling params.

Update `get_issue` / row parsing in `crosslink/src/db/issues.rs:98-105` to read the two new columns.

Update `update_issue` in `crosslink/src/db/issues.rs:189-243` to accept optional scheduling params and include them in the SET clause.

### Hydration (Layer 6)

In `crosslink/src/hydration.rs`, the hydration loop (around line 240) constructs `HydratedIssue` from `IssueFile`. Add:
```rust
let scheduled_at = issue.scheduled_at.map(|dt| dt.to_rfc3339());
let due_at = issue.due_at.map(|dt| dt.to_rfc3339());
```
Pass `scheduled_at.as_deref()` and `due_at.as_deref()` to the `HydratedIssue`.

### SharedWriter (Layer 7)

**`create_issue`** in `crosslink/src/shared_writer/mutations.rs:18-86`: Add optional `scheduled_at: Option<DateTime<Utc>>` and `due_at: Option<DateTime<Utc>>` parameters. Set on the `IssueFile` struct (line 38-56).

**`create_subissue`** in `crosslink/src/shared_writer/mutations.rs:91-160`: No changes — subissues don't support scheduling (REQ-7).

**`update_issue`** in `crosslink/src/shared_writer/mutations.rs:163-206`: Add optional scheduling params. The existing pattern does direct file mutation (no events), so scheduling follows the same approach: read issue file, merge fields, write back, push.

### CLI (Layer 8)

Add to `Create` and `Quick` structs in `crosslink/src/main.rs`:
```rust
#[arg(long, value_parser = parse_scheduled_date)]
scheduled: Option<DateTime<Utc>>,
#[arg(long, value_parser = parse_due_date)]
due: Option<DateTime<Utc>>,
```

Add to `Update` struct:
```rust
#[arg(long, value_parser = parse_scheduled_date)]
scheduled: Option<DateTime<Utc>>,
#[arg(long, conflicts_with = "scheduled")]
no_scheduled: bool,
#[arg(long, value_parser = parse_due_date)]
due: Option<DateTime<Utc>>,
#[arg(long, conflicts_with = "due")]
no_due: bool,
```

Two parser functions in a shared location (e.g. `crosslink/src/utils.rs` or inline in `main.rs`):
- `parse_scheduled_date`: `YYYY-MM-DD` -> `T00:00:00Z`, or parse as RFC 3339
- `parse_due_date`: `YYYY-MM-DD` -> `T23:59:59Z`, or parse as RFC 3339

### Commands (Layer 9)

**`commands/create.rs`** (`crosslink/src/commands/create.rs:115-197`): Validate REQ-7 (reject --parent + scheduling). Pass scheduling params through to `SharedWriter::create_issue` or `db.create_issue_with_parent`. Warn if `scheduled > due`.

**`commands/update.rs`** (`crosslink/src/commands/update.rs:8-39`): Extend "at least one field required" check at line 16. Compute new scheduling state (set/clear/unchanged) and pass to `SharedWriter::update_issue`.

**`commands/show.rs`** (`crosslink/src/commands/show.rs`): After the `Updated:` line, print:
```
Scheduled: 2026-03-20
Due:       2026-03-25
```
Only when set. Format as `%Y-%m-%d`.

**`commands/next.rs`** (`crosslink/src/commands/next.rs:49-157`):
1. After lock filter (line 77): skip if `scheduled_at > now`
2. In scoring (line 88): add +100 if `due_at < now` (overdue)
3. In output (line 114-135): print `Due in X hours/days` if `due_at` within 1 day

### Test Updates

Every test that constructs an `Issue` struct directly (models.rs tests, prop tests, various command tests) needs `scheduled_at: None, due_at: None` added. A `grep -r "Issue {" --include="*.rs"` will find all sites. The `#[serde(default)]` annotation handles deserialization backward compat automatically.

New unit tests needed:
- Date parser tests (YYYY-MM-DD vs RFC 3339, start-of-day vs end-of-day)
- `next.rs` scoring with scheduling (overdue boost, future filter, deadline warning)
- Show formatting with/without dates
- Create + update with scheduling params
- Schema migration v16

## Open Questions

### Q1: Event emission for scheduling changes — RESOLVED

**Decision**: Option A — direct file mutation, no event. Consistent with existing `update_issue` pattern for title/description/priority. Git history on the hub branch provides the audit trail. Event emission can be added later as a separate enhancement for all update types.

## Out of Scope
- Filtering `issue list` by scheduled/due dates
- Recurring / repeating schedule patterns
- Timezone-aware scheduling (all dates are UTC)
- Calendar integration or external notifications
- Relative date input (`+3d`, `tomorrow`)
- Subissue-level scheduling dates (inherited from parent via `next` lookup)
- Event emission for scheduling changes (deferred per Q1 unless team decides otherwise)
