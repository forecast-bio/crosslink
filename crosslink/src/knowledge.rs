use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::utils::{is_windows_reserved_name, resolve_main_repo_root};

/// Directory name under .crosslink for the knowledge cache worktree.
pub(crate) const KNOWLEDGE_CACHE_DIR: &str = ".knowledge-cache";

/// The knowledge branch name.
pub(crate) const KNOWLEDGE_BRANCH: &str = "crosslink/knowledge";

/// Manages the `crosslink/knowledge` orphan branch for shared research.
///
/// Uses a git worktree at `.crosslink/.knowledge-cache/` to avoid disturbing
/// the user's working tree. Follows the same pattern as `SyncManager`.
pub struct KnowledgeManager {
    /// Path to the .crosslink directory (used by signing support).
    crosslink_dir: PathBuf,
    /// Path to .crosslink/.knowledge-cache (worktree of crosslink/knowledge branch).
    cache_dir: PathBuf,
    /// The repo root (parent of .crosslink).
    repo_root: PathBuf,
    /// Git remote name for the knowledge branch (from config, defaults to "origin").
    remote: String,
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
pub fn has_conflict_markers(content: &str) -> bool {
    content.contains("<<<<<<<") && content.contains("=======") && content.contains(">>>>>>>")
}

/// Resolve merge conflicts in content by keeping both versions.
///
/// Replaces each conflict block with an HTML comment noting the conflict,
/// followed by both versions separated by horizontal rules. Content outside
/// conflict blocks is preserved unchanged.
pub fn resolve_accept_both(content: &str) -> String {
    let mut result = String::new();
    let mut in_ours = false;
    let mut in_theirs = false;
    let mut ours = String::new();
    let mut theirs = String::new();

    for line in content.lines() {
        if line.starts_with("<<<<<<<") {
            in_ours = true;
            in_theirs = false;
            ours.clear();
            theirs.clear();
        } else if line.starts_with("=======") && in_ours {
            in_ours = false;
            in_theirs = true;
        } else if line.starts_with(">>>>>>>") && in_theirs {
            in_theirs = false;
            // Emit the resolved version
            result.push_str("<!-- MERGE CONFLICT: Both versions kept. Cleanup recommended. -->\n");
            result.push_str("---\n");
            result.push_str(&ours);
            result.push_str("---\n");
            result.push_str(&theirs);
        } else if in_ours {
            ours.push_str(line);
            ours.push('\n');
        } else if in_theirs {
            theirs.push_str(line);
            theirs.push('\n');
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    // Handle unterminated conflict block (shouldn't happen, but be defensive)
    if in_ours || in_theirs {
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
        let local_repo_root = crosslink_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root from .crosslink dir"))?
            .to_path_buf();

        // If we're inside a git worktree, resolve the main repo root so the
        // knowledge cache lives in one shared location rather than per-worktree.
        let repo_root =
            resolve_main_repo_root(&local_repo_root).unwrap_or_else(|| local_repo_root.clone());

        let cache_dir = repo_root.join(".crosslink").join(KNOWLEDGE_CACHE_DIR);
        let remote = crate::sync::read_tracker_remote(crosslink_dir);

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

    /// Initialize the knowledge cache directory.
    ///
    /// If the `crosslink/knowledge` branch exists on the remote, fetches it and
    /// creates a worktree. If not, creates an orphan branch with an initial
    /// `index.md` page.
    pub fn init_cache(&self) -> Result<()> {
        if self.cache_dir.exists() {
            return Ok(());
        }

        // Check if remote branch exists
        let has_remote = self
            .git_in_repo(&["ls-remote", "--heads", &self.remote, KNOWLEDGE_BRANCH])
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

        if has_remote {
            // Fetch the remote branch
            self.git_in_repo(&["fetch", &self.remote, KNOWLEDGE_BRANCH])?;

            // Check if a local branch already exists
            let has_local = self
                .git_in_repo(&["rev-parse", "--verify", KNOWLEDGE_BRANCH])
                .is_ok();

            if has_local {
                self.git_in_repo(&["worktree", "add", &self.cache_path_str(), KNOWLEDGE_BRANCH])?;
            } else {
                // Create local branch tracking remote
                let remote_ref = format!("{}/{}", self.remote, KNOWLEDGE_BRANCH);
                self.git_in_repo(&[
                    "worktree",
                    "add",
                    "-b",
                    KNOWLEDGE_BRANCH,
                    &self.cache_path_str(),
                    &remote_ref,
                ])?;
            }
        } else {
            // No remote branch — create orphan branch with worktree
            self.git_in_repo(&[
                "worktree",
                "add",
                "--orphan",
                "-b",
                KNOWLEDGE_BRANCH,
                &self.cache_path_str(),
            ])?;

            // Initialize with index.md
            let now = Utc::now().format("%Y-%m-%d").to_string();
            let index_content = format!(
                "---\n\
                 title: Knowledge Index\n\
                 tags: [index]\n\
                 sources: []\n\
                 contributors: []\n\
                 created: {now}\n\
                 updated: {now}\n\
                 ---\n\
                 \n\
                 # Knowledge Index\n\
                 \n\
                 This is the shared knowledge repository for the project.\n"
            );

            std::fs::write(self.cache_dir.join("index.md"), index_content)?;

            // Commit the initial state so the branch has at least one commit.
            self.git_in_cache(&["add", "index.md"])?;
            self.git_in_cache(&["commit", "-m", "Initialize crosslink/knowledge branch"])?;
        }

        Ok(())
    }

    /// Fetch the latest state from remote and rebase local changes on top.
    ///
    /// If a rebase produces merge conflicts, falls back to an "accept both"
    /// strategy: aborts the rebase, merges instead, and resolves any remaining
    /// conflicts by keeping both versions. Returns the list of slugs that had
    /// conflicts resolved.
    pub fn sync(&self) -> Result<SyncOutcome> {
        let fetch_result = self.git_in_cache(&["fetch", &self.remote, KNOWLEDGE_BRANCH]);
        if let Err(e) = &fetch_result {
            let err_str = e.to_string();
            if err_str.contains("Could not resolve host")
                || err_str.contains("Could not read from remote")
                || err_str.contains("does not appear to be a git repository")
                || err_str.contains("No such remote")
                || err_str.contains("couldn't find remote ref")
            {
                return Ok(SyncOutcome::default());
            }
            fetch_result?;
        }

        // Check for unpushed local commits. If any exist, rebase to preserve them.
        let remote_ref = format!("{}/{}", self.remote, KNOWLEDGE_BRANCH);
        let log_result = self.git_in_cache(&["log", &format!("{}..HEAD", remote_ref), "--oneline"]);
        if let Ok(output) = &log_result {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.trim().is_empty() {
                let rebase_result = self.git_in_cache(&["rebase", &remote_ref]);
                if let Err(e) = &rebase_result {
                    let err_str = e.to_string();
                    if err_str.contains("unknown revision")
                        || err_str.contains("ambiguous argument")
                    {
                        return Ok(SyncOutcome::default());
                    }
                    // Rebase failed — likely a conflict. Try accept-both fallback.
                    let outcome = self.handle_rebase_conflict(&remote_ref)?;
                    if !outcome.resolved_conflicts.is_empty() {
                        return Ok(outcome);
                    }
                    rebase_result?;
                }
                return Ok(SyncOutcome::default());
            }
        }

        // No unpushed commits — safe to reset to match remote
        let reset_result = self.git_in_cache(&["reset", "--hard", &remote_ref]);
        if let Err(e) = &reset_result {
            let err_str = e.to_string();
            if err_str.contains("unknown revision") || err_str.contains("ambiguous argument") {
                return Ok(SyncOutcome::default());
            }
            reset_result?;
        }

        Ok(SyncOutcome::default())
    }

    /// Push local commits to the remote.
    ///
    /// If the push is rejected (non-fast-forward), attempts a pull --rebase.
    /// If that rebase produces conflicts, falls back to "accept both" resolution.
    pub fn push(&self) -> Result<SyncOutcome> {
        let push_result = self.git_in_cache(&["push", &self.remote, KNOWLEDGE_BRANCH]);
        if let Err(e) = &push_result {
            let err_str = e.to_string();
            if err_str.contains("Could not resolve host")
                || err_str.contains("Could not read from remote")
            {
                return Ok(SyncOutcome::default());
            }
            if err_str.contains("rejected") || err_str.contains("non-fast-forward") {
                let remote_ref = format!("{}/{}", self.remote, KNOWLEDGE_BRANCH);
                // Fetch latest
                let _ = self.git_in_cache(&["fetch", &self.remote, KNOWLEDGE_BRANCH]);
                // Try rebase
                let rebase_result = self.git_in_cache(&["rebase", &remote_ref]);
                if rebase_result.is_err() {
                    // Rebase failed — try accept-both fallback
                    let outcome = self.handle_rebase_conflict(&remote_ref)?;
                    let _ = self.git_in_cache(&["push", &self.remote, KNOWLEDGE_BRANCH]);
                    return Ok(outcome);
                }
                let _ = self.git_in_cache(&["push", &self.remote, KNOWLEDGE_BRANCH]);
                return Ok(SyncOutcome::default());
            }
            push_result?;
        }
        Ok(SyncOutcome::default())
    }

    /// Abort a failed rebase and fall back to merge with "accept both" resolution.
    ///
    /// 1. Aborts the in-progress rebase
    /// 2. Merges the remote ref
    /// 3. If merge conflicts, resolves each .md file using accept-both
    /// 4. Stages and commits the resolution
    fn handle_rebase_conflict(&self, remote_ref: &str) -> Result<SyncOutcome> {
        // Abort the failed rebase
        let _ = self.git_in_cache(&["rebase", "--abort"]);

        // Attempt a merge instead
        let merge_result = self.git_in_cache(&["merge", remote_ref, "--no-edit"]);

        let resolved = if merge_result.is_err() {
            // Merge has conflicts — resolve all .md files with accept-both
            self.resolve_conflicts_in_cache()?
        } else {
            Vec::new()
        };

        if !resolved.is_empty() {
            // Stage resolved files and commit
            self.git_in_cache(&["add", "-A"])?;
            let slugs_str = resolved.join(", ");
            self.commit(&format!(
                "knowledge: accept-both conflict resolution for {}",
                slugs_str
            ))?;
        }

        Ok(SyncOutcome {
            resolved_conflicts: resolved,
        })
    }

    /// Scan all `.md` files in the cache for conflict markers and resolve them.
    ///
    /// Returns the list of slugs that had conflicts resolved.
    fn resolve_conflicts_in_cache(&self) -> Result<Vec<String>> {
        let mut resolved = Vec::new();

        if !self.cache_dir.exists() {
            return Ok(resolved);
        }

        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                let content = std::fs::read_to_string(&path)?;
                if has_conflict_markers(&content) {
                    let slug = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let resolved_content = resolve_accept_both(&content);
                    std::fs::write(&path, &resolved_content)?;
                    resolved.push(slug);
                }
            }
        }

        Ok(resolved)
    }

    /// Stage all changes in the knowledge worktree and commit.
    pub fn commit(&self, message: &str) -> Result<()> {
        self.git_in_cache(&["add", "-A"])?;

        let commit_result = self.git_in_cache(&["commit", "-m", message]);
        if let Err(e) = &commit_result {
            let err_str = e.to_string();
            if err_str.contains("nothing to commit") || err_str.contains("no changes added") {
                return Ok(());
            }
            commit_result?;
        }
        Ok(())
    }

    /// List all `.md` pages in the knowledge worktree with parsed frontmatter.
    pub fn list_pages(&self) -> Result<Vec<PageInfo>> {
        let mut pages = Vec::new();

        if !self.cache_dir.exists() {
            return Ok(pages);
        }

        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "md").unwrap_or(false) {
                let slug = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let content = std::fs::read_to_string(&path)?;
                let frontmatter = parse_frontmatter(&content).unwrap_or_else(|| PageFrontmatter {
                    title: slug.clone(),
                    tags: Vec::new(),
                    sources: Vec::new(),
                    contributors: Vec::new(),
                    created: String::new(),
                    updated: String::new(),
                });
                pages.push(PageInfo { slug, frontmatter });
            }
        }

        pages.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(pages)
    }

    /// Validate a slug and return the safe path within the cache directory.
    ///
    /// Rejects slugs containing path separators, parent-directory traversals,
    /// or characters that are unsafe for filenames.
    fn safe_page_path(&self, slug: &str) -> Result<PathBuf> {
        if slug.is_empty() {
            bail!("Page slug cannot be empty");
        }
        if slug.contains('/') || slug.contains('\\') || slug.contains('\0') || slug.contains("..") {
            bail!(
                "Invalid page slug '{}': must not contain path separators or '..'",
                slug
            );
        }
        if is_windows_reserved_name(slug) {
            bail!("Invalid page slug '{}': Windows reserved filename", slug);
        }
        let path = self.cache_dir.join(format!("{}.md", slug));
        // Defense in depth: verify the resolved path is within cache_dir
        let canonical_cache = self
            .cache_dir
            .canonicalize()
            .unwrap_or_else(|_| self.cache_dir.clone());
        let canonical_parent = path.parent().and_then(|p| p.canonicalize().ok());
        if let Some(parent) = canonical_parent {
            if !parent.starts_with(&canonical_cache) {
                bail!(
                    "Invalid page slug '{}': resolves outside knowledge cache",
                    slug
                );
            }
        }
        Ok(path)
    }

    /// Read a page by its filename slug (without `.md` extension).
    pub fn read_page(&self, slug: &str) -> Result<String> {
        let path = self.safe_page_path(slug)?;
        if !path.exists() {
            bail!("Page '{}' not found", slug);
        }
        std::fs::read_to_string(&path).context("Failed to read page")
    }

    /// Write or overwrite a page by its filename slug.
    pub fn write_page(&self, slug: &str, content: &str) -> Result<()> {
        if !self.cache_dir.exists() {
            bail!("Knowledge cache not initialized. Run init_cache() first.");
        }
        let path = self.safe_page_path(slug)?;
        std::fs::write(&path, content).context("Failed to write page")
    }

    /// Check if a page exists by slug.
    pub fn page_exists(&self, slug: &str) -> bool {
        self.safe_page_path(slug)
            .map(|path| path.exists())
            .unwrap_or(false)
    }

    /// Delete a page by slug.
    pub fn delete_page(&self, slug: &str) -> Result<()> {
        let path = self.safe_page_path(slug)?;
        if !path.exists() {
            bail!("Page '{}' not found", slug);
        }
        std::fs::remove_file(&path).context("Failed to delete page")
    }

    /// Search knowledge page content using word-level fuzzy matching.
    ///
    /// Tokenizes the query into words and matches lines containing any query
    /// term (case-insensitive). Results are ranked by the number of distinct
    /// query terms matched within each page — pages matching more terms appear
    /// first. Within a page, contiguous matching lines are grouped with
    /// surrounding context.
    pub fn search_content(&self, query: &str, context: usize) -> Result<Vec<SearchMatch>> {
        if !self.cache_dir.exists() {
            return Ok(Vec::new());
        }

        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut entries: Vec<_> = std::fs::read_dir(&self.cache_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
            .collect();
        entries.sort_by_key(|e| e.file_name());

        // Collect (term_match_count, matches) per file for ranking
        let mut scored_results: Vec<(usize, Vec<SearchMatch>)> = Vec::new();

        for entry in entries {
            let path = entry.path();
            let slug = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let content = std::fs::read_to_string(&path)?;
            let lines: Vec<&str> = content.lines().collect();
            let content_lower = content.to_lowercase();

            // Count how many distinct query terms appear anywhere in this page
            let term_hits = terms
                .iter()
                .filter(|term| content_lower.contains(**term))
                .count();

            if term_hits == 0 {
                continue;
            }

            // Find lines matching any query term
            let matching_indices: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    let line_lower = line.to_lowercase();
                    terms.iter().any(|term| line_lower.contains(term))
                })
                .map(|(i, _)| i)
                .collect();

            let groups = group_matches(&matching_indices, context);
            let mut file_matches = Vec::new();

            for group in groups {
                let first_match = group[0];
                let start = first_match.saturating_sub(context);
                let last_match = group[group.len() - 1];
                let end = (last_match + context + 1).min(lines.len());

                let context_lines: Vec<(usize, String)> = (start..end)
                    .map(|i| (i + 1, lines[i].to_string()))
                    .collect();

                file_matches.push(SearchMatch {
                    slug: slug.clone(),
                    line_number: first_match + 1,
                    context_lines,
                });
            }

            if !file_matches.is_empty() {
                scored_results.push((term_hits, file_matches));
            }
        }

        // Sort by term hit count descending (pages matching more terms first)
        scored_results.sort_by(|a, b| b.0.cmp(&a.0));

        Ok(scored_results
            .into_iter()
            .flat_map(|(_, matches)| matches)
            .collect())
    }

    /// Search knowledge pages by source URL domain.
    ///
    /// Finds pages that have a source whose URL contains the given domain string.
    pub fn search_sources(&self, domain: &str) -> Result<Vec<PageInfo>> {
        let domain_lower = domain.to_lowercase();

        let pages = self.list_pages()?;
        let matches: Vec<PageInfo> = pages
            .into_iter()
            .filter(|page| {
                page.frontmatter
                    .sources
                    .iter()
                    .any(|src| src.url.to_lowercase().contains(&domain_lower))
            })
            .collect();

        Ok(matches)
    }

    /// Get the path to the `.crosslink` directory.
    pub fn crosslink_dir(&self) -> &Path {
        &self.crosslink_dir
    }

    /// Get the path to the cache directory.
    pub fn cache_path(&self) -> &Path {
        &self.cache_dir
    }

    // --- Private helpers ---

    fn cache_path_str(&self) -> String {
        self.cache_dir.to_string_lossy().to_string()
    }

    fn git_in_repo(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("git")
            .current_dir(&self.repo_root)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {:?}", args))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {:?} failed: {}", args, stderr);
        }
        Ok(output)
    }

    fn git_in_cache(&self, args: &[&str]) -> Result<std::process::Output> {
        let output = Command::new("git")
            .current_dir(&self.cache_dir)
            .args(args)
            .output()
            .with_context(|| format!("Failed to run git {:?} in knowledge cache", args))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {:?} in knowledge cache failed: {}", args, stderr);
        }
        Ok(output)
    }
}

/// Group matching line indices into contiguous groups based on context overlap.
///
/// Two matches are in the same group if their context windows overlap or are
/// adjacent (i.e., the distance between them is <= 2 * context).
fn group_matches(indices: &[usize], context: usize) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();

    for &idx in indices {
        let merged = if let Some(last_group) = groups.last_mut() {
            if let Some(&last_idx) = last_group.last() {
                if idx <= last_idx + 2 * context + 1 {
                    last_group.push(idx);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        if !merged {
            groups.push(vec![idx]);
        }
    }

    groups
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
fn yaml_escape(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped)
}

/// Serialize frontmatter back to YAML format.
pub fn serialize_frontmatter(fm: &PageFrontmatter) -> String {
    let mut out = String::from("---\n");

    out.push_str(&format!("title: {}\n", yaml_escape(&fm.title)));

    // Tags as inline array
    if fm.tags.is_empty() {
        out.push_str("tags: []\n");
    } else {
        out.push_str(&format!("tags: [{}]\n", fm.tags.join(", ")));
    }

    // Sources as multi-line array
    if fm.sources.is_empty() {
        out.push_str("sources: []\n");
    } else {
        out.push_str("sources:\n");
        for src in &fm.sources {
            out.push_str(&format!("  - url: {}\n", &src.url));
            out.push_str(&format!("    title: {}\n", yaml_escape(&src.title)));
            if let Some(ref accessed) = src.accessed_at {
                out.push_str(&format!("    accessed_at: {}\n", accessed));
            }
        }
    }

    // Contributors as inline array
    if fm.contributors.is_empty() {
        out.push_str("contributors: []\n");
    } else {
        out.push_str(&format!("contributors: [{}]\n", fm.contributors.join(", ")));
    }

    out.push_str(&format!("created: {}\n", &fm.created));
    out.push_str(&format!("updated: {}\n", &fm.updated));
    out.push_str("---\n");

    out
}

/// Split a YAML key-value line into (key, value).
///
/// Handles both `key: value` and bare `key:` (returns empty value).
fn split_kv_or_bare(line: &str) -> Option<(&str, &str)> {
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
fn parse_inline_array(value: &str) -> Option<Vec<String>> {
    let trimmed = value.trim();
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        let inner = &trimmed[1..trimmed.len() - 1];
        if inner.trim().is_empty() {
            return Some(Vec::new());
        }
        let items: Vec<String> = inner.split(',').map(|s| unquote(s.trim())).collect();
        Some(items)
    } else {
        None
    }
}

/// Remove surrounding quotes from a string value.
fn unquote(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_knowledge_manager_new() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
        assert_eq!(manager.cache_dir, crosslink_dir.join(KNOWLEDGE_CACHE_DIR));
        assert_eq!(manager.repo_root, dir.path());
    }

    #[test]
    fn test_knowledge_manager_not_initialized() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
        assert!(!manager.is_initialized());
    }

    #[test]
    fn test_list_pages_empty() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
        let pages = manager.list_pages().unwrap();
        assert!(pages.is_empty());
    }

    #[test]
    fn test_write_and_read_page() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        let content = "---\ntitle: Test Page\ntags: [rust, testing]\nsources: []\ncontributors: [alice]\ncreated: 2026-01-01\nupdated: 2026-01-02\n---\n\n# Test Page\n\nHello world.\n";

        manager.write_page("test-page", content).unwrap();
        let read_back = manager.read_page("test-page").unwrap();
        assert_eq!(read_back, content);
    }

    #[test]
    fn test_read_page_not_found() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
        let result = manager.read_page("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_list_pages_with_files() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        let page_a = "---\ntitle: Alpha\ntags: [a]\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nContent A\n";
        let page_b = "---\ntitle: Beta\ntags: [b, c]\nsources: []\ncontributors: [bob]\ncreated: 2026-01-02\nupdated: 2026-01-03\n---\n\nContent B\n";

        manager.write_page("alpha", page_a).unwrap();
        manager.write_page("beta", page_b).unwrap();

        // Write a non-md file that should be ignored
        std::fs::write(cache_dir.join("notes.txt"), "ignored").unwrap();

        let pages = manager.list_pages().unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].slug, "alpha");
        assert_eq!(pages[0].frontmatter.title, "Alpha");
        assert_eq!(pages[0].frontmatter.tags, vec!["a"]);
        assert_eq!(pages[1].slug, "beta");
        assert_eq!(pages[1].frontmatter.title, "Beta");
        assert_eq!(pages[1].frontmatter.tags, vec!["b", "c"]);
        assert_eq!(pages[1].frontmatter.contributors, vec!["bob"]);
    }

    // --- Frontmatter parsing tests ---

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = "---\ntitle: My Page\ntags: [rust, wasm]\nsources: []\ncontributors: [alice, bob]\ncreated: 2026-01-15\nupdated: 2026-02-20\n---\n\n# My Page\n";

        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "My Page");
        assert_eq!(fm.tags, vec!["rust", "wasm"]);
        assert!(fm.sources.is_empty());
        assert_eq!(fm.contributors, vec!["alice", "bob"]);
        assert_eq!(fm.created, "2026-01-15");
        assert_eq!(fm.updated, "2026-02-20");
    }

    #[test]
    fn test_parse_frontmatter_with_sources() {
        let content = "\
---
title: Research Notes
tags: [research]
sources:
  - url: https://example.com
    title: Example Site
    accessed_at: 2026-01-10
  - url: https://docs.rs
    title: Rust Docs
contributors: [carol]
created: 2026-01-01
updated: 2026-01-05
---

Body text.
";

        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "Research Notes");
        assert_eq!(fm.tags, vec!["research"]);
        assert_eq!(fm.sources.len(), 2);
        assert_eq!(fm.sources[0].url, "https://example.com");
        assert_eq!(fm.sources[0].title, "Example Site");
        assert_eq!(fm.sources[0].accessed_at, Some("2026-01-10".to_string()));
        assert_eq!(fm.sources[1].url, "https://docs.rs");
        assert_eq!(fm.sources[1].title, "Rust Docs");
        assert_eq!(fm.sources[1].accessed_at, None);
        assert_eq!(fm.contributors, vec!["carol"]);
    }

    #[test]
    fn test_parse_frontmatter_multiline_tags() {
        let content = "\
---
title: Tagged
tags:
  - alpha
  - beta
  - gamma
sources: []
contributors: []
created: 2026-01-01
updated: 2026-01-01
---
";
        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.tags, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn test_parse_frontmatter_empty_arrays() {
        let content = "---\ntitle: Empty\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";

        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "Empty");
        assert!(fm.tags.is_empty());
        assert!(fm.sources.is_empty());
        assert!(fm.contributors.is_empty());
    }

    #[test]
    fn test_parse_frontmatter_missing_fields() {
        let content = "---\ntitle: Minimal\ncreated: 2026-01-01\n---\n";

        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "Minimal");
        assert!(fm.tags.is_empty());
        assert!(fm.sources.is_empty());
        assert!(fm.contributors.is_empty());
        assert_eq!(fm.created, "2026-01-01");
        assert_eq!(fm.updated, "");
    }

    #[test]
    fn test_parse_frontmatter_no_frontmatter() {
        let content = "# Just a heading\n\nNo frontmatter here.\n";
        assert!(parse_frontmatter(content).is_none());
    }

    #[test]
    fn test_parse_frontmatter_crlf() {
        let content =
            "---\r\ntitle: CRLF Page\r\ntags: [rust, windows]\r\ncreated: 2026-03-12\r\nupdated: 2026-03-12\r\n---\r\n\r\n# Body\r\n";
        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "CRLF Page");
        assert_eq!(fm.tags, vec!["rust", "windows"]);
        assert_eq!(fm.created, "2026-03-12");
        assert_eq!(fm.updated, "2026-03-12");
    }

    #[test]
    fn test_parse_frontmatter_quoted_values() {
        let content = "---\ntitle: \"Quoted Title\"\ntags: ['a', \"b\"]\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";

        let fm = parse_frontmatter(content).unwrap();
        assert_eq!(fm.title, "Quoted Title");
        assert_eq!(fm.tags, vec!["a", "b"]);
    }

    #[test]
    fn test_serialize_frontmatter_roundtrip() {
        let fm = PageFrontmatter {
            title: "Test Page".to_string(),
            tags: vec!["rust".to_string(), "testing".to_string()],
            sources: vec![Source {
                url: "https://example.com".to_string(),
                title: "Example".to_string(),
                accessed_at: Some("2026-01-10".to_string()),
            }],
            contributors: vec!["alice".to_string()],
            created: "2026-01-01".to_string(),
            updated: "2026-01-15".to_string(),
        };

        let serialized = serialize_frontmatter(&fm);
        let parsed = parse_frontmatter(&serialized).unwrap();

        assert_eq!(parsed.title, fm.title);
        assert_eq!(parsed.tags, fm.tags);
        assert_eq!(parsed.sources.len(), fm.sources.len());
        assert_eq!(parsed.sources[0].url, fm.sources[0].url);
        assert_eq!(parsed.sources[0].title, fm.sources[0].title);
        assert_eq!(parsed.sources[0].accessed_at, fm.sources[0].accessed_at);
        assert_eq!(parsed.contributors, fm.contributors);
        assert_eq!(parsed.created, fm.created);
        assert_eq!(parsed.updated, fm.updated);
    }

    #[test]
    fn test_serialize_frontmatter_empty_collections() {
        let fm = PageFrontmatter {
            title: "Empty".to_string(),
            tags: Vec::new(),
            sources: Vec::new(),
            contributors: Vec::new(),
            created: "2026-01-01".to_string(),
            updated: "2026-01-01".to_string(),
        };

        let serialized = serialize_frontmatter(&fm);
        assert!(serialized.contains("tags: []"));
        assert!(serialized.contains("sources: []"));
        assert!(serialized.contains("contributors: []"));

        let parsed = parse_frontmatter(&serialized).unwrap();
        assert_eq!(parsed, fm);
    }

    #[test]
    fn test_serialize_frontmatter_multiple_sources() {
        let fm = PageFrontmatter {
            title: "Multi Source".to_string(),
            tags: Vec::new(),
            sources: vec![
                Source {
                    url: "https://a.com".to_string(),
                    title: "Site A".to_string(),
                    accessed_at: None,
                },
                Source {
                    url: "https://b.com".to_string(),
                    title: "Site B".to_string(),
                    accessed_at: Some("2026-02-01".to_string()),
                },
            ],
            contributors: Vec::new(),
            created: "2026-01-01".to_string(),
            updated: "2026-01-01".to_string(),
        };

        let serialized = serialize_frontmatter(&fm);
        let parsed = parse_frontmatter(&serialized).unwrap();

        assert_eq!(parsed.sources.len(), 2);
        assert_eq!(parsed.sources[0].url, "https://a.com");
        assert_eq!(parsed.sources[0].accessed_at, None);
        assert_eq!(parsed.sources[1].url, "https://b.com");
        assert_eq!(
            parsed.sources[1].accessed_at,
            Some("2026-02-01".to_string())
        );
    }

    #[test]
    fn test_inline_array_parsing() {
        assert_eq!(
            parse_inline_array("[a, b, c]"),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
        assert_eq!(parse_inline_array("[]"), Some(Vec::<String>::new()));
        assert_eq!(parse_inline_array("not an array"), None);
        assert_eq!(
            parse_inline_array("[single]"),
            Some(vec!["single".to_string()])
        );
    }

    #[test]
    fn test_unquote() {
        assert_eq!(unquote("hello"), "hello");
        assert_eq!(unquote("\"hello\""), "hello");
        assert_eq!(unquote("'hello'"), "hello");
        assert_eq!(unquote("  hello  "), "hello");
    }

    /// Helper: create a git repo with an initial commit.
    fn init_git_repo(path: &Path) {
        let p = path.to_string_lossy();
        Command::new("git").args(["init", &p]).output().unwrap();
        Command::new("git")
            .args(["-C", &p, "config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", &p, "config", "user.name", "Test"])
            .output()
            .unwrap();
        Command::new("git")
            .args(["-C", &p, "commit", "--allow-empty", "-m", "init"])
            .output()
            .unwrap();
    }

    // resolve_main_repo_root tests are in utils::tests

    #[test]
    fn test_knowledge_manager_in_worktree_uses_main_cache() {
        let dir = tempdir().unwrap();
        let main_root = dir.path().join("main");
        std::fs::create_dir_all(&main_root).unwrap();
        init_git_repo(&main_root);

        let main_crosslink = main_root.join(".crosslink");
        std::fs::create_dir_all(&main_crosslink).unwrap();

        // Create worktree
        Command::new("git")
            .args([
                "-C",
                &main_root.to_string_lossy(),
                "branch",
                "feature/knowledge-test",
            ])
            .output()
            .unwrap();
        let wt_path = main_root.join(".worktrees").join("knowledge-test");
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        Command::new("git")
            .args([
                "-C",
                &main_root.to_string_lossy(),
                "worktree",
                "add",
                &wt_path.to_string_lossy(),
                "feature/knowledge-test",
            ])
            .output()
            .unwrap();

        let wt_crosslink = wt_path.join(".crosslink");
        std::fs::create_dir_all(&wt_crosslink).unwrap();

        let manager = KnowledgeManager::new(&wt_crosslink).unwrap();

        // cache_dir should point to the main repo's knowledge cache, not the worktree's
        let expected_parent = main_crosslink.canonicalize().unwrap();
        let actual_parent = manager.cache_dir.parent().unwrap().canonicalize().unwrap();
        assert_eq!(actual_parent, expected_parent);
        assert_eq!(manager.cache_dir.file_name().unwrap(), KNOWLEDGE_CACHE_DIR);

        // repo_root should be the main repo, not the worktree
        assert_eq!(
            manager.repo_root.canonicalize().unwrap(),
            main_root.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_write_page_without_init_fails() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
        let result = manager.write_page("test", "content");
        assert!(result.is_err());
    }

    // --- Search tests ---

    /// Helper: create a KnowledgeManager with pre-populated pages.
    fn setup_search_manager(pages: &[(&str, &str)]) -> (tempfile::TempDir, KnowledgeManager) {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
        for (slug, content) in pages {
            manager.write_page(slug, content).unwrap();
        }
        (dir, manager)
    }

    #[test]
    fn test_search_content_finds_matches_across_files() {
        let page_a = "---\ntitle: Rust Testing\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\n# Property Testing\n\nUse proptest for property-based testing.\n";
        let page_b = "---\ntitle: CI Setup\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nIncludes property testing via cargo test.\n";

        let (_dir, manager) =
            setup_search_manager(&[("rust-testing", page_a), ("ci-setup", page_b)]);

        let results = manager.search_content("property testing", 0).unwrap();
        assert!(results.len() >= 2, "Should match in both files");

        let slugs: Vec<&str> = results.iter().map(|r| r.slug.as_str()).collect();
        assert!(slugs.contains(&"ci-setup"));
        assert!(slugs.contains(&"rust-testing"));
    }

    #[test]
    fn test_search_content_case_insensitive() {
        let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nPROPERTY Testing is great.\n";
        let (_dir, manager) = setup_search_manager(&[("test-page", page)]);

        let results = manager.search_content("property testing", 0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "test-page");
    }

    #[test]
    fn test_search_content_returns_context_lines() {
        let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nLine before match.\nThe match line here.\nLine after match.\n";
        let (_dir, manager) = setup_search_manager(&[("ctx-page", page)]);

        let results = manager.search_content("match line", 1).unwrap();
        assert_eq!(results.len(), 1);
        // Should have 3 lines: before, match, after
        assert!(results[0].context_lines.len() >= 3);
    }

    #[test]
    fn test_search_content_no_results() {
        let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nNothing relevant here.\n";
        let (_dir, manager) = setup_search_manager(&[("empty-page", page)]);

        let results = manager.search_content("nonexistent query", 0).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_content_empty_cache() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
        let results = manager.search_content("anything", 0).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_content_word_level_matching() {
        // "modular architecture" should match a page containing both words non-adjacently
        let page = "---\ntitle: Modular Tabular Ingestion\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\n# Modular Tabular Ingestion — Architecture Overview\n\nThe system uses a modular design.\n";
        let (_dir, manager) = setup_search_manager(&[("modular-ingestion", page)]);

        let results = manager.search_content("modular architecture", 0).unwrap();
        assert!(!results.is_empty(), "Should match on individual words");
        assert_eq!(results[0].slug, "modular-ingestion");
    }

    #[test]
    fn test_search_content_ranks_by_term_hits() {
        // Page A matches both terms, Page B matches only one
        let page_a = "---\ntitle: Full Match\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nRust async patterns for concurrency.\n";
        let page_b = "---\ntitle: Partial Match\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nJava concurrency utilities.\n";
        let (_dir, manager) =
            setup_search_manager(&[("full-match", page_a), ("partial-match", page_b)]);

        let results = manager.search_content("rust concurrency", 0).unwrap();
        assert_eq!(results.len(), 2);
        // Page matching both terms should come first
        assert_eq!(results[0].slug, "full-match");
        assert_eq!(results[1].slug, "partial-match");
    }

    #[test]
    fn test_search_content_single_word_still_works() {
        let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nProptest is a property testing framework.\n";
        let (_dir, manager) = setup_search_manager(&[("proptest", page)]);

        let results = manager.search_content("proptest", 0).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "proptest");
    }

    #[test]
    fn test_search_sources_by_domain() {
        let page_a = "---\ntitle: Rust Docs\ntags: []\nsources:\n  - url: https://doc.rust-lang.org/book\n    title: The Rust Book\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nContent.\n";
        let page_b = "---\ntitle: Other\ntags: []\nsources:\n  - url: https://example.com\n    title: Example\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nContent.\n";

        let (_dir, manager) = setup_search_manager(&[("rust-docs", page_a), ("other", page_b)]);

        let results = manager.search_sources("rust-lang.org").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "rust-docs");
    }

    #[test]
    fn test_search_sources_no_match() {
        let page = "---\ntitle: Test\ntags: []\nsources:\n  - url: https://example.com\n    title: Example\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nContent.\n";
        let (_dir, manager) = setup_search_manager(&[("test", page)]);

        let results = manager.search_sources("nonexistent.org").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_group_matches_separate_groups() {
        // With context=0, matches at 0 and 5 should be separate groups
        let groups = group_matches(&[0, 5], 0);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn test_group_matches_overlapping_context() {
        // With context=2, matches at 0 and 3 overlap (0+2+1 = 3 >= 3) so they merge
        let groups = group_matches(&[0, 3], 2);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], vec![0, 3]);
    }

    #[test]
    fn test_group_matches_empty() {
        let groups = group_matches(&[], 0);
        assert!(groups.is_empty());
    }

    // --- Slug validation / path traversal tests ---

    #[test]
    fn test_safe_page_path_rejects_traversal() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        assert!(manager.safe_page_path("../etc/passwd").is_err());
        assert!(manager.safe_page_path("../../sensitive").is_err());
        assert!(manager.safe_page_path("foo/bar").is_err());
        assert!(manager.safe_page_path("foo\\bar").is_err());
        assert!(manager.safe_page_path("..").is_err());
        assert!(manager.safe_page_path("").is_err());
    }

    #[test]
    fn test_safe_page_path_allows_valid_slugs() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        assert!(manager.safe_page_path("my-page").is_ok());
        assert!(manager.safe_page_path("test_page").is_ok());
        assert!(manager.safe_page_path("page123").is_ok());
        assert!(manager.safe_page_path("a").is_ok());
    }

    #[test]
    fn test_safe_page_path_rejects_windows_reserved_names() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        for name in &["CON", "con", "PRN", "AUX", "NUL", "COM1", "LPT1"] {
            let result = manager.safe_page_path(name);
            assert!(
                result.is_err(),
                "Should reject Windows reserved name: {}",
                name
            );
            assert!(
                result.unwrap_err().to_string().contains("Windows reserved"),
                "Error should mention Windows reserved for: {}",
                name
            );
        }
    }

    #[test]
    fn test_write_page_rejects_traversal() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        let result = manager.write_page("../escape", "malicious content");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path separators"));
    }

    #[test]
    fn test_read_page_rejects_traversal() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        let result = manager.read_page("../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn test_delete_page_rejects_traversal() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        let result = manager.delete_page("../../../important-file");
        assert!(result.is_err());
    }

    #[test]
    fn test_page_exists_rejects_traversal() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        // Should return false (not panic or escape) for traversal slugs
        assert!(!manager.page_exists("../etc/passwd"));
    }

    // --- Conflict detection and resolution tests ---

    #[test]
    fn test_has_conflict_markers_true() {
        let content = "before\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> branch\nafter\n";
        assert!(has_conflict_markers(content));
    }

    #[test]
    fn test_has_conflict_markers_false_no_markers() {
        let content = "# Normal page\n\nNo conflicts here.\n";
        assert!(!has_conflict_markers(content));
    }

    #[test]
    fn test_has_conflict_markers_false_partial() {
        // Only has opening marker, not a real conflict
        let content = "<<<<<<< HEAD\nsome text\n";
        assert!(!has_conflict_markers(content));
    }

    #[test]
    fn test_resolve_accept_both_single_conflict() {
        let content = "\
before conflict
<<<<<<< HEAD
local version line 1
local version line 2
=======
remote version line 1
remote version line 2
>>>>>>> origin/crosslink/knowledge
after conflict
";
        let resolved = resolve_accept_both(content);

        assert!(!has_conflict_markers(&resolved));
        assert!(resolved.contains("before conflict"));
        assert!(resolved.contains("after conflict"));
        assert!(resolved.contains("local version line 1"));
        assert!(resolved.contains("local version line 2"));
        assert!(resolved.contains("remote version line 1"));
        assert!(resolved.contains("remote version line 2"));
        assert!(
            resolved.contains("<!-- MERGE CONFLICT: Both versions kept. Cleanup recommended. -->")
        );
        // Both versions should be separated by horizontal rules
        assert!(resolved.contains("---\n"));
    }

    #[test]
    fn test_resolve_accept_both_multiple_conflicts() {
        let content = "\
# Header
<<<<<<< HEAD
first local
=======
first remote
>>>>>>> branch
middle content
<<<<<<< HEAD
second local
=======
second remote
>>>>>>> branch
footer
";
        let resolved = resolve_accept_both(content);

        assert!(!has_conflict_markers(&resolved));
        assert!(resolved.contains("# Header"));
        assert!(resolved.contains("first local"));
        assert!(resolved.contains("first remote"));
        assert!(resolved.contains("middle content"));
        assert!(resolved.contains("second local"));
        assert!(resolved.contains("second remote"));
        assert!(resolved.contains("footer"));
        // Should have two conflict comments
        assert_eq!(resolved.matches("<!-- MERGE CONFLICT:").count(), 2);
    }

    #[test]
    fn test_resolve_accept_both_no_conflicts() {
        let content = "# Normal content\n\nNo conflicts.\n";
        let resolved = resolve_accept_both(content);
        assert_eq!(resolved, content);
    }

    #[test]
    fn test_resolve_accept_both_preserves_frontmatter() {
        let content = "\
---
title: Test Page
tags: [rust]
sources: []
contributors: [alice]
created: 2026-01-01
updated: 2026-01-01
---

<<<<<<< HEAD
Local section content
=======
Remote section content
>>>>>>> origin/crosslink/knowledge
";
        let resolved = resolve_accept_both(content);

        assert!(!has_conflict_markers(&resolved));
        // Frontmatter should be intact
        assert!(resolved.contains("title: Test Page"));
        assert!(resolved.contains("tags: [rust]"));
        // Both versions kept
        assert!(resolved.contains("Local section content"));
        assert!(resolved.contains("Remote section content"));
    }

    #[test]
    fn test_resolve_accept_both_empty_sides() {
        let content = "\
<<<<<<< HEAD
=======
only remote
>>>>>>> branch
";
        let resolved = resolve_accept_both(content);

        assert!(!has_conflict_markers(&resolved));
        assert!(resolved.contains("only remote"));
        assert!(resolved.contains("<!-- MERGE CONFLICT:"));
    }

    #[test]
    fn test_resolve_conflicts_in_cache() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

        // Write a file with conflict markers
        let conflicted =
            "---\ntitle: Test\n---\n\n<<<<<<< HEAD\nlocal\n=======\nremote\n>>>>>>> branch\n";
        manager.write_page("conflicted", conflicted).unwrap();

        // Write a clean file
        let clean = "---\ntitle: Clean\n---\n\nNo conflicts.\n";
        manager.write_page("clean", clean).unwrap();

        let resolved = manager.resolve_conflicts_in_cache().unwrap();

        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], "conflicted");

        // Verify the file was actually resolved
        let content = manager.read_page("conflicted").unwrap();
        assert!(!has_conflict_markers(&content));
        assert!(content.contains("local"));
        assert!(content.contains("remote"));

        // Verify clean file was not touched
        let clean_content = manager.read_page("clean").unwrap();
        assert_eq!(clean_content, clean);
    }
}
