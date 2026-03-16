---
title: "Testing Strategy and Coverage Map"
tags: [conventions, testing]
sources: []
contributors: [maxine--basel]
created: 2026-03-16
updated: 2026-03-16
---

# Testing Strategy and Coverage Map

Established from adversarial review v1 (2026-03-16, GH issue #364).

## Test Tiers

### Tier 1: Unit Tests (inline `mod tests`)
- Pure logic: validators, parsers, data transformations
- Database operations against in-memory SQLite
- Already strong in: db.rs, utils.rs, clock_skew.rs, pipeline.rs, hydration.rs, orchestrator/

### Tier 2: Smoke Tests (`tests/smoke/`)
- CLI binary invocation via SmokeHarness
- Full roundtrip: create -> show -> update -> close -> list
- Multi-agent coordination via SmokeHarness::fork_agent()

### Tier 3: Adversarial Tests (new, `tests/smoke/adversarial_coordination.rs`)
- Two-agent same-issue write conflict -> convergence verification
- Clock-skewed agent sync -> total ordering resolution
- Hub branch corruption -> recovery via re-init
- Stale lock steal under contention -> stolen lock detection
- Event log divergence -> compaction consistency

### Tier 4: Concurrency Tests (new, `tests/smoke/concurrency.rs`)
- N-concurrent API requests -> unique ID verification
- Multi-threaded database writes -> no SQLITE_BUSY or data loss
- Parallel lock claims -> exactly-one-wins semantics

### Tier 5: Network Partition Tests (new, in adversarial_coordination.rs)
- Offline local operations succeed when remote unavailable
- Offline divergence + sync recovery after reconnect
- Split-brain lock detection across partitioned agents

## Coverage Gaps (March 2026)

| Area | Status | Gap |
|------|--------|-----|
| Core CLI (create, show, list, close) | Good | — |
| Multi-agent sync | Basic | No conflict/corruption scenarios |
| kickoff lifecycle | Fuzz only | No run->status->logs->stop->cleanup |
| swarm orchestration | Fuzz only | No init->launch->gate->merge |
| daemon start/stop | None | No PID file or signal tests |
| intervene | Minimal | No trigger type coverage |
| timer | Minimal | No start->stop->show roundtrip |
| Server API | Basic | No concurrency tests |
| WebSocket | Connectivity only | No backpressure/reconnection |
| Dashboard (React) | None | No test framework configured |
| VS Code extension | None | Separate workstream |

## Conventions for New Tests

- Every new command should have a smoke test in `tests/smoke/`
- Adversarial scenarios go in `adversarial_coordination.rs`
- Concurrency scenarios go in `concurrency.rs`
- Use `SmokeHarness` for CLI tests, `SmokeHarness::fork_agent()` for multi-agent
- Inline `mod tests` for pure logic; smoke tests for integration
