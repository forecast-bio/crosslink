use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve the main repository root when running inside a git worktree.
///
/// Compares `git rev-parse --git-common-dir` with `--git-dir`. If they
/// differ, we're in a worktree and the main repo root is the parent of
/// `git-common-dir`. Returns `None` if not in a git repo or if git
/// commands fail (e.g. in unit tests with plain temp directories).
pub fn resolve_main_repo_root(repo_root: &Path) -> Option<PathBuf> {
    let repo_str = repo_root.to_string_lossy();

    let common_output = Command::new("git")
        .args(["-C", &repo_str, "rev-parse", "--git-common-dir"])
        .output()
        .ok()?;

    let git_dir_output = Command::new("git")
        .args(["-C", &repo_str, "rev-parse", "--git-dir"])
        .output()
        .ok()?;

    if !common_output.status.success() || !git_dir_output.status.success() {
        return None;
    }

    let common_raw = String::from_utf8_lossy(&common_output.stdout)
        .trim()
        .to_string();
    let git_dir_raw = String::from_utf8_lossy(&git_dir_output.stdout)
        .trim()
        .to_string();

    // Resolve to absolute paths for reliable comparison
    let common_path = if Path::new(&common_raw).is_absolute() {
        PathBuf::from(&common_raw)
    } else {
        repo_root.join(&common_raw)
    };

    let git_dir_path = if Path::new(&git_dir_raw).is_absolute() {
        PathBuf::from(&git_dir_raw)
    } else {
        repo_root.join(&git_dir_raw)
    };

    // Canonicalize to handle symlinks and ".." components
    let common_canonical = common_path.canonicalize().unwrap_or(common_path);
    let git_dir_canonical = git_dir_path.canonicalize().unwrap_or(git_dir_path);

    if common_canonical != git_dir_canonical {
        // We're in a worktree — git-common-dir points to the main .git directory.
        // Its parent is the main repo root.
        common_canonical.parent().map(|p| p.to_path_buf())
    } else {
        // Not in a worktree — use the given repo root as-is.
        Some(repo_root.to_path_buf())
    }
}

/// Format a display ID for output. Negative IDs (offline) show as "L1", "L2", etc.
pub fn format_issue_id(id: i64) -> String {
    if id < 0 {
        format!("L{}", id.unsigned_abs())
    } else {
        format!("#{}", id)
    }
}

/// Truncate a string to a maximum number of characters, adding "..." if truncated.
/// Handles Unicode correctly by counting characters, not bytes.
pub fn truncate(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{}...", truncated)
    }
}

/// Check whether a name matches a Windows reserved device name.
///
/// Windows reserves names like CON, PRN, AUX, NUL, COM1-COM9, and LPT1-LPT9.
/// Files with these names (with or without extensions) cause silent failures on
/// Windows. We reject them on all platforms since data may be synced cross-platform.
pub fn is_windows_reserved_name(name: &str) -> bool {
    let upper = name.to_uppercase();
    let stem = upper.split('.').next().unwrap_or(&upper);
    matches!(
        stem,
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Atomically write content to a file by writing to a temporary file first,
/// then renaming. This prevents corrupted files from interrupted writes.
pub fn atomic_write(path: &std::path::Path, content: &[u8]) -> anyhow::Result<()> {
    use anyhow::Context;
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    let tmp_path = parent.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
    ));
    std::fs::write(&tmp_path, content)
        .with_context(|| format!("Failed to write temp file: {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to rename {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use tempfile::tempdir;

    fn init_git_repo(path: &Path) {
        let p = path.to_string_lossy().to_string();
        StdCommand::new("git")
            .args(["-C", &p, "init"])
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["-C", &p, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["-C", &p, "config", "user.name", "Test"])
            .output()
            .unwrap();
        StdCommand::new("git")
            .args(["-C", &p, "commit", "--allow-empty", "-m", "init"])
            .output()
            .unwrap();
    }

    #[test]
    fn test_resolve_main_repo_root_not_a_repo() {
        let dir = tempdir().unwrap();
        let result = resolve_main_repo_root(dir.path());
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_main_repo_root_normal_repo() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());

        let result = resolve_main_repo_root(dir.path());
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn test_resolve_main_repo_root_in_worktree() {
        let dir = tempdir().unwrap();
        let main_root = dir.path().join("main");
        std::fs::create_dir_all(&main_root).unwrap();
        init_git_repo(&main_root);

        StdCommand::new("git")
            .args([
                "-C",
                &main_root.to_string_lossy(),
                "branch",
                "feature/wt-test",
            ])
            .output()
            .unwrap();

        let wt_path = main_root.join(".worktrees").join("wt-test");
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        StdCommand::new("git")
            .args([
                "-C",
                &main_root.to_string_lossy(),
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "feature/wt-test",
            ])
            .output()
            .unwrap();

        let result = resolve_main_repo_root(&wt_path);
        assert!(result.is_some());
        assert_eq!(
            result.unwrap().canonicalize().unwrap(),
            main_root.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn test_truncate_unicode() {
        assert_eq!(truncate("héllo wörld", 8), "héllo...");
    }

    #[test]
    fn test_truncate_emoji() {
        assert_eq!(truncate("👋🌍🎉🚀🎯", 4), "👋...");
    }

    #[test]
    fn test_truncate_empty() {
        assert_eq!(truncate("", 10), "");
    }

    #[test]
    fn test_truncate_zero_max() {
        assert_eq!(truncate("hello", 0), "...");
    }

    #[test]
    fn test_windows_reserved_names_rejected() {
        for name in &["CON", "PRN", "AUX", "NUL", "COM1", "COM9", "LPT1", "LPT9"] {
            assert!(
                is_windows_reserved_name(name),
                "{} should be reserved",
                name
            );
        }
    }

    #[test]
    fn test_windows_reserved_names_case_insensitive() {
        assert!(is_windows_reserved_name("con"));
        assert!(is_windows_reserved_name("Con"));
        assert!(is_windows_reserved_name("nul"));
        assert!(is_windows_reserved_name("Aux"));
    }

    #[test]
    fn test_windows_reserved_names_with_extension() {
        assert!(is_windows_reserved_name("CON.txt"));
        assert!(is_windows_reserved_name("nul.md"));
    }

    #[test]
    fn test_non_reserved_names_allowed() {
        assert!(!is_windows_reserved_name("console"));
        assert!(!is_windows_reserved_name("printer"));
        assert!(!is_windows_reserved_name("auxiliary"));
        assert!(!is_windows_reserved_name("my-agent"));
        assert!(!is_windows_reserved_name("com10"));
        assert!(!is_windows_reserved_name("lpt10"));
    }
}
