use anyhow::Result;

use super::core::{KnowledgeManager, PageInfo, SearchMatch};

impl KnowledgeManager {
    /// Search knowledge page content using word-level fuzzy matching.
    ///
    /// Tokenizes the query into words and matches lines containing any query
    /// term (case-insensitive). Results are ranked by the number of distinct
    /// query terms matched within each page — pages matching more terms appear
    /// first. Within a page, contiguous matching lines are grouped with
    /// surrounding context.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be read.
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
            .filter_map(std::result::Result::ok)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);

        // Collect (term_match_count, matches) per file for ranking
        let mut scored_results: Vec<(usize, Vec<SearchMatch>)> = Vec::new();

        for entry in entries {
            let path = entry.path();
            let slug = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let page_text = std::fs::read_to_string(&path)?;
            let lines: Vec<&str> = page_text.lines().collect();

            // Lowercase each line once and reuse for both term-hit counting
            // and per-line matching (avoids redundant lowercasing of the
            // entire content separately).
            let lines_lower: Vec<String> = lines.iter().map(|l| l.to_lowercase()).collect();

            // Count how many distinct query terms appear anywhere in this page
            let term_hits = terms
                .iter()
                .filter(|term| lines_lower.iter().any(|ll| ll.contains(**term)))
                .count();

            if term_hits == 0 {
                continue;
            }

            // Find lines matching any query term
            let matching_indices: Vec<usize> = lines_lower
                .iter()
                .enumerate()
                .filter(|(_, line_lower)| terms.iter().any(|term| line_lower.contains(term)))
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
        scored_results.sort_by_key(|b| std::cmp::Reverse(b.0));

        Ok(scored_results
            .into_iter()
            .flat_map(|(_, matches)| matches)
            .collect())
    }

    /// Search knowledge pages by source URL domain.
    ///
    /// Finds pages that have a source whose URL contains the given domain string.
    ///
    /// # Errors
    /// Returns an error if listing pages fails.
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
}

/// Group matching line indices into contiguous groups based on context overlap.
///
/// Two matches are in the same group if their context windows overlap or are
/// adjacent (i.e., the distance between them is <= 2 * context).
pub(super) fn group_matches(indices: &[usize], context: usize) -> Vec<Vec<usize>> {
    let mut groups: Vec<Vec<usize>> = Vec::new();

    for &idx in indices {
        let should_merge = groups
            .last()
            .and_then(|g| g.last())
            .is_some_and(|&last_idx| idx <= last_idx + 2 * context + 1);

        if should_merge {
            if let Some(last_group) = groups.last_mut() {
                last_group.push(idx);
            }
        } else {
            groups.push(vec![idx]);
        }
    }

    groups
}
