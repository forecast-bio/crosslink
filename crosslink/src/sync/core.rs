use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{read_tracker_remote, HUB_CACHE_DIR};
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
    /// Operation mode, resolved at construction (754a PASS 2) and re-resolved
    /// after a fresh-hub bootstrap (754b). `V3` routes mutations to per-agent
    /// refs with state-based hydration; `V2` keeps the worktree-file flow.
    /// Cached so the per-call hot paths (`fetch`, `lock_check`) do not re-probe
    /// refs on every invocation. Interior mutability lets [`Self::init_cache`]
    /// flip a fresh `Absent`-resolved `V2` to `V3` once it bootstraps the v3
    /// marker refs, without invalidating the `&self` API surface.
    pub(super) hub_mode: std::cell::Cell<crate::hub_v3::HubMode>,
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
        // so the v3 marker refs (`refs/heads/crosslink/meta` + `/checkpoint`) resolve
        // there. When the cache is absent (fresh repo, never synced) detection
        // returns `Absent` ⇒ `V2`, preserving today's init behavior.
        let hub_mode = crate::hub_v3::HubMode::resolve(&cache_dir);

        Ok(Self {
            crosslink_dir: crosslink_dir.to_path_buf(),
            cache_dir,
            repo_root,
            remote,
            hub_mode: std::cell::Cell::new(hub_mode),
        })
    }

    /// The resolved operation mode for this hub (V2 worktree-file or V3
    /// event-only). Decided at construction and re-resolved after a fresh-hub
    /// bootstrap; see [`crate::hub_v3::HubMode`].
    #[must_use]
    pub fn hub_mode(&self) -> crate::hub_v3::HubMode {
        self.hub_mode.get()
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
}
