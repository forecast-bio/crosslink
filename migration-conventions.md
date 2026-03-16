---
title: "Schema Migration Conventions"
tags: [conventions, database]
sources: []
contributors: [maxine--basel]
created: 2026-03-16
updated: 2026-03-16
---

# Schema Migration Conventions

Established from adversarial review v1 (2026-03-16, GH issue #364).

## Two-Era Migration System

### Legacy (v1-v15): Do Not Touch
- Uses `migrate()` and `migrate_batch()` helpers that silently ignore "duplicate column" / "already exists" errors
- Version-gated with `if version < N` guards (v7+)
- Battle-tested through 15 schema versions — do not modify
- The "ignore duplicate column" heuristic is defense-in-depth for early migrations (v1-v6) that lack version guards

### Modern (v16+): Proper Runner
- Use `run_migration(version, migration_fn)` which:
  - Wraps each migration in its own transaction
  - Propagates errors (no silent ignoring)
  - Bumps `user_version` per-step (not one big jump at the end)
  - Rolls back on failure

### Pattern for New Migrations

```rust
// In init_schema(), after all v1-v15 legacy migrations:
if version < 16 { self.run_migration(16, Self::migrate_v16)?; }
if version < 17 { self.run_migration(17, Self::migrate_v17)?; }
```

Each migration function:
```rust
fn migrate_v16(conn: &Connection) -> Result<()> {
    conn.execute("ALTER TABLE ...", [])?;
    Ok(())
}
```

## Version Read

Always read schema version via:
```rust
self.conn.pragma_query_value(None, "user_version", |row| row.get(0))?
```

Never use `.unwrap_or(0)` — this masks errors and causes all migrations to re-run, which is how the v7/sessions_new bug happened.

## Testing

- `test_migration_idempotent`: Run all migrations twice on same DB, assert identical schema
- `test_fresh_schema_matches_migrated`: Compare fresh vN schema to one migrated from v1 through all versions
- Run these in CI to catch migration regressions
