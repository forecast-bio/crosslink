use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::utils::resolve_main_repo_root;

/// Directory name under .crosslink for the knowledge cache worktree.
pub const KNOWLEDGE_CACHE_DIR: &str = ".knowledge-cache";

/// The knowledge branch name.
pub const KNOWLEDGE_BRANCH: &str = "crosslink/knowledge";

/// Manages the `crosslink/knowledge` orphan branch for shared research.
///
/// Uses a git worktree at `.crosslink/.knowledge-cache/` to avoid disturbing
/// the user's working tree. Follows the same pattern as `SyncManager`.
pub struct KnowledgeManager {
    /// Path to the .crosslink directory (used by signing support).
    pub(super) crosslink_dir: PathBuf,
    /// Path to .crosslink/.knowledge-cache (worktree of crosslink/knowledge branch).
    pub(super) cache_dir: PathBuf,
    /// The repo root (parent of .crosslink).
    pub(super) repo_root: PathBuf,
    /// Git remote name for the knowledge branch (from config, defaults to "origin").
    pub(super) remote: String,
}

/// Parsed YAML frontmatter from a knowledge page.
#[derive(Debug, Clone, PartialEq)]
pub struct PageFrontmatter {
    pub title: String,
    pub tags: Vec<String>,
    pub sources: Vec<Source>,
    pub contributors: Vec<String>,
    pub created: String,
    pub updated: String,
}

/// A source reference in page frontmatter.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub url: String,
    pub title: String,
    pub accessed_at: Option<String>,
}

/// Summary info about a knowledge page.
#[derive(Debug, Clone)]
pub struct PageInfo {
    pub slug: String,
    pub frontmatter: PageFrontmatter,
}

/// A single search match within a knowledge page.
#[derive(Debug, Clone)]
pub struct SearchMatch {
    pub slug: String,
    pub line_number: usize,
    /// The matching line and surrounding context lines.
    pub context_lines: Vec<(usize, String)>,
}

/// Outcome of a sync or push operation that may involve conflict resolution.
#[derive(Debug, Default)]
pub struct SyncOutcome {
    /// Slugs of knowledge pages that had merge conflicts resolved via "accept both".
    pub resolved_conflicts: Vec<String>,
}

/// Check if content contains git merge conflict markers.
///
/// Only triggers when the three marker types appear in the correct sequence
/// (opening `<<<<<<<`, separator `=======`, closing `>>>>>>>`) with each
/// marker at the start of a line. This avoids false positives on content
/// that happens to contain those character sequences mid-line or out of order.
pub fn has_conflict_markers(content: &str) -> bool {
    #[derive(PartialEq)]
    enum ConflictScan {
        Ours,
        Separator,
        Theirs,
    }
    let mut state = ConflictScan::Ours;
    for line in content.lines() {
        match state {
            ConflictScan::Ours => {
                if line.starts_with("<<<<<<<") {
                    state = ConflictScan::Separator;
                }
            }
            ConflictScan::Separator => {
                if line.starts_with("=======") {
                    state = ConflictScan::Theirs;
                }
            }
            ConflictScan::Theirs => {
                if line.starts_with(">>>>>>>") {
                    return true;
                }
            }
        }
    }
    false
}

/// Resolve merge conflicts in content by keeping both versions.
///
/// Replaces each conflict block with an HTML comment noting the conflict,
/// followed by both versions separated by horizontal rules. Content outside
/// conflict blocks is preserved unchanged.
pub fn resolve_accept_both(content: &str) -> String {
    /// Tracks which section of a conflict block we are currently inside.
    enum ConflictState {
        /// Outside any conflict block — normal content.
        Outside,
        /// Inside the "ours" section (between `<<<<<<<` and `=======`).
        InOurs,
        /// Inside the "theirs" section (between `=======` and `>>>>>>>`).
        InTheirs,
    }

    let mut result = String::new();
    let mut state = ConflictState::Outside;
    let mut ours = String::new();
    let mut theirs = String::new();

    for line in content.lines() {
        match state {
            ConflictState::Outside => {
                if line.starts_with("<<<<<<<") {
                    state = ConflictState::InOurs;
                    ours.clear();
                    theirs.clear();
                } else {
                    result.push_str(line);
                    result.push('\n');
                }
            }
            ConflictState::InOurs => {
                if line.starts_with("=======") {
                    state = ConflictState::InTheirs;
                } else {
                    ours.push_str(line);
                    ours.push('\n');
                }
            }
            ConflictState::InTheirs => {
                if line.starts_with(">>>>>>>") {
                    state = ConflictState::Outside;
                    // Emit the resolved version
                    result.push_str(
                        "<!-- MERGE CONFLICT: Both versions kept. Cleanup recommended. -->\n",
                    );
                    result.push_str("---\n");
                    result.push_str(&ours);
                    result.push_str("---\n");
                    result.push_str(&theirs);
                } else {
                    theirs.push_str(line);
                    theirs.push('\n');
                }
            }
        }
    }

    // Handle unterminated conflict block (shouldn't happen, but be defensive)
    if !matches!(state, ConflictState::Outside) {
        if !ours.is_empty() {
            result.push_str(&ours);
        }
        if !theirs.is_empty() {
            result.push_str(&theirs);
        }
    }

    result
}

impl KnowledgeManager {
    /// Create a new KnowledgeManager for the given .crosslink directory.
    ///
    /// When running inside a git worktree, automatically detects the main
    /// repository root and uses its `.crosslink/.knowledge-cache/` so that the
    /// shared knowledge branch worktree is never duplicated.
    pub fn new(crosslink_dir: &Path) -> Result<Self> {
        let remote = crate::sync::read_tracker_remote(crosslink_dir);
        Self::with_remote(crosslink_dir, remote)
    }

    /// Create a KnowledgeManager with an explicit remote name.
    ///
    /// Useful for testing (avoids reading config from disk) and for callers
    /// that already know the remote.
    pub fn with_remote(crosslink_dir: &Path, remote: String) -> Result<Self> {
        let local_repo_root = crosslink_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root from .crosslink dir"))?
            .to_path_buf();

        // If we're inside a git worktree, resolve the main repo root so the
        // knowledge cache lives in one shared location rather than per-worktree.
        let repo_root =
            resolve_main_repo_root(&local_repo_root).unwrap_or_else(|| local_repo_root.clone());

        let cache_dir = repo_root.join(".crosslink").join(KNOWLEDGE_CACHE_DIR);

        Ok(KnowledgeManager {
            crosslink_dir: crosslink_dir.to_path_buf(),
            cache_dir,
            repo_root,
            remote,
        })
    }

    /// Check if the knowledge cache directory is initialized.
    pub fn is_initialized(&self) -> bool {
        self.cache_dir.exists()
    }

    /// Get the path to the `.crosslink` directory.
    pub fn crosslink_dir(&self) -> &Path {
        &self.crosslink_dir
    }

    /// Get the path to the cache directory.
    pub fn cache_path(&self) -> &Path {
        &self.cache_dir
    }

    /// Return the cache directory path as a `String` for use in git CLI args.
    ///
    /// Uses lossy conversion: non-UTF-8 bytes are replaced with U+FFFD. This
    /// is acceptable because git worktree paths must be valid filesystem paths
    /// and all supported platforms (Linux, macOS, Windows) use UTF-8-compatible
    /// encodings for paths created by crosslink.
    pub(super) fn cache_path_str(&self) -> String {
        self.cache_dir.to_string_lossy().to_string()
    }
}

// --- Frontmatter parsing ---

/// Parse YAML frontmatter from a markdown page.
///
/// Expects content starting with `---\n`, followed by YAML key-value pairs,
/// and closed with `---\n`. Returns `None` if no valid frontmatter is found.
pub fn parse_frontmatter(content: &str) -> Option<PageFrontmatter> {
    // Normalize CRLF to LF so the parser handles Windows line endings.
    let content = if content.contains("\r\n") {
        std::borrow::Cow::Owned(content.replace("\r\n", "\n"))
    } else {
        std::borrow::Cow::Borrowed(content)
    };
    let content = content.trim_start();
    if !content.starts_with("---") {
        return None;
    }

    // Find the closing delimiter
    let after_first = &content[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);
    let end_idx = after_first.find("\n---")?;
    let yaml_block = &after_first[..end_idx];

    let mut title = String::new();
    let mut tags = Vec::new();
    let mut sources: Vec<Source> = Vec::new();
    let mut contributors = Vec::new();
    let mut created = String::new();
    let mut updated = String::new();

    // State machine for multi-line array items
    enum ParseState {
        TopLevel,
        InTags,
        InSources,
        InContributors,
        InSourceItem,
    }

    let mut state = ParseState::TopLevel;
    let mut current_source = Source {
        url: String::new(),
        title: String::new(),
        accessed_at: None,
    };

    for line in yaml_block.lines() {
        let trimmed = line.trim();

        // Skip empty lines
        if trimmed.is_empty() {
            continue;
        }

        // Check if this is a top-level key (not indented)
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
        let is_list_item = trimmed.starts_with("- ");
        let is_nested_key = line.starts_with("    ") && !is_list_item && trimmed.contains(": ");

        let is_top_level_kv = is_top_level && (trimmed.contains(": ") || trimmed.ends_with(':'));

        if is_top_level_kv {
            // Flush any pending source item
            if let ParseState::InSourceItem = state {
                if !current_source.url.is_empty() || !current_source.title.is_empty() {
                    sources.push(current_source.clone());
                    current_source = Source {
                        url: String::new(),
                        title: String::new(),
                        accessed_at: None,
                    };
                }
            }

            let (key, value) = split_kv_or_bare(trimmed)?;
            match key {
                "title" => {
                    title = unquote(value);
                    state = ParseState::TopLevel;
                }
                "tags" => {
                    if let Some(inline) = parse_inline_array(value) {
                        tags = inline;
                        state = ParseState::TopLevel;
                    } else if value.is_empty() || value == "[]" {
                        tags = Vec::new();
                        state = if value == "[]" {
                            ParseState::TopLevel
                        } else {
                            ParseState::InTags
                        };
                    } else {
                        state = ParseState::TopLevel;
                    }
                }
                "sources" => {
                    if value == "[]" {
                        sources = Vec::new();
                        state = ParseState::TopLevel;
                    } else {
                        state = ParseState::InSources;
                    }
                }
                "contributors" => {
                    if let Some(inline) = parse_inline_array(value) {
                        contributors = inline;
                        state = ParseState::TopLevel;
                    } else if value.is_empty() || value == "[]" {
                        contributors = Vec::new();
                        state = if value == "[]" {
                            ParseState::TopLevel
                        } else {
                            ParseState::InContributors
                        };
                    } else {
                        state = ParseState::TopLevel;
                    }
                }
                "created" => {
                    created = unquote(value);
                    state = ParseState::TopLevel;
                }
                "updated" => {
                    updated = unquote(value);
                    state = ParseState::TopLevel;
                }
                _ => {
                    state = ParseState::TopLevel;
                }
            }
        } else {
            match state {
                ParseState::InTags => {
                    if is_list_item {
                        tags.push(unquote(trimmed.strip_prefix("- ").unwrap_or(trimmed)));
                    }
                }
                ParseState::InContributors => {
                    if is_list_item {
                        contributors.push(unquote(trimmed.strip_prefix("- ").unwrap_or(trimmed)));
                    }
                }
                ParseState::InSources => {
                    if is_list_item {
                        // Starting a new source item
                        current_source = Source {
                            url: String::new(),
                            title: String::new(),
                            accessed_at: None,
                        };

                        // The list item itself might have inline content: `- url: https://...`
                        let after_dash = trimmed.strip_prefix("- ").unwrap_or("");
                        if let Some((k, v)) = after_dash.split_once(": ") {
                            let k = k.trim();
                            let v = v.trim();
                            match k {
                                "url" => current_source.url = unquote(v),
                                "title" => current_source.title = unquote(v),
                                "accessed_at" => {
                                    current_source.accessed_at = Some(unquote(v));
                                }
                                _ => {}
                            }
                        }
                        state = ParseState::InSourceItem;
                    }
                }
                ParseState::InSourceItem => {
                    if is_list_item {
                        // New source item — flush current
                        if !current_source.url.is_empty() || !current_source.title.is_empty() {
                            sources.push(current_source.clone());
                        }
                        current_source = Source {
                            url: String::new(),
                            title: String::new(),
                            accessed_at: None,
                        };
                        let after_dash = trimmed.strip_prefix("- ").unwrap_or("");
                        if let Some((k, v)) = after_dash.split_once(": ") {
                            let k = k.trim();
                            let v = v.trim();
                            match k {
                                "url" => current_source.url = unquote(v),
                                "title" => current_source.title = unquote(v),
                                "accessed_at" => {
                                    current_source.accessed_at = Some(unquote(v));
                                }
                                _ => {}
                            }
                        }
                    } else if is_nested_key {
                        if let Some((k, v)) = trimmed.split_once(": ") {
                            let k = k.trim();
                            let v = v.trim();
                            match k {
                                "url" => current_source.url = unquote(v),
                                "title" => current_source.title = unquote(v),
                                "accessed_at" => {
                                    current_source.accessed_at = Some(unquote(v));
                                }
                                _ => {}
                            }
                        }
                    }
                }
                ParseState::TopLevel => {}
            }
        }
    }

    // Flush final source item
    if !current_source.url.is_empty() || !current_source.title.is_empty() {
        sources.push(current_source);
    }

    Some(PageFrontmatter {
        title,
        tags,
        sources,
        contributors,
        created,
        updated,
    })
}

/// Escape a string value for safe inclusion in YAML frontmatter.
///
/// Wraps the value in double quotes, escaping any internal backslashes and
/// double quotes to prevent YAML injection via crafted titles or other fields.
pub(super) fn yaml_escape(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped)
}

/// Serialize frontmatter back to YAML format.
pub fn serialize_frontmatter(fm: &PageFrontmatter) -> String {
    let mut out = String::from("---\n");

    out.push_str(&format!("title: {}\n", yaml_escape(&fm.title)));

    // Tags as inline array (each value escaped to prevent YAML injection)
    if fm.tags.is_empty() {
        out.push_str("tags: []\n");
    } else {
        let escaped_tags: Vec<String> = fm.tags.iter().map(|t| yaml_escape(t)).collect();
        out.push_str(&format!("tags: [{}]\n", escaped_tags.join(", ")));
    }

    // Sources as multi-line array
    if fm.sources.is_empty() {
        out.push_str("sources: []\n");
    } else {
        out.push_str("sources:\n");
        for src in &fm.sources {
            out.push_str(&format!("  - url: {}\n", yaml_escape(&src.url)));
            out.push_str(&format!("    title: {}\n", yaml_escape(&src.title)));
            if let Some(ref accessed) = src.accessed_at {
                out.push_str(&format!("    accessed_at: {}\n", yaml_escape(accessed)));
            }
        }
    }

    // Contributors as inline array (each value escaped to prevent YAML injection)
    if fm.contributors.is_empty() {
        out.push_str("contributors: []\n");
    } else {
        let escaped_contribs: Vec<String> =
            fm.contributors.iter().map(|c| yaml_escape(c)).collect();
        out.push_str(&format!(
            "contributors: [{}]\n",
            escaped_contribs.join(", ")
        ));
    }

    out.push_str(&format!("created: {}\n", &fm.created));
    out.push_str(&format!("updated: {}\n", &fm.updated));
    out.push_str("---\n");

    out
}

/// Split a YAML key-value line into (key, value).
///
/// Handles both `key: value` and bare `key:` (returns empty value).
pub(super) fn split_kv_or_bare(line: &str) -> Option<(&str, &str)> {
    if let Some(idx) = line.find(": ") {
        let key = line[..idx].trim();
        let value = line[idx + 2..].trim();
        Some((key, value))
    } else if let Some(stripped) = line.strip_suffix(':') {
        let key = stripped.trim();
        Some((key, ""))
    } else {
        None
    }
}

/// Parse an inline YAML array like `[foo, bar, baz]`.
///
/// Handles quoted values that may contain commas (e.g., `["foo,bar", baz]`)
/// by tracking quote state rather than naively splitting on commas.
pub(super) fn parse_inline_array(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let inner = &trimmed[1..trimmed.len() - 1];
        if inner.trim().is_empty() {
            return Some(Vec::new());
        }
        let items: Vec<String> = split_yaml_array_items(inner)
            .iter()
            .map(|s| unquote(s.trim()))
            .collect();
        Some(items)
    } else {
        None
    }
}

/// Split a YAML inline array body on commas, respecting double-quoted strings.
///
/// Commas inside double quotes are treated as literal characters rather than
/// separators, preventing corruption when tag or contributor values contain
/// commas (e.g., `"last, first"`).
fn split_yaml_array_items(s: &str) -> Vec<&str> {
    let mut items = Vec::new();
    let mut start = 0;
    let mut in_quotes = false;
    let mut escaped = false;

    for (i, ch) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                items.push(&s[start..i]);
                start = i + 1; // skip the comma
            }
            _ => {}
        }
    }
    items.push(&s[start..]);
    items
}

/// Remove surrounding quotes from a string value.
pub(super) fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.starts_with('"') && s.ends_with('"') {
        s[1..s.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else if s.starts_with('\'') && s.ends_with('\'') {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}
