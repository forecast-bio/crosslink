---
title: ADR: Full-System Adversarial Review Findings
tags: [adr, architecture]
sources:
  - url: ADR.md
    title: 
    accessed_at: 2026-03-11
contributors: [maxine--basel--gh-287-and-gh-291]
created: 2026-03-11
updated: 2026-03-11
---

# ADR: Full-System Adversarial Review Findings

**Date**: 2026-02-28
**Status**: Proposed
**Issue**: crosslink #35

## Context

A full-system adversarial review was performed across the entire crosslink codebase. Five parallel review streams covered: database layer, identity/signing, daemon/sync, knowledge/input-validation, and dependencies/CI/fuzz. Two critical issues were fixed immediately (path traversal in knowledge slugs, Unicode panic in truncate). All remaining findings are documented below as architectural recommendations for discussion.

## Fixes Already Applied

| # | Severity | File | Fix |
|---|----------|------|-----|
| F1 | CRITICAL | `knowledge.rs` | Added `safe_page_path()` validation to prevent path traversal via `../` in knowledge page slugs. Defense-in-depth with canonicalization check. 6 new tests. |
| F2 | HIGH | `commands/knowledge.rs` | Replaced byte-based `&s[..n]` truncation (panics on multi-byte Unicode) with `crate::utils::truncate` which uses `.chars().take()`. |

**Commit**: `5be0eca` on `feature/full-system-adversarial-review`

---

## Recommendations: Security — Signing & Trust

*Files: `signing.rs`, `sync.rs`, `commands/trust.rs`*

### S1 — SSH keys generated without passphrase (HIGH)

**Finding**: `generate_agent_key()` in `signing.rs:68` uses `-N ""` (empty passphrase). Any process with filesystem access to `.crosslink/keys/` can use the key.

**Recommendation**: Consider encrypted key storage or at minimum document the risk and ensure `.crosslink/keys/` directory has mode 700. Verify file permissions after key generation with `std::fs::set_permissions()`.

### S2 — allowed_signers has no cryptographic integrity protection (HIGH)

**Finding**: The `trust/allowed_signers` file is stored in git but has no signature verification of its own. An attacker who can merge a PR could add their own public key.

**Recommendation**: Require that changes to `trust/allowed_signers` are made via signed commits by a designated admin. Consider adding a detached signature file (`allowed_signers.sig`).

### S3 — No key revocation or rotation mechanism (HIGH)

**Finding**: Once a key is added to `allowed_signers`, it remains valid forever. No way to say "this key is no longer trusted as of timestamp X."

**Recommendation**: Add `expires_at` field to trust entries. Implement key revocation lists. Require periodic re-approval.

### S4 — verify_content() only checks exit code (MEDIUM)

**Finding**: `signing.rs:482` returns `output.status.success()` without parsing ssh-keygen's stdout/stderr to confirm the principal in the verification output matches expectations.

**Recommendation**: Parse stderr to confirm "Good signature" message and extract the verified principal. Add timeout guards around ssh-keygen execution.

### S5 — AllowedSigners parsing is overly permissive (MEDIUM)

**Finding**: `signing.rs:253-271` splits on first space and accepts anything as a public key. No validation of key format or principal format.

**Recommendation**: Validate public key starts with `ssh-` or `ecdsa-`. Validate principal matches `^[a-z0-9_-]+@crosslink$`. Return errors on malformed entries instead of silently skipping.

### S6 — No audit logging of signature verification results (MEDIUM)

**Finding**: `sync.rs:390-441` counts verified/failed/unsigned but doesn't log which specific signatures failed or why.

**Recommendation**: Log each failed verification with: comment ID, author, expected principal, actual error. Consider logging all checks for audit trail.

### S7 — Trust modifications don't record approver metadata (MEDIUM)

**Finding**: When agents are approved/revoked in `commands/trust.rs`, the only record is a git commit message. No structured metadata about who approved and when.

**Recommendation**: Add `approved_at: DateTime<Utc>` and `approved_by: String` to trust entries or store as comments in the allowed_signers file.

---

## Recommendations: Concurrency & Data Integrity

*Files: `sync.rs`, `shared_writer.rs`, `daemon.rs`, `issue_file.rs`*

### C1 — TOCTOU race condition in lock claiming (HIGH)

**Finding**: `sync.rs:593-629` — `claim_lock()` reads locks.json, checks if lock exists, modifies in memory, writes and pushes. Between read and write, another agent could claim the same lock.

**Recommendation**: Use git merge conflict detection as the atomic locking mechanism. If two agents push conflicting lock claims, the second push will fail and trigger retry.

### C2 — Display ID collision under concurrent creation (HIGH)

**Finding**: `shared_writer.rs:136-188` — Two agents creating issues concurrently could read the same counter value from `counters.json` and claim the same display ID. The retry loop handles push conflicts but the initial ID claim is not atomic.

**Recommendation**: Accept that IDs may need reassignment during conflict resolution. Add a uniqueness check during hydration.

### C3 — Daemon error handling is insufficient (HIGH)

**Finding**: `daemon.rs:174-238` — Database open failures and sync init failures are silently swallowed with `if let Ok(...)`. The daemon appears to run normally while doing nothing.

**Recommendation**: Add error counters with exponential backoff. Log warnings. Alert user after N consecutive failures (e.g., write a status file).

### C4 — Daemon has no signal handler for graceful shutdown (HIGH)

**Finding**: `daemon.rs` only monitors stdin closure. SIGTERM/SIGINT cause immediate termination without flushing pending writes or updating session state.

**Recommendation**: Register signal handlers using `signal-hook` crate or `ctrlc` crate for graceful shutdown coordination.

### C5 — File writes are not atomic (MEDIUM)

**Finding**: `issue_file.rs:165-169` and `shared_writer.rs` use `std::fs::write()` which is not atomic. Interrupted writes corrupt JSON files (partial content, truncated braces).

**Recommendation**: Use write-to-temp-then-atomic-rename pattern: write to `file.tmp`, then `std::fs::rename("file.tmp", "file.json")`.

### C6 — Stale lock detection vulnerable to clock skew (MEDIUM)

**Finding**: `sync.rs:569-571` computes `now - hb.last_heartbeat` which can underflow if the heartbeat timestamp is in the future due to clock skew between machines.

**Recommendation**: Guard against negative durations. Use `checked_sub` or clamp to zero.

---

## Recommendations: Database

*File: `db.rs`*

### D1 — Silent error suppression in schema migrations (HIGH)

**Finding**: `db.rs:202-284` — Migration errors suppressed with `let _ = self.conn.execute(...)`. If a migration fails, subsequent code assumes the schema is correct, potentially causing runtime errors or data loss.

**Recommendation**: Log migration errors. Consider failing hard on unexpected errors (not "column already exists"). Track which migrations succeeded.

### D2 — No CHECK constraints on enum columns (MEDIUM)

**Finding**: `db.rs:97-174` — Columns like `status`, `priority`, `kind` lack CHECK constraints. Any string value is accepted.

**Recommendation**: Add `CHECK (status IN ('open', 'closed', 'archived'))`, `CHECK (priority IN ('low', 'medium', 'high', 'critical'))` etc.

### D3 — Transaction rollback errors silently suppressed (MEDIUM)

**Finding**: `db.rs:76` — `let _ = self.conn.execute("ROLLBACK", [])` silently ignores rollback failures, potentially leaving the database in an inconsistent state with an open transaction.

**Recommendation**: Log rollback failures. Consider propagating them as a secondary error.

### D4 — parse_datetime silently falls back to Utc::now() (MEDIUM)

**Finding**: `db.rs:1392-1396` — If a malformed datetime string is stored in the database, it silently defaults to current time, destroying the original value.

**Recommendation**: Log a warning when datetime parsing fails. Consider storing the original string alongside the parsed value.

### D5 — UUID column nullable with UNIQUE index (LOW)

**Finding**: `db.rs:245` — `uuid TEXT` is nullable. In SQLite, NULL values are excluded from UNIQUE index checks, so multiple issues can have `uuid = NULL`.

**Recommendation**: Consider `NOT NULL` constraint or handle in application logic during hydration.

---

## Recommendations: Input Validation

*Files: `models.rs`, `knowledge.rs`, `db.rs`, `commands/import.rs`*

### V1 — No maximum length on string inputs (MEDIUM)

**Finding**: Issue titles, descriptions, comments, labels, and session notes have no length limits. Arbitrarily large strings are accepted, risking DoS via database/memory bloat.

**Recommendation**: Add reasonable limits at CLI boundary: title 500 chars, description 50KB, comment 50KB, label 100 chars.

### V2 — No enum validation for status/priority/kind fields (MEDIUM)

**Finding**: `db.rs:298-534` — Functions like `create_issue()` accept any string for priority. `"invalid-priority"` is stored without error.

**Recommendation**: Validate against known enum values at the DB boundary before insertion.

### V3 — JSON import deserialization has no size limits (MEDIUM)

**Finding**: `commands/import.rs:15-19` — `serde_json::from_str()` on imported files has no file size check. A 1GB JSON file would attempt full in-memory deserialization.

**Recommendation**: Add max file size check before parsing (e.g., 10MB). Consider streaming JSON parser for large imports.

### V4 — Agent ID allows single-character names (LOW)

**Finding**: `identity.rs:66-68` — Minimum agent ID length is 1 character. Single-char IDs reduce uniqueness.

**Recommendation**: Add minimum length of 3 characters.

---

## Recommendations: CI/CD & Testing

*Files: `.github/workflows/ci.yml`, `crosslink/fuzz/`*

### T1 — Limited fuzz target coverage (HIGH)

**Finding**: Only 6 fuzz targets exist (create_issue, search, import, dependency_graph, state_machine, cli_output). Missing critical paths: labels, comments, updates, relations, sessions, timers, export, archive, milestones.

**Recommendation**: Add fuzz targets for uncovered operations, prioritizing comment creation, label operations, and update operations.

### T2 — Fuzz CI jobs silently swallow failures (MEDIUM)

**Finding**: `ci.yml:181-191` — All fuzz commands use `continue-on-error: true`. Crashes and panics discovered by fuzzing are silently ignored in CI.

**Recommendation**: Remove `continue-on-error: true` or at minimum report fuzz failures as CI warnings. Consider a separate nightly fuzz job that fails on crashes.

### T3 — Fuzz durations too short (MEDIUM)

**Finding**: All fuzz runs limited to 30 seconds (`-max_total_time=30`). Insufficient to discover edge cases.

**Recommendation**: Increase to 300-600 seconds per target for CI, or run extended fuzzing nightly.

### T4 — No explicit permissions block on CI jobs (MEDIUM)

**Finding**: CI workflows don't declare explicit `permissions:`. Jobs inherit default GitHub Actions permissions.

**Recommendation**: Add `permissions: { contents: read }` to all jobs for least-privilege.

### T5 — Cargo dependencies use broad version ranges (MEDIUM)

**Finding**: `Cargo.toml:31-37` — Dependencies like `clap = "4"` and `rusqlite = "0.38"` allow automatic minor updates.

**Recommendation**: Use more specific version ranges. Ensure `Cargo.lock` is committed and respected in CI.

---

## Recommendations: VS Code Extension

*Files: `vscode-extension/src/extension.ts`, `vscode-extension/src/daemon.ts`*

### E1 — Binary installed without checksum verification (MEDIUM)

**Finding**: `extension.ts:682-686` — Binary copied to `~/.local/bin` with `fs.copyFileSync()` without SHA256 checksum verification.

**Recommendation**: Compute and verify SHA256 checksum before installation. Store expected checksum in extension package.

### E2 — execSync used instead of execFileSync (MEDIUM)

**Finding**: `extension.ts:763-766` — Uses `execSync()` for Python version check. While the command comes from a hardcoded array (`['python3', 'python']`), `execFileSync()` is the safer pattern.

**Recommendation**: Switch to `child_process.execFileSync('python3', ['--version'])`.

### E3 — Daemon output not sanitized (LOW)

**Finding**: `daemon.ts:71-84` — Daemon stdout piped directly to VS Code OutputChannel without stripping ANSI escape codes or control sequences.

**Recommendation**: Strip ANSI codes before displaying. Use a sanitization function on daemon output lines.

---

## Findings Triaged as Non-Issues

These were flagged by review agents but determined to be non-issues after manual verification:

| Finding | Why Not an Issue |
|---------|-----------------|
| Export path allows arbitrary filesystem writes | CLI tool — user already has filesystem access. This is expected behavior. |
| Symlink attacks on issue files in `.crosslink/` | Local-only directory owned by user. Same threat model as user editing files directly. |
| Principal derivation bypasses signature verification | Incorrect analysis — `ssh-keygen -Y verify -I principal` does verify the principal matches the signature. |
| Database file permissions not explicitly set | SQLite creates files with umask-controlled permissions. Standard for CLI tools. |
| No rate limiting on signature verification | Batch operation, not exposed to untrusted callers. Performance concern only at scale. |

---

## Decision

**Accepted for immediate fix**: F1 (path traversal), F2 (Unicode panic) — committed.

**Remaining items**: To be reviewed individually. Each recommendation should be evaluated for cost/benefit before implementation. Priority should be given to:
1. High-severity security items (S1-S3, C1, C3-C4, D1)
2. Fuzz coverage gaps (T1)
3. Medium-severity hardening (remainder)
