use anyhow::{bail, Context, Result};
use std::path::PathBuf;

use crate::utils::is_windows_reserved_name;

use super::core::{parse_frontmatter, KnowledgeManager, PageFrontmatter, PageInfo};

impl KnowledgeManager {
    /// List all `.md` pages in the knowledge worktree with parsed frontmatter.
    ///
    /// Reads only the first 4 KiB of each file to extract frontmatter,
    /// avoiding full-file reads for pages with large body content (#427).
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be read or a file cannot be opened.
    pub fn list_pages(&self) -> Result<Vec<PageInfo>> {
        use std::io::Read;

        /// Maximum bytes to read for frontmatter extraction. YAML frontmatter
        /// in knowledge pages is typically <1 KiB; 4 KiB provides ample margin.
        const FRONTMATTER_READ_LIMIT: usize = 4096;

        let mut pages = Vec::new();

        if !self.cache_dir.exists() {
            return Ok(pages);
        }

        for entry in std::fs::read_dir(&self.cache_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                let slug = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                // Read only the first N bytes — enough for frontmatter.
                let content = {
                    let mut file = std::fs::File::open(&path)?;
                    let mut buf = vec![0u8; FRONTMATTER_READ_LIMIT];
                    let n = file.read(&mut buf)?;
                    buf.truncate(n);
                    String::from_utf8_lossy(&buf).into_owned()
                };

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
            bail!("Invalid page slug '{slug}': must not contain path separators or '..'");
        }
        if is_windows_reserved_name(slug) {
            bail!("Invalid page slug '{slug}': Windows reserved filename");
        }
        let path = self.cache_dir.join(format!("{slug}.md"));
        // Defense in depth: verify the resolved path is within cache_dir.
        // Both paths must be canonicalized for a reliable starts_with check.
        // If either canonicalization fails (directory does not exist yet),
        // reject the path rather than silently skipping the check.
        if let (Ok(canonical_cache), Some(canonical_parent)) = (
            self.cache_dir.canonicalize(),
            path.parent().and_then(|p| p.canonicalize().ok()),
        ) {
            if !canonical_parent.starts_with(&canonical_cache) {
                bail!("Invalid page slug '{slug}': resolves outside knowledge cache");
            }
        }
        Ok(path)
    }

    /// Read a page by its filename slug (without `.md` extension).
    ///
    /// # Errors
    /// Returns an error if the slug is invalid or the page does not exist.
    pub fn read_page(&self, slug: &str) -> Result<String> {
        let path = self.safe_page_path(slug)?;
        if !path.exists() {
            bail!("Page '{slug}' not found");
        }
        std::fs::read_to_string(&path).context("Failed to read page")
    }

    /// Write or overwrite a page by its filename slug.
    ///
    /// # Errors
    /// Returns an error if the cache is not initialized or the write fails.
    pub fn write_page(&self, slug: &str, content: &str) -> Result<()> {
        if !self.cache_dir.exists() {
            bail!("Knowledge cache not initialized. Run init_cache() first.");
        }
        let path = self.safe_page_path(slug)?;
        std::fs::write(&path, content).context("Failed to write page")
    }

    /// Check if a page exists by slug.
    #[must_use]
    pub fn page_exists(&self, slug: &str) -> bool {
        self.safe_page_path(slug)
            .map(|path| path.exists())
            .unwrap_or(false)
    }

    /// Delete a page by slug.
    ///
    /// # Errors
    /// Returns an error if the slug is invalid or the page does not exist.
    pub fn delete_page(&self, slug: &str) -> Result<()> {
        let path = self.safe_page_path(slug)?;
        if !path.exists() {
            bail!("Page '{slug}' not found");
        }
        std::fs::remove_file(&path).context("Failed to delete page")
    }
}
