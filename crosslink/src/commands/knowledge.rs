use anyhow::{bail, Result};
use chrono::Utc;
use std::path::Path;

use crate::knowledge::{
    parse_frontmatter, serialize_frontmatter, KnowledgeManager, PageFrontmatter, Source,
};

/// Get the current agent ID, falling back to "unknown".
fn current_agent_id(crosslink_dir: &Path) -> String {
    crate::identity::AgentConfig::load(crosslink_dir)
        .ok()
        .flatten()
        .map(|a| a.agent_id)
        .unwrap_or_else(|| "unknown".to_string())
}

/// Ensure the knowledge cache is initialized, creating it if needed.
fn ensure_initialized(km: &KnowledgeManager) -> Result<()> {
    if !km.is_initialized() {
        km.init_cache()?;
    }
    Ok(())
}

pub fn add(
    crosslink_dir: &Path,
    slug: &str,
    title: Option<&str>,
    tags: &[String],
    sources: &[String],
    content: Option<&str>,
) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    km.sync()?;

    if km.page_exists(slug) {
        bail!(
            "Page '{}' already exists. Use 'crosslink knowledge edit' instead.",
            slug
        );
    }

    let now = Utc::now().format("%Y-%m-%d").to_string();
    let display_title = title.unwrap_or(slug);
    let agent_id = current_agent_id(crosslink_dir);

    let parsed_sources: Vec<Source> = sources
        .iter()
        .map(|url| Source {
            url: url.clone(),
            title: String::new(),
            accessed_at: Some(now.clone()),
        })
        .collect();

    let fm = PageFrontmatter {
        title: display_title.to_string(),
        tags: tags.to_vec(),
        sources: parsed_sources,
        contributors: vec![agent_id],
        created: now.clone(),
        updated: now,
    };

    let mut page_content = serialize_frontmatter(&fm);
    page_content.push('\n');
    if let Some(body) = content {
        page_content.push_str(body);
        if !body.ends_with('\n') {
            page_content.push('\n');
        }
    } else {
        page_content.push_str(&format!("# {}\n", display_title));
    }

    km.write_page(slug, &page_content)?;
    km.commit(&format!("knowledge: add {}", slug))?;
    km.push()?;

    println!("Created knowledge page: {}", slug);
    Ok(())
}

pub fn show(crosslink_dir: &Path, slug: &str, json: bool) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    km.sync()?;

    let content = km.read_page(slug)?;

    if json {
        if let Some(fm) = parse_frontmatter(&content) {
            let json_obj = serde_json::json!({
                "slug": slug,
                "title": fm.title,
                "tags": fm.tags,
                "sources": fm.sources.iter().map(|s| {
                    let mut m = serde_json::Map::new();
                    m.insert("url".to_string(), serde_json::Value::String(s.url.clone()));
                    m.insert("title".to_string(), serde_json::Value::String(s.title.clone()));
                    if let Some(ref a) = s.accessed_at {
                        m.insert("accessed_at".to_string(), serde_json::Value::String(a.clone()));
                    }
                    serde_json::Value::Object(m)
                }).collect::<Vec<_>>(),
                "contributors": fm.contributors,
                "created": fm.created,
                "updated": fm.updated,
            });
            println!("{}", serde_json::to_string_pretty(&json_obj)?);
        } else {
            bail!("Page '{}' has no valid frontmatter", slug);
        }
    } else {
        print!("{}", content);
    }

    Ok(())
}

pub fn list(
    crosslink_dir: &Path,
    tag_filter: Option<&str>,
    contributor_filter: Option<&str>,
) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    km.sync()?;

    let pages = km.list_pages()?;

    let filtered: Vec<_> = pages
        .iter()
        .filter(|p| {
            if let Some(tag) = tag_filter {
                if !p.frontmatter.tags.iter().any(|t| t == tag) {
                    return false;
                }
            }
            if let Some(contributor) = contributor_filter {
                if !p.frontmatter.contributors.iter().any(|c| c == contributor) {
                    return false;
                }
            }
            true
        })
        .collect();

    if filtered.is_empty() {
        println!("No knowledge pages found.");
        return Ok(());
    }

    // Header
    println!("{:<30} {:<30} {:<20} UPDATED", "SLUG", "TITLE", "TAGS");
    println!("{}", "-".repeat(90));

    for page in &filtered {
        let tags_str = if page.frontmatter.tags.is_empty() {
            String::new()
        } else {
            page.frontmatter.tags.join(", ")
        };
        let updated = &page.frontmatter.updated;

        println!(
            "{:<30} {:<30} {:<20} {updated}",
            truncate(&page.slug, 28),
            truncate(&page.frontmatter.title, 28),
            truncate(&tags_str, 18),
        );
    }

    println!("\n{} page(s)", filtered.len());
    Ok(())
}

pub fn edit(
    crosslink_dir: &Path,
    slug: &str,
    append: Option<&str>,
    content: Option<&str>,
    tags: &[String],
    sources: &[String],
) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    km.sync()?;

    if !km.page_exists(slug) {
        bail!(
            "Page '{}' not found. Use 'crosslink knowledge add' to create it.",
            slug
        );
    }

    let existing = km.read_page(slug)?;
    let now = Utc::now().format("%Y-%m-%d").to_string();
    let agent_id = current_agent_id(crosslink_dir);

    let mut fm = parse_frontmatter(&existing).unwrap_or_else(|| PageFrontmatter {
        title: slug.to_string(),
        tags: Vec::new(),
        sources: Vec::new(),
        contributors: Vec::new(),
        created: now.clone(),
        updated: now.clone(),
    });

    // Update timestamp
    fm.updated = now.clone();

    // Add contributor if not already present
    if !fm.contributors.iter().any(|c| c == &agent_id) {
        fm.contributors.push(agent_id);
    }

    // Add new tags without duplicating
    for tag in tags {
        if !fm.tags.iter().any(|t| t == tag) {
            fm.tags.push(tag.clone());
        }
    }

    // Add new sources without duplicating
    for url in sources {
        if !fm.sources.iter().any(|s| s.url == *url) {
            fm.sources.push(Source {
                url: url.clone(),
                title: String::new(),
                accessed_at: Some(now.clone()),
            });
        }
    }

    // Determine the body
    let existing_body = extract_body(&existing);

    let new_body = if let Some(full_content) = content {
        // Replace content entirely
        let mut body = full_content.to_string();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body
    } else if let Some(append_text) = append {
        // Append to existing content
        let mut body = existing_body.to_string();
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body.push('\n');
        body.push_str(append_text);
        if !body.ends_with('\n') {
            body.push('\n');
        }
        body
    } else {
        existing_body.to_string()
    };

    let mut page_content = serialize_frontmatter(&fm);
    page_content.push('\n');
    page_content.push_str(&new_body);

    km.write_page(slug, &page_content)?;
    km.commit(&format!("knowledge: edit {}", slug))?;
    km.push()?;

    println!("Updated knowledge page: {}", slug);
    Ok(())
}

pub fn remove(crosslink_dir: &Path, slug: &str) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    km.sync()?;

    if !km.page_exists(slug) {
        bail!("Page '{}' not found", slug);
    }

    // Check for pages that reference this slug
    let pages = km.list_pages()?;
    let referencing: Vec<_> = pages
        .iter()
        .filter(|p| p.slug != slug)
        .filter(|p| {
            if let Ok(content) = km.read_page(&p.slug) {
                content.contains(slug)
            } else {
                false
            }
        })
        .collect();

    if !referencing.is_empty() {
        let slugs: Vec<_> = referencing.iter().map(|p| p.slug.as_str()).collect();
        eprintln!(
            "Warning: the following pages reference '{}': {}",
            slug,
            slugs.join(", ")
        );
    }

    km.delete_page(slug)?;
    km.commit(&format!("knowledge: remove {}", slug))?;
    km.push()?;

    println!("Removed knowledge page: {}", slug);
    Ok(())
}

pub fn sync(crosslink_dir: &Path) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    km.sync()?;

    println!("Knowledge cache synced.");
    Ok(())
}

/// Extract the body content after frontmatter.
fn extract_body(content: &str) -> &str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content;
    }
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);
    if let Some(end_idx) = after_first.find("\n---") {
        let after_closing = &after_first[end_idx + 4..];
        // Skip the newline after the closing ---
        after_closing.strip_prefix('\n').unwrap_or(after_closing)
    } else {
        content
    }
}

/// Truncate a string to a max length, adding "..." if truncated.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::{PageFrontmatter, Source, KNOWLEDGE_CACHE_DIR};
    use tempfile::tempdir;

    /// Create a KnowledgeManager with a pre-created cache directory (no git needed).
    fn setup_km() -> (KnowledgeManager, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
        std::fs::create_dir_all(&cache_dir).unwrap();

        let km = KnowledgeManager::new(&crosslink_dir).unwrap();
        (km, dir)
    }

    // ==================== extract_body Tests ====================

    #[test]
    fn test_extract_body_with_frontmatter() {
        let content = "---\ntitle: Test\ntags: []\n---\n\n# Test\n\nBody text.\n";
        let body = extract_body(content);
        assert_eq!(body, "\n# Test\n\nBody text.\n");
    }

    #[test]
    fn test_extract_body_no_frontmatter() {
        let content = "# Just a heading\n\nNo frontmatter.\n";
        let body = extract_body(content);
        assert_eq!(body, content);
    }

    // ==================== truncate Tests ====================

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world foo bar", 10), "hello w...");
    }

    // ==================== add Tests ====================

    #[test]
    fn test_add_creates_file_with_correct_frontmatter() {
        let (km, dir) = setup_km();
        let crosslink_dir = dir.path().join(".crosslink");

        let tags = vec!["rust".to_string(), "testing".to_string()];
        let sources = vec!["https://example.com".to_string()];

        // Call add directly without git operations - write the page manually
        let now = Utc::now().format("%Y-%m-%d").to_string();
        let fm = PageFrontmatter {
            title: "Rust Testing Patterns".to_string(),
            tags: tags.clone(),
            sources: sources
                .iter()
                .map(|url| Source {
                    url: url.clone(),
                    title: String::new(),
                    accessed_at: Some(now.clone()),
                })
                .collect(),
            contributors: vec![current_agent_id(&crosslink_dir)],
            created: now.clone(),
            updated: now,
        };

        let mut page_content = serialize_frontmatter(&fm);
        page_content.push_str("\n# Rust Testing Patterns\n");
        km.write_page("rust-testing-patterns", &page_content)
            .unwrap();

        // Verify
        let read_back = km.read_page("rust-testing-patterns").unwrap();
        let parsed = parse_frontmatter(&read_back).unwrap();
        assert_eq!(parsed.title, "Rust Testing Patterns");
        assert_eq!(parsed.tags, vec!["rust", "testing"]);
        assert_eq!(parsed.sources.len(), 1);
        assert_eq!(parsed.sources[0].url, "https://example.com");
    }

    #[test]
    fn test_add_with_content() {
        let (km, _dir) = setup_km();

        let now = Utc::now().format("%Y-%m-%d").to_string();
        let fm = PageFrontmatter {
            title: "Test".to_string(),
            tags: Vec::new(),
            sources: Vec::new(),
            contributors: vec!["test-agent".to_string()],
            created: now.clone(),
            updated: now,
        };

        let mut page_content = serialize_frontmatter(&fm);
        page_content.push_str("\nCustom body content\n");
        km.write_page("test-page", &page_content).unwrap();

        let read_back = km.read_page("test-page").unwrap();
        assert!(read_back.contains("Custom body content"));
    }

    // ==================== show Tests ====================

    #[test]
    fn test_show_displays_content() {
        let (km, _dir) = setup_km();

        let content =
            "---\ntitle: Demo\ntags: [demo]\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\n# Demo\n\nSome text.\n";
        km.write_page("demo", content).unwrap();

        let read = km.read_page("demo").unwrap();
        assert_eq!(read, content);

        let fm = parse_frontmatter(&read).unwrap();
        assert_eq!(fm.title, "Demo");
    }

    // ==================== list Tests ====================

    #[test]
    fn test_list_filters_by_tag() {
        let (km, _dir) = setup_km();

        let page_a = "---\ntitle: Alpha\ntags: [rust]\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nA\n";
        let page_b = "---\ntitle: Beta\ntags: [python]\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nB\n";

        km.write_page("alpha", page_a).unwrap();
        km.write_page("beta", page_b).unwrap();

        let pages = km.list_pages().unwrap();

        // Filter for rust tag
        let rust_pages: Vec<_> = pages
            .iter()
            .filter(|p| p.frontmatter.tags.iter().any(|t| t == "rust"))
            .collect();
        assert_eq!(rust_pages.len(), 1);
        assert_eq!(rust_pages[0].slug, "alpha");

        // Filter for python tag
        let python_pages: Vec<_> = pages
            .iter()
            .filter(|p| p.frontmatter.tags.iter().any(|t| t == "python"))
            .collect();
        assert_eq!(python_pages.len(), 1);
        assert_eq!(python_pages[0].slug, "beta");
    }

    #[test]
    fn test_list_filters_by_contributor() {
        let (km, _dir) = setup_km();

        let page_a = "---\ntitle: Alpha\ntags: []\nsources: []\ncontributors: [alice]\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nA\n";
        let page_b = "---\ntitle: Beta\ntags: []\nsources: []\ncontributors: [bob]\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nB\n";

        km.write_page("alpha", page_a).unwrap();
        km.write_page("beta", page_b).unwrap();

        let pages = km.list_pages().unwrap();

        let alice_pages: Vec<_> = pages
            .iter()
            .filter(|p| p.frontmatter.contributors.iter().any(|c| c == "alice"))
            .collect();
        assert_eq!(alice_pages.len(), 1);
        assert_eq!(alice_pages[0].slug, "alpha");
    }

    // ==================== edit Tests ====================

    #[test]
    fn test_edit_appends_content_and_updates_metadata() {
        let (km, _dir) = setup_km();

        let original = "---\ntitle: Test\ntags: [rust]\nsources: []\ncontributors: [alice]\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\n# Test\n\nOriginal content.\n";
        km.write_page("test-page", original).unwrap();

        // Simulate edit: parse, modify, rewrite
        let existing = km.read_page("test-page").unwrap();
        let mut fm = parse_frontmatter(&existing).unwrap();
        let now = Utc::now().format("%Y-%m-%d").to_string();
        fm.updated = now;

        if !fm.contributors.iter().any(|c| c == "bob") {
            fm.contributors.push("bob".to_string());
        }

        let existing_body = extract_body(&existing);
        let mut body = existing_body.to_string();
        body.push_str("\n## Appended Section\n\nNew content.\n");

        let mut page_content = serialize_frontmatter(&fm);
        page_content.push('\n');
        page_content.push_str(&body);
        km.write_page("test-page", &page_content).unwrap();

        // Verify
        let updated = km.read_page("test-page").unwrap();
        assert!(updated.contains("Original content."));
        assert!(updated.contains("Appended Section"));
        assert!(updated.contains("New content."));

        let updated_fm = parse_frontmatter(&updated).unwrap();
        assert!(updated_fm.contributors.contains(&"alice".to_string()));
        assert!(updated_fm.contributors.contains(&"bob".to_string()));
    }

    #[test]
    fn test_edit_adds_source_without_duplicating() {
        let (km, _dir) = setup_km();

        let original = "---\ntitle: Test\ntags: []\nsources:\n  - url: https://existing.com\n    title: Existing\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nBody.\n";
        km.write_page("test-page", original).unwrap();

        let existing = km.read_page("test-page").unwrap();
        let mut fm = parse_frontmatter(&existing).unwrap();

        // Add same source - should not duplicate
        let existing_url = "https://existing.com";
        if !fm.sources.iter().any(|s| s.url == existing_url) {
            fm.sources.push(Source {
                url: existing_url.to_string(),
                title: String::new(),
                accessed_at: None,
            });
        }
        assert_eq!(fm.sources.len(), 1);

        // Add new source - should be added
        let new_url = "https://new.com";
        if !fm.sources.iter().any(|s| s.url == new_url) {
            fm.sources.push(Source {
                url: new_url.to_string(),
                title: String::new(),
                accessed_at: None,
            });
        }
        assert_eq!(fm.sources.len(), 2);
        assert_eq!(fm.sources[0].url, "https://existing.com");
        assert_eq!(fm.sources[1].url, "https://new.com");
    }

    // ==================== remove Tests ====================

    #[test]
    fn test_remove_deletes_page() {
        let (km, _dir) = setup_km();

        let content = "---\ntitle: Temp\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nTemp page.\n";
        km.write_page("temp-page", content).unwrap();
        assert!(km.page_exists("temp-page"));

        km.delete_page("temp-page").unwrap();
        assert!(!km.page_exists("temp-page"));
    }

    #[test]
    fn test_remove_nonexistent_fails() {
        let (km, _dir) = setup_km();

        let result = km.delete_page("nonexistent");
        assert!(result.is_err());
    }

    #[test]
    fn test_remove_warns_about_broken_links() {
        let (km, _dir) = setup_km();

        let target = "---\ntitle: Target\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nTarget page.\n";
        let referencing = "---\ntitle: Referencing\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nSee target-page for details.\n";

        km.write_page("target-page", target).unwrap();
        km.write_page("referencing-page", referencing).unwrap();

        // Check that the referencing page mentions the target slug
        let pages = km.list_pages().unwrap();
        let referencing_pages: Vec<_> = pages
            .iter()
            .filter(|p| p.slug != "target-page")
            .filter(|p| {
                if let Ok(content) = km.read_page(&p.slug) {
                    content.contains("target-page")
                } else {
                    false
                }
            })
            .collect();
        assert_eq!(referencing_pages.len(), 1);
        assert_eq!(referencing_pages[0].slug, "referencing-page");
    }

    // ==================== page_exists / delete_page Tests ====================

    #[test]
    fn test_page_exists() {
        let (km, _dir) = setup_km();

        assert!(!km.page_exists("nope"));

        km.write_page("exists", "content").unwrap();
        assert!(km.page_exists("exists"));
    }

    #[test]
    fn test_delete_page() {
        let (km, _dir) = setup_km();

        km.write_page("to-delete", "content").unwrap();
        assert!(km.page_exists("to-delete"));

        km.delete_page("to-delete").unwrap();
        assert!(!km.page_exists("to-delete"));
    }
}

/// Run knowledge search: content search, source search, or both.
pub fn search(
    crosslink_dir: &Path,
    query: Option<&str>,
    context: usize,
    source: Option<&str>,
    json: bool,
) -> Result<()> {
    if query.is_none() && source.is_none() {
        bail!("Provide a search query or --source domain");
    }

    let manager = KnowledgeManager::new(crosslink_dir)?;

    if !manager.is_initialized() {
        if json {
            println!("[]");
        } else {
            println!("Knowledge cache not initialized. Run 'crosslink knowledge init' or add a page first.");
        }
        return Ok(());
    }

    if let Some(domain) = source {
        return search_sources(&manager, domain, json);
    }

    let Some(query) = query else {
        bail!("Provide a search query or --source domain");
    };
    let matches = manager.search_content(query, context)?;

    if json {
        print_content_json(&matches);
        return Ok(());
    }

    if matches.is_empty() {
        println!(
            "No knowledge pages match \"{}\". Consider adding one.",
            query
        );
        return Ok(());
    }

    for (i, m) in matches.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("{}.md (line {}):", m.slug, m.line_number);
        for (line_num, line) in &m.context_lines {
            println!("  {:>4} | {}", line_num, line);
        }
    }

    Ok(())
}

fn search_sources(manager: &KnowledgeManager, domain: &str, json: bool) -> Result<()> {
    let matches = manager.search_sources(domain)?;

    if json {
        print_sources_json(&matches);
        return Ok(());
    }

    if matches.is_empty() {
        println!(
            "No knowledge pages cite \"{}\". Consider adding one.",
            domain
        );
        return Ok(());
    }

    for page in &matches {
        let matching_sources: Vec<&crate::knowledge::Source> = page
            .frontmatter
            .sources
            .iter()
            .filter(|src| src.url.to_lowercase().contains(&domain.to_lowercase()))
            .collect();

        println!("{}.md — {}", page.slug, page.frontmatter.title);
        for src in matching_sources {
            print!("  {} ({})", src.url, src.title);
            if let Some(ref accessed) = src.accessed_at {
                print!(" [accessed: {}]", accessed);
            }
            println!();
        }
    }

    Ok(())
}

fn print_content_json(matches: &[crate::knowledge::SearchMatch]) {
    let entries: Vec<String> = matches
        .iter()
        .map(|m| {
            let lines: Vec<String> = m
                .context_lines
                .iter()
                .map(|(num, text)| {
                    format!("{{\"line\":{},\"text\":{}}}", num, serde_json_string(text))
                })
                .collect();
            format!(
                "{{\"slug\":{},\"line_number\":{},\"context\":[{}]}}",
                serde_json_string(&m.slug),
                m.line_number,
                lines.join(",")
            )
        })
        .collect();
    println!("[{}]", entries.join(","));
}

fn print_sources_json(pages: &[crate::knowledge::PageInfo]) {
    let entries: Vec<String> = pages
        .iter()
        .map(|page| {
            let sources: Vec<String> = page
                .frontmatter
                .sources
                .iter()
                .map(|src| {
                    let accessed = match &src.accessed_at {
                        Some(a) => serde_json_string(a),
                        None => "null".to_string(),
                    };
                    format!(
                        "{{\"url\":{},\"title\":{},\"accessed_at\":{}}}",
                        serde_json_string(&src.url),
                        serde_json_string(&src.title),
                        accessed
                    )
                })
                .collect();
            format!(
                "{{\"slug\":{},\"title\":{},\"sources\":[{}]}}",
                serde_json_string(&page.slug),
                serde_json_string(&page.frontmatter.title),
                sources.join(",")
            )
        })
        .collect();
    println!("[{}]", entries.join(","));
}

/// Minimal JSON string escaping without pulling in serde.
fn serde_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
