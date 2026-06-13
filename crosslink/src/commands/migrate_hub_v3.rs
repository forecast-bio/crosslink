//! `crosslink migrate hub-v3` — one-shot conversion of a v2 hub into the v3
//! per-agent-ref layout (`.design/hub-v3-per-agent-refs.md`, REQ-9).
//!
//! # Flow (Phase A — `migrate hub-v3`)
//!
//! 1. Preflight: init + fetch the hub cache, hold the hub write lock for the
//!    whole migration, refuse cleanly on no-cache / already-migrated / pending
//!    offline promotions.
//! 2. Force a compaction so v2 state is fully reduced and the watermark embedded.
//! 3. Build the GENESIS [`CheckpointState`] FROM THE FILES (authoritative
//!    materialized state) — independent of the event reducer.
//! 4. Record pre-existing tips of every v3 hub branch (for rollback),
//!    seed per-agent refs from each v2 `events.log`, commit the genesis
//!    checkpoint, and write the meta marker.
//! 5. AC-6 verification gate: reduce a fresh [`RefHubSource`] and compare it
//!    field-complete against the files + invariants.
//! 6. On any failure: roll back every v3 hub branch to its recorded
//!    pre-migration tip (the v2 branch is never touched).
//! 7. On success: push all created/updated refs and print the cutover next step.
//!
//! # Flow (Phase B — `migrate hub-v3 --finalize --yes-delete-v2`)
//!
//! Re-verifies, then deletes the legacy `crosslink/hub` branch local + remote
//! and stamps `HubMeta.finalized_at`. This is the only hard stop for already
//! deployed v2 binaries.
//!
//! # Deterministic UUIDs
//!
//! V1 inline comments and time entries carry an `i64` id but no uuid. The
//! genesis builder derives a stable uuid via
//! `Uuid::from_bytes(sha256(canonical)[0..16])` where `canonical` is
//! `"crosslink-hub-v3:<kind>:<issue_uuid>:<i64-id>"`. The scheme is stable
//! across re-runs (no randomness) and disjoint across kinds.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::checkpoint::{
    CheckpointState, CompactComment, CompactIssue, CompactMilestone, CompactTimeEntry,
};
use crate::compaction;
use crate::events::OrderingKey;
use crate::hub_source::RefHubSource;
use crate::hub_v3::{
    self, agent_ref_name, read_hub_meta, HubMeta, HubVersion, PushOutcome, CHECKPOINT_REF, META_REF,
};
use crate::issue_file::{
    read_all_issue_files, read_all_milestone_files, read_comment_files, read_counters, IssueFile,
};
use crate::sync::SyncManager;

/// Legacy v2 hub branch name (matches `hub_v3::V2_HUB_BRANCH`).
const V2_HUB_BRANCH: &str = "refs/heads/crosslink/hub";

// ── Public entry points ──────────────────────────────────────────────

/// `crosslink migrate hub-v3 [--finalize --yes-delete-v2]`.
///
/// Dispatches to Phase A (migrate) or Phase B (finalize) based on `finalize`.
///
/// # Errors
///
/// Returns an error if preflight refuses, a git plumbing step fails, or the
/// AC-6 verification gate fails (after a rollback).
pub fn hub_v3(crosslink_dir: &Path, finalize: bool, yes_delete_v2: bool) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;
    sync.fetch()?;

    // Hold the hub write lock for the whole migration: it mutates ref state and
    // must serialize against every other hub read-modify-write (REQ-8).
    let hub_lock = sync.acquire_lock()?;

    let cache_dir = sync.cache_path().to_path_buf();
    let remote = sync.remote().to_string();

    if finalize {
        finalize_migration(&cache_dir, &remote, yes_delete_v2, &hub_lock)
    } else {
        migrate_phase_a(crosslink_dir, &cache_dir, &remote, &hub_lock)
    }
}

// ── `migrate hub-branches` — visible-branch rename (#767) ────────────

/// One OLD→NEW ref rename pair for the hub-branches migration.
struct RenamePair {
    /// Old hidden ref, e.g. `refs/crosslink/checkpoint`.
    old: String,
    /// New visible branch, e.g. `refs/heads/crosslink/checkpoint`.
    new: String,
}

/// Adopt an already-migrated remote hub (#774).
///
/// Reached when the LOCAL hub is still v2 but the REMOTE was migrated to v3 by
/// another machine (the "machine that slept through the migration" path).
/// Migrating here would mint a conflicting genesis from stale local state;
/// instead, fetch the remote's v3 branches into the local namespace, verify
/// detection now reports V3, and hydrate `SQLite` from the adopted state. The
/// local `crosslink/hub` v2 branch is left untouched as the same read-only
/// escape hatch the migrating machine kept.
fn adopt_remote_v3(crosslink_dir: &Path, cache_dir: &Path, remote: &str) -> Result<()> {
    println!("remote '{remote}' already hosts a v3 hub — adopting it (no migration performed).");

    hub_v3::fetch_v3_refs_for_join(cache_dir, remote)
        .context("fetching the remote's v3 branches for adoption failed")?;

    match hub_v3::detect_hub_version(cache_dir)? {
        HubVersion::V3 { .. } => {}
        other => bail!(
            "adoption fetch completed but local detection still reports {other:?}; \
             the remote's v3 refs may be incomplete — inspect \
             `git ls-remote {remote} 'refs/heads/crosslink/*'`"
        ),
    }

    if let Some(meta) = read_hub_meta(cache_dir)? {
        print_hub_meta(&meta);
    }

    // Hydrate SQLite from the adopted state so the local DB reflects the hub
    // immediately (same reduction path `crosslink sync` uses in v3 mode).
    let db = crate::db::Database::open(&crosslink_dir.join("issues.db"))
        .context("opening issues.db for post-adoption hydration")?;
    let source = crate::hub_source::RefHubSource::new(cache_dir)?;
    let outcome = crate::compaction::reduce(&source)?;
    let stats = crate::hydration::hydrate_from_state(&outcome.state, &db)
        .context("post-adoption hydration failed")?;
    println!(
        "adopted v3 hub: {} issue(s), {} comment(s) hydrated. This machine now \
         operates v3; its agent branch is created on first write.",
        stats.issues, stats.comments
    );

    // Honesty guard: adopting an EMPTY hub while the local v2 cache holds
    // real issues means the remote's v3 genesis did not come from this
    // project's data (e.g. a fresh machine bootstrapped against a remote
    // that did not advertise the v2 branch). The local v2 data is NOT lost
    // (frozen branch), but it was not migrated either — say so loudly.
    if stats.issues == 0 {
        let v2_issue_count = read_all_issue_files(&cache_dir.join("issues")).map_or(0, |v| v.len());
        if v2_issue_count > 0 {
            println!(
                "WARNING: the adopted v3 hub is EMPTY but the local v2 hub holds \
                 {v2_issue_count} issue(s). The remote's v3 genesis did not come from this \
                 project's v2 data. Your v2 data is intact on the frozen crosslink/hub \
                 branch but has NOT been migrated — this usually means a machine \
                 bootstrapped a fresh hub against a remote that did not advertise the \
                 v2 branch. Consider deleting the remote's empty v3 branches and \
                 re-running the migration from a machine with the populated v2 hub."
            );
        }
    }
    print_mixed_version_warning();
    Ok(())
}

/// Map an old hidden hub ref to its new visible-branch name, or `None` if the
/// ref is not a hub ref this migration owns (so siblings are never renamed).
fn old_to_new_ref(old: &str) -> Option<String> {
    if old == hub_v3::OLD_CHECKPOINT_REF {
        Some(hub_v3::CHECKPOINT_REF.to_string())
    } else if old == hub_v3::OLD_META_REF {
        Some(hub_v3::META_REF.to_string())
    } else {
        old.strip_prefix(hub_v3::OLD_AGENT_REF_PREFIX)
            .map(|agent_id| format!("{}{agent_id}", hub_v3::AGENT_REF_PREFIX))
    }
}

/// `crosslink migrate hub-branches` — move the v3 hub refs to visible branches.
///
/// Idempotent, one-shot per machine (#767, correcting OQ-1). For every old hidden
/// ref present locally or on the remote (`refs/crosslink/agents/*`,
/// `refs/crosslink/checkpoint`, `refs/crosslink/meta`):
///
/// 1. Create the matching `refs/heads/crosslink/*` branch at the same SHA —
///    locally via `update-ref`, remotely via `git push <remote> <sha>:<new>`.
/// 2. Delete the old ref locally (`update-ref -d`) and remotely
///    (`git push <remote> :<old>`).
///
/// After the rename, one [`hub_v3::compact_v3`] is run so the browsable state
/// tree (#767 part 2) materializes on the now-visible checkpoint branch.
///
/// Skips cleanly when no old refs exist (already migrated / fresh hub) and
/// reports per-ref actions.
///
/// # Errors
///
/// Returns an error only if the hub cache cannot be initialized or a git
/// plumbing step fails fatally; per-ref remote-push problems are reported, not
/// propagated (re-run is the retry mechanism).
pub fn hub_branches(crosslink_dir: &Path) -> Result<()> {
    let sync = SyncManager::new(crosslink_dir)?;
    sync.init_cache()?;

    let hub_lock = sync.acquire_lock()?;
    let cache_dir = sync.cache_path().to_path_buf();
    let remote = sync.remote().to_string();
    let has_remote = sync.remote_exists();

    // Collect the OLD-namespace refs present locally and (if reachable) remotely.
    let mut old_refs: BTreeSet<String> = BTreeSet::new();
    for r in for_each_ref(&cache_dir, "refs/crosslink/*")? {
        if old_to_new_ref(&r).is_some() {
            old_refs.insert(r);
        }
    }
    if has_remote {
        match ls_remote_old_namespace(&cache_dir, &remote) {
            Ok(remote_refs) => {
                for r in remote_refs.keys() {
                    if old_to_new_ref(r).is_some() {
                        old_refs.insert(r.clone());
                    }
                }
            }
            Err(e) => {
                tracing::warn!("hub-branches: could not list remote old refs (continuing): {e}");
            }
        }
    }

    if old_refs.is_empty() {
        println!(
            "no old-namespace hub refs found — the hub is already on visible \
             branches (or is fresh). Nothing to rename."
        );
        // Still materialize the browse tree if a v3 hub is present and lacks it.
        maybe_compact_after_rename(crosslink_dir, &cache_dir, &remote, has_remote, &hub_lock);
        return Ok(());
    }

    let pairs: Vec<RenamePair> = old_refs
        .iter()
        .filter_map(|old| {
            old_to_new_ref(old).map(|new| RenamePair {
                old: old.clone(),
                new,
            })
        })
        .collect();

    println!("Renaming {} hub ref(s) to visible branches:", pairs.len());
    let remote_old = if has_remote {
        ls_remote_old_namespace(&cache_dir, &remote).unwrap_or_default()
    } else {
        BTreeMap::new()
    };

    for pair in &pairs {
        rename_one_ref(&cache_dir, &remote, has_remote, pair, &remote_old)?;
    }

    // Materialize the browse tree on the now-visible checkpoint branch.
    maybe_compact_after_rename(crosslink_dir, &cache_dir, &remote, has_remote, &hub_lock);

    print_hub_branches_summary(&cache_dir, &remote, has_remote);
    Ok(())
}

/// Rename a single ref: create the new branch at the old SHA (local + remote),
/// then delete the old ref (local + remote). CAS-guards the local create against
/// the old SHA and the remote create against the expected old remote SHA where
/// the plumbing supports it.
fn rename_one_ref(
    cache_dir: &Path,
    remote: &str,
    has_remote: bool,
    pair: &RenamePair,
    remote_old: &BTreeMap<String, String>,
) -> Result<()> {
    let local_sha = git_rev_parse(cache_dir, &pair.old)?;
    let remote_sha = remote_old.get(&pair.old).cloned();

    // The authoritative SHA to place at the new ref: prefer the local tip (the
    // single-writer ref), fall back to the remote tip if only the remote has it.
    let sha = local_sha.clone().or_else(|| remote_sha.clone());
    let Some(sha) = sha else {
        // Ref vanished between listing and now — nothing to do.
        return Ok(());
    };

    // 1. Local create (idempotent — update-ref to the same sha is a no-op).
    if local_sha.is_some() {
        let existing_new = git_rev_parse(cache_dir, &pair.new)?;
        if existing_new.as_deref() != Some(sha.as_str()) {
            git_update_ref(cache_dir, &pair.new, &sha)?;
        }
        println!("  local  {} -> {}", pair.old, pair.new);
    }

    // 2. Remote create + old-ref delete (when a remote is configured).
    if has_remote {
        // Create the new branch at the SHA. A plain push is the create; if the
        // remote already holds it at this SHA the push is a no-op.
        let create_spec = format!("{sha}:{}", pair.new);
        match run_git(cache_dir, &["push", remote, &create_spec]) {
            Ok(_) => println!("  remote {} -> {} (created)", pair.old, pair.new),
            Err(e) => {
                tracing::warn!("hub-branches: remote create of {} failed: {e}", pair.new);
                println!(
                    "  remote {} -> {}: SKIPPED (push failed: {e})",
                    pair.old, pair.new
                );
                // Do not delete the old remote ref if the new one did not land.
                // Local rename already happened; a re-run retries the remote.
            }
        }

        // Delete the old remote ref, CAS-guarded against its expected old SHA so
        // a concurrently-advanced ref is not silently dropped.
        if let Some(old_remote_sha) = &remote_sha {
            let delete_spec = format!(":{}", pair.old);
            // CAS-guard the delete against the expected old remote SHA so a
            // concurrently-advanced ref is never silently dropped.
            let lease = format!("--force-with-lease={}:{old_remote_sha}", pair.old);
            let args = ["push", &lease, remote, &delete_spec];
            match run_git(cache_dir, &args) {
                Ok(_) => println!("  remote deleted {}", pair.old),
                Err(e) => {
                    tracing::warn!("hub-branches: remote delete of {} failed: {e}", pair.old);
                    println!("  remote delete of {}: SKIPPED ({e})", pair.old);
                }
            }
        }
    }

    // 3. Delete the old LOCAL ref last, only after the new local ref exists.
    if local_sha.is_some() && git_rev_parse(cache_dir, &pair.new)?.is_some() {
        git_delete_ref(cache_dir, &pair.old)?;
    }

    Ok(())
}

/// Run one [`hub_v3::compact_v3`] so the browse tree materializes on the visible
/// checkpoint branch. Only when the hub is v3; non-fatal on failure (the rename
/// already succeeded; a later `crosslink compact` retries the tree).
fn maybe_compact_after_rename(
    crosslink_dir: &Path,
    cache_dir: &Path,
    remote: &str,
    has_remote: bool,
    hub_lock: &crate::sync::HubWriteLock,
) {
    if !matches!(
        hub_v3::detect_hub_version(cache_dir),
        Ok(HubVersion::V3 { .. })
    ) {
        return;
    }
    let agent_id = crate::identity::AgentConfig::load(crosslink_dir)
        .ok()
        .flatten()
        .map_or_else(|| "hub-v3-bootstrap".to_string(), |a| a.agent_id);
    let remote_opt = has_remote.then_some(remote);
    match hub_v3::compact_v3(cache_dir, &agent_id, hub_lock, remote_opt) {
        Ok(_) => println!("Materialized the browsable state tree on crosslink/checkpoint."),
        Err(e) => tracing::warn!(
            "hub-branches: post-rename compaction failed (non-fatal; run `crosslink compact`): {e}"
        ),
    }
}

/// `git ls-remote <remote> refs/crosslink/*` → map of OLD-namespace refname → sha.
fn ls_remote_old_namespace(repo_dir: &Path, remote: &str) -> Result<BTreeMap<String, String>> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["ls-remote", remote, "refs/crosslink/*"])
        .output()
        .with_context(|| format!("failed to run git ls-remote for '{remote}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git ls-remote failed for '{remote}': {}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut map = BTreeMap::new();
    for line in stdout.lines() {
        if let Some((sha, name)) = line.split_once('\t') {
            map.insert(name.trim().to_string(), sha.trim().to_string());
        }
    }
    Ok(map)
}

/// Print the closing summary + the GitHub web-UI hint.
fn print_hub_branches_summary(cache_dir: &Path, remote: &str, has_remote: bool) {
    println!("\nDone. The hub now lives on visible branches under crosslink/* .");
    if has_remote {
        if let Some(url) = github_branches_url(cache_dir, remote) {
            println!("Browse it on GitHub: {url}");
        } else {
            println!("Browse the crosslink/checkpoint branch on your git host's web UI.");
        }
    }
}

/// Best-effort `https://github.com/<owner>/<repo>/branches` URL from the remote
/// URL. Returns `None` for non-GitHub or unparseable remotes.
fn github_branches_url(cache_dir: &Path, remote: &str) -> Option<String> {
    let output = Command::new("git")
        .current_dir(cache_dir)
        .args(["remote", "get-url", remote])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // git@github.com:owner/repo.git  OR  https://github.com/owner/repo(.git)
    let slug = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest.to_string()
    } else if let Some(rest) = url.strip_prefix("https://github.com/") {
        rest.to_string()
    } else {
        return None;
    };
    let slug = slug.strip_suffix(".git").unwrap_or(&slug);
    Some(format!("https://github.com/{slug}/branches"))
}

// ── Phase A: migrate ─────────────────────────────────────────────────

fn migrate_phase_a(
    crosslink_dir: &Path,
    cache_dir: &Path,
    remote: &str,
    hub_lock: &crate::sync::HubWriteLock,
) -> Result<()> {
    // Preflight: refuse when there is no hub cache to migrate.
    if !cache_dir.exists() {
        bail!(
            "no hub cache at {} — nothing to migrate (run `crosslink sync` first, \
             or this repo has no shared hub)",
            cache_dir.display()
        );
    }

    // Idempotent no-op: already migrated. Re-attempt pushing any refs the remote
    // lacks (re-run is the retry mechanism), then print the marker and exit Ok.
    match hub_v3::detect_hub_version(cache_dir)? {
        HubVersion::V3 { .. } => {
            println!("hub already migrated to v3 — no migration performed.");
            if let Some(meta) = read_hub_meta(cache_dir)? {
                print_hub_meta(&meta);
            }
            retry_push_missing_refs(cache_dir, remote)?;
            print_mixed_version_warning();
            return Ok(());
        }
        HubVersion::Absent => {
            bail!(
                "no v2 hub detected at {} (neither a crosslink/hub branch nor v3 marker refs) — \
                 nothing to migrate",
                cache_dir.display()
            );
        }
        HubVersion::V2Only => {
            // The LOCAL hub is v2 — but if the REMOTE has already been
            // migrated by another machine, running a second migration here
            // would mint a conflicting genesis from this machine's stale
            // pre-migration state (#774, the machine-that-slept-through-the-
            // migration path). Consult the remote and ADOPT instead.
            match hub_v3::detect_remote_hub_version(cache_dir, remote) {
                Ok(HubVersion::V3 { .. }) => {
                    return adopt_remote_v3(crosslink_dir, cache_dir, remote);
                }
                Ok(_) => {
                    // Remote is v2 or absent — this machine performs the
                    // first migration as usual.
                }
                Err(e) => {
                    bail!(
                        "cannot determine the remote hub version ({e}). Refusing to migrate \
                         blind: if another machine already migrated, a second migration here \
                         would mint a conflicting genesis from stale local state. Retry when \
                         the remote '{remote}' is reachable."
                    );
                }
            }
        }
    }

    let agent_id = crate::identity::AgentConfig::load(crosslink_dir)?
        .map_or_else(|| "hub-v3-migrate".to_string(), |a| a.agent_id);

    // Refuse only when pending offline issues are PROMOTABLE by the current
    // agent — `crosslink sync` will claim their real ids (offline promotion
    // filters on created_by == self). Offline issues created by OTHER
    // identities (dead kickoff agents, anonymous writers) have no live
    // promotion path in any session; the genesis builder mints deterministic
    // ids for those instead of blocking the migration forever.
    let pending = find_pending_offline(cache_dir)?;
    let promotable: Vec<&IssueFile> = pending
        .iter()
        .filter(|i| i.created_by == agent_id)
        .collect();
    if !promotable.is_empty() {
        let names: Vec<String> = promotable
            .iter()
            .map(|i| format!("  {} (\"{}\")", i.uuid, i.title))
            .collect();
        bail!(
            "refusing to migrate: {} offline issue(s) created by this agent have no display_id \
             yet (pending promotion). Run `crosslink sync` to promote and publish them first, \
             then re-run the migration.\n{}",
            promotable.len(),
            names.join("\n")
        );
    }

    // Force a compaction so v2 state is fully reduced and the watermark embedded.
    if let Some(result) = compaction::compact(cache_dir, &agent_id, true, hub_lock)
        .context("forced pre-migration compaction failed")?
    {
        println!(
            "  pre-migration compaction: {} event(s) reduced, {} issue(s) / {} lock(s) materialized.",
            result.events_processed, result.issues_materialized, result.locks_materialized
        );
        if result.skew_warnings > 0 || result.git_skew_violations > 0 {
            tracing::warn!(
                "pre-migration compaction saw {} skew warning(s) and {} git-skew violation(s)",
                result.skew_warnings,
                result.git_skew_violations
            );
        }
        if result.unsigned_warnings > 0 {
            tracing::warn!(
                "pre-migration compaction saw {} unsigned event(s)",
                result.unsigned_warnings
            );
        }
    }

    // Build the authoritative genesis state from the files.
    let genesis = build_genesis_from_files(cache_dir)?;

    // Report ids minted for orphaned offline relics (created by identities
    // with no live promotion path). The v2 branch keeps their None ids — the
    // escape-hatch divergence is limited to exactly these issues.
    for orphan in &pending {
        if let Some(id) = genesis.display_id_map.get(&orphan.uuid) {
            println!(
                "  minted genesis display id #{id} for orphaned offline issue {} (\"{}\", created_by {})",
                orphan.uuid, orphan.title, orphan.created_by
            );
        }
    }

    // Record the v2 hub tip for provenance and rollback awareness.
    let v2_tip = git_rev_parse(cache_dir, V2_HUB_BRANCH)?
        .ok_or_else(|| anyhow::anyhow!("crosslink/hub branch vanished mid-migration"))?;

    // Snapshot every v3 hub branch tip for rollback (these are the only refs
    // the migration touches; the v2 branch is never modified).
    let pre_tips = snapshot_crosslink_refs(cache_dir)?;

    // Seed agent refs + checkpoint + meta. Any failure here triggers rollback.
    let seed_result = seed_v3_refs(cache_dir, &genesis, &v2_tip);
    let seeded = match seed_result {
        Ok(s) => s,
        Err(e) => {
            rollback_refs(cache_dir, &pre_tips)?;
            return Err(e.context("migration seeding failed; rolled back all v3 hub branches"));
        }
    };

    // AC-6 verification gate against the freshly written refs.
    let report = match verify_against_files(cache_dir, &genesis) {
        Ok(r) => r,
        Err(e) => {
            rollback_refs(cache_dir, &pre_tips)?;
            return Err(e.context(
                "AC-6 verification gate failed; rolled back all v3 hub branches \
                 (the crosslink/hub branch was never touched)",
            ));
        }
    };

    // Local migration is complete and verified. Push refs (failures are
    // reported per-ref but do NOT roll back local state — re-run retries pushes).
    let push_summary = push_v3_refs(cache_dir, remote, &seeded);

    print_phase_a_summary(&seeded, &genesis, &report, &push_summary);
    print_mixed_version_warning();
    Ok(())
}

// ── Genesis construction ─────────────────────────────────────────────

/// Per-issue layout location, used to find inline vs. separate comments.
struct IssueLayout {
    /// Inline comments (V1 flat layout) carried on the `IssueFile`.
    inline_comments: Vec<crate::issue_file::CommentEntry>,
    /// Directory holding separate comment files (V2 layout), if it exists.
    comments_dir: Option<PathBuf>,
}

/// Build the genesis [`CheckpointState`] from the materialized files.
///
/// This is the authoritative materialized state, independent of the event
/// reducer. It reads every issue file (both layouts), comment files / inline
/// comments, time entries, milestones, and counters, plus the freshly compacted
/// checkpoint's lock state, and embeds a watermark equal to the max
/// [`OrderingKey`] across ALL v2 agent logs so v3 readers apply nothing
/// pre-genesis.
fn build_genesis_from_files(cache_dir: &Path) -> Result<CheckpointState> {
    let issues_dir = cache_dir.join("issues");
    let issue_files = read_all_issue_files(&issues_dir)?;

    let mut issues: BTreeMap<Uuid, CompactIssue> = BTreeMap::new();
    let mut display_id_map: BTreeMap<Uuid, i64> = BTreeMap::new();
    let mut max_display_id: i64 = 0;
    let mut max_comment_id: i64 = 0;

    // Detect duplicate display_id claims across issue files — corrupt v2 state
    // must be repaired (via integrity), not silently remapped.
    let mut display_id_owner: BTreeMap<i64, Uuid> = BTreeMap::new();

    for issue in &issue_files {
        if let Some(did) = issue.display_id {
            if let Some(prev) = display_id_owner.insert(did, issue.uuid) {
                bail!(
                    "duplicate display_id #{did} claimed by two issues ({prev} and {}); \
                     refusing to migrate — repair the v2 hub first \
                     (`crosslink integrity` / `crosslink compact`)",
                    issue.uuid
                );
            }
            display_id_map.insert(issue.uuid, did);
            max_display_id = max_display_id.max(did);
        }

        let layout = issue_layout(&issues_dir, issue);
        let (comments, comment_max) = build_comments(issue.uuid, issue, &layout)?;
        max_comment_id = max_comment_id.max(comment_max);
        let time_entries = build_time_entries(issue.uuid, issue);

        let compact = CompactIssue {
            uuid: issue.uuid,
            display_id: issue.display_id,
            title: issue.title.clone(),
            description: issue.description.clone(),
            status: issue.status,
            priority: issue.priority,
            parent_uuid: issue.parent_uuid,
            created_by: issue.created_by.clone(),
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            closed_at: issue.closed_at,
            scheduled_at: issue.scheduled_at,
            due_at: issue.due_at,
            labels: issue.labels.iter().cloned().collect(),
            blockers: issue.blockers.iter().copied().collect(),
            related: issue.related.iter().copied().collect(),
            milestone_uuid: issue.milestone_uuid,
            comments,
            time_entries,
        };
        issues.insert(issue.uuid, compact);
    }

    // Milestones from meta/milestones/{uuid}.json.
    let milestones_dir = cache_dir.join("meta").join("milestones");
    let milestone_files = read_all_milestone_files(&milestones_dir)?;
    let mut milestones: BTreeMap<Uuid, CompactMilestone> = BTreeMap::new();
    let mut max_milestone_id: i64 = 0;
    for ms in &milestone_files {
        max_milestone_id = max_milestone_id.max(ms.display_id);
        milestones.insert(
            ms.uuid,
            CompactMilestone {
                uuid: ms.uuid,
                display_id: Some(ms.display_id),
                name: ms.name.clone(),
                description: ms.description.clone(),
                status: ms.status,
                created_at: ms.created_at,
                closed_at: ms.closed_at,
            },
        );
    }

    // Locks from the freshly compacted checkpoint (lock state is event-authoritative).
    let locks = crate::checkpoint::read_checkpoint(cache_dir)?.locks;

    // Counters: max(counters.json values, on-disk maxima + 1).
    let counters = read_counters(&cache_dir.join("meta").join("counters.json"))?;
    let mut next_display_id = counters.next_display_id.max(max_display_id + 1);
    let next_comment_id = counters.next_comment_id.max(max_comment_id + 1);
    let next_milestone_id = counters.next_milestone_id.max(max_milestone_id + 1);

    // Orphaned offline issues: files still carrying display_id None at this
    // point were created by identities with no live promotion path (the
    // preflight refuses when the CURRENT agent could promote via sync).
    // Mint deterministic genesis ids — sorted by (created_at, uuid) — above
    // every existing claim, so the genesis display_id_map is total. The v2
    // branch's files keep their None ids (escape-hatch divergence is limited
    // to these relics and is reported by the caller).
    let mut orphan_keys: Vec<(chrono::DateTime<chrono::Utc>, Uuid)> = issues
        .values()
        .filter(|i| i.display_id.is_none())
        .map(|i| (i.created_at, i.uuid))
        .collect();
    orphan_keys.sort_unstable();
    for (_, uuid) in orphan_keys {
        let id = next_display_id;
        next_display_id += 1;
        display_id_map.insert(uuid, id);
        if let Some(ci) = issues.get_mut(&uuid) {
            ci.display_id = Some(id);
        }
    }

    // Watermark: max OrderingKey across ALL v2 agent logs, so the v3 read path
    // applies nothing pre-genesis. When no events exist anywhere, synthesize a
    // genesis-moment sentinel so the watermark is ALWAYS Some — a None watermark
    // would make reduce() RESET the genesis state to default (see compaction::reduce).
    let watermark =
        max_event_ordering_key(cache_dir)?.unwrap_or_else(hub_v3::genesis_sentinel_watermark);

    Ok(CheckpointState {
        next_display_id,
        next_comment_id,
        display_id_map,
        locks,
        issues,
        milestones,
        deleted_issues: BTreeSet::new(),
        next_milestone_id,
        skew_warnings: Vec::new(),
        compaction_lease: None,
        unsigned_event_warnings: Vec::new(),
        watermark: Some(watermark),
    })
}

/// Resolve the comment layout for an issue: inline (V1) plus an optional
/// separate comments directory (V2 `issues/{uuid}/comments/`).
fn issue_layout(issues_dir: &Path, issue: &IssueFile) -> IssueLayout {
    let v2_comments = issues_dir.join(issue.uuid.to_string()).join("comments");
    let comments_dir = if v2_comments.is_dir() {
        Some(v2_comments)
    } else {
        None
    };
    IssueLayout {
        inline_comments: issue.comments.clone(),
        comments_dir,
    }
}

/// Build the comment map for an issue and return `(map, max_comment_display_id)`.
///
/// V2 comment files carry their own uuid (used directly). V1 inline comments
/// lack a uuid — a deterministic uuid is derived from the issue uuid + the
/// comment's i64 id via [`derive_uuid`].
fn build_comments(
    issue_uuid: Uuid,
    issue: &IssueFile,
    layout: &IssueLayout,
) -> Result<(BTreeMap<Uuid, CompactComment>, i64)> {
    let mut map: BTreeMap<Uuid, CompactComment> = BTreeMap::new();
    let mut max_id: i64 = 0;

    // V2 separate comment files (uuid-keyed natively).
    if let Some(dir) = &layout.comments_dir {
        for cf in read_comment_files(dir)? {
            map.insert(
                cf.uuid,
                CompactComment {
                    display_id: None,
                    author: cf.author,
                    content: cf.content,
                    created_at: cf.created_at,
                    kind: cf.kind,
                    trigger_type: cf.trigger_type,
                    intervention_context: cf.intervention_context,
                    driver_key_fingerprint: cf.driver_key_fingerprint,
                    signed_by: cf.signed_by,
                    signature: cf.signature,
                },
            );
        }
    }

    // V1 inline comments (derive deterministic uuids from issue uuid + i64 id).
    let _ = issue; // inline comments already captured on the layout
    for ce in &layout.inline_comments {
        max_id = max_id.max(ce.id);
        let cuuid = derive_uuid("comment", issue_uuid, ce.id);
        map.entry(cuuid).or_insert_with(|| CompactComment {
            display_id: Some(ce.id),
            author: ce.author.clone(),
            content: ce.content.clone(),
            created_at: ce.created_at,
            kind: ce.kind.clone(),
            trigger_type: ce.trigger_type.clone(),
            intervention_context: ce.intervention_context.clone(),
            driver_key_fingerprint: ce.driver_key_fingerprint.clone(),
            signed_by: ce.signed_by.clone(),
            signature: ce.signature.clone(),
        });
    }

    Ok((map, max_id))
}

/// Build the time-entry map for an issue. V1 inline time entries carry an i64
/// id but no uuid — a deterministic uuid is derived from the issue uuid + id.
fn build_time_entries(issue_uuid: Uuid, issue: &IssueFile) -> BTreeMap<Uuid, CompactTimeEntry> {
    let mut map: BTreeMap<Uuid, CompactTimeEntry> = BTreeMap::new();
    for te in &issue.time_entries {
        let tuuid = derive_uuid("time-entry", issue_uuid, te.id);
        map.entry(tuuid).or_insert_with(|| CompactTimeEntry {
            display_id: Some(te.id),
            started_at: te.started_at,
            ended_at: te.ended_at,
            duration_seconds: te.duration_seconds,
        });
    }
    map
}

/// Derive a deterministic uuid for an inline (uuid-less) V1 sub-entity.
///
/// `Uuid::from_bytes(sha256("crosslink-hub-v3:<kind>:<issue_uuid>:<id>")[0..16])`.
/// No randomness, so the same inputs always yield the same uuid across re-runs.
fn derive_uuid(kind: &str, issue_uuid: Uuid, id: i64) -> Uuid {
    let canonical = format!("crosslink-hub-v3:{kind}:{issue_uuid}:{id}");
    let digest = Sha256::digest(canonical.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[0..16]);
    Uuid::from_bytes(bytes)
}

/// Compute the maximum [`OrderingKey`] across every v2 agent event log.
///
/// Returns `None` when no agent log contains any event.
fn max_event_ordering_key(cache_dir: &Path) -> Result<Option<OrderingKey>> {
    let agents_dir = cache_dir.join("agents");
    let mut max_key: Option<OrderingKey> = None;
    if !agents_dir.exists() {
        return Ok(None);
    }
    for entry in std::fs::read_dir(&agents_dir)
        .with_context(|| format!("failed to read agents dir {}", agents_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let log_path = entry.path().join("events.log");
        if !log_path.exists() {
            continue;
        }
        let events = crate::events::read_events(&log_path)?;
        for ev in &events {
            let key = OrderingKey::from_envelope(ev);
            match &max_key {
                Some(m) if *m >= key => {}
                _ => max_key = Some(key),
            }
        }
    }
    Ok(max_key)
}

// ── Ref seeding ──────────────────────────────────────────────────────

/// Summary of what `seed_v3_refs` created/updated, used for push + reporting.
struct SeededRefs {
    /// Agent IDs whose per-agent ref was seeded.
    agents: Vec<String>,
    /// Whether the checkpoint ref was written.
    checkpoint_written: bool,
    /// Whether the meta ref was written.
    meta_written: bool,
}

/// Seed per-agent refs from v2 event logs, commit the genesis checkpoint, and
/// write the meta marker. The v2 branch is never touched.
fn seed_v3_refs(cache_dir: &Path, genesis: &CheckpointState, v2_tip: &str) -> Result<SeededRefs> {
    // 1. Per-agent refs from each v2 agents/<id>/events.log (byte-superset by the
    //    dual-write parity invariant; child-commit over any existing shadow tip).
    let agents_dir = cache_dir.join("agents");
    let mut agents = Vec::new();
    if agents_dir.exists() {
        for entry in std::fs::read_dir(&agents_dir)
            .with_context(|| format!("failed to read agents dir {}", agents_dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(agent_id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            let log_path = entry.path().join("events.log");
            if !log_path.exists() {
                continue;
            }
            let log_bytes = std::fs::read(&log_path)
                .with_context(|| format!("failed to read {}", log_path.display()))?;
            if log_bytes.is_empty() {
                continue;
            }
            hub_v3::commit_log_bytes(
                cache_dir,
                &agent_id,
                &log_bytes,
                &format!("hub-v3 genesis: seed agent {agent_id} from v2 events.log"),
            )
            .with_context(|| format!("failed to seed agent ref for '{agent_id}'"))?;
            agents.push(agent_id);
        }
    }
    agents.sort();

    // 2. Genesis checkpoint state.json onto CHECKPOINT_REF.
    let state_bytes = serde_json::to_vec_pretty(genesis)
        .context("failed to serialize genesis checkpoint state")?;
    hub_v3::commit_blob_to_ref(
        cache_dir,
        CHECKPOINT_REF,
        "state.json",
        &state_bytes,
        "hub-v3 genesis: checkpoint state",
    )
    .context("failed to commit genesis checkpoint")?;

    // 3. META_REF: hub.json + allowed_signers (when present).
    let meta = HubMeta {
        hub_version: 3,
        migrated_from_commit: v2_tip.to_string(),
        migrated_at: Utc::now(),
        finalized_at: None,
    };
    let hub_json = serde_json::to_vec_pretty(&meta).context("failed to serialize HubMeta")?;

    let signers_path = cache_dir.join("trust").join("allowed_signers");
    let signers_bytes = if signers_path.exists() {
        Some(
            std::fs::read(&signers_path)
                .with_context(|| format!("failed to read {}", signers_path.display()))?,
        )
    } else {
        None
    };

    let mut files: Vec<(&str, &[u8])> = vec![("hub.json", &hub_json)];
    if let Some(bytes) = &signers_bytes {
        files.push(("allowed_signers", bytes));
    }
    hub_v3::commit_files_to_ref(cache_dir, META_REF, &files, "hub-v3 genesis: meta marker")
        .context("failed to commit meta marker")?;

    Ok(SeededRefs {
        agents,
        checkpoint_written: true,
        meta_written: true,
    })
}

// ── AC-6 verification gate ───────────────────────────────────────────

/// Per-category counts checked by the verification gate.
struct VerifyReport {
    issues: usize,
    comments: usize,
    milestones: usize,
    locks: usize,
}

/// AC-6 gate: reduce a fresh [`RefHubSource`] over the new refs and compare it
/// field-complete against the genesis (rebuilt independently from the files),
/// AND assert the file-level invariants directly to avoid symmetric-bug
/// blindness.
fn verify_against_files(cache_dir: &Path, genesis: &CheckpointState) -> Result<VerifyReport> {
    // Independent re-read of the files (a second genesis build).
    let rebuilt = build_genesis_from_files(cache_dir)?;

    // reduce(RefHubSource) must yield the genesis state unchanged (the watermark
    // covers every seeded event, so nothing is re-applied).
    let source = RefHubSource::new(cache_dir)?;
    let outcome = compaction::reduce(&source)?;
    let reduced = &outcome.state;

    // 1. Whole-state field-complete equality, both directions.
    let genesis_val = serde_json::to_value(genesis).context("serialize genesis")?;
    let rebuilt_val = serde_json::to_value(&rebuilt).context("serialize rebuilt genesis")?;
    let reduced_val = serde_json::to_value(reduced).context("serialize reduced state")?;
    if genesis_val != rebuilt_val {
        bail!("verification failed: independent file re-read disagrees with genesis state");
    }
    if reduced_val != genesis_val {
        bail!(
            "verification failed: reduce(RefHubSource) does not equal the genesis state \
             (events were applied above/around the watermark, or a ref is wrong)"
        );
    }

    // 2. Direct invariants against the files (not symmetric with the builder).
    let issues_dir = cache_dir.join("issues");
    let issue_files = read_all_issue_files(&issues_dir)?;

    // Issue count equality.
    if issue_files.len() != reduced.issues.len() {
        bail!(
            "verification failed: issue count mismatch (files {}, reduced {})",
            issue_files.len(),
            reduced.issues.len()
        );
    }

    let mut comment_total = 0usize;
    let mut display_ids_seen: BTreeSet<i64> = BTreeSet::new();
    for issue in &issue_files {
        // Every issue file uuid is present in the reduced state.
        let Some(reduced_issue) = reduced.issues.get(&issue.uuid) else {
            bail!(
                "verification failed: issue {} present on disk but missing from reduced state",
                issue.uuid
            );
        };

        // Per-issue serde_json equality of CompactIssue against the genesis.
        let g = genesis
            .issues
            .get(&issue.uuid)
            .ok_or_else(|| anyhow::anyhow!("genesis missing issue {}", issue.uuid))?;
        if serde_json::to_value(g)? != serde_json::to_value(reduced_issue)? {
            bail!(
                "verification failed: issue {} differs between genesis and reduced state",
                issue.uuid
            );
        }

        // Display-id uniqueness across issue files.
        if let Some(did) = issue.display_id {
            if !display_ids_seen.insert(did) {
                bail!("verification failed: duplicate display_id #{did} across issue files");
            }
        }

        // Comment count per issue matches the comment files / inline comments.
        let layout = issue_layout(&issues_dir, issue);
        let on_disk_comments = count_disk_comments(issue, &layout)?;
        if on_disk_comments != reduced_issue.comments.len() {
            bail!(
                "verification failed: comment count mismatch for issue {} (disk {on_disk_comments}, \
                 reduced {})",
                issue.uuid,
                reduced_issue.comments.len()
            );
        }
        comment_total += on_disk_comments;
    }

    // Milestone count matches the milestone files.
    let milestones_dir = cache_dir.join("meta").join("milestones");
    let milestone_files = read_all_milestone_files(&milestones_dir)?;
    if milestone_files.len() != reduced.milestones.len() {
        bail!(
            "verification failed: milestone count mismatch (files {}, reduced {})",
            milestone_files.len(),
            reduced.milestones.len()
        );
    }

    // next_* >= on-disk maxima.
    let counters = read_counters(&cache_dir.join("meta").join("counters.json"))?;
    if reduced.next_display_id < counters.next_display_id
        || reduced.next_milestone_id < counters.next_milestone_id
        || reduced.next_comment_id < counters.next_comment_id
    {
        bail!("verification failed: next_* counters regressed below counters.json");
    }

    // Locks equal the compacted checkpoint's locks.
    let compacted = crate::checkpoint::read_checkpoint(cache_dir)?;
    if serde_json::to_value(&compacted.locks)? != serde_json::to_value(&reduced.locks)? {
        bail!("verification failed: lock state differs from the compacted checkpoint");
    }

    Ok(VerifyReport {
        issues: reduced.issues.len(),
        comments: comment_total,
        milestones: reduced.milestones.len(),
        locks: reduced.locks.len(),
    })
}

/// Count the comments on disk for an issue (V2 files + V1 inline, de-duplicated
/// the same way the genesis builder does so the counts line up).
fn count_disk_comments(issue: &IssueFile, layout: &IssueLayout) -> Result<usize> {
    let mut keys: BTreeSet<Uuid> = BTreeSet::new();
    if let Some(dir) = &layout.comments_dir {
        for cf in read_comment_files(dir)? {
            keys.insert(cf.uuid);
        }
    }
    for ce in &issue.comments {
        keys.insert(derive_uuid("comment", issue.uuid, ce.id));
    }
    Ok(keys.len())
}

// ── Ref snapshot / rollback ──────────────────────────────────────────

/// Snapshot of a v3 hub branch's tip before migration.
struct RefTip {
    name: String,
    /// `Some(sha)` if the ref existed before migration; `None` if it did not.
    old_sha: Option<String>,
}

/// Record the current tip of every v3 hub branch. The set is the union
/// of refs that exist now and the refs the migration is about to write, so that
/// a ref created by the migration but absent before is rolled back by deletion.
fn snapshot_crosslink_refs(cache_dir: &Path) -> Result<Vec<RefTip>> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for r in for_each_ref(cache_dir, "refs/heads/crosslink/*")? {
        // Only the v3 hub refs (checkpoint, meta, agents/*); never the frozen v2
        // `crosslink/hub` branch or the `crosslink/hub-v3-host` worktree host,
        // which share the prefix but are not hub state (#767).
        if hub_v3::is_v3_hub_ref(&r) {
            names.insert(r);
        }
    }
    // The refs the migration writes (so a newly-created one rolls back to absent).
    names.insert(CHECKPOINT_REF.to_string());
    names.insert(META_REF.to_string());
    // Per-agent refs the migration may create.
    let agents_dir = cache_dir.join("agents");
    if agents_dir.exists() {
        for entry in std::fs::read_dir(&agents_dir)?.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if let Some(id) = entry.file_name().to_str() {
                    if let Ok(name) = agent_ref_name(id) {
                        names.insert(name);
                    }
                }
            }
        }
    }

    let mut tips = Vec::with_capacity(names.len());
    for name in names {
        let old_sha = git_rev_parse(cache_dir, &name)?;
        tips.push(RefTip { name, old_sha });
    }
    Ok(tips)
}

/// Restore every recorded ref to its pre-migration tip: refs that existed are
/// reset to their old sha, refs that did not exist are deleted. The v2 branch
/// is never in this set, so it is never touched.
fn rollback_refs(cache_dir: &Path, tips: &[RefTip]) -> Result<()> {
    for tip in tips {
        let current = git_rev_parse(cache_dir, &tip.name)?;
        match (&tip.old_sha, &current) {
            (Some(old), _) => {
                // Restore to the old value (idempotent if already there).
                git_update_ref(cache_dir, &tip.name, old)?;
            }
            (None, Some(_)) => {
                // Did not exist before — delete what the migration created.
                git_delete_ref(cache_dir, &tip.name)?;
            }
            (None, None) => {}
        }
    }
    Ok(())
}

// ── Push ─────────────────────────────────────────────────────────────

/// Per-ref push results, for reporting.
struct PushSummary {
    pushed: Vec<String>,
    failed: Vec<(String, String)>,
}

/// Push every created/updated ref. Agent + meta refs use a plain fast-forward
/// push; the checkpoint ref uses `--force-with-lease` (REQ-7). Failures are
/// collected, not propagated (re-run is the retry mechanism).
fn push_v3_refs(cache_dir: &Path, remote: &str, seeded: &SeededRefs) -> PushSummary {
    let mut summary = PushSummary {
        pushed: Vec::new(),
        failed: Vec::new(),
    };

    let mut record = |name: String, outcome: Result<PushOutcome>| match outcome {
        Ok(PushOutcome::Pushed) => summary.pushed.push(name),
        Ok(PushOutcome::NonFastForward) => summary
            .failed
            .push((name, "non-fast-forward (remote diverged)".to_string())),
        Ok(PushOutcome::NoRemote) => {
            summary
                .failed
                .push((name, "remote not available".to_string()));
        }
        Ok(PushOutcome::Failed(e)) => summary.failed.push((name, e)),
        Err(e) => summary.failed.push((name, e.to_string())),
    };

    for agent_id in &seeded.agents {
        if let Ok(name) = agent_ref_name(agent_id) {
            let outcome = hub_v3::push_ref(cache_dir, remote, &name);
            record(name, outcome);
        }
    }
    if seeded.meta_written {
        let outcome = hub_v3::push_ref(cache_dir, remote, META_REF);
        record(META_REF.to_string(), outcome);
    }
    if seeded.checkpoint_written {
        let outcome = hub_v3::push_ref_with_lease(cache_dir, remote, CHECKPOINT_REF, None);
        record(CHECKPOINT_REF.to_string(), outcome);
    }

    summary
}

/// On the already-migrated re-run path, push any v3 ref the remote lacks. This
/// makes re-run the retry mechanism for an interrupted remote propagation.
fn retry_push_missing_refs(cache_dir: &Path, remote: &str) -> Result<()> {
    let local_refs: Vec<String> = for_each_ref(cache_dir, "refs/heads/crosslink/*")?
        .into_iter()
        .filter(|r| hub_v3::is_v3_hub_ref(r))
        .collect();
    if local_refs.is_empty() {
        return Ok(());
    }
    let remote_shas = ls_remote_crosslink(cache_dir, remote).unwrap_or_default();

    let mut retried = 0usize;
    for name in &local_refs {
        let local_sha = git_rev_parse(cache_dir, name)?;
        let remote_sha = remote_shas.get(name);
        if local_sha.as_deref() != remote_sha.map(String::as_str) {
            let outcome = if name == CHECKPOINT_REF {
                hub_v3::push_ref_with_lease(cache_dir, remote, name, None)
            } else {
                hub_v3::push_ref(cache_dir, remote, name)
            };
            match outcome {
                Ok(PushOutcome::Pushed) => {
                    println!("  re-pushed {name}");
                    retried += 1;
                }
                Ok(PushOutcome::NoRemote) => return Ok(()),
                Ok(other) => {
                    tracing::warn!("re-run push of {name} did not complete: {other:?}");
                }
                Err(e) => tracing::warn!("re-run push of {name} failed: {e}"),
            }
        }
    }
    if retried > 0 {
        println!("re-pushed {retried} ref(s) the remote was missing.");
    }
    Ok(())
}

// ── Phase B: finalize ────────────────────────────────────────────────

fn finalize_migration(
    cache_dir: &Path,
    remote: &str,
    yes_delete_v2: bool,
    _hub_lock: &crate::sync::HubWriteLock,
) -> Result<()> {
    if !yes_delete_v2 {
        bail!(
            "`migrate hub-v3 --finalize` is destructive: it deletes the legacy crosslink/hub \
             branch locally and on the remote. After finalize, already-deployed v2 binaries will \
             FAIL LOUDLY (the branch they read is gone) — that is the intended hard stop. \
             Re-run with `--yes-delete-v2` to confirm."
        );
    }

    // Precondition: must already be v3.
    match hub_v3::detect_hub_version(cache_dir)? {
        HubVersion::V3 { v2_branch_present } => {
            if !v2_branch_present {
                println!("crosslink/hub branch already deleted — finalize is a no-op.");
                if let Some(meta) = read_hub_meta(cache_dir)? {
                    print_hub_meta(&meta);
                }
                return Ok(());
            }
        }
        HubVersion::V2Only | HubVersion::Absent => {
            bail!("refusing to finalize: hub is not v3 (run `crosslink migrate hub-v3` first)");
        }
    }

    // Resolve the main repo root NOW, while the cache worktree still exists.
    // After the worktree is removed, all ref operations must run from the repo
    // root (which shares the same object store + ref namespace).
    let repo_root = git_main_repo_root(cache_dir)?;

    // Re-verify the AC-6 gate against the current refs/files: v2 and v3 must
    // still agree. (fetch already ran in the dispatcher.)
    let genesis = build_genesis_from_files(cache_dir)?;
    verify_against_files(cache_dir, &genesis)
        .context("refusing to finalize: AC-6 re-verification failed; v2 and v3 no longer agree")?;

    // Stamp HubMeta.finalized_at (new commit on META_REF preserving provenance)
    // BEFORE removing the worktree, while ref reads from cache_dir still work.
    let mut meta = read_hub_meta(cache_dir)?
        .ok_or_else(|| anyhow::anyhow!("v3 meta marker missing during finalize"))?;
    meta.finalized_at = Some(Utc::now());
    let hub_json = serde_json::to_vec_pretty(&meta).context("serialize finalized HubMeta")?;

    // Preserve allowed_signers if the meta ref currently carries it.
    let signers = read_meta_allowed_signers(cache_dir)?;
    let mut files: Vec<(&str, &[u8])> = vec![("hub.json", &hub_json)];
    if let Some(bytes) = &signers {
        files.push(("allowed_signers", bytes));
    }
    hub_v3::commit_files_to_ref(
        cache_dir,
        META_REF,
        &files,
        "hub-v3 finalize: stamp finalized_at",
    )
    .context("failed to update meta marker with finalized_at")?;

    // Now delete the local crosslink/hub branch + cache worktree, and push the
    // deletion to the remote. All subsequent ref ops run from `repo_root`.
    delete_v2_branch_local(cache_dir, &repo_root)?;
    push_v2_branch_deletion(&repo_root, remote)?;

    // Push the updated meta ref (best-effort, re-run retries) from the repo root.
    match hub_v3::push_ref(&repo_root, remote, META_REF) {
        Ok(PushOutcome::Pushed | PushOutcome::NoRemote) => {}
        Ok(other) => tracing::warn!("finalize: meta ref push did not complete: {other:?}"),
        Err(e) => tracing::warn!("finalize: meta ref push failed: {e}"),
    }

    println!("Finalized hub-v3 migration:");
    println!("  deleted crosslink/hub branch (local + remote)");
    println!(
        "  HubMeta.finalized_at = {}",
        meta.finalized_at.unwrap_or_else(Utc::now)
    );
    println!();
    println!(
        "Note: any already-deployed v0.5.x / v2 binary will now FAIL LOUDLY when it tries to \
         read the deleted crosslink/hub branch. This is the intended hard cutover."
    );
    Ok(())
}

/// Read `allowed_signers` from the current `META_REF` tip, if present.
fn read_meta_allowed_signers(cache_dir: &Path) -> Result<Option<Vec<u8>>> {
    let Some(tip) = git_rev_parse(cache_dir, META_REF)? else {
        return Ok(None);
    };
    let spec = format!("{tip}:allowed_signers");
    git_cat_file_blob_optional(cache_dir, &spec)
}

/// Delete the local crosslink/hub branch. The branch is checked out in the hub
/// cache worktree, so the worktree must be removed first (`git worktree remove`).
/// `repo_root` is the main repository that owns the branch and object store.
fn delete_v2_branch_local(cache_dir: &Path, repo_root: &Path) -> Result<()> {
    // Remove the cache worktree from the main repo (force: it may hold the lock
    // file and runtime artifacts). Best-effort: if it is already gone, continue.
    let worktree = cache_dir.to_string_lossy().to_string();
    let _ = run_git(repo_root, &["worktree", "remove", "--force", &worktree]);
    // Prune any stale worktree administrative entries.
    let _ = run_git(repo_root, &["worktree", "prune"]);
    run_git(repo_root, &["branch", "-D", "crosslink/hub"])
        .context("failed to delete local crosslink/hub branch")?;
    Ok(())
}

/// Push the crosslink/hub branch deletion to the remote. `repo_root` is the
/// main repository (the cache worktree has already been removed by this point).
fn push_v2_branch_deletion(repo_root: &Path, remote: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_root)
        .args(["push", remote, ":refs/heads/crosslink/hub"])
        .output()
        .context("failed to run git push to delete crosslink/hub on the remote")?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A remote that doesn't have the branch (or no remote) is not fatal here —
    // the local deletion already happened and re-run can retry.
    if stderr.contains("remote ref does not exist")
        || stderr.contains("Could not read from remote")
        || stderr.contains("does not appear to be a git repository")
        || stderr.contains("No such remote")
    {
        tracing::warn!("remote crosslink/hub deletion skipped: {}", stderr.trim());
        return Ok(());
    }
    bail!(
        "failed to delete crosslink/hub on remote '{remote}': {}",
        stderr.trim()
    );
}

// ── Pending-offline detection ────────────────────────────────────────

/// Find issue files that have no `display_id` (pending offline promotion).
///
/// Mirrors the signal `shared_writer::offline::find_offline_issues` keys on
/// (`display_id.is_none()`), but reads the hub-cache files directly — the
/// migration must refuse before any agent/SQLite context is required.
fn find_pending_offline(cache_dir: &Path) -> Result<Vec<IssueFile>> {
    let issues_dir = cache_dir.join("issues");
    let all = read_all_issue_files(&issues_dir)?;
    Ok(all.into_iter().filter(|i| i.display_id.is_none()).collect())
}

// ── Output helpers ───────────────────────────────────────────────────

fn print_hub_meta(meta: &HubMeta) {
    println!("  hub_version: {}", meta.hub_version);
    println!("  migrated_from_commit: {}", meta.migrated_from_commit);
    println!("  migrated_at: {}", meta.migrated_at.to_rfc3339());
    if let Some(f) = meta.finalized_at {
        println!("  finalized_at: {}", f.to_rfc3339());
    }
}

fn print_phase_a_summary(
    seeded: &SeededRefs,
    genesis: &CheckpointState,
    report: &VerifyReport,
    push: &PushSummary,
) {
    println!("hub-v3 migration complete (verified).");
    println!();
    println!("  agents seeded:       {}", seeded.agents.len());
    println!("  issues in genesis:   {}", genesis.issues.len());
    println!("  comments in genesis: {}", report.comments);
    println!("  milestones:          {}", genesis.milestones.len());
    println!("  locks:               {}", genesis.locks.len());
    println!();
    println!(
        "  verification checked: issues={} comments={} milestones={} locks={}",
        report.issues, report.comments, report.milestones, report.locks
    );
    println!();
    println!("  refs pushed: {}", push.pushed.len());
    for name in &push.pushed {
        println!("    {name}");
    }
    if !push.failed.is_empty() {
        println!(
            "  refs NOT pushed ({}) — local migration is complete; re-run `crosslink migrate \
             hub-v3` to retry pushing:",
            push.failed.len()
        );
        for (name, why) in &push.failed {
            println!("    {name}: {why}");
        }
    }
    println!();
    println!(
        "Next: soak/cutover. The old crosslink/hub branch is left intact as a read-only escape \
         hatch. When you are confident the v3 refs are correct, run \
         `crosslink migrate hub-v3 --finalize --yes-delete-v2` to delete it."
    );
}

fn print_mixed_version_warning() {
    println!();
    println!(
        "WARNING: until you finalize, already-deployed v2 binaries keep operating the \
         crosslink/hub branch and their writes are NOT reflected in v3. Avoid mixed-version \
         operation, or finish the cutover."
    );
}

// ── Private git plumbing ─────────────────────────────────────────────

fn git_rev_parse(repo_dir: &Path, ref_name: &str) -> Result<Option<String>> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["rev-parse", "--verify", "--quiet", ref_name])
        .output()
        .with_context(|| format!("failed to run git rev-parse for '{ref_name}'"))?;
    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Ok(None);
        }
        Ok(Some(sha))
    } else {
        Ok(None)
    }
}

fn git_update_ref(repo_dir: &Path, ref_name: &str, sha: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["update-ref", ref_name, sha])
        .output()
        .with_context(|| format!("failed to run git update-ref for '{ref_name}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "git update-ref {ref_name} -> {sha} failed: {}",
            stderr.trim()
        );
    }
    Ok(())
}

fn git_delete_ref(repo_dir: &Path, ref_name: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["update-ref", "-d", ref_name])
        .output()
        .with_context(|| format!("failed to run git update-ref -d for '{ref_name}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git update-ref -d {ref_name} failed: {}", stderr.trim());
    }
    Ok(())
}

fn for_each_ref(repo_dir: &Path, pattern: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["for-each-ref", "--format=%(refname)", pattern])
        .output()
        .with_context(|| format!("failed to run git for-each-ref for '{pattern}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git for-each-ref failed for '{pattern}': {}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// `git ls-remote <remote> refs/heads/crosslink/*` → map of refname → sha.
fn ls_remote_crosslink(repo_dir: &Path, remote: &str) -> Result<BTreeMap<String, String>> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["ls-remote", remote, "refs/heads/crosslink/*"])
        .output()
        .with_context(|| format!("failed to run git ls-remote for '{remote}'"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git ls-remote failed for '{remote}': {}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut map = BTreeMap::new();
    for line in stdout.lines() {
        if let Some((sha, name)) = line.split_once('\t') {
            map.insert(name.trim().to_string(), sha.trim().to_string());
        }
    }
    Ok(map)
}

fn git_cat_file_blob_optional(repo_dir: &Path, blob_spec: &str) -> Result<Option<Vec<u8>>> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["cat-file", "blob", blob_spec])
        .output()
        .with_context(|| format!("failed to run git cat-file for '{blob_spec}'"))?;
    if output.status.success() {
        return Ok(Some(output.stdout));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("does not exist")
        || stderr.contains("Not a valid object name")
        || stderr.contains("not found")
    {
        return Ok(None);
    }
    bail!("git cat-file failed for '{blob_spec}': {}", stderr.trim())
}

fn run_git(repo_dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {args:?}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {args:?} failed: {}", stderr.trim());
    }
    Ok(output)
}

/// Resolve the main (non-worktree) repository root that owns `repo_dir`'s
/// object store. Works whether `repo_dir` is the cache worktree or already the
/// main repo. Uses `git rev-parse --git-common-dir` and strips a trailing
/// `/.git`.
fn git_main_repo_root(repo_dir: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .context("failed to run git rev-parse --git-common-dir")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git rev-parse --git-common-dir failed: {}", stderr.trim());
    }
    let common = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let common_path = PathBuf::from(&common);
    let root = if common_path.file_name().and_then(|n| n.to_str()) == Some(".git") {
        common_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or(common_path)
    } else {
        common_path
    };
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{AgentConfig, AgentRole};
    use std::process::Command;
    use tempfile::TempDir;

    /// Build a realistic populated v2 hub in a temp repo with a bare remote.
    ///
    /// Returns `(work_dir, remote_dir, crosslink_dir, cache_dir)`. The hub is
    /// populated with two issues (labels, comments, a dependency, a relation),
    /// a milestone, and a lock by writing the v2 agent event log directly (the
    /// v2 write path is deleted, #754) and materializing with
    /// `compaction::compact`. A second agent's events.log + issue file are
    /// written directly so two agent refs seed.
    fn setup_v2_hub() -> (TempDir, TempDir, std::path::PathBuf, std::path::PathBuf) {
        let remote_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();

        run(remote_dir.path(), &["init", "--bare", "-b", "main"]);
        run(work_dir.path(), &["init", "-b", "main"]);
        let wp = work_dir.path().to_path_buf();
        run(&wp, &["config", "user.email", "test@test.local"]);
        run(&wp, &["config", "user.name", "Test"]);
        run(&wp, &["config", "commit.gpgsign", "false"]);
        run(
            &wp,
            &[
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
        );
        std::fs::write(wp.join("README.md"), "# test\n").unwrap();
        run(&wp, &["add", "."]);
        run(&wp, &["commit", "-m", "init", "--no-gpg-sign"]);
        run(&wp, &["push", "-u", "origin", "main"]);

        let crosslink_dir = wp.join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{"remote":"origin","layout":"v2"}"#,
        )
        .unwrap();

        write_agent(&crosslink_dir, "alpha");

        let sync = SyncManager::new(&crosslink_dir).unwrap();
        // Since 754b a fresh `init_cache` bootstraps v3; the migration tests need
        // a legacy v2 hub to migrate FROM, so build the `crosslink/hub` worktree
        // with the v2 layout explicitly (the way pre-754b `init_cache` did).
        let cache_dir = sync.cache_path().to_path_buf();
        run(
            &wp,
            &[
                "worktree",
                "add",
                "--orphan",
                "-b",
                "crosslink/hub",
                cache_dir.to_str().unwrap(),
            ],
        );
        run(&cache_dir, &["config", "user.email", "test@test.local"]);
        run(&cache_dir, &["config", "user.name", "Test"]);
        run(&cache_dir, &["config", "commit.gpgsign", "false"]);
        let meta_dir = cache_dir.join("meta");
        std::fs::create_dir_all(meta_dir.join("milestones")).unwrap();
        std::fs::create_dir_all(cache_dir.join("issues")).unwrap();
        std::fs::create_dir_all(cache_dir.join("locks")).unwrap();
        std::fs::create_dir_all(cache_dir.join("trust")).unwrap();
        crate::issue_file::write_layout_version(
            &meta_dir,
            crate::issue_file::CURRENT_LAYOUT_VERSION,
        )
        .unwrap();
        std::fs::write(
            cache_dir.join("locks.json"),
            serde_json::to_string(&serde_json::json!({"version":1,"locks":{},"settings":{"stale_lock_timeout_minutes":60}})).unwrap(),
        )
        .unwrap();
        run(&cache_dir, &["add", "-A"]);
        run(
            &cache_dir,
            &[
                "commit",
                "-m",
                "Initialize crosslink/hub branch",
                "--no-gpg-sign",
            ],
        );

        // Populate the pre-migration v2 hub by writing the agent event log
        // directly (the v2 SharedWriter write path is deleted, #754), then
        // materializing with `compaction::compact` (kept for migration).
        populate_alpha_v2(&cache_dir);

        // Second agent: write an events.log + issue file directly so the
        // migration seeds a second agent ref.
        write_second_agent(&cache_dir);

        // Force a compaction to materialize everything consistently.
        let lock = sync.acquire_lock().unwrap();
        crate::compaction::compact(&cache_dir, "alpha", true, &lock).unwrap();
        drop(lock);

        (work_dir, remote_dir, crosslink_dir, cache_dir)
    }

    /// Write agent `alpha`'s v2 event log directly: two issues, labels,
    /// comments, a blocker, a relation, a milestone, and a lock — the same
    /// state the old `SharedWriter` population produced. `compaction::compact`
    /// then materializes the worktree issue/lock/checkpoint files the migration
    /// genesis reads.
    fn populate_alpha_v2(cache_dir: &Path) {
        use crate::events::{append_event, Event, EventEnvelope};
        let i1 = Uuid::parse_str("a1a1a1a1-a1a1-a1a1-a1a1-a1a1a1a1a1a1").unwrap();
        let i2 = Uuid::parse_str("a2a2a2a2-a2a2-a2a2-a2a2-a2a2a2a2a2a2").unwrap();
        let ms = Uuid::parse_str("cccccccc-cccc-cccc-cccc-cccccccccccc").unwrap();
        let c1 = Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddddd").unwrap();
        let c2 = Uuid::parse_str("eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee").unwrap();
        let base = Utc::now() - chrono::Duration::seconds(300);
        let log_path = cache_dir.join("agents").join("alpha").join("events.log");

        let events = vec![
            Event::IssueCreated {
                uuid: i1,
                title: "First issue".to_string(),
                description: Some("desc one".to_string()),
                priority: "high".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "alpha".to_string(),
                display_id: Some(1),
                scheduled_at: None,
                due_at: None,
            },
            Event::IssueCreated {
                uuid: i2,
                title: "Second issue".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "alpha".to_string(),
                display_id: Some(2),
                scheduled_at: None,
                due_at: None,
            },
            Event::LabelAdded {
                issue_uuid: i1,
                label: "bug".to_string(),
            },
            Event::LabelAdded {
                issue_uuid: i1,
                label: "urgent".to_string(),
            },
            Event::CommentAdded {
                issue_uuid: i1,
                comment_uuid: c1,
                display_id: Some(1),
                author: "alpha".to_string(),
                content: "a note".to_string(),
                created_at: base,
                kind: "note".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None,
                signature: None,
            },
            Event::CommentAdded {
                issue_uuid: i1,
                comment_uuid: c2,
                display_id: Some(2),
                author: "alpha".to_string(),
                content: "a plan".to_string(),
                created_at: base,
                kind: "plan".to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None,
                signature: None,
            },
            Event::DependencyAdded {
                blocked_uuid: i1,
                blocker_uuid: i2,
            },
            Event::RelationAdded {
                uuid_a: i1,
                uuid_b: i2,
            },
            Event::MilestoneCreated {
                uuid: ms,
                display_id: Some(1),
                name: "v1.0".to_string(),
                description: Some("first release".to_string()),
                created_at: base,
            },
            Event::LockClaimed {
                issue_display_id: 2,
                branch: Some("feature/x".to_string()),
            },
        ];

        for (i, event) in events.into_iter().enumerate() {
            let env = EventEnvelope {
                agent_id: "alpha".to_string(),
                agent_seq: (i + 1) as u64,
                timestamp: base + chrono::Duration::seconds(i as i64),
                event,
                signed_by: None,
                signature: None,
            };
            append_event(&log_path, &env).unwrap();
        }

        // Materialize the V2 comment files (compaction writes issue.json but
        // not the per-comment files the genesis reads via read_comment_files).
        let comments_dir = cache_dir
            .join("issues")
            .join(i1.to_string())
            .join("comments");
        std::fs::create_dir_all(&comments_dir).unwrap();
        for (cuuid, content, kind) in [(c1, "a note", "note"), (c2, "a plan", "plan")] {
            let cf = crate::issue_file::CommentFile {
                uuid: cuuid,
                issue_uuid: i1,
                author: "alpha".to_string(),
                content: content.to_string(),
                created_at: base,
                kind: kind.to_string(),
                trigger_type: None,
                intervention_context: None,
                driver_key_fingerprint: None,
                signed_by: None,
                signature: None,
            };
            crate::issue_file::write_comment_file(&comments_dir.join(format!("{cuuid}.json")), &cf)
                .unwrap();
        }
    }

    fn write_agent(crosslink_dir: &Path, id: &str) {
        let agent = AgentConfig {
            agent_id: id.to_string(),
            machine_id: "test-machine".to_string(),
            description: Some("test".to_string()),
            role: AgentRole::Driver,
            ssh_key_path: None,
            ssh_fingerprint: None,
            ssh_public_key: None,
        };
        std::fs::write(
            crosslink_dir.join("agent.json"),
            serde_json::to_string_pretty(&agent).unwrap(),
        )
        .unwrap();
    }

    /// Write a beta agent events.log plus a matching issue file directly into
    /// the cache so the migration seeds a second agent ref and the issue is
    /// part of the genesis.
    fn write_second_agent(cache_dir: &Path) {
        use crate::events::{append_event, Event, EventEnvelope};
        let uuid = Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
        let now = Utc::now();
        let env = EventEnvelope {
            agent_id: "beta".to_string(),
            agent_seq: 1,
            timestamp: now - chrono::Duration::seconds(120),
            event: Event::IssueCreated {
                uuid,
                title: "Beta issue".to_string(),
                description: None,
                priority: "low".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "beta".to_string(),
                display_id: Some(3),
                scheduled_at: None,
                due_at: None,
            },
            signed_by: None,
            signature: None,
        };
        let log_path = cache_dir.join("agents").join("beta").join("events.log");
        append_event(&log_path, &env).unwrap();

        // Materialize the issue file (V2 layout) so the file-derived genesis
        // includes it. compact() will also produce it, but writing it here makes
        // the fixture explicit and robust.
        let issue = crate::issue_file::IssueFile {
            uuid,
            display_id: Some(3),
            title: "Beta issue".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Low,
            parent_uuid: None,
            created_by: "beta".to_string(),
            created_at: now - chrono::Duration::seconds(120),
            updated_at: now - chrono::Duration::seconds(120),
            closed_at: None,
            scheduled_at: None,
            due_at: None,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };
        let dir = cache_dir.join("issues").join(uuid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        crate::issue_file::write_issue_file(&dir.join("issue.json"), &issue).unwrap();
    }

    fn run(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn rev(dir: &Path, name: &str) -> Option<String> {
        git_rev_parse(dir, name).unwrap()
    }

    // ── Test 1: happy path + idempotent re-run ───────────────────────

    #[test]
    fn migrate_happy_path_and_rerun_is_noop() {
        let (_w, _r, crosslink_dir, cache_dir) = setup_v2_hub();

        hub_v3(&crosslink_dir, false, false).unwrap();

        // v3 refs exist.
        assert!(rev(&cache_dir, CHECKPOINT_REF).is_some());
        assert!(rev(&cache_dir, META_REF).is_some());
        assert!(rev(&cache_dir, &agent_ref_name("alpha").unwrap()).is_some());
        assert!(rev(&cache_dir, &agent_ref_name("beta").unwrap()).is_some());

        // HubMeta is correct.
        let meta = read_hub_meta(&cache_dir).unwrap().unwrap();
        assert_eq!(meta.hub_version, 3);
        assert!(!meta.migrated_from_commit.is_empty());
        assert!(meta.finalized_at.is_none());

        // Detected as V3 with the v2 branch still present.
        assert_eq!(
            hub_v3::detect_hub_version(&cache_dir).unwrap(),
            HubVersion::V3 {
                v2_branch_present: true
            }
        );

        // Snapshot tips, re-run, confirm idempotent no-op (tips unchanged).
        let cp = rev(&cache_dir, CHECKPOINT_REF);
        let mt = rev(&cache_dir, META_REF);
        let al = rev(&cache_dir, &agent_ref_name("alpha").unwrap());
        hub_v3(&crosslink_dir, false, false).unwrap();
        assert_eq!(cp, rev(&cache_dir, CHECKPOINT_REF));
        assert_eq!(mt, rev(&cache_dir, META_REF));
        assert_eq!(al, rev(&cache_dir, &agent_ref_name("alpha").unwrap()));

        // v2 branch is untouched.
        assert!(rev(&cache_dir, V2_HUB_BRANCH).is_some());
    }

    // ── Test 2: genesis equals files ─────────────────────────────────

    #[test]
    fn genesis_equals_files_via_refhubsource() {
        let (_w, _r, crosslink_dir, cache_dir) = setup_v2_hub();
        hub_v3(&crosslink_dir, false, false).unwrap();

        let genesis = build_genesis_from_files(&cache_dir).unwrap();
        let source = RefHubSource::new(&cache_dir).unwrap();
        let reduced = crate::compaction::reduce(&source).unwrap().state;

        assert_eq!(reduced.issues.len(), genesis.issues.len());
        assert_eq!(reduced.milestones.len(), genesis.milestones.len());

        // Per-issue full CompactIssue serde equality.
        for (uuid, g) in &genesis.issues {
            let r = reduced
                .issues
                .get(uuid)
                .expect("issue present after reduce");
            assert_eq!(
                serde_json::to_value(g).unwrap(),
                serde_json::to_value(r).unwrap(),
                "issue {uuid} must match"
            );
        }

        // Comment count spot-check: at least one issue has 2 comments.
        let with_comments = genesis
            .issues
            .values()
            .filter(|i| i.comments.len() == 2)
            .count();
        assert!(with_comments >= 1, "expected an issue with 2 comments");
    }

    // ── Test 3: watermark correctness ────────────────────────────────

    #[test]
    fn new_event_above_watermark_is_applied_pre_genesis_is_not() {
        let (_w, _r, crosslink_dir, cache_dir) = setup_v2_hub();
        hub_v3(&crosslink_dir, false, false).unwrap();

        let genesis = build_genesis_from_files(&cache_dir).unwrap();

        // Baseline: reduce equals genesis (no double-application of pre-genesis
        // events such as the label-add already folded into the genesis state).
        let base = crate::compaction::reduce(&RefHubSource::new(&cache_dir).unwrap())
            .unwrap()
            .state;
        assert_eq!(
            serde_json::to_value(&genesis).unwrap(),
            serde_json::to_value(&base).unwrap(),
            "reduce must equal genesis (pre-genesis events not re-applied)"
        );

        // Append a NEW event ABOVE the watermark to the alpha ref and confirm it
        // IS applied by reduce.
        use crate::events::{Event, EventEnvelope};
        let new_uuid = Uuid::new_v4();
        let env = EventEnvelope {
            agent_id: "alpha".to_string(),
            agent_seq: 9999,
            timestamp: Utc::now() + chrono::Duration::seconds(60),
            event: Event::IssueCreated {
                uuid: new_uuid,
                title: "Post-genesis issue".to_string(),
                description: None,
                priority: "medium".to_string(),
                labels: vec![],
                parent_uuid: None,
                created_by: "alpha".to_string(),
                display_id: None,
                scheduled_at: None,
                due_at: None,
            },
            signed_by: None,
            signature: None,
        };
        // Read current alpha log, append the new line, commit it onto the ref.
        let tip = rev(&cache_dir, &agent_ref_name("alpha").unwrap()).unwrap();
        let mut bytes = git_cat_file_blob_optional(&cache_dir, &format!("{tip}:events.log"))
            .unwrap()
            .unwrap();
        bytes.extend_from_slice(serde_json::to_string(&env).unwrap().as_bytes());
        bytes.push(b'\n');
        hub_v3::commit_log_bytes(&cache_dir, "alpha", &bytes, "test: post-genesis event").unwrap();

        let after = crate::compaction::reduce(&RefHubSource::new(&cache_dir).unwrap())
            .unwrap()
            .state;
        assert!(
            after.issues.contains_key(&new_uuid),
            "event above watermark must be applied"
        );
        // The new issue is the only difference vs genesis.
        assert_eq!(after.issues.len(), genesis.issues.len() + 1);
    }

    // ── Test 4: rollback on verification failure ─────────────────────

    #[test]
    fn rollback_restores_all_refs_when_verify_fails() {
        let (_w, _r, _crosslink_dir, cache_dir) = setup_v2_hub();

        // Snapshot pre-migration crosslink ref tips (including any dual-write
        // shadow refs — here none exist, all should roll back to absent).
        let pre = snapshot_crosslink_refs(&cache_dir).unwrap();

        // Build genesis, then TAMPER it so verification will fail (drop an issue).
        let mut genesis = build_genesis_from_files(&cache_dir).unwrap();
        let victim = *genesis.issues.keys().next().unwrap();
        genesis.issues.remove(&victim);

        let v2_tip = git_rev_parse(&cache_dir, V2_HUB_BRANCH).unwrap().unwrap();
        let pre_tips = snapshot_crosslink_refs(&cache_dir).unwrap();
        // Seed with the tampered genesis: refs get created.
        seed_v3_refs(&cache_dir, &genesis, &v2_tip).unwrap();
        assert!(rev(&cache_dir, CHECKPOINT_REF).is_some());

        // Verification must fail (tampered genesis disagrees with files).
        let result = verify_against_files(&cache_dir, &genesis);
        assert!(result.is_err(), "tampered genesis must fail verification");

        // Roll back and assert all refs restored to pre-migration tips.
        rollback_refs(&cache_dir, &pre_tips).unwrap();
        for tip in &pre {
            assert_eq!(
                rev(&cache_dir, &tip.name),
                tip.old_sha,
                "ref {} must be restored to its pre-migration tip",
                tip.name
            );
        }
        // v2 branch untouched.
        assert!(rev(&cache_dir, V2_HUB_BRANCH).is_some());
    }

    // ── Test 5: duplicate display_id refusal ─────────────────────────

    #[test]
    fn duplicate_display_id_refuses_migration() {
        let (_w, _r, crosslink_dir, cache_dir) = setup_v2_hub();

        // Write a second issue file claiming an already-used display_id (#1).
        let dup_uuid = Uuid::new_v4();
        let issue = crate::issue_file::IssueFile {
            uuid: dup_uuid,
            display_id: Some(1),
            title: "Dup id".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "alpha".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            scheduled_at: None,
            due_at: None,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };
        let dir = cache_dir.join("issues").join(dup_uuid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        crate::issue_file::write_issue_file(&dir.join("issue.json"), &issue).unwrap();

        let err = hub_v3(&crosslink_dir, false, false).unwrap_err();
        assert!(
            err.to_string().contains("duplicate display_id"),
            "must refuse on duplicate display_id, got: {err}"
        );
        // Nothing created.
        assert!(rev(&cache_dir, CHECKPOINT_REF).is_none());
        assert!(rev(&cache_dir, META_REF).is_none());
    }

    /// An offline issue created by a DEAD identity (not the current agent)
    /// has no live promotion path — the migration must mint a deterministic
    /// genesis display id for it instead of blocking forever.
    #[test]
    fn orphaned_offline_issue_gets_minted_genesis_id() {
        let (_w, _r, crosslink_dir, cache_dir) = setup_v2_hub();

        let orphan_uuid = Uuid::new_v4();
        let issue = crate::issue_file::IssueFile {
            uuid: orphan_uuid,
            display_id: None,
            title: "Orphaned offline relic".to_string(),
            description: None,
            status: crate::models::IssueStatus::Closed,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "dead-kickoff-agent".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: Some(Utc::now()),
            scheduled_at: None,
            due_at: None,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };
        let dir = cache_dir.join("issues").join(orphan_uuid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        crate::issue_file::write_issue_file(&dir.join("issue.json"), &issue).unwrap();

        hub_v3(&crosslink_dir, false, false).expect("orphaned offline relic must not block");

        let source = crate::hub_source::RefHubSource::new(&cache_dir).unwrap();
        let state = crate::compaction::reduce(&source).unwrap().state;
        let minted = state
            .display_id_map
            .get(&orphan_uuid)
            .copied()
            .expect("orphan must receive a minted genesis id");
        assert!(minted > 0, "minted id must be a real positive id");
        assert_eq!(
            state.issues[&orphan_uuid].display_id,
            Some(minted),
            "CompactIssue must carry the minted id"
        );
        // Uniqueness across the whole map.
        let mut seen = std::collections::BTreeSet::new();
        for id in state.display_id_map.values() {
            assert!(seen.insert(*id), "minted id collided: {id}");
        }
    }

    /// An offline issue created by the CURRENT agent still refuses — the
    /// promotion path (`crosslink sync`) exists and must run first.
    #[test]
    fn promotable_offline_issue_still_refuses_migration() {
        let (_w, _r, crosslink_dir, cache_dir) = setup_v2_hub();

        let mine_uuid = Uuid::new_v4();
        let issue = crate::issue_file::IssueFile {
            uuid: mine_uuid,
            display_id: None,
            title: "My pending offline issue".to_string(),
            description: None,
            status: crate::models::IssueStatus::Open,
            priority: crate::models::Priority::Medium,
            parent_uuid: None,
            created_by: "alpha".to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            closed_at: None,
            scheduled_at: None,
            due_at: None,
            labels: vec![],
            comments: vec![],
            blockers: vec![],
            related: vec![],
            milestone_uuid: None,
            time_entries: vec![],
        };
        let dir = cache_dir.join("issues").join(mine_uuid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        crate::issue_file::write_issue_file(&dir.join("issue.json"), &issue).unwrap();

        let err = hub_v3(&crosslink_dir, false, false).unwrap_err();
        assert!(
            err.to_string().contains("created by this agent"),
            "promotable offline issue must still refuse, got: {err}"
        );
        assert!(rev(&cache_dir, CHECKPOINT_REF).is_none());
    }

    // ── Test 6: no-events hub ────────────────────────────────────────

    #[test]
    fn no_events_hub_migrates_with_synthesized_watermark() {
        let remote_dir = tempfile::tempdir().unwrap();
        let work_dir = tempfile::tempdir().unwrap();
        run(remote_dir.path(), &["init", "--bare", "-b", "main"]);
        let wp = work_dir.path().to_path_buf();
        run(&wp, &["init", "-b", "main"]);
        run(&wp, &["config", "user.email", "t@t.local"]);
        run(&wp, &["config", "user.name", "T"]);
        run(&wp, &["config", "commit.gpgsign", "false"]);
        run(
            &wp,
            &[
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
        );
        std::fs::write(wp.join("README.md"), "# t\n").unwrap();
        run(&wp, &["add", "."]);
        run(&wp, &["commit", "-m", "init", "--no-gpg-sign"]);
        run(&wp, &["push", "-u", "origin", "main"]);

        let crosslink_dir = wp.join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        std::fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{"remote":"origin","layout":"v2"}"#,
        )
        .unwrap();
        write_agent(&crosslink_dir, "alpha");
        let sync = SyncManager::new(&crosslink_dir).unwrap();
        sync.init_cache().unwrap();
        let cache_dir = sync.cache_path().to_path_buf();

        hub_v3(&crosslink_dir, false, false).unwrap();

        // Watermark must be Some (synthesized), so reduce returns genesis unchanged.
        let cp = crate::checkpoint::read_checkpoint(&cache_dir);
        let _ = cp; // checkpoint dir form differs; read via the ref source below.
        let genesis = build_genesis_from_files(&cache_dir).unwrap();
        assert!(
            genesis.watermark.is_some(),
            "no-events genesis must have a watermark"
        );
        let reduced = crate::compaction::reduce(&RefHubSource::new(&cache_dir).unwrap())
            .unwrap()
            .state;
        assert_eq!(
            serde_json::to_value(&genesis).unwrap(),
            serde_json::to_value(&reduced).unwrap(),
            "no-events reduce must equal genesis"
        );
        assert!(reduced.issues.is_empty());
    }

    // ── Test 7: finalize ─────────────────────────────────────────────

    #[test]
    fn finalize_requires_confirmation_then_deletes_v2() {
        let (_w, _r, crosslink_dir, cache_dir) = setup_v2_hub();
        hub_v3(&crosslink_dir, false, false).unwrap();

        // Without --yes-delete-v2 → refusal.
        let err = hub_v3(&crosslink_dir, true, false).unwrap_err();
        assert!(
            err.to_string().contains("yes-delete-v2"),
            "finalize without confirmation must refuse, got: {err}"
        );
        // v2 branch still present.
        assert!(rev(&cache_dir, V2_HUB_BRANCH).is_some());

        // With confirmation → v2 branch gone local + remote, finalized_at set.
        let repo_root = wp_of(&crosslink_dir);
        hub_v3(&crosslink_dir, true, true).unwrap();

        // Local branch deleted (probe from the main repo root — cache worktree removed).
        assert!(
            rev(&repo_root, V2_HUB_BRANCH).is_none(),
            "local v2 branch must be gone"
        );

        // Remote branch deleted.
        let ls = Command::new("git")
            .current_dir(&repo_root)
            .args(["ls-remote", "--heads", "origin", "crosslink/hub"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&ls.stdout).trim().is_empty(),
            "remote crosslink/hub must be deleted"
        );

        // HubMeta.finalized_at is set.
        let meta = read_hub_meta(&repo_root).unwrap().unwrap();
        assert!(meta.finalized_at.is_some(), "finalized_at must be stamped");

        // Old-binary simulation: SyncManager fetch now fails loudly because the
        // branch is gone. A fresh SyncManager re-init will try to fetch
        // crosslink/hub; the branch is absent on the remote.
        let ls2 = Command::new("git")
            .current_dir(&repo_root)
            .args(["fetch", "origin", "crosslink/hub"])
            .output()
            .unwrap();
        assert!(
            !ls2.status.success(),
            "fetching the deleted crosslink/hub branch must fail (old-binary hard stop)"
        );
        let stderr = String::from_utf8_lossy(&ls2.stderr);
        assert!(
            stderr.contains("crosslink/hub") || stderr.contains("couldn't find remote ref"),
            "fetch error should reference the missing branch: {stderr}"
        );
    }

    fn wp_of(crosslink_dir: &Path) -> std::path::PathBuf {
        crosslink_dir.parent().unwrap().to_path_buf()
    }

    // ── Test 8: warn wiring ──────────────────────────────────────────

    #[test]
    fn warn_detects_migrated_hub_for_v2_operation() {
        let (_w, _r, crosslink_dir, cache_dir) = setup_v2_hub();
        hub_v3(&crosslink_dir, false, false).unwrap();

        // No tracing-capture harness exists in this crate's tests, so assert the
        // detection behavior the warn path keys on: a migrated hub reports V3.
        assert!(matches!(
            hub_v3::detect_hub_version(&cache_dir).unwrap(),
            HubVersion::V3 { .. }
        ));
        // The warn function must run without panicking on a migrated hub when
        // the caller is (exotically) still in V2 mode, and must be a silent
        // no-op for a V3-mode caller.
        hub_v3::warn_if_migrated_v2_operation(&cache_dir, hub_v3::HubMode::V2);
        hub_v3::warn_if_migrated_v2_operation(&cache_dir, hub_v3::HubMode::V3);
    }

    /// #774 — the "machine that slept through the migration" path: machine B
    /// clones while the hub is still v2, machine A migrates, then B runs
    /// `migrate hub-v3`. B must ADOPT the remote's v3 hub — never mint a
    /// second genesis from its stale local state.
    #[test]
    fn v2_local_v3_remote_adopts_instead_of_migrating() {
        // Machine A: populated v2 hub pushed to the bare remote.
        let (_wa, remote_dir, cl_a, cache_a) = setup_v2_hub();

        // Publish A's v2 hub branch so the remote ADVERTISES v2 (real v2
        // projects do; the fixture builds the hub locally only). Without
        // this, B's init_cache would see an Absent hub and bootstrap a
        // conflicting fresh v3 genesis.
        let push = std::process::Command::new("git")
            .current_dir(&cache_a)
            .args(["push", "origin", "crosslink/hub"])
            .output()
            .unwrap();
        assert!(
            push.status.success(),
            "fixture must publish the v2 hub branch: {}",
            String::from_utf8_lossy(&push.stderr)
        );

        // Machine B: clones while the remote is still v2-only.
        let work_b = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
            vec![
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
            vec!["fetch", "origin", "main"],
            vec!["checkout", "-b", "main", "origin/main"],
        ] {
            std::process::Command::new("git")
                .current_dir(work_b.path())
                .args(&args)
                .output()
                .unwrap();
        }
        let cl_b = work_b.path().join(".crosslink");
        std::fs::create_dir_all(&cl_b).unwrap();
        std::fs::write(cl_b.join("hook-config.json"), r#"{"remote":"origin"}"#).unwrap();
        write_agent(&cl_b, "beta");
        let sync_b = SyncManager::new(&cl_b).unwrap();
        sync_b.init_cache().unwrap();
        let cache_b = sync_b.cache_path().to_path_buf();
        assert!(
            matches!(
                hub_v3::detect_hub_version(&cache_b).unwrap(),
                HubVersion::V2Only
            ),
            "B must start as a v2-only clone"
        );

        // Machine A migrates; the remote now hosts the authoritative v3 hub.
        hub_v3(&cl_a, false, false).expect("A's migration must succeed");
        let remote_checkpoint_before = std::process::Command::new("git")
            .current_dir(&cache_a)
            .args(["ls-remote", "origin", "refs/heads/crosslink/checkpoint"])
            .output()
            .unwrap();
        let sha_before = String::from_utf8_lossy(&remote_checkpoint_before.stdout)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        assert!(!sha_before.is_empty(), "remote checkpoint must exist");

        // Machine B runs migrate hub-v3: must ADOPT, not re-migrate.
        hub_v3(&cl_b, false, false).expect("B must adopt the remote v3 hub");

        // B's local detection flips to V3.
        assert!(
            matches!(
                hub_v3::detect_hub_version(&cache_b).unwrap(),
                HubVersion::V3 { .. }
            ),
            "B must operate v3 after adoption"
        );

        // No second genesis: the remote checkpoint is byte-identical.
        let remote_checkpoint_after = std::process::Command::new("git")
            .current_dir(&cache_a)
            .args(["ls-remote", "origin", "refs/heads/crosslink/checkpoint"])
            .output()
            .unwrap();
        let sha_after = String::from_utf8_lossy(&remote_checkpoint_after.stdout)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            sha_before, sha_after,
            "adoption must never move the remote checkpoint"
        );

        // B's reduction sees A's state.
        let source = crate::hub_source::RefHubSource::new(&cache_b).unwrap();
        let state = crate::compaction::reduce(&source).unwrap().state;
        assert!(
            !state.issues.is_empty(),
            "B must see the migrated issues after adoption"
        );

        // Idempotent: a second run hits the already-migrated no-op path.
        hub_v3(&cl_b, false, false).expect("re-run after adoption must be a no-op");
    }
}
