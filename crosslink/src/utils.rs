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
}
