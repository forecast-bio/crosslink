//! Git capability detection + compatibility shims for older Git versions.
//!
//! `git worktree add --orphan` was introduced in Git 2.42.0
//! (forecast-bio/crosslink#655). Distros like Ubuntu 22.04 LTS ship Git 2.34.1,
//! where it fails with `error: unknown option 'orphan'`. This module detects
//! support once and provides an equivalent fallback (a regular detached
//! worktree, then `git checkout --orphan` and clearing the tree) that produces
//! the same post-condition: a worktree checked out on a brand-new, unborn
//! orphan branch with an empty index and working tree.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};

/// First Git version with `git worktree add --orphan`.
const MIN_ORPHAN_WORKTREE: (u32, u32) = (2, 42);

/// Environment variable that forces the pre-2.42 fallback path regardless of
/// the installed Git version — used to exercise the shim on modern Git.
const FORCE_FALLBACK_ENV: &str = "CROSSLINK_FORCE_WORKTREE_ORPHAN_FALLBACK";

/// Parse the `major.minor` from `git --version` output, e.g.
/// `"git version 2.34.1"` → `(2, 34)` and
/// `"git version 2.39.3 (Apple Git-145)"` → `(2, 39)`.
fn parse_git_version(output: &str) -> Option<(u32, u32)> {
    let rest = output.trim().strip_prefix("git version ")?;
    let mut nums = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty());
    let major = nums.next()?.parse().ok()?;
    let minor = nums.next()?.parse().ok()?;
    Some((major, minor))
}

/// Whether the local `git` supports `worktree add --orphan` (Git >= 2.42.0).
///
/// Detected once via `git --version` and cached for the process. Setting
/// [`FORCE_FALLBACK_ENV`] forces `false` (the fallback) so the shim can be
/// exercised on a modern Git. If the version cannot be determined, assume
/// support (the fast path): a truly broken/absent git surfaces a clearer error
/// from the very next git call.
#[must_use]
pub fn supports_worktree_orphan() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        if std::env::var_os(FORCE_FALLBACK_ENV).is_some() {
            return false;
        }
        let Ok(output) = Command::new("git").arg("--version").output() else {
            return true;
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_git_version(&stdout).is_none_or(|v| v >= MIN_ORPHAN_WORKTREE)
    })
}

/// Create a worktree at `worktree_path` checked out on a new, unborn orphan
/// branch `branch` (empty index + working tree).
///
/// Uses `git worktree add --orphan` on Git >= 2.42.0 and an equivalent fallback
/// on older Git (forecast-bio/crosslink#655). `repo_root` is the main
/// repository whose ref namespace the worktree shares.
///
/// # Errors
///
/// Returns an error if any underlying git invocation fails.
pub fn add_orphan_worktree(repo_root: &Path, branch: &str, worktree_path: &str) -> Result<()> {
    add_orphan_worktree_impl(repo_root, branch, worktree_path, supports_worktree_orphan())
}

/// Mechanism for [`add_orphan_worktree`], split out so both paths are testable
/// on any installed Git via the `use_orphan_flag` argument.
fn add_orphan_worktree_impl(
    repo_root: &Path,
    branch: &str,
    worktree_path: &str,
    use_orphan_flag: bool,
) -> Result<()> {
    if use_orphan_flag {
        run_git(
            repo_root,
            &["worktree", "add", "--orphan", "-b", branch, worktree_path],
        )?;
        return Ok(());
    }

    // Fallback for Git < 2.42.0. `git worktree add --orphan` does not exist, so:
    //   1. add a regular DETACHED worktree (checks out the current HEAD tree),
    //   2. convert HEAD to a new unborn orphan branch in that worktree,
    //   3. remove every file the checkout populated, clearing index + working
    //      tree, so the orphan branch starts empty.
    // The end state matches `worktree add --orphan`: an unborn branch with an
    // empty tree, ready for the caller's first commit.
    run_git(repo_root, &["worktree", "add", "--detach", worktree_path])?;
    let wt = Path::new(worktree_path);
    run_git(wt, &["checkout", "--orphan", branch])?;

    // `git rm -rf .` clears tracked files from index AND working tree. Tolerate
    // the empty-repo case where the HEAD tree had no files to remove.
    let out = Command::new("git")
        .current_dir(wt)
        .args(["rm", "-rf", "."])
        .output()
        .with_context(|| format!("Failed to run git rm -rf . in {worktree_path}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if !stderr.contains("did not match any files") {
            bail!("git rm -rf . failed in {worktree_path}: {stderr}");
        }
    }
    Ok(())
}

/// Run a git command in `dir`, bailing with captured stderr on failure.
fn run_git(dir: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run git {args:?}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {args:?} failed: {stderr}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_git_version_handles_common_formats() {
        assert_eq!(parse_git_version("git version 2.34.1"), Some((2, 34)));
        assert_eq!(parse_git_version("git version 2.42.0"), Some((2, 42)));
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-145)"),
            Some((2, 39))
        );
        assert_eq!(parse_git_version("git version 2.54.0\n"), Some((2, 54)));
        assert_eq!(parse_git_version("not git output"), None);
    }

    #[test]
    fn version_gate_matches_2_42_boundary() {
        assert!((2, 42) >= MIN_ORPHAN_WORKTREE);
        assert!((2, 54) >= MIN_ORPHAN_WORKTREE);
        assert!((3, 0) >= MIN_ORPHAN_WORKTREE);
        assert!((2, 41) < MIN_ORPHAN_WORKTREE);
        assert!((2, 34) < MIN_ORPHAN_WORKTREE);
    }

    /// The fallback path (forced, so it runs on any Git) produces the same end
    /// state as `worktree add --orphan`: a worktree on an unborn orphan branch
    /// with an empty tree, where the caller's first commit becomes a root commit.
    #[test]
    fn fallback_creates_empty_unborn_orphan_worktree() {
        let repo = tempfile::tempdir().unwrap();
        let rp = repo.path();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.email", "t@t.local"],
            vec!["config", "user.name", "T"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            run_git(rp, &args).unwrap();
        }
        std::fs::write(rp.join("README.md"), "# main\n").unwrap();
        run_git(rp, &["add", "."]).unwrap();
        run_git(rp, &["commit", "-m", "init", "--no-gpg-sign"]).unwrap();

        let wt_path = rp.join(".crosslink").join(".hub-cache");
        let wt_str = wt_path.to_string_lossy().to_string();

        // Force the fallback regardless of the test machine's Git version.
        add_orphan_worktree_impl(rp, "crosslink/hub-v3-host", &wt_str, false).unwrap();

        // The worktree is on the orphan branch with an UNBORN HEAD (no commit).
        let head = Command::new("git")
            .current_dir(&wt_path)
            .args(["symbolic-ref", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&head.stdout).trim(),
            "refs/heads/crosslink/hub-v3-host"
        );
        assert!(
            !Command::new("git")
                .current_dir(&wt_path)
                .args(["rev-parse", "--verify", "HEAD"])
                .output()
                .unwrap()
                .status
                .success(),
            "HEAD must be unborn (no commit yet), like worktree add --orphan"
        );

        // The index is empty — main's README.md did NOT leak onto the branch.
        let staged = Command::new("git")
            .current_dir(&wt_path)
            .args(["ls-files"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&staged.stdout).trim().is_empty(),
            "orphan worktree index must be empty"
        );
        assert!(
            !wt_path.join("README.md").exists(),
            "main's files must not be present in the orphan worktree"
        );

        // The first commit becomes a ROOT commit (no parents) — a true orphan.
        run_git(&wt_path, &["commit", "--allow-empty", "-m", "genesis"]).unwrap();
        let parents = Command::new("git")
            .current_dir(&wt_path)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&parents.stdout).trim(),
            "1",
            "the genesis commit must be a root commit"
        );
    }
}
