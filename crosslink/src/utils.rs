use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve the main repository root when running inside a git worktree.
///
/// Compares `git rev-parse --git-common-dir` with `--git-dir`. If they
/// differ, we're in a worktree and the main repo root is the parent of
/// `git-common-dir`. Returns `None` if not in a git repo or if git
/// commands fail (e.g. in unit tests with plain temp directories).
#[must_use]
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

    if common_canonical == git_dir_canonical {
        // Not in a worktree — use the given repo root as-is.
        Some(repo_root.to_path_buf())
    } else {
        // We're in a worktree — git-common-dir points to the main .git directory.
        // Its parent is the main repo root.
        common_canonical.parent().map(std::path::Path::to_path_buf)
    }
}

/// Format a display ID for output. Negative IDs (offline) show as "L1", "L2", etc.
#[must_use]
pub fn format_issue_id(id: i64) -> String {
    if id < 0 {
        format!("L{}", id.unsigned_abs())
    } else {
        format!("#{id}")
    }
}

/// Truncate a string to a maximum number of characters, adding "..." if truncated.
/// Handles Unicode correctly by counting characters, not bytes.
#[must_use]
pub fn truncate(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

/// Check whether a name matches a Windows reserved device name.
///
/// Windows reserves names like CON, PRN, AUX, NUL, COM1-COM9, and LPT1-LPT9.
/// Files with these names (with or without extensions) cause silent failures on
/// Windows. We reject them on all platforms since data may be synced cross-platform.
#[must_use]
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
///
/// # Errors
///
/// Returns an error if writing the temporary file or renaming it fails.
pub fn atomic_write(path: &std::path::Path, content: &[u8]) -> anyhow::Result<()> {
    use anyhow::Context;
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
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

/// Escape a string for safe interpolation into a shell command.
/// Wraps in single quotes with embedded single quotes escaped as `'\''`.
#[must_use]
pub fn shell_escape_arg(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ── Compact identifiers (base62) ─────────────────────────────────────────

const BASE62_CHARS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Generate a random 4-character base62 identifier.
///
/// 4 chars of base62 = 62^4 ≈ 14.8M possibilities — sufficient for both
/// per-repo IDs and per-kickoff agent IDs.
pub fn generate_compact_id() -> String {
    use std::time::SystemTime;

    // Counter to avoid collisions within the same nanosecond
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let pid = std::process::id();
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mixed = u64::from(nanos)
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(u64::from(pid))
        .wrapping_add(u64::from(count));
    base62_encode_4(mixed)
}

/// Encode a u64 value into a 4-character base62 string.
#[must_use]
pub fn base62_encode_4(mut value: u64) -> String {
    let mut result = String::with_capacity(4);
    let mut buf = [0u8; 4];
    for b in buf.iter_mut().rev() {
        *b = BASE62_CHARS[(value % 62) as usize];
        value /= 62;
    }
    for &b in &buf {
        result.push(b as char);
    }
    result
}

/// Compose a structured name from repo ID, agent ID, and slug.
///
/// Format: `<repo>-<agent>-<slug>` (max 64 chars total).
/// The slug is truncated at a word boundary if the full name would exceed 64 chars.
#[must_use]
pub fn compose_compact_name(repo_id: &str, agent_id: &str, slug: &str) -> String {
    let prefix_len = repo_id.len() + 1 + agent_id.len() + 1; // "repo-agent-"
    let max_slug = 64 - prefix_len;
    let truncated_slug = truncate_slug(slug, max_slug);
    format!("{repo_id}-{agent_id}-{truncated_slug}")
}

/// Truncate a slug to fit within `max_len`, cutting at a word boundary (hyphen).
#[must_use]
pub fn truncate_slug(slug: &str, max_len: usize) -> &str {
    if slug.len() <= max_len {
        return slug;
    }
    // Cut at the last hyphen before max_len to avoid mid-word truncation
    slug[..max_len]
        .rfind('-')
        .map_or(&slug[..max_len], |pos| &slug[..pos])
}

/// Validate that a composed name fits within the 64-char `agent_id` limit.
///
/// # Errors
///
/// Returns an error if the name exceeds 64 characters or contains invalid characters.
pub fn validate_compact_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        name.len() <= 64,
        "Composed name '{}' exceeds 64-char limit ({} chars)",
        name,
        name.len()
    );
    anyhow::ensure!(
        name.chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_'),
        "Composed name contains invalid characters: '{name}'"
    );
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

    #[test]
    fn test_format_issue_id_positive() {
        assert_eq!(format_issue_id(1), "#1");
        assert_eq!(format_issue_id(42), "#42");
        assert_eq!(format_issue_id(0), "#0");
    }

    #[test]
    fn test_format_issue_id_negative() {
        assert_eq!(format_issue_id(-1), "L1");
        assert_eq!(format_issue_id(-99), "L99");
    }

    #[test]
    fn test_atomic_write_creates_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.txt");
        atomic_write(&path, b"hello world").unwrap();
        let contents = std::fs::read(&path).unwrap();
        assert_eq!(contents, b"hello world");
    }

    #[test]
    fn test_atomic_write_overwrites_existing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.txt");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        let contents = std::fs::read(&path).unwrap();
        assert_eq!(contents, b"second");
    }

    #[test]
    fn test_atomic_write_leaves_no_tmp_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("output.txt");
        atomic_write(&path, b"data").unwrap();
        // The .output.txt.tmp file should not remain after a successful write
        let tmp_path = dir.path().join(".output.txt.tmp");
        assert!(!tmp_path.exists());
    }

    // ── Compact identifier tests ─────────────────────────────────────────

    #[test]
    fn test_base62_encode_4_produces_4_chars() {
        assert_eq!(base62_encode_4(0).len(), 4);
        assert_eq!(base62_encode_4(u64::MAX).len(), 4);
        assert_eq!(base62_encode_4(12345).len(), 4);
    }

    #[test]
    fn test_base62_encode_4_zero() {
        assert_eq!(base62_encode_4(0), "0000");
    }

    #[test]
    fn test_base62_encode_4_deterministic() {
        assert_eq!(base62_encode_4(999999), base62_encode_4(999999));
    }

    #[test]
    fn test_base62_encode_4_all_valid_chars() {
        let result = base62_encode_4(0xDEADBEEF);
        assert!(result.chars().all(|c| c.is_alphanumeric()));
    }

    #[test]
    fn test_generate_compact_id_length() {
        let id = generate_compact_id();
        assert_eq!(id.len(), 4);
    }

    #[test]
    fn test_generate_compact_id_unique() {
        let ids: Vec<String> = (0..100).map(|_| generate_compact_id()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        // With 14.8M possibilities, 100 calls should be all unique
        assert_eq!(unique.len(), 100);
    }

    #[test]
    fn test_compose_compact_name_basic() {
        let name = compose_compact_name("XZ3j", "81jF", "auth-system");
        assert_eq!(name, "XZ3j-81jF-auth-system");
        assert!(name.len() <= 64);
    }

    #[test]
    fn test_compose_compact_name_truncates_long_slug() {
        let long_slug = "a]".repeat(60);
        let slug = long_slug.trim_end_matches(']').replace(']', "-long");
        let name = compose_compact_name("XZ3j", "81jF", &slug);
        assert!(name.len() <= 64, "Name too long: {} chars", name.len());
    }

    #[test]
    fn test_truncate_slug_short() {
        assert_eq!(truncate_slug("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_slug_at_word_boundary() {
        assert_eq!(truncate_slug("hello-world-test", 12), "hello-world");
    }

    #[test]
    fn test_truncate_slug_no_hyphen() {
        assert_eq!(truncate_slug("abcdefghij", 5), "abcde");
    }

    #[test]
    fn test_validate_compact_name_ok() {
        assert!(validate_compact_name("XZ3j-81jF-auth-system").is_ok());
    }

    #[test]
    fn test_validate_compact_name_too_long() {
        let name = "a".repeat(65);
        assert!(validate_compact_name(&name).is_err());
    }

    #[test]
    fn test_validate_compact_name_invalid_chars() {
        assert!(validate_compact_name("hello world").is_err());
        assert!(validate_compact_name("hello/world").is_err());
    }
}
