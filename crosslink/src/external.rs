//! External repository query support.
//!
//! Enables read-only queries against knowledge pages and issues from other
//! repositories, either by fetching remote `crosslink/knowledge` and
//! `crosslink/hub` branches or by reading from a local repo's `.crosslink` data.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::issue_file::{read_all_issue_files, IssueFile};
use crate::knowledge::{parse_frontmatter, PageFrontmatter, PageInfo, SearchMatch};

/// Default TTL for cached data (knowledge pages, issue JSON): 5 minutes.
const DEFAULT_DATA_TTL_SECS: u64 = 300;

/// Default TTL for resolved URLs (HTTPS vs SSH probe result): 24 hours.
const DEFAULT_URL_TTL_SECS: u64 = 86400;

/// Timeout for `git ls-remote` probes.
const PROBE_TIMEOUT_SECS: u64 = 5;

// ───────────────────────────────────────────────────────────────────────────
// Source resolution
// ───────────────────────────────────────────────────────────────────────────

/// Where an external repo lives.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum RepoSource {
    /// A local filesystem path (e.g. `/Users/maxine/code/other-repo`).
    Local(PathBuf),
    /// A remote git URL (fully resolved, fetchable).
    Remote(String),
}

/// Resolve a `--repo` value to a [`RepoSource`].
///
/// Resolution order:
/// 1. Named alias (`@name`) -- looked up in config `repo-alias.<name>`
/// 2. Local path -- if it exists on disk and contains `.crosslink/` or `.git/`
/// 3. Git URL -- HTTPS-first, SSH-fallback probe for shorthands
///
/// # Errors
///
/// Returns an error if an alias cannot be resolved or the repo value is invalid.
pub fn resolve_repo(value: &str, crosslink_dir: &Path) -> Result<RepoSource> {
    // 1. Named alias
    if let Some(alias_name) = value.strip_prefix('@') {
        let alias_value = read_repo_alias(crosslink_dir, alias_name)?;
        // Recurse with the resolved alias (but don't allow nested aliases)
        return Ok(resolve_repo_inner(&alias_value));
    }

    Ok(resolve_repo_inner(value))
}

fn resolve_repo_inner(value: &str) -> RepoSource {
    // 2. Local path
    let path = PathBuf::from(value);
    if path.exists() {
        let has_crosslink = path.join(".crosslink").exists();
        let has_git = path.join(".git").exists();
        if has_crosslink || has_git {
            return RepoSource::Local(path);
        }
    }

    // 3. Git URL — if fully qualified, use directly
    if value.starts_with("https://")
        || value.starts_with("http://")
        || value.starts_with("git@")
        || value.starts_with("ssh://")
    {
        return RepoSource::Remote(value.to_string());
    }

    // Shorthand like `github.com/org/repo` — will be probed during fetch
    RepoSource::Remote(value.to_string())
}

/// Read a repo alias from config.
fn read_repo_alias(crosslink_dir: &Path, name: &str) -> Result<String> {
    let config_path = crosslink_dir.join("hook-config.json");
    if !config_path.exists() {
        bail!("Unknown repo alias: @{name}. No config file found.");
    }
    let content =
        std::fs::read_to_string(&config_path).context("Failed to read hook-config.json")?;
    let config: serde_json::Value =
        serde_json::from_str(&content).context("Failed to parse hook-config.json")?;

    config
        .get("repo-alias")
        .and_then(|v| v.get(name))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown repo alias: @{name}. Set it with: crosslink config set repo-alias.{name} <url>"
            )
        })
}

// ───────────────────────────────────────────────────────────────────────────
// URL probing
// ───────────────────────────────────────────────────────────────────────────

/// For shorthand URLs like `github.com/org/repo`, probe HTTPS then SSH.
/// Returns the first fetchable URL. Fully qualified URLs are returned as-is.
///
/// # Errors
///
/// Returns an error if the repository cannot be reached via HTTPS or SSH.
pub fn probe_url(shorthand: &str) -> Result<String> {
    if shorthand.starts_with("https://")
        || shorthand.starts_with("http://")
        || shorthand.starts_with("git@")
        || shorthand.starts_with("ssh://")
    {
        return Ok(shorthand.to_string());
    }

    let https_url = format!("https://{shorthand}");
    if git_ls_remote_ok(&https_url) {
        return Ok(https_url);
    }

    // Try SSH: github.com/org/repo → git@github.com:org/repo.git
    if let Some((host, path)) = shorthand.split_once('/') {
        let ssh_url = format!("git@{host}:{path}.git");
        if git_ls_remote_ok(&ssh_url) {
            return Ok(ssh_url);
        }
        bail!(
            "Cannot reach repository '{shorthand}'.\n\
             Tried:\n  HTTPS: {https_url}\n  SSH:   {ssh_url}\n\
             Check your credentials and network connection."
        );
    }

    bail!("Invalid repository shorthand: '{shorthand}'. Expected format: host/org/repo");
}

fn git_ls_remote_ok(url: &str) -> bool {
    let Ok(mut child) = Command::new("git")
        .args(["ls-remote", "--quiet", "--exit-code", url])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };

    // Wait with timeout
    let deadline = std::time::Instant::now() + Duration::from_secs(PROBE_TIMEOUT_SECS);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(_) => return false,
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Cache management
// ───────────────────────────────────────────────────────────────────────────

/// Manages cached external repository data under `.crosslink/.external-cache/`.
pub struct ExternalCache {
    /// Root cache directory for this specific source.
    cache_dir: PathBuf,
    /// The original repo value (for display).
    repo_label: String,
}

/// Metadata stored in `meta.json` per cached source.
#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct CacheMeta {
    /// The original repo value provided by the user.
    #[serde(default)]
    url: String,
    /// The resolved fetchable URL (after HTTPS/SSH probe).
    #[serde(default)]
    resolved_url: Option<String>,
    /// ISO-8601 timestamp of last knowledge branch fetch.
    #[serde(default)]
    knowledge_fetched_at: Option<String>,
    /// ISO-8601 timestamp of last hub branch fetch.
    #[serde(default)]
    hub_fetched_at: Option<String>,
    /// ISO-8601 timestamp of URL resolution.
    #[serde(default)]
    url_resolved_at: Option<String>,
}

impl ExternalCache {
    /// Create a cache handle for a remote source.
    #[must_use]
    pub fn new(crosslink_dir: &Path, repo_label: &str) -> Self {
        let hash = cache_hash(repo_label);
        let cache_dir = crosslink_dir.join(".external-cache").join(hash);
        Self {
            cache_dir,
            repo_label: repo_label.to_string(),
        }
    }

    /// Get path to the knowledge pages directory.
    #[must_use]
    pub fn knowledge_dir(&self) -> PathBuf {
        self.cache_dir.join("knowledge")
    }

    /// Read existing cache metadata.
    fn read_meta(&self) -> CacheMeta {
        let meta_path = self.cache_dir.join("meta.json");
        if meta_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&meta_path) {
                if let Ok(meta) = serde_json::from_str(&content) {
                    return meta;
                }
            }
        }
        CacheMeta {
            url: self.repo_label.clone(),
            ..Default::default()
        }
    }

    /// Write cache metadata.
    fn write_meta(&self, meta: &CacheMeta) -> Result<()> {
        std::fs::create_dir_all(&self.cache_dir)?;
        let content = serde_json::to_string_pretty(meta)?;
        std::fs::write(self.cache_dir.join("meta.json"), content)?;
        Ok(())
    }

    /// Ensure the knowledge branch is fetched and cached. Returns the knowledge dir path.
    ///
    /// # Errors
    ///
    /// Returns an error if URL resolution or branch fetching fails.
    pub fn ensure_knowledge(
        &self,
        data_ttl: u64,
        url_ttl: u64,
        force_refresh: bool,
    ) -> Result<PathBuf> {
        let dir = self.knowledge_dir();
        if !force_refresh && self.is_data_fresh("knowledge", data_ttl) {
            return Ok(dir);
        }
        let url = self.resolve_url(url_ttl, force_refresh)?;
        self.fetch_branch(&url, "crosslink/knowledge", &dir, "knowledge")?;
        Ok(dir)
    }

    /// Ensure the hub branch is fetched and cached. Returns the hub dir path.
    ///
    /// # Errors
    ///
    /// Returns an error if URL resolution or branch fetching fails.
    pub fn ensure_hub(&self, data_ttl: u64, url_ttl: u64, force_refresh: bool) -> Result<PathBuf> {
        let dir = self.cache_dir.join("hub");
        if !force_refresh && self.is_data_fresh("hub", data_ttl) {
            return Ok(dir);
        }
        let url = self.resolve_url(url_ttl, force_refresh)?;
        self.fetch_branch(&url, "crosslink/hub", &dir, "hub")?;
        Ok(dir)
    }

    /// Resolve the fetchable URL, using cached resolution if within TTL.
    fn resolve_url(&self, url_ttl: u64, force: bool) -> Result<String> {
        let mut meta = self.read_meta();

        if !force {
            if let Some(ref resolved) = meta.resolved_url {
                if let Some(ref resolved_at) = meta.url_resolved_at {
                    if is_within_ttl(resolved_at, url_ttl) {
                        return Ok(resolved.clone());
                    }
                }
            }
        }

        let resolved = probe_url(&self.repo_label)?;
        meta.resolved_url = Some(resolved.clone());
        meta.url_resolved_at = Some(now_iso());
        meta.url.clone_from(&self.repo_label);
        self.write_meta(&meta)?;
        Ok(resolved)
    }

    /// Check if cached data for a branch type is still fresh.
    fn is_data_fresh(&self, branch_type: &str, ttl_secs: u64) -> bool {
        let meta = self.read_meta();
        let fetched_at = match branch_type {
            "knowledge" => meta.knowledge_fetched_at.as_deref(),
            "hub" => meta.hub_fetched_at.as_deref(),
            _ => None,
        };
        fetched_at.is_some_and(|ts| is_within_ttl(ts, ttl_secs))
    }

    /// Fetch a branch from a remote URL and materialize its files into `output_dir`.
    fn fetch_branch(
        &self,
        url: &str,
        branch: &str,
        output_dir: &Path,
        branch_type: &str,
    ) -> Result<()> {
        std::fs::create_dir_all(output_dir)?;

        // Use a bare repo as fetch target
        let bare_dir = self.cache_dir.join("bare.git");
        if !bare_dir.join("HEAD").exists() {
            let status = Command::new("git")
                .args(["init", "--bare"])
                .arg(&bare_dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .context("Failed to run git init --bare")?;
            if !status.success() {
                bail!("git init --bare failed for external cache");
            }
        }

        // Fetch the specific branch
        let refspec = format!("+refs/heads/{branch}:refs/heads/{branch}");
        let status = Command::new("git")
            .current_dir(&bare_dir)
            .args(["fetch", "--depth=1", url, &refspec])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .context("Failed to run git fetch")?;
        if !status.success() {
            bail!(
                "Failed to fetch {branch} from '{url}'. \
                 Ensure the repository exists and you have access."
            );
        }

        // Materialize the branch content into output_dir using checkout-index
        // First, clean the output directory
        if output_dir.exists() {
            // Remove old files but keep the directory
            for entry in std::fs::read_dir(output_dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
            }
        }

        let output = Command::new("git")
            .current_dir(&bare_dir)
            .env("GIT_WORK_TREE", output_dir)
            .args(["checkout", branch, "--", "."])
            .output()
            .context("Failed to run git checkout for materialization")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to materialize {branch} from cache: {stderr}");
        }

        // Update metadata
        let mut meta = self.read_meta();
        let now = now_iso();
        match branch_type {
            "knowledge" => meta.knowledge_fetched_at = Some(now),
            "hub" => meta.hub_fetched_at = Some(now),
            _ => {}
        }
        self.write_meta(&meta)?;

        Ok(())
    }
}

/// Generate a cache directory name from a repo label.
fn cache_hash(label: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..8]) // 16 hex chars
}

/// Simple hex encoding (avoid adding another dependency).
mod hex {
    use std::fmt::Write as _;

    pub fn encode(bytes: &[u8]) -> String {
        bytes.iter().fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
    }
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn is_within_ttl(timestamp: &str, ttl_secs: u64) -> bool {
    chrono::DateTime::parse_from_rfc3339(timestamp).is_ok_and(|ts| {
        let elapsed = chrono::Utc::now().signed_duration_since(ts);
        elapsed.num_seconds() < i64::from(u32::try_from(ttl_secs).unwrap_or(u32::MAX))
    })
}

// ───────────────────────────────────────────────────────────────────────────
// TTL configuration helpers
// ───────────────────────────────────────────────────────────────────────────

/// Read the data TTL from config, falling back to the default.
#[must_use]
pub fn read_data_ttl(crosslink_dir: &Path) -> u64 {
    read_config_u64(crosslink_dir, "external-cache-ttl").unwrap_or(DEFAULT_DATA_TTL_SECS)
}

/// Read the URL resolution TTL from config, falling back to the default.
#[must_use]
pub fn read_url_ttl(crosslink_dir: &Path) -> u64 {
    read_config_u64(crosslink_dir, "external-url-ttl").unwrap_or(DEFAULT_URL_TTL_SECS)
}

fn read_config_u64(crosslink_dir: &Path, key: &str) -> Option<u64> {
    let config_path = crosslink_dir.join("hook-config.json");
    let content = std::fs::read_to_string(&config_path).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    config
        .get(key)?
        .as_u64()
        .or_else(|| config.get(key)?.as_str()?.parse::<u64>().ok())
}

// ───────────────────────────────────────────────────────────────────────────
// ExternalKnowledgeReader
// ───────────────────────────────────────────────────────────────────────────

/// Reads knowledge pages from an arbitrary directory (external cache or local repo).
pub struct ExternalKnowledgeReader {
    /// Directory containing `.md` knowledge pages.
    pages_dir: PathBuf,
}

impl ExternalKnowledgeReader {
    #[must_use]
    pub const fn new(pages_dir: PathBuf) -> Self {
        Self { pages_dir }
    }

    /// Create a reader for a local repo's knowledge cache.
    #[must_use]
    pub fn for_local(repo_path: &Path) -> Self {
        Self {
            pages_dir: repo_path.join(".crosslink").join(".knowledge-cache"),
        }
    }

    /// List all pages with parsed frontmatter.
    ///
    /// # Errors
    ///
    /// Returns an error if the pages directory cannot be read.
    pub fn list_pages(&self) -> Result<Vec<PageInfo>> {
        list_pages_in_dir(&self.pages_dir)
    }

    /// Read a single page by slug.
    ///
    /// # Errors
    ///
    /// Returns an error if the page does not exist or cannot be read.
    pub fn read_page(&self, slug: &str) -> Result<String> {
        let path = self.pages_dir.join(format!("{slug}.md"));
        if !path.exists() {
            bail!("Page '{slug}' not found in external source");
        }
        std::fs::read_to_string(&path).context("Failed to read external page")
    }

    /// Search page content (same algorithm as `KnowledgeManager::search_content`).
    ///
    /// # Errors
    ///
    /// Returns an error if the pages directory cannot be read.
    pub fn search_content(&self, query: &str, context: usize) -> Result<Vec<SearchMatch>> {
        search_content_in_dir(&self.pages_dir, query, context)
    }

    /// Search by source URL domain.
    ///
    /// # Errors
    ///
    /// Returns an error if listing pages fails.
    pub fn search_sources(&self, domain: &str) -> Result<Vec<PageInfo>> {
        let domain_lower = domain.to_lowercase();
        let pages = self.list_pages()?;
        Ok(pages
            .into_iter()
            .filter(|page| {
                page.frontmatter
                    .sources
                    .iter()
                    .any(|src| src.url.to_lowercase().contains(&domain_lower))
            })
            .collect())
    }
}

// ───────────────────────────────────────────────────────────────────────────
// ExternalIssueReader
// ───────────────────────────────────────────────────────────────────────────

/// Reads and filters issues from an external hub cache.
pub struct ExternalIssueReader {
    issues: Vec<IssueFile>,
}

impl ExternalIssueReader {
    /// Create a reader from a hub directory that contains an `issues/` subdirectory.
    ///
    /// # Errors
    ///
    /// Returns an error if the issues directory cannot be read or contains invalid data.
    pub fn from_hub_dir(hub_dir: &Path) -> Result<Self> {
        let issues_dir = hub_dir.join("issues");
        let issues = read_all_issue_files(&issues_dir)?;
        Ok(Self { issues })
    }

    /// Create a reader for a local repo's hub cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the hub cache directory cannot be read.
    pub fn for_local(repo_path: &Path) -> Result<Self> {
        let hub_dir = repo_path.join(".crosslink").join(".hub-cache");
        Self::from_hub_dir(&hub_dir)
    }

    /// List issues with optional filters (mirrors `db.list_issues` semantics).
    #[must_use]
    pub fn list_issues(
        &self,
        status_filter: Option<&str>,
        label_filter: Option<&str>,
        priority_filter: Option<&str>,
    ) -> Vec<&IssueFile> {
        self.issues
            .iter()
            .filter(|issue| {
                // Status filter
                match status_filter {
                    Some("all") | None => true,
                    Some(s) => s
                        .parse::<crate::models::IssueStatus>()
                        .map(|st| issue.status == st)
                        .unwrap_or(false),
                }
            })
            .filter(|issue| {
                label_filter.is_none_or(|label| issue.labels.iter().any(|l| l == label))
            })
            .filter(|issue| {
                priority_filter
                    .and_then(|p| p.parse::<crate::models::Priority>().ok())
                    .is_none_or(|p| issue.priority == p)
            })
            .collect()
    }

    /// Search issues by text (case-insensitive substring in title, description, comments).
    #[must_use]
    pub fn search_issues(&self, query: &str) -> Vec<&IssueFile> {
        let query_lower = query.to_lowercase();
        self.issues
            .iter()
            .filter(|issue| {
                issue.title.to_lowercase().contains(&query_lower)
                    || issue
                        .description
                        .as_ref()
                        .is_some_and(|d: &String| d.to_lowercase().contains(&query_lower))
                    || issue
                        .comments
                        .iter()
                        .any(|c| c.content.to_lowercase().contains(&query_lower))
            })
            .collect()
    }

    /// Find a single issue by `display_id`.
    #[must_use]
    pub fn get_issue(&self, display_id: i64) -> Option<&IssueFile> {
        self.issues
            .iter()
            .find(|issue| issue.display_id == Some(display_id))
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Standalone functions extracted from KnowledgeManager
// ───────────────────────────────────────────────────────────────────────────

/// List all `.md` pages in a directory with parsed frontmatter.
///
/// # Errors
///
/// Returns an error if the directory cannot be read or a page file is unreadable.
pub fn list_pages_in_dir(dir: &Path) -> Result<Vec<PageInfo>> {
    let mut pages = Vec::new();
    if !dir.exists() {
        return Ok(pages);
    }

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "md") {
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

/// Search page content in a directory (same algorithm as `KnowledgeManager::search_content`).
///
/// # Errors
///
/// Returns an error if the directory cannot be read or a page file is unreadable.
pub fn search_content_in_dir(
    dir: &Path,
    query: &str,
    ctx_lines: usize,
) -> Result<Vec<SearchMatch>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let query_lower = query.to_lowercase();
    let terms: Vec<&str> = query_lower.split_whitespace().collect();
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

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

        let term_hits = terms
            .iter()
            .filter(|term| content_lower.contains(**term))
            .count();

        if term_hits == 0 {
            continue;
        }

        let matching_indices: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                let line_lower = line.to_lowercase();
                terms.iter().any(|term| line_lower.contains(term))
            })
            .map(|(i, _)| i)
            .collect();

        let groups = group_matches(&matching_indices, ctx_lines);
        let mut file_matches = Vec::new();

        for group in groups {
            let first_match = group[0];
            let start = first_match.saturating_sub(ctx_lines);
            let last_match = group[group.len() - 1];
            let end = (last_match + ctx_lines + 1).min(lines.len());

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

    scored_results.sort_by(|a, b| b.0.cmp(&a.0));

    Ok(scored_results
        .into_iter()
        .flat_map(|(_, matches)| matches)
        .collect())
}

/// Group contiguous match indices with context overlap.
fn group_matches(indices: &[usize], context: usize) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &idx in indices {
        if let Some(last_group) = groups.last_mut() {
            let Some(&last_idx) = last_group.last() else {
                groups.push(vec![idx]);
                continue;
            };
            // Merge if this match's context window overlaps with previous
            if idx <= last_idx + 2 * context + 1 {
                last_group.push(idx);
            } else {
                groups.push(vec![idx]);
            }
        } else {
            groups.push(vec![idx]);
        }
    }
    groups
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_cache_hash_deterministic() {
        let h1 = cache_hash("github.com/org/repo");
        let h2 = cache_hash("github.com/org/repo");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn test_cache_hash_different_inputs() {
        let h1 = cache_hash("github.com/org/repo-a");
        let h2 = cache_hash("github.com/org/repo-b");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_resolve_repo_local_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo_path = tmp.path().join("my-repo");
        fs::create_dir_all(repo_path.join(".crosslink")).unwrap();

        let result = resolve_repo_inner(repo_path.to_str().unwrap());
        match result {
            RepoSource::Local(p) => assert_eq!(p, repo_path),
            _ => panic!("Expected Local variant"),
        }
    }

    #[test]
    fn test_resolve_repo_git_url() {
        let result = resolve_repo_inner("https://github.com/org/repo");
        match result {
            RepoSource::Remote(url) => assert_eq!(url, "https://github.com/org/repo"),
            _ => panic!("Expected Remote variant"),
        }
    }

    #[test]
    fn test_resolve_repo_shorthand() {
        let result = resolve_repo_inner("github.com/org/repo");
        match result {
            RepoSource::Remote(url) => assert_eq!(url, "github.com/org/repo"),
            _ => panic!("Expected Remote variant"),
        }
    }

    #[test]
    fn test_is_within_ttl() {
        let now = now_iso();
        assert!(is_within_ttl(&now, 60));
        assert!(!is_within_ttl("2020-01-01T00:00:00+00:00", 60));
    }

    #[test]
    fn test_list_pages_in_dir_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let pages = list_pages_in_dir(tmp.path()).unwrap();
        assert!(pages.is_empty());
    }

    #[test]
    fn test_list_pages_in_dir_with_pages() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("auth.md"),
            "---\ntitle: Auth\ntags:\n  - security\n---\n# Auth\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("api.md"),
            "---\ntitle: API Design\ntags:\n  - design\n---\n# API\n",
        )
        .unwrap();

        let pages = list_pages_in_dir(tmp.path()).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].slug, "api");
        assert_eq!(pages[1].slug, "auth");
    }

    #[test]
    fn test_search_content_in_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("auth.md"),
            "---\ntitle: Auth\n---\nJWT tokens are used for authentication.\nRS256 algorithm.\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("api.md"),
            "---\ntitle: API\n---\nREST endpoints.\n",
        )
        .unwrap();

        let results = search_content_in_dir(tmp.path(), "JWT", 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "auth");
    }

    #[test]
    fn test_external_issue_reader_search() {
        let tmp = tempfile::tempdir().unwrap();
        let issues_dir = tmp.path().join("issues");
        fs::create_dir_all(&issues_dir).unwrap();

        let issue = serde_json::json!({
            "uuid": "00000000-0000-0000-0000-000000000001",
            "display_id": 1,
            "title": "Fix authentication bug",
            "status": "open",
            "priority": "high",
            "created_by": "agent",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        });
        fs::write(
            issues_dir.join("00000000-0000-0000-0000-000000000001.json"),
            serde_json::to_string_pretty(&issue).unwrap(),
        )
        .unwrap();

        let reader = ExternalIssueReader::from_hub_dir(tmp.path()).unwrap();
        let results = reader.search_issues("authentication");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Fix authentication bug");
    }

    #[test]
    fn test_external_issue_reader_list_filter() {
        let tmp = tempfile::tempdir().unwrap();
        let issues_dir = tmp.path().join("issues");
        fs::create_dir_all(&issues_dir).unwrap();

        for (i, (status, priority)) in [("open", "high"), ("closed", "low"), ("open", "medium")]
            .iter()
            .enumerate()
        {
            let uuid = format!("00000000-0000-0000-0000-{:012}", i);
            let issue = serde_json::json!({
                "uuid": uuid,
                "display_id": i as i64 + 1,
                "title": format!("Issue {}", i + 1),
                "status": status,
                "priority": priority,
                "created_by": "agent",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z"
            });
            fs::write(
                issues_dir.join(format!("{uuid}.json")),
                serde_json::to_string_pretty(&issue).unwrap(),
            )
            .unwrap();
        }

        let reader = ExternalIssueReader::from_hub_dir(tmp.path()).unwrap();

        let open = reader.list_issues(Some("open"), None, None);
        assert_eq!(open.len(), 2);

        let closed = reader.list_issues(Some("closed"), None, None);
        assert_eq!(closed.len(), 1);

        let high = reader.list_issues(None, None, Some("high"));
        assert_eq!(high.len(), 1);
    }

    #[test]
    fn test_external_issue_reader_get_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let issues_dir = tmp.path().join("issues");
        fs::create_dir_all(&issues_dir).unwrap();

        let issue = serde_json::json!({
            "uuid": "00000000-0000-0000-0000-000000000042",
            "display_id": 42,
            "title": "The answer",
            "status": "open",
            "priority": "medium",
            "created_by": "agent",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-01T00:00:00Z"
        });
        fs::write(
            issues_dir.join("00000000-0000-0000-0000-000000000042.json"),
            serde_json::to_string_pretty(&issue).unwrap(),
        )
        .unwrap();

        let reader = ExternalIssueReader::from_hub_dir(tmp.path()).unwrap();
        let found = reader.get_issue(42);
        assert!(found.is_some());
        assert_eq!(found.unwrap().title, "The answer");

        assert!(reader.get_issue(999).is_none());
    }

    #[test]
    fn test_group_matches() {
        let indices = vec![2, 3, 10, 11, 20];
        let groups = group_matches(&indices, 1);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0], vec![2, 3]);
        assert_eq!(groups[1], vec![10, 11]);
        assert_eq!(groups[2], vec![20]);
    }

    #[test]
    fn test_external_knowledge_reader_search_sources() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("auth.md"),
            "---\ntitle: Auth\nsources:\n  - url: https://rust-lang.org/docs\n    title: Rust Docs\n---\n# Auth\n",
        )
        .unwrap();
        fs::write(
            tmp.path().join("api.md"),
            "---\ntitle: API\nsources:\n  - url: https://example.com\n    title: Example\n---\n# API\n",
        )
        .unwrap();

        let reader = ExternalKnowledgeReader::new(tmp.path().to_path_buf());
        let results = reader.search_sources("rust-lang.org").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].slug, "auth");
    }

    #[test]
    fn test_repo_alias_not_found() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("hook-config.json"), "{}").unwrap();

        let result = read_repo_alias(tmp.path(), "nonexistent");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown repo alias"));
    }

    #[test]
    fn test_repo_alias_found() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("hook-config.json"),
            r#"{"repo-alias": {"upstream": "github.com/org/repo"}}"#,
        )
        .unwrap();

        let result = read_repo_alias(tmp.path(), "upstream").unwrap();
        assert_eq!(result, "github.com/org/repo");
    }
}
