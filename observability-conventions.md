---
title: "Logging and Observability Conventions"
tags: [conventions, observability]
sources: []
contributors: [maxine--basel]
created: 2026-03-17
updated: 2026-03-17
---

# Logging and Observability Conventions

Established from adversarial review v1 (2026-03-16, GH issue #364).

## Logging Stack

- **Crate**: `tracing` + `tracing-subscriber`
- **Rationale**: Native tokio/axum integration, structured spans for multi-agent debugging, async-aware
- **CLI default**: `warn` level, compact single-line formatter to stderr
- **Server default**: `info` level, JSON formatter (machine-parseable for log aggregation)
- **Override**: `crosslink --log-level debug <command>`

## Rules

### No `eprintln!` in Production Code
All diagnostic output must route through `tracing`. This ensures:
- Log levels are filterable
- Daemon/server output is machine-parseable
- Multi-agent debugging has correlation context via spans

### Use Spans for Context
```rust
let _span = tracing::info_span!("sync", agent_id = %agent_id).entered();
// All tracing calls within this scope include agent_id
```

### Level Guide
- `error!` — Operation failed and caller needs to handle it (also propagate via Result)
- `warn!` — Something unexpected happened but we recovered or it's best-effort
- `info!` — Significant state transitions (session start/end, sync complete, migration applied)
- `debug!` — Detailed operation progress (git commands, SQL queries, file I/O)
- `trace!` — Verbose internals (event serialization, CRDT merge steps)

## Call Sites to Migrate (March 2026)

18+ `eprintln!` sites across:
- `db.rs` (3): migration warnings, rollback failure
- `daemon.rs` (9+): PID file ops, signal handling, lifecycle
- `commands/create.rs` (4): lock operation warnings
- `commands/session.rs` (3): handoff sync warnings
- `shared_writer.rs` (2): push failure warnings
- `server/watcher.rs` (5+): file watcher diagnostics
