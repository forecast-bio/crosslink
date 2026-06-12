# Feature: Hub v3 — Per-Agent Refs and Pure Event Sourcing

## Summary

Restructure the hub from a single shared `crosslink/hub` branch containing mutable files into per-agent git refs carrying append-only event logs only. Each agent writes exclusively to its own ref (`refs/crosslink/agents/<agent-id>`), making every push a guaranteed fast-forward. All shared state — issues, display IDs, locks — is derived deterministically from the union of agent event logs via the existing OrderingKey reduction. The mutable files that have caused every recorded hub corruption incident (`meta/counters.json`, `locks.json`, V1 flat issue JSON) are eliminated, and with them the rebase-retry, conflict-recovery, and dirty-state-repair machinery they require. Git remains the only infrastructure; GitHub (or any git remote) remains a dumb transport whose sole consistency primitive — atomic compare-and-swap on ref updates — is the only one this design needs.

## Motivation

Twelve distinct hub corruption classes were fixed across v0.5.1–v0.5.2 (GH#427, GH#428, GH#430, GH#443, GH#450, GH#451, GH#528, GH#574, GH#602, GH#604, CL-372, plus TUI corruption GH#447). A 2026-06 audit found five further live defects (non-unique `atomic_write` temp names, live-holder lock force-steal, disjoint lock domains between compaction and the write path, double-mint display ID window in rebase-retry, silent issue drop in hydration dedup). Every incident traces to one of two structural causes:

1. **Mutable shared files on a multi-writer branch.** `counters.json` and `locks.json` require linearizable read-modify-write; git provides optimistic concurrency with seconds of latency. The conflict pressure this creates is what the rebase-retry and `clean_dirty_state` machinery exists to manage, and that machinery has itself caused corruption.
2. **A single shared worktree mutated by concurrent local processes.** Git's index is not safe under concurrent mutation; the recovery code that commits "whatever is in the working tree" turns transient races into durable corruption.

The append-only event-log half of the current design (V2 path) has not been implicated in any incident. Hub v3 keeps it and removes everything else.

## Requirements

- REQ-1: Each agent writes events exclusively to its own ref `refs/crosslink/agents/<agent-id>`, containing a single append-only NDJSON event log (`events.log`) plus the agent's public key and metadata. Pushes to this ref MUST always be fast-forward; any non-fast-forward push outcome is a hard error (indicates identity collision or history tampering), never silently rebased.
- REQ-2: Writes use git plumbing (`hash-object`, `mktree`, `commit-tree`, `update-ref`) against the local repository object store — no shared worktree, no index, no checkout. The `.crosslink/.hub-cache/` worktree is eliminated.
- REQ-3: Reads fetch `refs/crosslink/*` via an explicit refspec into `refs/crosslink-remote/*` tracking refs and materialize state in memory using the existing `OrderingKey` `(timestamp, agent_id, agent_seq)` total order and compaction reduction rules. Materialized state hydrates SQLite exactly as today.
- REQ-4: `meta/counters.json` is eliminated. Display IDs are assigned solely by the deterministic reduction, preserving the existing `CheckpointState.display_id_map` freeze semantics: once a checkpoint's watermark covers an `IssueCreated` event, its display ID is frozen; late-arriving events with earlier OrderingKeys receive IDs above the frozen maximum. Before checkpoint coverage, the CLI shows the locally computed ID marked provisional (suffix `~`), or the short UUID where no local computation is possible.
- REQ-5: `locks.json` and `sync/locks.rs` are eliminated. Locks are pure T1 `LockClaimed`/`LockReleased` events resolved first-claim-wins by OrderingKey (current V2 semantics). The claim-confirm protocol is: emit claim → push own ref → fetch all agent refs → verify self is the deterministic winner → proceed or back off. Confirm timeout remains 30 seconds (design doc v2 section 8).
- REQ-6: Lock enforcement model is unchanged and documented: (a) cooperative gate — `lock_check`/PreToolUse hooks read materialized state and block work on `LockedByOther`; (b) stampede resolution — first-claim-wins yields exactly one winner per issue deterministically, with no state in which two agents both verify ownership; (c) rogue-agent backstop — signed events + `allowed_signers` attribution, `trust revoke`, worktree isolation, and the merge gate. No hub data structure is treated as enforcement against a non-cooperating agent.
- REQ-7: The checkpoint is a pure cache on `refs/crosslink/checkpoint`, written by whichever process compacts, pushed with `--force-with-lease`. Concurrent compactions are harmless: both produce the same deterministic state for the same event set; the lease loser refetches and either fast-forwards or discards its identical result. `checkpoint/compaction.lock` (cross-machine lease) is retained only as a politeness optimization to avoid duplicate work.
- REQ-8: Local same-machine concurrency is serialized by exactly one mandatory lock: a per-repository `flock` (or lock-file with live-holder respect, per #750) held across any hub read-modify-write sequence. There are no other lock domains.
- REQ-9: Migration: `crosslink migrate hub-v3` performs a one-shot conversion — replay the current materialized hub state into a genesis checkpoint on `refs/crosslink/checkpoint` plus per-agent genesis refs seeded from existing `agents/{id}/events.log` files. The old `crosslink/hub` branch is left untouched (read-only escape hatch) and deleted only by explicit `crosslink migrate hub-v3 --finalize`. Mixed-version operation is refused: v3 clients detect a v2 hub and prompt to migrate; v2 clients detect the v3 marker ref and refuse with an upgrade message.
- REQ-10: All v2 conflict machinery is deleted, not deprecated: `recover_from_push_conflict`, rebase-retry loops, `clean_dirty_state` conflict-marker repair, `hub_health_check` worktree repair, `reconcile_display_counter`, `dedup_issue_files` display-ID collision handling, V1 layout support, and the `verify_cache_worktree` walk-up guard (no worktree remains to walk up from). **DELIVERED (754b):** also deleted — the v2 push paths (`push_hub_if_ahead`, `commit_and_push_locks`, `rebase_preserving_local`, `check_divergence`/`count_unpushed_commits`/`MAX_DIVERGENCE`, the `PushFailure` classifier), `locks.json` read+write machinery (`read_locks`/`read_locks_v2`/`claim_lock`/`release_lock`/`find_stale_locks_v2`), V1 heartbeat support (`read_heartbeats`, the `heartbeats/*.json` write path, `ensure_agent_dir`/`create_agent_dir_files`), `upgrade_to_v2`/`cleanup_stale_layout_files`/`migrate_inline_comments_to_v2`, the `.gitignore` self-heal (`ensure_hub_gitignore`), the v2 hub-commit signing-enforcement (`verify_recent_commits`/`verify_entry_signatures`/`read_keyring`/`Keyring`/`is_bootstrap_message`). New hubs **bootstrap directly as v3** (`hub_v3::bootstrap_v3_hub`, wired at the `SyncManager::init_cache` creation seam); a fresh clone of an already-migrated remote **joins** via `fetch_v3_refs_for_join` instead of minting a conflicting genesis. The v2 `fetch` is reduced to a read-only mirror update (`fetch_v2_readonly`) for inspection + migration; it never writes to the v2 branch. **Kept for the migration** (`crosslink migrate hub-v3`): v2 issue/comment/milestone/counter file READING, `compaction::compact`/`materialize`, `hydrate_to_sqlite`, the cache worktree as a migration-time artifact, `read_heartbeats_v2` for inspecting a frozen hub, and `verify_locks_signature`/`verify_commit_signature` (still surfaced by the dashboard).
- REQ-11: Event log growth is bounded by the existing prune path: once a checkpoint watermark covers events, agents MAY rewrite their own ref to drop covered events (single-writer ref, so rewrite is safe; readers treat the checkpoint as authoritative below its watermark).
- REQ-12: Remote compatibility: works against any git remote supporting custom refs (GitHub, GitLab, Gitea, bare SSH). Crosslink configures its own fetch/push refspecs; no reliance on default branch refspecs or remote UI visibility.
- REQ-13: Extension point (non-goal for v3.0): an optional coordination server MAY later grant sub-second lock claims, recorded as ordinary events for durability. The git path remains the source of truth and the only required infrastructure.

## Acceptance Criteria

- [ ] AC-1: Two agents on separate clones create issues concurrently for 100 iterations; zero push conflicts occur, zero rebases are executed, and after mutual fetch + compaction both materialize identical state including identical display IDs. (REQ-1, REQ-3, REQ-4)
- [ ] AC-2: `kill -9` during any write sequence leaves the local repo and remote refs in a state from which the next command proceeds with no repair step and no data loss — verified by a crash-injection harness over the plumbing write path. (REQ-2)
- [ ] AC-3: Ten agents race `lock claim` on one issue; exactly one verifies ownership after confirm; the other nine receive `LockedByOther` naming the winner. Repeated 50 times with shuffled timing. (REQ-5, REQ-6)
- [ ] AC-4: An agent's push to its own ref that would be non-fast-forward (simulated by moving the remote ref) fails hard with a diagnostic naming the ref — no automatic rebase, no force. (REQ-1)
- [ ] AC-5: Two machines compact concurrently from the same event set; both checkpoints are byte-identical; the force-with-lease loser recovers without error or state change. (REQ-7)
- [ ] AC-6: `crosslink migrate hub-v3` on a populated v2 hub preserves every issue, comment, label, dependency, relation, milestone, time entry, and display ID, verified by full-state diff against pre-migration hydration. Old branch remains readable until `--finalize`. (REQ-9)
- [ ] AC-7: A v0.5.x client pointed at a v3 hub refuses with an upgrade message; a v3 client pointed at a v2 hub prompts to migrate and performs no writes. (REQ-9)
- [ ] AC-8: After prune, an agent's ref contains only post-watermark events, and a fresh clone materializes correct full state from checkpoint + remaining events. (REQ-11)
- [ ] AC-9: Daemon, TUI, `serve`, and a foreground CLI command run simultaneously against one repository under a write-heavy loop with no corruption and no lock-steal, serialized only by the single REQ-8 lock. (REQ-8)
- [ ] AC-10: `grep -r "recover_from_push_conflict\|clean_dirty_state\|reconcile_display_counter" src/` returns nothing; net diffstat of the migration PR series is negative. (REQ-10)
- [ ] AC-11: Full smoke suite passes against a GitHub remote and a bare local remote, exercising the custom refspecs. (REQ-12)

## Architecture

### Ref layout

```
refs/crosslink/agents/<agent-id>     one writer (that agent); append-only events.log + identity
refs/crosslink/checkpoint            any compactor; force-with-lease; pure cache of reduction
refs/crosslink/meta                  hub version marker, allowed_signers (driver-written, CAS)
```

Fetch refspec: `+refs/crosslink/*:refs/crosslink-remote/*`. Push refspec per agent: `refs/crosslink/agents/<id>:refs/crosslink/agents/<id>` (no `+`; fast-forward enforced by the remote — this is the CAS).

### Write path (replaces SharedWriter commit/push/rebase machinery)

1. Acquire the single local lock (REQ-8).
2. Append event(s) to the in-memory log; build the new `events.log` blob via `hash-object -w`.
3. `mktree` → `commit-tree` with parent = current own-ref tip → `update-ref refs/crosslink/agents/<id>`.
4. Push own ref. Success is unconditional under correct operation; failure is diagnostic, never triggers rebase.
5. Release lock. Fetch + materialize opportunistically (or on next read).

No index, no worktree, no `git add`, no working-tree files to corrupt or recover.

### Read path

Fetch `refs/crosslink/*` → load checkpoint → stream events above its watermark from each agent ref (via `cat-file`) → reduce with existing compaction rules → hydrate SQLite. The reduction code (`compaction.rs` reducer, `CheckpointState`, `OrderingKey`) is reused nearly verbatim; only its I/O changes from worktree files to object-store reads.

### Display IDs without a counter

The existing `display_id_map` + watermark freeze in `CheckpointState` already implements deterministic, stable-once-frozen assignment. v3 makes it the only path by deleting the `counters.json` fast path. UX cost: an issue created offline shows `#~42` (provisional) until its event is covered by a pushed checkpoint — in connected operation this is seconds. UUIDs remain the true identity throughout, as today.

### Locks and the enforcement model

First-claim-wins by OrderingKey, exactly one winner, computed identically by every reader — this is current V2 compaction semantics promoted to the only semantics. The claim-confirm round trip bounds the mutual-exclusion window to push+fetch latency (single-digit seconds), identical to v2's effective window but without the v2 defect where rebase-merge of `locks.json` lets two claimants both verify ownership. Enforcement against rogue agents remains layered (hook gate, signed attribution, revocation, merge gate) per REQ-6; this is explicitly documented as the trust boundary.

### What gets deleted

`sync/locks.rs`; `recover_from_push_conflict`; the rebase-retry loop in `shared_writer/core.rs`; `clean_dirty_state` conflict repair; `hub_health_check`; `verify_cache_worktree`; V1 layout support and `issue_rel_path` dual-format handling; `reconcile_display_counter`; `dedup_issue_files`; `meta/counters.json` and `locks.json` schemas; the hub-cache worktree lifecycle (`init_cache`, gitignore self-heal, walk-up guards). Estimated net diffstat strongly negative.

### Migration sequencing

1. PR 1: read-side abstraction — materialization reads through a `HubSource` trait (worktree-file impl + object-store impl), proving the reducer against both.
2. PR 2: plumbing write path behind `hub_v3` config flag; dual-write soak in this repository.
3. PR 3: `crosslink migrate hub-v3` + version detection/refusal logic.
4. PR 4: flip default, delete v2 machinery (REQ-10), `--finalize`.

### Risks

- **Ref proliferation**: one ref per agent ever registered. Mitigated by archiving refs for agents inactive past a threshold (`crosslink prune` extension).
- **Plumbing portability**: `commit-tree`/`update-ref` are stable plumbing, but Windows path/quoting needs the same care the current code already takes; covered by existing CI matrix.
- **Provisional display IDs**: visible UX change in offline-heavy workflows; mitigated by the `~` marker and immediate stabilization on first sync.
- **Migration correctness**: AC-6 full-state diff is the gate; old branch retained until explicit finalize makes rollback trivial.

## Open Questions

- OQ-1: Should agent refs live under `refs/crosslink/*` (hidden from host UIs, requires explicit refspec) or `refs/heads/crosslink/agents/*` (visible as branches, fetched by default, more clutter)? Recommendation: `refs/crosslink/*`; revisit if a popular host refuses custom ref namespaces.
- OQ-2: Knowledge base (`crosslink/knowledge` branch) is out of scope here — it is human-edited markdown where git merge semantics are appropriate. Confirm it stays on its current branch model.
- OQ-3: Minimum supported git version for the plumbing path (notably `update-ref --create-reflog` behavior); propose git >= 2.30 and document it.
