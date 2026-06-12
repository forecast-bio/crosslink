use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::core::SyncManager;
use super::HUB_BRANCH;

// ---------------------------------------------------------------------------
// Hub cache write lock — the single REQ-8 local lock serializing every hub
// read-modify-write sequence (v3 ref writes, fetch, compaction). The v2 write
// path it once also guarded is gone (#754).
// ---------------------------------------------------------------------------

/// RAII guard for the hub cache write lock.
///
/// Holds the lock file handle open so the OS releases it on crash.
/// On normal drop, removes the lock file.
pub struct HubWriteLock {
    path: PathBuf,
    _file: std::fs::File,
}

impl Drop for HubWriteLock {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    "failed to release hub write lock {}: {}",
                    self.path.display(),
                    e
                );
            }
        }
    }
}

/// Try to atomically create the lock file and write our PID.
/// Returns the guard on success, or the IO error on failure.
fn try_create_lock(lock_path: &Path) -> std::io::Result<HubWriteLock> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)?;
    writeln!(f, "{}", std::process::id())?;
    Ok(HubWriteLock {
        path: lock_path.to_path_buf(),
        _file: f,
    })
}

/// Acquire the hub cache write lock at the given path.
///
/// Blocks up to 30 seconds, checking for stale locks via PID liveness.
/// Returns an RAII guard that releases the lock on drop.
pub fn acquire_hub_lock(lock_path: &Path) -> Result<HubWriteLock> {
    acquire_hub_lock_with_timeout(lock_path, Duration::from_secs(30))
}

/// Inner implementation of lock acquisition with a configurable timeout.
///
/// Separated from [`acquire_hub_lock`] so tests can pass a short timeout
/// without waiting 30 seconds. Production callers use [`acquire_hub_lock`]
/// which hard-codes the 30-second budget.
fn acquire_hub_lock_with_timeout(lock_path: &Path, max_wait: Duration) -> Result<HubWriteLock> {
    let poll_interval = Duration::from_millis(100);
    let start = std::time::Instant::now();

    loop {
        match try_create_lock(lock_path) {
            Ok(guard) => return Ok(guard),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Lock file exists — check if the holder is still alive.
                let holder_alive = std::fs::read_to_string(lock_path)
                    .ok()
                    .and_then(|content| content.trim().parse::<u32>().ok())
                    .is_some_and(|pid| {
                        std::process::Command::new("kill")
                            .args(["-0", &pid.to_string()])
                            .output()
                            .is_ok_and(|o| o.status.success())
                    });

                if !holder_alive {
                    // Stale lock — remove and immediately re-attempt in the same
                    // iteration to minimize the TOCTOU window (#347).
                    let _ = std::fs::remove_file(lock_path);
                    if let Ok(guard) = try_create_lock(lock_path) {
                        return Ok(guard);
                    }
                    // Another process won the race — fall through to retry loop
                }

                if start.elapsed() > max_wait {
                    // On timeout, only force-remove the lock when the holder is
                    // confirmed dead (or when the PID content is unreadable/absent).
                    // If the holder is a live process, bail instead of stealing the
                    // lock — stealing would allow two processes to mutate the hub
                    // worktree concurrently, which is the exact bug this lock prevents.
                    if holder_alive {
                        bail!(
                            "hub write lock held by live process for >30s ({}); \
                             waiting aborted to avoid concurrent worktree mutation — \
                             retry, or remove the lock file if the process is hung: {}",
                            std::fs::read_to_string(lock_path)
                                .ok()
                                .and_then(|c| c.trim().parse::<u32>().ok())
                                .map_or_else(
                                    || "<unknown PID>".to_string(),
                                    |pid| format!("PID {pid}")
                                ),
                            lock_path.display()
                        );
                    }
                    // Holder is dead (or PID was unreadable) — force-remove the stale
                    // lock and try to acquire. This mirrors the not-alive fast path
                    // above but is reached only after the wait budget is exhausted.
                    let _ = std::fs::remove_file(lock_path);
                    match try_create_lock(lock_path) {
                        Ok(guard) => return Ok(guard),
                        Err(_) => bail!(
                            "Hub lock held for >30s and could not be acquired after force-removal"
                        ),
                    }
                }
                std::thread::sleep(poll_interval);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

impl SyncManager {
    /// Acquire the hub cache write lock.
    ///
    /// All code that mutates the hub (v3 ref writes, fetch, compaction) must
    /// hold this single REQ-8 lock to prevent races (#457, #459).
    pub(crate) fn acquire_lock(&self) -> Result<HubWriteLock> {
        let lock_path = self.cache_dir.join(".hub-write-lock");
        acquire_hub_lock(&lock_path)
    }
    /// Initialize the hub cache directory.
    ///
    /// The hub cache is a linked git worktree whose `.git` link shares the main
    /// repository's object store and ref namespace, so the v3 `refs/crosslink/*`
    /// refs resolve from it. The worktree branch is only a host for that working
    /// directory; v3 stores no data in its tree.
    ///
    /// Behavior by detected hub version (754b REQ-10 — fresh hubs bootstrap v3):
    ///
    /// - A `crosslink/hub` v2 branch exists (local or remote): create a worktree
    ///   on it. This is the read-only / migration path — v2 is never written
    ///   anymore, only read for inspection and consumed by `migrate hub-v3`.
    /// - The remote already advertises v3 marker refs (fresh clone of a migrated
    ///   project): create an orphan host worktree, fetch the v3 refs to join the
    ///   existing hub, and resolve [`crate::hub_v3::HubMode::V3`].
    /// - Neither exists (brand-new hub): create an orphan host worktree and
    ///   bootstrap the v3 marker refs ([`crate::hub_v3::bootstrap_v3_hub`]), then
    ///   resolve [`crate::hub_v3::HubMode::V3`].
    ///
    /// # Errors
    ///
    /// Returns an error if git operations (fetch, worktree, commit) or the v3
    /// bootstrap fail.
    pub fn init_cache(&self) -> Result<()> {
        // Auto-migrate from old crosslink/locks branch if needed
        self.migrate_from_locks_branch()?;

        if self.cache_dir.exists() {
            return Ok(());
        }

        // Does a v2 `crosslink/hub` branch exist anywhere?
        let has_remote_v2 = self
            .git_in_repo(&["ls-remote", "--heads", &self.remote, HUB_BRANCH])
            .is_ok_and(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty());
        let has_local_v2 = self
            .git_in_repo(&["rev-parse", "--verify", HUB_BRANCH])
            .is_ok();

        if has_remote_v2 || has_local_v2 {
            // V2 hub (read-only / migration path) — worktree it as today.
            self.init_v2_worktree(has_remote_v2, has_local_v2)?;
        } else {
            // No v2 hub. Either the remote already advertises v3 refs (join the
            // existing hub) or this is a brand-new hub (bootstrap v3).
            self.init_v3_host_worktree()?;
            let remote = self.remote_exists().then(|| self.remote.clone());
            // The configured remote, but only when it already advertises v3 refs.
            let remote_with_v3 = remote.clone().filter(|r| {
                matches!(
                    crate::hub_v3::detect_remote_hub_version(&self.repo_root, r),
                    Ok(crate::hub_v3::HubVersion::V3 { .. })
                )
            });
            if let Some(remote) = remote_with_v3 {
                // Fresh clone of a migrated project — fetch the v3 refs to join.
                crate::hub_v3::fetch_v3_refs_for_join(&self.cache_dir, &remote)?;
            } else {
                // Brand-new hub — bootstrap the v3 marker refs.
                let agent_id = crate::identity::AgentConfig::load(&self.crosslink_dir)?
                    .map_or_else(|| "hub-v3-bootstrap".to_string(), |a| a.agent_id);
                let outcome =
                    crate::hub_v3::bootstrap_v3_hub(&self.cache_dir, &agent_id, remote.as_deref())?;
                if let Some(pushes) = &outcome.pushed {
                    for (ref_name, push) in pushes {
                        if !matches!(
                            push,
                            crate::hub_v3::PushOutcome::Pushed
                                | crate::hub_v3::PushOutcome::NoRemote
                        ) {
                            tracing::warn!(
                                "v3 bootstrap: pushing {ref_name} did not complete: {push:?} \
                                 (local hub is ready; a later `crosslink sync` retries the push)"
                            );
                        }
                    }
                }
            }
            // The hub is now v3 locally — flip the cached mode (resolved as
            // `Absent` => `V2` at construction, before these refs existed).
            self.hub_mode.set(crate::hub_v3::HubMode::V3);
        }

        // Ensure identity so callers that commit in the cache don't fail in CI.
        self.ensure_cache_git_identity()?;

        // Propagate .claude/hooks into the cache worktree so that PreToolUse
        // hooks (which resolve via `git rev-parse --show-toplevel`) still work
        // when an agent's CWD lands inside the hub cache.
        self.propagate_claude_hooks()?;

        Ok(())
    }

    /// Create a worktree on the legacy v2 `crosslink/hub` branch (read-only /
    /// migration path). v2 is never written anymore; this exists so the
    /// migration and v2 inspection can read issue files, counters, and logs.
    fn init_v2_worktree(&self, has_remote_v2: bool, has_local_v2: bool) -> Result<()> {
        if has_remote_v2 {
            self.git_in_repo(&["fetch", &self.remote, HUB_BRANCH])?;
        }
        if has_local_v2 {
            self.git_in_repo(&["worktree", "add", &self.cache_path_str(), HUB_BRANCH])?;
        } else {
            let remote_ref = format!("{}/{}", self.remote, HUB_BRANCH);
            self.git_in_repo(&[
                "worktree",
                "add",
                "-b",
                HUB_BRANCH,
                &self.cache_path_str(),
                &remote_ref,
            ])?;
        }
        Ok(())
    }

    /// Create an empty orphan worktree to host the v3 working directory.
    ///
    /// The host branch ([`super::HUB_V3_HOST_BRANCH`], an orphan with one empty
    /// commit) carries no hub data; it only makes the cache a valid git worktree
    /// whose `.git` link shares the main repo's ref namespace, so `refs/crosslink/*`
    /// resolve. It is deliberately NOT [`HUB_BRANCH`] (`crosslink/hub`), whose
    /// presence would make detection report a v2 hub. A single empty genesis
    /// commit gives `git log` etc. a valid HEAD.
    fn init_v3_host_worktree(&self) -> Result<()> {
        self.git_in_repo(&[
            "worktree",
            "add",
            "--orphan",
            "-b",
            super::HUB_V3_HOST_BRANCH,
            &self.cache_path_str(),
        ])?;
        self.ensure_cache_git_identity()?;
        self.git_commit_in_cache(&[
            "--allow-empty",
            "-m",
            "Initialize crosslink v3 hub worktree",
        ])?;
        Ok(())
    }

    /// Fetch the latest hub state from remote and integrate it.
    ///
    /// Routes by mode: v3 adopts every agent ref + the checkpoint and refreshes
    /// the local checkpoint cache ([`Self::fetch_v3`]); a frozen v2 hub takes a
    /// read-only mirror update ([`Self::fetch_v2_readonly`]) for inspection /
    /// migration. Never rebases or commits — there are no local-only hub commits
    /// anymore (#754).
    ///
    /// # Errors
    ///
    /// Returns an error if acquiring the lock or the v2 mirror fetch fails.
    pub fn fetch(&self) -> Result<()> {
        // Acquire the single REQ-8 hub write lock so fetch's ref/worktree
        // mutation does not race a concurrent hub write.
        let lock_guard = self.acquire_lock()?;

        // V3: ref-based fetch (754a PASS 2). No worktree reset/rebase — adopt
        // other agents' refs + checkpoint and compact.
        if self.hub_mode.get().is_v3() {
            self.fetch_v3(&lock_guard);
            return Ok(());
        }
        // V2 path holds the guard for the rest of this scope (RAII release).
        let _lock_guard = lock_guard;

        // 754b: the v2 branch is FROZEN — no client writes it anymore (the v2
        // write path was deleted in B1, the conflict/repair machinery in B2).
        // This fetch is a READ-ONLY mirror update for inspection and as the
        // source for `crosslink migrate hub-v3`: fetch the branch and
        // reset-to-remote, with NO recovery commits, NO rebase, NO dirty-state
        // writes. Because nothing local ever diverges, reset-to-remote is always
        // a safe, lossless mirror.
        self.fetch_v2_readonly()
    }

    /// Read-only mirror update of the frozen v2 `crosslink/hub` branch (754b).
    ///
    /// Fetches the branch and resets the worktree to the remote tip so v2 issue
    /// files, counters, and logs can be inspected and consumed by
    /// `migrate hub-v3`. Never commits, rebases, or writes to the branch — the
    /// v2 era is over and the only writers left are pre-754b binaries.
    fn fetch_v2_readonly(&self) -> Result<()> {
        // Try fetching. Offline / missing remote / missing branch is non-fatal:
        // fall back to whatever local mirror state already exists.
        let fetch_result = self.git_in_cache(&["fetch", &self.remote, HUB_BRANCH]);
        if let Err(e) = &fetch_result {
            let err_str = e.to_string();
            if err_str.contains("Could not resolve host")
                || err_str.contains("Could not read from remote")
                || err_str.contains("does not appear to be a git repository")
                || err_str.contains("No such remote")
                || err_str.contains("couldn't find remote ref")
            {
                return Ok(());
            }
            fetch_result?;
        }

        // Reset the worktree to the remote tip (lossless mirror — v2 is frozen).
        let remote_ref = format!("{}/{}", self.remote, HUB_BRANCH);
        let reset_result = self.git_in_cache(&["reset", "--hard", &remote_ref]);
        if let Err(e) = &reset_result {
            let err_str = e.to_string();
            // Remote branch not present yet — keep local mirror.
            if err_str.contains("unknown revision") || err_str.contains("ambiguous argument") {
                return Ok(());
            }
            reset_result?;
        }

        Ok(())
    }

    /// V3 ref-based fetch (754a PASS 2, REQ-3).
    ///
    /// 1. `git fetch <remote> '+refs/crosslink/checkpoint:refs/crosslink-remote/checkpoint'
    ///    'refs/crosslink/agents/*:refs/crosslink-remote/agents/*'` — checkpoint
    ///    forced (pure cache), agent refs non-forced into tracking refs.
    /// 2. For each OTHER agent's ref, adopt the remote tracking tip
    ///    (writer-authoritative: the agent is the single writer of its ref, so
    ///    its remote tip is canonical even after a REQ-11 prune rewrote history
    ///    non-fast-forward — we never need to merge another writer's ref). Our
    ///    OWN ref is never moved by fetch (we are its writer).
    /// 3. Adopt the checkpoint remote tip when its watermark >= our local
    ///    watermark; otherwise keep local (either is deterministic content).
    /// 4. Refresh the LOCAL checkpoint from the adopted refs (reduce + write,
    ///    NO prune). Hydration is driven separately by the caller.
    ///
    /// # Why fetch does NOT prune
    ///
    /// The REQ-11 own-ref prune rewrites the agent's own ref to a shorter
    /// history. Doing that on the READ-mostly fetch path would make the next
    /// own-ref push non-fast-forward against the un-pruned remote ref (our
    /// pushes are plain fast-forward, REQ-1). Prune is therefore confined to the
    /// explicit `compact` command (where the checkpoint is pushed and the prune
    /// is intentional). Fetch only refreshes the local checkpoint CACHE.
    ///
    /// Offline / missing remote is non-fatal — local refs are used as-is.
    fn fetch_v3(&self, hub_lock: &super::HubWriteLock) {
        let _ = hub_lock; // caller already holds the hub write lock (REQ-8)
        self.fetch_and_adopt_v3_refs();
        // Refresh the local checkpoint cache from the adopted refs (no prune).
        self.refresh_local_checkpoint();
    }

    /// Fetch the v3 refs and apply the adoption rules WITHOUT acquiring the hub
    /// lock or writing the checkpoint. The caller MUST already hold the hub
    /// write lock (REQ-8). Used by [`Self::fetch_v3`] (which then refreshes the
    /// checkpoint) and by the v3 write path (`commit_v3`), which fetches other
    /// agents' refs before reducing so a lock claim-confirm sees the full event
    /// set. Offline / missing-remote is a no-op (local refs are used as-is).
    pub(crate) fn fetch_and_adopt_v3_refs(&self) {
        // 1. Fetch checkpoint (forced) + agent refs into tracking refs.
        let fetch_result = self.git_in_cache(&[
            "fetch",
            &self.remote,
            "+refs/crosslink/checkpoint:refs/crosslink-remote/checkpoint",
            "refs/crosslink/agents/*:refs/crosslink-remote/agents/*",
        ]);
        if fetch_result.is_err() {
            // Offline / no remote refs yet — nothing to adopt; local refs stand.
            return;
        }

        // Our own agent id, so we never move our own ref from the remote.
        let own_agent_id = crate::identity::AgentConfig::load(&self.crosslink_dir)
            .ok()
            .flatten()
            .map(|a| a.agent_id);

        // 2. Adopt OTHER agents' refs to their remote tracking tip.
        let tips = match self.list_remote_agent_tips() {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("v3 fetch: could not list remote agent tips: {e}");
                return;
            }
        };
        for (agent_id, remote_tip) in tips {
            if own_agent_id.as_deref() == Some(agent_id.as_str()) {
                continue; // never move our own ref from the remote
            }
            let local_ref = format!("{}{agent_id}", crate::hub_v3::AGENT_REF_PREFIX);
            // Writer-authoritative: adopt unconditionally (the remote tip is the
            // single writer's canonical history, FF or not after their prune).
            if let Err(e) = self.git_in_cache(&["update-ref", &local_ref, &remote_tip]) {
                tracing::warn!("v3 fetch: failed to adopt ref '{local_ref}': {e}");
            }
        }

        // 3. Adopt the checkpoint by watermark comparison.
        self.adopt_checkpoint_by_watermark();
    }

    /// Enumerate `(agent_id, sha)` for every remote-tracking agent ref under
    /// `refs/crosslink-remote/agents/*`.
    fn list_remote_agent_tips(&self) -> Result<Vec<(String, String)>> {
        let output = self.git_in_cache(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/crosslink-remote/agents/*",
        ])?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut out = Vec::new();
        for line in stdout.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((refname, sha)) = line.split_once(' ') else {
                continue;
            };
            if let Some(agent_id) = refname.strip_prefix("refs/crosslink-remote/agents/") {
                out.push((agent_id.to_string(), sha.to_string()));
            }
        }
        Ok(out)
    }

    /// Adopt the remote checkpoint tracking tip into the local checkpoint ref
    /// when the remote watermark is >= the local watermark. Either checkpoint is
    /// deterministic content for its covered event set, so adopting the
    /// higher-watermark one minimizes re-reduction without risking data loss.
    fn adopt_checkpoint_by_watermark(&self) {
        let remote_tracking = "refs/crosslink-remote/checkpoint";
        let Some(remote_tip) =
            crate::hub_v3::git_rev_parse_optional(&self.cache_dir, remote_tracking)
                .ok()
                .flatten()
        else {
            return; // no remote checkpoint
        };
        let local_wm = self.checkpoint_watermark_count(crate::hub_v3::CHECKPOINT_REF);
        let remote_wm = self.checkpoint_watermark_count(remote_tracking);
        if remote_wm >= local_wm {
            if let Err(e) =
                self.git_in_cache(&["update-ref", crate::hub_v3::CHECKPOINT_REF, &remote_tip])
            {
                tracing::warn!("v3 fetch: failed to adopt remote checkpoint: {e}");
            }
        }
    }

    /// Read a coarse "watermark rank" for a checkpoint ref: the number of events
    /// its watermark covers, approximated by the watermark's `agent_seq` plus a
    /// presence bit. Returns `i64::MIN`-like 0 when absent. Used only for the
    /// adopt-higher comparison; the content is identical for equal coverage.
    fn checkpoint_watermark_count(&self, ref_name: &str) -> i64 {
        let Some(tip) = crate::hub_v3::git_rev_parse_optional(&self.cache_dir, ref_name)
            .ok()
            .flatten()
        else {
            return -1;
        };
        let spec = format!("{tip}:state.json");
        let Some(bytes) = crate::hub_v3::git_cat_file_blob_optional(&self.cache_dir, &spec)
            .ok()
            .flatten()
        else {
            return 0;
        };
        match crate::checkpoint::CheckpointState::from_slice(&bytes) {
            Ok(state) => state
                .watermark
                .map_or(0, |w| i64::try_from(w.agent_seq).unwrap_or(i64::MAX)),
            Err(_) => 0,
        }
    }

    /// Refresh the LOCAL checkpoint ref's `state.json` from a fresh reduction of
    /// the v3 ref namespace, WITHOUT pruning any agent ref and WITHOUT pushing.
    ///
    /// The checkpoint is a pure local cache here (REQ-7): writing it lets the
    /// cheap [`crate::sync::SyncManager::read_locks_v3`] path read materialized
    /// locks without a full reduce. The idempotency guard inside the checkpoint
    /// write makes this a true no-op when the state is unchanged. Best-effort: a
    /// reduce/write failure is logged, never propagated (readers reduce on
    /// demand regardless).
    fn refresh_local_checkpoint(&self) {
        let source = match crate::hub_source::RefHubSource::new(&self.cache_dir) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("v3 fetch: RefHubSource construction failed (non-fatal): {e}");
                return;
            }
        };
        let mut state = match crate::compaction::reduce(&source) {
            Ok(o) => o.state,
            Err(e) => {
                tracing::warn!("v3 fetch: reduction failed (non-fatal): {e}");
                return;
            }
        };
        state.compaction_lease = None;
        let bytes = match serde_json::to_vec_pretty(&state) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("v3 fetch: checkpoint serialization failed (non-fatal): {e}");
                return;
            }
        };
        // Skip the write when the local checkpoint already matches (idempotent).
        if let Ok(Some(tip)) =
            crate::hub_v3::git_rev_parse_optional(&self.cache_dir, crate::hub_v3::CHECKPOINT_REF)
        {
            let spec = format!("{tip}:state.json");
            if let Ok(Some(existing)) =
                crate::hub_v3::git_cat_file_blob_optional(&self.cache_dir, &spec)
            {
                if existing == bytes {
                    return;
                }
            }
        }
        if let Err(e) = crate::hub_v3::commit_blob_to_ref(
            &self.cache_dir,
            crate::hub_v3::CHECKPOINT_REF,
            "state.json",
            &bytes,
            "crosslink v3 checkpoint (fetch refresh)",
        ) {
            tracing::warn!("v3 fetch: local checkpoint refresh failed (non-fatal): {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Verify that when the lock file contains a live PID (our own process),
    /// `acquire_hub_lock_with_timeout` returns an error that names the PID
    /// and does NOT remove the lock file.
    ///
    /// This tests Fix 2: before the fix, the timeout branch would force-remove
    /// the lock regardless of holder liveness, allowing concurrent worktree
    /// mutation.
    #[test]
    fn test_acquire_hub_lock_live_holder_bails_without_stealing() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join(".hub-write-lock");

        // Write our own PID into the lock file — the current process is
        // definitely alive, so the liveness check must return true.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
                .expect("failed to create lock file");
            writeln!(f, "{}", std::process::id()).unwrap();
        }

        // Use a short timeout (300 ms > 2 × poll_interval=100 ms) so the
        // test completes quickly.
        let timeout = Duration::from_millis(300);
        let err = match acquire_hub_lock_with_timeout(&lock_path, timeout) {
            Err(e) => e,
            Ok(_guard) => panic!("expected acquire to fail when a live process holds the lock"),
        };

        let msg = err.to_string();
        assert!(
            msg.contains(&std::process::id().to_string()),
            "error should include holder PID, got: {msg}"
        );
        assert!(
            msg.contains("live process"),
            "error should mention live process, got: {msg}"
        );

        // Lock file must still exist — we did not steal it.
        assert!(
            lock_path.exists(),
            "lock file was removed by the acquire attempt (lock was stolen)"
        );
    }
}
