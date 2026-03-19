use anyhow::{bail, Context, Result};
use std::path::PathBuf;

use crate::utils::is_windows_reserved_name;

use super::core::{parse_frontmatter, KnowledgeManager, PageFrontmatter, PageInfo};

impl KnowledgeManager {
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
    pub(crate) fn safe_page_path(&self, slug: &str) -> Result<PathBuf> {
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
}
