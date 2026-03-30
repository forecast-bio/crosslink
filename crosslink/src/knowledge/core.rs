use anyhow::Result;
use std::fmt::Write;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFrontmatter {
    pub title: String,
    pub tags: Vec<String>,
    pub sources: Vec<Source>,
    pub contributors: Vec<String>,
    pub created: String,
    pub updated: String,
}

/// A source reference in page frontmatter.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[must_use]
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
#[must_use]
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
    /// Create a new `KnowledgeManager` for the given .crosslink directory.
    ///
    /// When running inside a git worktree, automatically detects the main
    /// repository root and uses its `.crosslink/.knowledge-cache/` so that the
    /// shared knowledge branch worktree is never duplicated.
    ///
    /// # Errors
    /// Returns an error if the repo root cannot be determined from the crosslink directory.
    pub fn new(crosslink_dir: &Path) -> Result<Self> {
        let remote = crate::sync::read_tracker_remote(crosslink_dir);
        Self::with_remote(crosslink_dir, remote)
    }

    /// Create a `KnowledgeManager` with an explicit remote name.
    ///
    /// Useful for testing (avoids reading config from disk) and for callers
    /// that already know the remote.
    ///
    /// # Errors
    /// Returns an error if the repo root cannot be determined from the crosslink directory.
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

        Ok(Self {
            crosslink_dir: crosslink_dir.to_path_buf(),
            cache_dir,
            repo_root,
            remote,
        })
    }

    /// Check if the knowledge cache directory is initialized.
    #[must_use]
    pub fn is_initialized(&self) -> bool {
        self.cache_dir.exists()
    }

    /// Get the path to the `.crosslink` directory.
    #[must_use]
    pub fn crosslink_dir(&self) -> &Path {
        &self.crosslink_dir
    }

    /// Get the path to the cache directory.
    #[must_use]
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

/// State machine for multi-line array items in YAML frontmatter.
enum ParseState {
    TopLevel,
    InTags,
    InSources,
    InContributors,
    InSourceItem,
}

/// Apply a source key-value pair to a `Source` struct.
fn apply_source_kv(source: &mut Source, key: &str, value: &str) {
    match key {
        "url" => source.url = unquote(value),
        "title" => source.title = unquote(value),
        "accessed_at" => source.accessed_at = Some(unquote(value)),
        _ => {}
    }
}

/// Parse the inline key-value from a YAML list item prefix (`- key: value`).
fn parse_source_list_item(source: &mut Source, trimmed: &str) {
    let after_dash = trimmed.strip_prefix("- ").unwrap_or("");
    if let Some((k, v)) = after_dash.split_once(": ") {
        apply_source_kv(source, k.trim(), v.trim());
    }
}

/// Accumulator for frontmatter fields during parsing.
struct FrontmatterBuilder {
    title: String,
    tags: Vec<String>,
    sources: Vec<Source>,
    contributors: Vec<String>,
    created: String,
    updated: String,
    state: ParseState,
    current_source: Source,
}

impl FrontmatterBuilder {
    const fn new() -> Self {
        Self {
            title: String::new(),
            tags: Vec::new(),
            sources: Vec::new(),
            contributors: Vec::new(),
            created: String::new(),
            updated: String::new(),
            state: ParseState::TopLevel,
            current_source: Source {
                url: String::new(),
                title: String::new(),
                accessed_at: None,
            },
        }
    }

    fn flush_current_source(&mut self) {
        if !self.current_source.url.is_empty() || !self.current_source.title.is_empty() {
            self.sources.push(self.current_source.clone());
            self.current_source = Source {
                url: String::new(),
                title: String::new(),
                accessed_at: None,
            };
        }
    }

    /// Process a top-level key-value line. Returns `None` if the line is malformed.
    fn handle_top_level_kv(&mut self, trimmed: &str) -> Option<()> {
        if matches!(self.state, ParseState::InSourceItem) {
            self.flush_current_source();
        }
        let (key, value) = split_kv_or_bare(trimmed)?;
        match key {
            "title" => {
                self.title = unquote(value);
                self.state = ParseState::TopLevel;
            }
            "tags" => self.parse_inline_or_begin_list(value, FieldKind::Tags),
            "sources" => {
                if value == "[]" {
                    self.sources = Vec::new();
                    self.state = ParseState::TopLevel;
                } else {
                    self.state = ParseState::InSources;
                }
            }
            "contributors" => self.parse_inline_or_begin_list(value, FieldKind::Contributors),
            "created" => {
                self.created = unquote(value);
                self.state = ParseState::TopLevel;
            }
            "updated" => {
                self.updated = unquote(value);
                self.state = ParseState::TopLevel;
            }
            _ => self.state = ParseState::TopLevel,
        }
        Some(())
    }

    /// Handle inline array or begin multi-line list for tags/contributors.
    fn parse_inline_or_begin_list(&mut self, value: &str, kind: FieldKind) {
        if let Some(inline) = parse_inline_array(value) {
            match kind {
                FieldKind::Tags => self.tags = inline,
                FieldKind::Contributors => self.contributors = inline,
            }
            self.state = ParseState::TopLevel;
        } else if value.is_empty() || value == "[]" {
            match kind {
                FieldKind::Tags => {
                    self.tags = Vec::new();
                    self.state = if value == "[]" {
                        ParseState::TopLevel
                    } else {
                        ParseState::InTags
                    };
                }
                FieldKind::Contributors => {
                    self.contributors = Vec::new();
                    self.state = if value == "[]" {
                        ParseState::TopLevel
                    } else {
                        ParseState::InContributors
                    };
                }
            }
        } else {
            self.state = ParseState::TopLevel;
        }
    }

    /// Process a non-top-level line (list items, nested keys).
    fn handle_nested_line(&mut self, trimmed: &str, is_list_item: bool, is_nested_key: bool) {
        match self.state {
            ParseState::InTags => {
                if is_list_item {
                    self.tags
                        .push(unquote(trimmed.strip_prefix("- ").unwrap_or(trimmed)));
                }
            }
            ParseState::InContributors => {
                if is_list_item {
                    self.contributors
                        .push(unquote(trimmed.strip_prefix("- ").unwrap_or(trimmed)));
                }
            }
            ParseState::InSources => {
                if is_list_item {
                    self.current_source = Source {
                        url: String::new(),
                        title: String::new(),
                        accessed_at: None,
                    };
                    parse_source_list_item(&mut self.current_source, trimmed);
                    self.state = ParseState::InSourceItem;
                }
            }
            ParseState::InSourceItem => {
                if is_list_item {
                    self.flush_current_source();
                    self.current_source = Source {
                        url: String::new(),
                        title: String::new(),
                        accessed_at: None,
                    };
                    parse_source_list_item(&mut self.current_source, trimmed);
                } else if is_nested_key {
                    if let Some((k, v)) = trimmed.split_once(": ") {
                        apply_source_kv(&mut self.current_source, k.trim(), v.trim());
                    }
                }
            }
            ParseState::TopLevel => {}
        }
    }

    fn build(mut self) -> PageFrontmatter {
        // Flush final source item
        self.flush_current_source();
        PageFrontmatter {
            title: self.title,
            tags: self.tags,
            sources: self.sources,
            contributors: self.contributors,
            created: self.created,
            updated: self.updated,
        }
    }
}

/// Which list-like field we are parsing.
#[derive(Clone, Copy)]
enum FieldKind {
    Tags,
    Contributors,
}

/// Parse YAML frontmatter from a markdown page.
///
/// Expects content starting with `---\n`, followed by YAML key-value pairs,
/// and closed with `---\n`. Returns `None` if no valid frontmatter is found.
#[must_use]
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

    let mut builder = FrontmatterBuilder::new();

    for line in yaml_block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
        let is_list_item = trimmed.starts_with("- ");
        let is_nested_key = line.starts_with("    ") && !is_list_item && trimmed.contains(": ");
        let is_top_level_kv = is_top_level && (trimmed.contains(": ") || trimmed.ends_with(':'));

        if is_top_level_kv {
            builder.handle_top_level_kv(trimmed)?;
        } else {
            builder.handle_nested_line(trimmed, is_list_item, is_nested_key);
        }
    }

    Some(builder.build())
}

/// Escape a string value for safe inclusion in YAML frontmatter.
///
/// Wraps the value in double quotes, escaping any internal backslashes and
/// double quotes to prevent YAML injection via crafted titles or other fields.
pub(super) fn yaml_escape(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

/// Serialize frontmatter back to YAML format.
#[must_use]
pub fn serialize_frontmatter(fm: &PageFrontmatter) -> String {
    let mut out = String::from("---\n");

    let _ = writeln!(out, "title: {}", yaml_escape(&fm.title));

    if fm.tags.is_empty() {
        out.push_str("tags: []\n");
    } else {
        let escaped_tags: Vec<String> = fm.tags.iter().map(|t| yaml_escape(t)).collect();
        let _ = writeln!(out, "tags: [{}]", escaped_tags.join(", "));
    }

    if fm.sources.is_empty() {
        out.push_str("sources: []\n");
    } else {
        out.push_str("sources:\n");
        for src in &fm.sources {
            let _ = writeln!(out, "  - url: {}", yaml_escape(&src.url));
            let _ = writeln!(out, "    title: {}", yaml_escape(&src.title));
            if let Some(ref accessed) = src.accessed_at {
                let _ = writeln!(out, "    accessed_at: {}", yaml_escape(accessed));
            }
        }
    }

    if fm.contributors.is_empty() {
        out.push_str("contributors: []\n");
    } else {
        let escaped_contribs: Vec<String> =
            fm.contributors.iter().map(|c| yaml_escape(c)).collect();
        let _ = writeln!(out, "contributors: [{}]", escaped_contribs.join(", "));
    }

    let _ = writeln!(out, "created: {}", &fm.created);
    let _ = writeln!(out, "updated: {}", &fm.updated);
    out.push_str("---\n");

    out
}

/// Split a YAML key-value line into (key, value).
///
/// Handles both `key: value` and bare `key:` (returns empty value).
pub(super) fn split_kv_or_bare(line: &str) -> Option<(&str, &str)> {
    line.find(": ").map_or_else(
        || line.strip_suffix(':').map(|stripped| (stripped.trim(), "")),
        |idx| {
            let key = line[..idx].trim();
            let value = line[idx + 2..].trim();
            Some((key, value))
        },
    )
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
