use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{read_tracker_remote, HUB_BRANCH, HUB_CACHE_DIR, MAX_DIVERGENCE};
use crate::signing;
use crate::utils::resolve_main_repo_root;

/// Manages synchronization with the `crosslink/hub` coordination branch.
///
/// Uses a git worktree at `.crosslink/.hub-cache/` to avoid disturbing
/// the user's working tree.
pub struct SyncManager {
    /// Path to the .crosslink directory.
    pub(super) crosslink_dir: PathBuf,
    /// Path to .crosslink/.hub-cache (worktree of crosslink/hub branch).
    pub(super) cache_dir: PathBuf,
    /// The repo root (parent of .crosslink).
    pub(super) repo_root: PathBuf,
    /// Git remote name for the hub branch (from config, defaults to "origin").
    pub(super) remote: String,
    /// Operation mode, resolved ONCE here (754a PASS 2). `V3` routes mutations
    /// to per-agent refs with state-based hydration; `V2` keeps the worktree-
    /// file flow. Cached so the per-call hot paths (`fetch`, `lock_check`) do
    /// not re-probe refs on every invocation.
    pub(super) hub_mode: crate::hub_v3::HubMode,
}

impl SyncManager {
    /// Create a new `SyncManager` for the given .crosslink directory.
    ///
    /// When running inside a git worktree, automatically detects the main
    /// repository root and uses its `.crosslink/.hub-cache/` so that the
    /// shared coordination branch worktree is never duplicated.
    ///
    /// # Errors
    ///
    /// Returns an error if the repo root cannot be determined from the
    /// crosslink directory path.
    pub fn new(crosslink_dir: &Path) -> Result<Self> {
        let local_repo_root = crosslink_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root from .crosslink dir"))?
            .to_path_buf();

        // If we're inside a git worktree, resolve the main repo root so the
        // hub cache lives in one shared location rather than per-worktree.
        let repo_root =
            resolve_main_repo_root(&local_repo_root).unwrap_or_else(|| local_repo_root.clone());

        let cache_dir = repo_root.join(".crosslink").join(HUB_CACHE_DIR);
        let remote = read_tracker_remote(crosslink_dir);

        // Resolve the operation mode ONCE (754a PASS 2). Probe the hub-cache
        // worktree: its `.git` link shares the main repository's ref namespace,
        // so the v3 marker refs (`refs/crosslink/meta` + `/checkpoint`) resolve
        // there. When the cache is absent (fresh repo, never synced) detection
        // returns `Absent` ⇒ `V2`, preserving today's init behavior.
        let hub_mode = crate::hub_v3::HubMode::resolve(&cache_dir);

        Ok(Self {
            crosslink_dir: crosslink_dir.to_path_buf(),
            cache_dir,
            repo_root,
            remote,
            hub_mode,
        })
    }

    /// The resolved operation mode for this hub (V2 worktree-file or V3
    /// event-only). Decided once at construction; see [`crate::hub_v3::HubMode`].
    #[must_use]
    pub const fn hub_mode(&self) -> crate::hub_v3::HubMode {
        self.hub_mode
    }

    /// Get the configured git remote name for the hub branch.
    #[must_use]
    pub fn remote(&self) -> &str {
        &self.remote
    }

    /// Check if the cache directory is initialized.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.cache_dir.exists()
    }

    /// Get the path to the cache directory.
    #[must_use]
    pub fn cache_path(&self) -> &Path {
        &self.cache_dir
    }

    /// Check whether the configured git remote actually exists in the repo.
    ///
    /// Returns `false` when the remote (e.g. "origin") is not configured,
    /// which means hub sync operations cannot work.
    #[must_use]
    pub fn remote_exists(&self) -> bool {
        Command::new("git")
            .current_dir(&self.repo_root)
            .args(["remote", "get-url", &self.remote])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Check if the hub uses V2 layout (per-entity lock files in `locks/`).
    #[must_use]
    pub fn is_v2_layout(&self) -> bool {
        let meta_dir = self.cache_dir.join("meta");
        crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1) >= 2
    }

    // --- Private/crate helpers ---

    /// Return the cache directory path as a UTF-8 string, or bail with a
    /// clear error when the path contains non-UTF-8 bytes.
    pub(super) fn cache_path_str(&self) -> String {
        self.cache_dir.to_str().map_or_else(
            || {
                // Log and fall back to lossy conversion. A non-UTF-8 cache
                // path will cause git commands to target the wrong directory,
                // so this is loud on purpose.
                tracing::error!(
                    "hub cache path contains non-UTF-8 characters: {:?}; \
                     git operations may fail",
                    self.cache_dir
                );
                self.cache_dir.to_string_lossy().to_string()
            },
            str::to_string,
        )
    }

    pub(super) fn git_in_repo(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {args:?}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {args:?} failed: {stderr}");
        }
        Ok(output)
    }

    /// Run a git commit in the cache worktree with signing-awareness.
    ///
    /// If `commit.gpgsign` was explicitly configured at local or worktree scope
    /// (e.g. by `crosslink agent init` / `configure_signing()`), honour it so
    /// hub-cache commits carry the agent's signature for audit trail. If signing
    /// was only inherited from the user's global git config, bypass it to avoid
    /// failures when the global key isn't usable in the cache context.
    ///
    /// Before committing, self-heals a stale `user.signingkey` left over from
    /// a deleted agent worktree (GH #565) — a repair failure is logged but
    /// doesn't abort the commit, so the existing signing error still surfaces
    /// to the caller if auto-repair can't fix things.
    ///
    /// # Errors
    ///
    /// Returns an error if the git commit command fails.
    pub(super) fn git_commit_in_cache(&self, args: &[&str]) -> Result<std::process::Output> {
        // Defense-in-depth: refuse to commit if the cache worktree is broken
        // or pointed at a non-hub branch (#574). This stops every recovery /
        // bootstrap / upgrade / sync commit path from accidentally writing
        // into the parent repository if the hub-cache .git link ever breaks.
        self.verify_cache_worktree()?;

        // Best-effort self-heal for stale signingkey configs (GH #565).
        // If this fails, let the commit proceed — it may still succeed, or
        // the real signing error will surface below with full context.
        if let Err(e) = self.repair_stale_signingkey() {
            tracing::warn!("signingkey self-heal failed (non-fatal): {e}");
        }

        let local_configured = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["config", "--local", "commit.gpgsign"])
            .output()
            .is_ok_and(|o| o.status.success());
        let worktree_configured = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["config", "--worktree", "commit.gpgsign"])
            .output()
            .is_ok_and(|o| o.status.success());

        let mut cmd = Command::new("git");
        cmd.current_dir(&self.cache_dir);
        if !local_configured && !worktree_configured {
            cmd.args(["-c", "commit.gpgsign=false"]);
        }
        cmd.arg("commit").args(args);
        let output = cmd
            .output()
            .with_context(|| format!("Failed to run git commit {args:?} in cache"))?;
        if !output.status.success() {
            // Capture BOTH streams (#601). `git commit`'s "nothing to
            // commit" status message goes to stdout, not stderr — without
            // this, "nothing to commit" failures surfaced as empty errors.
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "git commit {args:?} in cache failed ({}):\nstdout: {}\nstderr: {}",
                output.status,
                stdout.trim(),
                stderr.trim(),
            );
        }
        Ok(output)
    }

    /// Get the subject line of a commit in the cache worktree.
    pub fn commit_message(&self, commit: &str) -> Result<String> {
        let output = self.git_in_cache(&["log", "-1", "--format=%s", commit])?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    pub(super) fn git_in_cache(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {args:?} in cache"))?;
        if !output.status.success() {
            // Capture BOTH streams (#601). Some git diagnostics — including
            // hook output and certain push rejections — go to stdout, not
            // stderr; capturing only stderr drops those details.
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "git {args:?} in cache failed ({}):\nstdout: {}\nstderr: {}",
                output.status,
                stdout.trim(),
                stderr.trim(),
            );
        }
        Ok(output)
    }

    /// Verify that the hub cache directory is a working tree pointed at
    /// `HUB_BRANCH`, refusing to operate otherwise.
    ///
    /// Guards against a class of data-loss bugs (#574) where a missing or
    /// broken `.git` link in the hub cache causes git invoked with
    /// `current_dir(cache_dir)` to walk up to the parent repository's git
    /// directory. Without this guard, `clean_dirty_state` and other recovery
    /// paths run `git add -A` + `git commit` against the parent repo's index
    /// and HEAD — silently landing recovery commits on whatever feature branch
    /// the user has checked out, replacing its tree with hub-branch artifacts.
    ///
    /// Two invariants are checked:
    ///
    /// 1. `git rev-parse --show-toplevel` from the cache dir resolves to the
    ///    cache dir itself (catches the walk-up case).
    /// 2. `git symbolic-ref --short HEAD` returns `HUB_BRANCH` (catches a
    ///    detached HEAD or any accidental checkout of a non-hub branch).
    ///
    /// `symbolic-ref` is used rather than `rev-parse --abbrev-ref` because the
    /// latter fails with "ambiguous argument 'HEAD'" on an unborn orphan
    /// branch, which is the legitimate state during the very first
    /// `init_cache()` commit.
    pub(super) fn verify_cache_worktree(&self) -> Result<()> {
        let toplevel_out = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .with_context(|| {
                format!(
                    "Failed to run git rev-parse in hub cache {}",
                    self.cache_dir.display()
                )
            })?;
        if !toplevel_out.status.success() {
            bail!(
                "hub cache at {} is not a git working tree \
                 (git rev-parse --show-toplevel failed: {}). \
                 Refusing to operate to avoid corrupting the parent repository (#574).",
                self.cache_dir.display(),
                String::from_utf8_lossy(&toplevel_out.stderr).trim(),
            );
        }
        let resolved_raw = String::from_utf8_lossy(&toplevel_out.stdout)
            .trim()
            .to_string();
        let resolved =
            std::fs::canonicalize(&resolved_raw).unwrap_or_else(|_| PathBuf::from(&resolved_raw));
        let expected =
            std::fs::canonicalize(&self.cache_dir).unwrap_or_else(|_| self.cache_dir.clone());
        if resolved != expected {
            bail!(
                "hub cache at {} is not bound to its own git directory — \
                 git rev-parse --show-toplevel resolved to {}. This usually means \
                 the worktree's .git link is missing or broken, so git is walking \
                 up to the parent repository. Refusing to operate to avoid \
                 corrupting the parent repository (#574).",
                self.cache_dir.display(),
                resolved_raw,
            );
        }

        let head_out = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
            .context("Failed to run git symbolic-ref --short HEAD in hub cache")?;
        if !head_out.status.success() {
            bail!(
                "hub cache at {} has detached HEAD or no HEAD: {}. \
                 Refusing to operate to avoid commits landing on the wrong ref (#574).",
                self.cache_dir.display(),
                String::from_utf8_lossy(&head_out.stderr).trim(),
            );
        }
        let branch = String::from_utf8_lossy(&head_out.stdout).trim().to_string();
        if branch != HUB_BRANCH {
            bail!(
                "hub cache at {} is on branch {} (expected {}). \
                 Refusing to operate to avoid commits landing on the wrong branch (#574).",
                self.cache_dir.display(),
                branch,
                HUB_BRANCH,
            );
        }

        Ok(())
    }

    /// Copy `.claude/hooks/` from the repo root into the hub cache worktree.
    ///
    /// `PreToolUse` hooks resolve their path via `git rev-parse --show-toplevel`.
    /// When an agent's CWD is inside the hub cache, that resolves to the cache
    /// root instead of the main repo, so the hooks must exist there too.
    /// This is a best-effort operation — if `.claude/hooks/` doesn't exist in
    /// the repo root, we silently skip.
    ///
    /// **Note**: This performs a shallow copy — only regular files in the
    /// top-level `hooks/` directory are copied. Subdirectories and symlinks
    /// are ignored. The copy runs once (skips if `dst` already exists),
    /// so hook updates in the source require deleting the cache copy to
    /// re-trigger propagation.
    pub(super) fn propagate_claude_hooks(&self) -> Result<()> {
        let src = self.repo_root.join(".claude").join("hooks");
        if !src.is_dir() {
            return Ok(());
        }
        let dst = self.cache_dir.join(".claude").join("hooks");
        if dst.is_dir() {
            return Ok(()); // already propagated
        }
        std::fs::create_dir_all(&dst)?;
        for entry in std::fs::read_dir(&src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            if ty.is_file() {
                std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
            }
        }
        Ok(())
    }

    /// Ensure the cache worktree has a git identity configured so commits
    /// succeed even in environments without a global git config (e.g. CI).
    ///
    /// Uses `--worktree` scope when the cache dir is a linked worktree to
    /// avoid leaking identity config into the shared `.git/config`.
    pub(super) fn ensure_cache_git_identity(&self) -> Result<()> {
        let has_email = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(["config", "user.email"])
            .output()
            .is_ok_and(|o| o.status.success());
        if !has_email {
            let use_worktree = signing::is_linked_worktree(&self.cache_dir);
            if use_worktree {
                signing::enable_worktree_config(&self.cache_dir)?;
            }
            let scope_flag = if use_worktree {
                "--worktree"
            } else {
                "--local"
            };
            let email_output = Command::new("git")
                .current_dir(&self.cache_dir)
                .args(["config", scope_flag, "user.email", "crosslink@localhost"])
                .output()
                .context("Failed to run git config for user.email")?;
            if !email_output.status.success() {
                bail!(
                    "git config {} user.email failed: {}",
                    scope_flag,
                    String::from_utf8_lossy(&email_output.stderr)
                );
            }

            let name_output = Command::new("git")
                .current_dir(&self.cache_dir)
                .args(["config", scope_flag, "user.name", "crosslink"])
                .output()
                .context("Failed to run git config for user.name")?;
            if !name_output.status.success() {
                bail!(
                    "git config {} user.name failed: {}",
                    scope_flag,
                    String::from_utf8_lossy(&name_output.stderr)
                );
            }

            // Verify identity is actually set — don't let commits fail later
            // with "Author identity unknown" (#469)
            let verified = Command::new("git")
                .current_dir(&self.cache_dir)
                .args(["config", "user.email"])
                .output()
                .is_ok_and(|o| o.status.success());
            if !verified {
                bail!(
                    "Failed to verify git identity in hub cache: \
                     git config set succeeded but user.email is not readable"
                );
            }
        }
        Ok(())
    }

    /// Count how many commits the local hub branch is ahead of the remote.
    /// Returns 0 if the remote ref doesn't exist or the count can't be determined.
    pub(super) fn count_unpushed_commits(&self) -> usize {
        let remote_ref = format!("{}/{}", self.remote, HUB_BRANCH);
        let range = format!("{remote_ref}..HEAD");
        match self.git_in_cache(&["rev-list", "--count", &range]) {
            Ok(output) => String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse::<usize>()
                .unwrap_or(0),
            Err(_) => 0,
        }
    }

    /// Check if local has diverged too far from remote and bail if so.
    pub(crate) fn check_divergence(&self) -> Result<()> {
        let ahead = self.count_unpushed_commits();
        if ahead > MAX_DIVERGENCE {
            bail!(
                "Hub branch has diverged: {} local commits ahead of remote \
                 (threshold: {}). This likely indicates a rebase loop. \
                 Resolve manually with: cd {} && git log --oneline {}/{}..HEAD",
                ahead,
                MAX_DIVERGENCE,
                self.cache_dir.display(),
                self.remote,
                HUB_BRANCH
            );
        }
        Ok(())
    }
}
