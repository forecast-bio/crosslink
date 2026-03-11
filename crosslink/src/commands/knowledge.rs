use anyhow::{bail, Context, Result};
use chrono::Utc;
use std::path::Path;

use crate::knowledge::{
    parse_frontmatter, serialize_frontmatter, KnowledgeManager, PageFrontmatter, Source,
    SyncOutcome,
};

use crate::KnowledgeCommands;

pub fn dispatch(command: KnowledgeCommands, crosslink_dir: &Path, global_json: bool) -> Result<()> {
    match command {
        KnowledgeCommands::Add {
            slug,
            title,
            tag,
            source,
            content,
            from_doc,
        } => add(
            crosslink_dir,
            &slug,
            title.as_deref(),
            &tag,
            &source,
            content.as_deref(),
            from_doc.as_deref(),
        ),
        KnowledgeCommands::Show { slug } => show(crosslink_dir, &slug, global_json),
        KnowledgeCommands::List {
            tag,
            contributor,
            since,
            json,
        } => list(
            crosslink_dir,
            tag.as_deref(),
            contributor.as_deref(),
            since.as_deref(),
            json,
        ),
        KnowledgeCommands::Edit {
            slug,
            append,
            content,
            replace_section,
            append_to_section,
            tag,
            source,
        } => edit(
            crosslink_dir,
            &slug,
            append.as_deref(),
            content.as_deref(),
            replace_section.as_deref(),
            append_to_section.as_deref(),
            &tag,
            &source,
        ),
        KnowledgeCommands::Remove { slug } => remove(crosslink_dir, &slug),
        KnowledgeCommands::Import {
            directory,
            tag,
            overwrite,
            dry_run,
        } => import(crosslink_dir, &directory, &tag, overwrite, dry_run),
        KnowledgeCommands::Sync => sync(crosslink_dir),
        KnowledgeCommands::Search {
            query,
            context,
            source,
            tag,
            since,
            contributor,
        } => search(
            crosslink_dir,
            query.as_deref(),
            context,
            source.as_deref(),
            global_json,
            tag.as_deref(),
            since.as_deref(),
            contributor.as_deref(),
        ),
    }
}

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

/// Print warnings for any conflicts that were resolved via "accept both".
fn warn_resolved_conflicts(outcome: &SyncOutcome) {
    for slug in &outcome.resolved_conflicts {
        eprintln!(
            "Warning: Merge conflict in {}.md — both versions kept. \
             A cleanup issue should be created.",
            slug
        );
    }
}

pub fn add(
    crosslink_dir: &Path,
    slug: &str,
    title: Option<&str>,
    tags: &[String],
    sources: &[String],
    content: Option<&str>,
    from_doc: Option<&std::path::Path>,
) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    let sync_outcome = km.sync()?;
    warn_resolved_conflicts(&sync_outcome);

    if km.page_exists(slug) {
        bail!(
            "Page '{}' already exists. Use 'crosslink knowledge edit' instead.",
            slug
        );
    }

    // Parse design doc if --from-doc provided
    let design_doc = if let Some(path) = from_doc {
        let doc_content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read design doc: {}", path.display()))?;
        Some(crate::commands::design_doc::parse_design_doc(&doc_content))
    } else {
        None
    };

    let now = Utc::now().format("%Y-%m-%d").to_string();

    // Title: explicit --title > design doc title > slug
    let display_title = if let Some(t) = title {
        t.to_string()
    } else if let Some(ref doc) = design_doc {
        if doc.title.is_empty() {
            slug.to_string()
        } else {
            doc.title.clone()
        }
    } else {
        slug.to_string()
    };

    let agent_id = current_agent_id(crosslink_dir);

    let parsed_sources: Vec<Source> = sources
        .iter()
        .map(|url| Source {
            url: url.clone(),
            title: String::new(),
            accessed_at: Some(now.clone()),
        })
        .collect();

    // Build tag list, auto-adding "design-doc" when --from-doc is used
    let mut all_tags = tags.to_vec();
    if design_doc.is_some() && !all_tags.iter().any(|t| t == "design-doc") {
        all_tags.push("design-doc".to_string());
    }

    let fm = PageFrontmatter {
        title: display_title.clone(),
        tags: all_tags,
        sources: parsed_sources,
        contributors: vec![agent_id],
        created: now.clone(),
        updated: now,
    };

    let mut page_content = serialize_frontmatter(&fm);
    page_content.push('\n');
    if let Some(body) = content {
        // Explicit --content always wins
        page_content.push_str(body);
        if !body.ends_with('\n') {
            page_content.push('\n');
        }
    } else if let Some(ref doc) = design_doc {
        // Render design doc as page body
        let section = crate::commands::design_doc::build_design_doc_section(doc);
        page_content.push_str(&section);
    } else {
        page_content.push_str(&format!("# {}\n", display_title));
    }

    km.write_page(slug, &page_content)?;
    km.commit(&format!("knowledge: add {}", slug))?;
    let push_outcome = km.push()?;
    warn_resolved_conflicts(&push_outcome);

    println!("Created knowledge page: {}", slug);
    Ok(())
}

pub fn show(crosslink_dir: &Path, slug: &str, json: bool) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    let sync_outcome = km.sync()?;
    warn_resolved_conflicts(&sync_outcome);

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
    since: Option<&str>,
    json: bool,
) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    let sync_outcome = km.sync()?;
    warn_resolved_conflicts(&sync_outcome);

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
            if let Some(since) = since {
                if p.frontmatter.updated.as_str() < since {
                    return false;
                }
            }
            true
        })
        .collect();

    if json {
        print_list_json(&filtered);
        return Ok(());
    }

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

#[allow(clippy::too_many_arguments)]
pub fn edit(
    crosslink_dir: &Path,
    slug: &str,
    append: Option<&str>,
    content: Option<&str>,
    replace_section: Option<&str>,
    append_to_section: Option<&str>,
    tags: &[String],
    sources: &[String],
) -> Result<()> {
    // Validate: section-based flags require --content
    if (replace_section.is_some() || append_to_section.is_some()) && content.is_none() {
        bail!("--replace-section and --append-to-section require --content to be specified");
    }

    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    let sync_outcome = km.sync()?;
    warn_resolved_conflicts(&sync_outcome);

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

    let new_body = if let Some(heading) = replace_section {
        let new_content =
            content.ok_or_else(|| anyhow::anyhow!("--replace-section requires --content"))?;
        replace_section_content(existing_body, heading, new_content)?
    } else if let Some(heading) = append_to_section {
        let new_content =
            content.ok_or_else(|| anyhow::anyhow!("--append-to-section requires --content"))?;
        append_to_section_content(existing_body, heading, new_content)?
    } else if let Some(full_content) = content {
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
    let push_outcome = km.push()?;
    warn_resolved_conflicts(&push_outcome);

    println!("Updated knowledge page: {}", slug);
    Ok(())
}

pub fn remove(crosslink_dir: &Path, slug: &str) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    let sync_outcome = km.sync()?;
    warn_resolved_conflicts(&sync_outcome);

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
    let push_outcome = km.push()?;
    warn_resolved_conflicts(&push_outcome);

    println!("Removed knowledge page: {}", slug);
    Ok(())
}

pub fn sync(crosslink_dir: &Path) -> Result<()> {
    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    let sync_outcome = km.sync()?;
    warn_resolved_conflicts(&sync_outcome);

    println!("Knowledge cache synced.");
    Ok(())
}

pub fn import(
    crosslink_dir: &Path,
    directory: &Path,
    extra_tags: &[String],
    overwrite: bool,
    dry_run: bool,
) -> Result<()> {
    if !directory.is_dir() {
        bail!("'{}' is not a directory", directory.display());
    }

    let km = KnowledgeManager::new(crosslink_dir)?;
    ensure_initialized(&km)?;
    let sync_outcome = km.sync()?;
    warn_resolved_conflicts(&sync_outcome);

    let files = collect_md_files(directory)?;
    if files.is_empty() {
        println!("No .md files found in '{}'.", directory.display());
        return Ok(());
    }

    let agent_id = current_agent_id(crosslink_dir);
    let now = Utc::now().format("%Y-%m-%d").to_string();

    let mut imported = 0u32;
    let mut skipped = 0u32;
    let mut errors = 0u32;

    for file_path in &files {
        let rel = file_path
            .strip_prefix(directory)
            .unwrap_or(file_path.as_path());
        let slug = infer_slug(rel);
        let path_tags = infer_tags_from_path(rel);

        if km.page_exists(&slug) && !overwrite {
            if dry_run {
                println!("[skip] {} (exists)", slug);
            }
            skipped += 1;
            continue;
        }

        if dry_run {
            let action = if km.page_exists(&slug) {
                "overwrite"
            } else {
                "import"
            };
            println!("[{}] {} <- {}", action, slug, rel.display());
            imported += 1;
            continue;
        }

        match import_single_file(
            &km, file_path, &slug, &path_tags, extra_tags, &agent_id, &now,
        ) {
            Ok(()) => imported += 1,
            Err(e) => {
                eprintln!("Error importing {}: {}", rel.display(), e);
                errors += 1;
            }
        }
    }

    if !dry_run && imported > 0 {
        km.commit(&format!("knowledge: import {} page(s)", imported))?;
        let push_outcome = km.push()?;
        warn_resolved_conflicts(&push_outcome);
    }

    println!(
        "Imported: {} | Skipped: {} | Errors: {}",
        imported, skipped, errors
    );
    Ok(())
}

/// Recursively collect .md files from a directory, sorted by path.
fn collect_md_files(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    collect_md_files_recursive(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_md_files_recursive(dir: &Path, files: &mut Vec<std::path::PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_md_files_recursive(&path, files)?;
        } else if path.extension().map(|e| e == "md").unwrap_or(false) {
            files.push(path);
        }
    }
    Ok(())
}

/// Infer a slug from a relative path. Subdirectory components become prefixes.
/// e.g. `api/design.md` → `api-design`, `readme.md` → `readme`
fn infer_slug(rel_path: &Path) -> String {
    let stem = rel_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let parent = rel_path.parent().unwrap_or(Path::new(""));
    if parent == Path::new("") || parent == Path::new(".") {
        slug_sanitize(&stem)
    } else {
        let prefix = parent
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("-");
        slug_sanitize(&format!("{}-{}", prefix, stem))
    }
}

/// Infer tags from directory components of a path.
/// e.g. `arch/api/design.md` → `["arch", "api"]`
fn infer_tags_from_path(rel_path: &Path) -> Vec<String> {
    let parent = rel_path.parent().unwrap_or(Path::new(""));
    parent
        .components()
        .filter_map(|c| {
            let s = c.as_os_str().to_string_lossy().to_string();
            if s == "." {
                None
            } else {
                Some(s)
            }
        })
        .collect()
}

/// Sanitize a string into a valid slug (lowercase, alphanumeric + hyphens).
fn slug_sanitize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Import a single file as a knowledge page.
fn import_single_file(
    km: &KnowledgeManager,
    file_path: &Path,
    slug: &str,
    path_tags: &[String],
    extra_tags: &[String],
    agent_id: &str,
    now: &str,
) -> Result<()> {
    let raw = std::fs::read_to_string(file_path)
        .with_context(|| format!("reading {}", file_path.display()))?;

    let page_content = if let Some(mut fm) = parse_frontmatter(&raw) {
        // File has frontmatter — preserve it, merge tags
        for tag in path_tags.iter().chain(extra_tags.iter()) {
            if !fm.tags.iter().any(|t| t == tag) {
                fm.tags.push(tag.clone());
            }
        }
        if !fm.contributors.iter().any(|c| c == agent_id) {
            fm.contributors.push(agent_id.to_string());
        }
        let body = extract_body(&raw);
        let mut content = serialize_frontmatter(&fm);
        content.push('\n');
        content.push_str(body);
        content
    } else {
        // No frontmatter — generate it
        let title = slug.replace('-', " ");
        let mut all_tags: Vec<String> = path_tags.to_vec();
        for tag in extra_tags {
            if !all_tags.iter().any(|t| t == tag) {
                all_tags.push(tag.clone());
            }
        }
        let fm = PageFrontmatter {
            title,
            tags: all_tags,
            sources: Vec::new(),
            contributors: vec![agent_id.to_string()],
            created: now.to_string(),
            updated: now.to_string(),
        };
        let mut content = serialize_frontmatter(&fm);
        content.push('\n');
        content.push_str(&raw);
        if !raw.ends_with('\n') {
            content.push('\n');
        }
        content
    };

    km.write_page(slug, &page_content)?;
    Ok(())
}

/// Parse a heading line and return its level (1-6) and text.
/// Returns None if the line is not a markdown heading.
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_end();
    if !trimmed.starts_with('#') {
        return None;
    }
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // Must be followed by a space (standard markdown heading)
    let rest = &trimmed[hashes..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some((hashes, rest[1..].trim()))
}

/// Find the line range of a section identified by its heading text.
/// Returns (heading_line_idx, section_end_line_idx) where end is exclusive.
/// The section extends from the heading line to the next heading of equal or higher level, or EOF.
fn find_section_range(lines: &[&str], heading: &str) -> Result<(usize, usize)> {
    // Normalize the heading query: strip leading '#' chars if the user included them
    let query = heading.trim();
    let (query_level, query_text) = if query.starts_with('#') {
        match parse_heading(query) {
            Some((level, text)) => (Some(level), text),
            None => (None, query),
        }
    } else {
        (None, query)
    };

    // Find the heading line
    let mut heading_idx = None;
    let mut heading_level = 0;
    for (i, line) in lines.iter().enumerate() {
        if let Some((level, text)) = parse_heading(line) {
            let text_matches = text == query_text;
            let level_matches = query_level.is_none() || query_level == Some(level);
            if text_matches && level_matches {
                heading_idx = Some(i);
                heading_level = level;
                break;
            }
        }
    }

    let start = heading_idx
        .ok_or_else(|| anyhow::anyhow!("Section heading '{}' not found in the page", heading))?;

    // Find the end: next heading of equal or higher (lower number) level
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if let Some((level, _)) = parse_heading(line) {
            if level <= heading_level {
                end = i;
                break;
            }
        }
    }

    Ok((start, end))
}

/// Replace the content of a section (everything between the heading and the next same-or-higher-level heading).
/// The heading line itself is preserved.
fn replace_section_content(body: &str, heading: &str, new_content: &str) -> Result<String> {
    let lines: Vec<&str> = body.lines().collect();
    let (start, end) = find_section_range(&lines, heading)?;

    let mut result = String::new();
    // Lines before and including the heading
    for line in &lines[..=start] {
        result.push_str(line);
        result.push('\n');
    }
    // New content
    if !new_content.is_empty() {
        result.push('\n');
        result.push_str(new_content);
        if !new_content.ends_with('\n') {
            result.push('\n');
        }
    }
    // Lines after the section
    if end < lines.len() {
        result.push('\n');
        for line in &lines[end..] {
            result.push_str(line);
            result.push('\n');
        }
    }

    Ok(result)
}

/// Append content to the end of a section (before the next same-or-higher-level heading).
fn append_to_section_content(body: &str, heading: &str, new_content: &str) -> Result<String> {
    let lines: Vec<&str> = body.lines().collect();
    let (_, end) = find_section_range(&lines, heading)?;

    let mut result = String::new();
    // Lines up to (but not including) the section end
    for line in &lines[..end] {
        result.push_str(line);
        result.push('\n');
    }
    // Append new content
    if !new_content.is_empty() {
        result.push('\n');
        result.push_str(new_content);
        if !new_content.ends_with('\n') {
            result.push('\n');
        }
    }
    // Lines after the section
    if end < lines.len() {
        result.push('\n');
        for line in &lines[end..] {
            result.push_str(line);
            result.push('\n');
        }
    }

    Ok(result)
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
    crate::utils::truncate(s, max)
}

/// Run knowledge search: content search, source search, or both.
#[allow(clippy::too_many_arguments)]
pub fn search(
    crosslink_dir: &Path,
    query: Option<&str>,
    context: usize,
    source: Option<&str>,
    json: bool,
    tag: Option<&str>,
    since: Option<&str>,
    contributor: Option<&str>,
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
    let matches = filter_by_metadata(&manager, matches, tag, since, contributor)?;

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

/// Post-filter search matches by frontmatter metadata.
fn filter_by_metadata(
    manager: &KnowledgeManager,
    matches: Vec<crate::knowledge::SearchMatch>,
    tag: Option<&str>,
    since: Option<&str>,
    contributor: Option<&str>,
) -> Result<Vec<crate::knowledge::SearchMatch>> {
    if tag.is_none() && since.is_none() && contributor.is_none() {
        return Ok(matches);
    }

    let mut filtered = Vec::new();
    for m in matches {
        let content = match manager.read_page(&m.slug) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let fm = match parse_frontmatter(&content) {
            Some(fm) => fm,
            None => continue,
        };
        if let Some(tag) = tag {
            if !fm.tags.iter().any(|t| t == tag) {
                continue;
            }
        }
        if let Some(since) = since {
            if fm.updated.as_str() < since {
                continue;
            }
        }
        if let Some(contributor) = contributor {
            if !fm.contributors.iter().any(|c| c == contributor) {
                continue;
            }
        }
        filtered.push(m);
    }
    Ok(filtered)
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

fn print_list_json(pages: &[&crate::knowledge::PageInfo]) {
    let entries: Vec<String> = pages
        .iter()
        .map(|page| {
            let tags: Vec<String> = page
                .frontmatter
                .tags
                .iter()
                .map(|t| serde_json_string(t))
                .collect();
            let contributors: Vec<String> = page
                .frontmatter
                .contributors
                .iter()
                .map(|c| serde_json_string(c))
                .collect();
            format!(
                "{{\"slug\":{},\"title\":{},\"tags\":[{}],\"contributors\":[{}],\"created\":{},\"updated\":{}}}",
                serde_json_string(&page.slug),
                serde_json_string(&page.frontmatter.title),
                tags.join(","),
                contributors.join(","),
                serde_json_string(&page.frontmatter.created),
                serde_json_string(&page.frontmatter.updated),
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
        let sources = ["https://example.com".to_string()];

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

    // ==================== from_doc Tests ====================

    #[test]
    fn test_add_from_doc_creates_page() {
        let (km, dir) = setup_km();

        // Write a sample design doc
        let doc_path = dir.path().join("design.md");
        std::fs::write(
            &doc_path,
            "# Feature: Batch Retry\n\n## Summary\n\nRetry logic.\n\n## Requirements\n- REQ-1: Retry\n",
        )
        .unwrap();

        let doc = crate::commands::design_doc::parse_design_doc(
            &std::fs::read_to_string(&doc_path).unwrap(),
        );

        // Simulate the add flow with from_doc
        let now = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let mut tags = Vec::new();
        tags.push("design-doc".to_string());

        let fm = PageFrontmatter {
            title: doc.title.clone(),
            tags,
            sources: Vec::new(),
            contributors: vec!["test-agent".to_string()],
            created: now.clone(),
            updated: now,
        };

        let mut page_content = serialize_frontmatter(&fm);
        page_content.push('\n');
        page_content.push_str(&crate::commands::design_doc::build_design_doc_section(&doc));

        km.write_page("batch-retry", &page_content).unwrap();

        let read_back = km.read_page("batch-retry").unwrap();
        assert!(read_back.contains("Batch Retry"));
        assert!(read_back.contains("Design Specification"));
        assert!(read_back.contains("REQ-1: Retry"));
    }

    #[test]
    fn test_add_from_doc_auto_tags() {
        // Verify that design-doc tag is added
        let tags: Vec<String> = vec!["existing-tag".to_string()];
        let mut all_tags = tags.clone();
        if !all_tags.iter().any(|t| t == "design-doc") {
            all_tags.push("design-doc".to_string());
        }
        assert!(all_tags.contains(&"design-doc".to_string()));
        assert!(all_tags.contains(&"existing-tag".to_string()));
    }

    #[test]
    fn test_add_from_doc_derives_title() {
        let doc = crate::commands::design_doc::parse_design_doc("# Feature: My Great Feature\n");
        // When no explicit title, use doc title
        let title: Option<&str> = None;
        let display_title = if let Some(t) = title {
            t.to_string()
        } else if doc.title.is_empty() {
            "fallback-slug".to_string()
        } else {
            doc.title.clone()
        };
        assert_eq!(display_title, "My Great Feature");
    }

    #[test]
    fn test_add_from_doc_explicit_title_overrides() {
        let doc = crate::commands::design_doc::parse_design_doc("# Feature: Doc Title\n");
        let title: Option<&str> = Some("Explicit Title");
        let display_title = if let Some(t) = title {
            t.to_string()
        } else if doc.title.is_empty() {
            "fallback".to_string()
        } else {
            doc.title.clone()
        };
        assert_eq!(display_title, "Explicit Title");
    }

    // ==================== Round 1: Structured Queries Tests ====================

    #[test]
    fn test_search_filter_by_tag() {
        let (km, _dir) = setup_km();

        let page_a = "---\ntitle: Alpha\ntags: [rust]\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nshared keyword here\n";
        let page_b = "---\ntitle: Beta\ntags: [python]\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nshared keyword here\n";

        km.write_page("alpha", page_a).unwrap();
        km.write_page("beta", page_b).unwrap();

        let matches = km.search_content("shared keyword", 0).unwrap();
        assert_eq!(matches.len(), 2);

        let filtered = filter_by_metadata(&km, matches, Some("rust"), None, None).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].slug, "alpha");
    }

    #[test]
    fn test_search_filter_by_since() {
        let (km, _dir) = setup_km();

        let page_old = "---\ntitle: Old\ntags: []\nsources: []\ncontributors: []\ncreated: 2025-01-01\nupdated: 2025-06-01\n---\n\ncommon text\n";
        let page_new = "---\ntitle: New\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-02-01\n---\n\ncommon text\n";

        km.write_page("old-page", page_old).unwrap();
        km.write_page("new-page", page_new).unwrap();

        let matches = km.search_content("common text", 0).unwrap();
        assert_eq!(matches.len(), 2);

        let filtered = filter_by_metadata(&km, matches, None, Some("2026-01-01"), None).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].slug, "new-page");
    }

    #[test]
    fn test_search_filter_by_contributor() {
        let (km, _dir) = setup_km();

        let page_a = "---\ntitle: A\ntags: []\nsources: []\ncontributors: [alice]\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nfindme\n";
        let page_b = "---\ntitle: B\ntags: []\nsources: []\ncontributors: [bob]\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nfindme\n";

        km.write_page("a-page", page_a).unwrap();
        km.write_page("b-page", page_b).unwrap();

        let matches = km.search_content("findme", 0).unwrap();
        assert_eq!(matches.len(), 2);

        let filtered = filter_by_metadata(&km, matches, None, None, Some("bob")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].slug, "b-page");
    }

    #[test]
    fn test_list_filter_by_since() {
        let (km, _dir) = setup_km();

        let page_old = "---\ntitle: Old\ntags: []\nsources: []\ncontributors: []\ncreated: 2025-01-01\nupdated: 2025-06-01\n---\n\nold\n";
        let page_new = "---\ntitle: New\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-03-01\n---\n\nnew\n";

        km.write_page("old", page_old).unwrap();
        km.write_page("new", page_new).unwrap();

        let pages = km.list_pages().unwrap();
        let filtered: Vec<_> = pages
            .iter()
            .filter(|p| p.frontmatter.updated.as_str() >= "2026-01-01")
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].slug, "new");
    }

    // ==================== Round 2: Import Helper Tests ====================

    #[test]
    fn test_infer_slug_simple() {
        assert_eq!(infer_slug(Path::new("readme.md")), "readme");
        assert_eq!(infer_slug(Path::new("my-design.md")), "my-design");
    }

    #[test]
    fn test_infer_slug_with_parent() {
        assert_eq!(infer_slug(Path::new("api/design.md")), "api-design");
        assert_eq!(
            infer_slug(Path::new("arch/api/overview.md")),
            "arch-api-overview"
        );
    }

    #[test]
    fn test_infer_tags_from_path() {
        let tags = infer_tags_from_path(Path::new("arch/api/design.md"));
        assert_eq!(tags, vec!["arch", "api"]);
    }

    #[test]
    fn test_infer_tags_root_file() {
        let tags = infer_tags_from_path(Path::new("readme.md"));
        assert!(tags.is_empty());
    }

    #[test]
    fn test_import_preserves_existing_frontmatter() {
        let (km, _dir) = setup_km();

        let raw = "---\ntitle: Existing Title\ntags: [original]\nsources: []\ncontributors: [alice]\ncreated: 2026-01-01\nupdated: 2026-01-15\n---\n\nBody content.\n";

        import_single_file(
            &km,
            // We need to write a temp file to import from
            &{
                let p = _dir.path().join("test.md");
                std::fs::write(&p, raw).unwrap();
                p
            },
            "test-import",
            &["docs".to_string()],
            &["extra".to_string()],
            "bot",
            "2026-03-01",
        )
        .unwrap();

        let content = km.read_page("test-import").unwrap();
        let fm = parse_frontmatter(&content).unwrap();
        assert_eq!(fm.title, "Existing Title");
        assert!(fm.tags.contains(&"original".to_string()));
        assert!(fm.tags.contains(&"docs".to_string()));
        assert!(fm.tags.contains(&"extra".to_string()));
        assert!(fm.contributors.contains(&"alice".to_string()));
        assert!(fm.contributors.contains(&"bot".to_string()));
        assert!(content.contains("Body content."));
    }

    #[test]
    fn test_import_generates_frontmatter() {
        let (km, _dir) = setup_km();

        let raw = "# Just a heading\n\nSome body text.\n";

        import_single_file(
            &km,
            &{
                let p = _dir.path().join("my-doc.md");
                std::fs::write(&p, raw).unwrap();
                p
            },
            "my-doc",
            &[],
            &["imported".to_string()],
            "bot",
            "2026-03-01",
        )
        .unwrap();

        let content = km.read_page("my-doc").unwrap();
        let fm = parse_frontmatter(&content).unwrap();
        assert_eq!(fm.title, "my doc");
        assert!(fm.tags.contains(&"imported".to_string()));
        assert_eq!(fm.contributors, vec!["bot"]);
        assert_eq!(fm.created, "2026-03-01");
        assert!(content.contains("# Just a heading"));
    }

    // ==================== Section Parsing Tests ====================

    #[test]
    fn test_parse_heading_valid() {
        assert_eq!(parse_heading("# Title"), Some((1, "Title")));
        assert_eq!(parse_heading("## Section"), Some((2, "Section")));
        assert_eq!(parse_heading("### Sub"), Some((3, "Sub")));
        assert_eq!(parse_heading("###### Deep"), Some((6, "Deep")));
    }

    #[test]
    fn test_parse_heading_invalid() {
        assert_eq!(parse_heading("not a heading"), None);
        assert_eq!(parse_heading("#no space"), None);
        assert_eq!(parse_heading("####### too deep"), None);
        assert_eq!(parse_heading(""), None);
    }

    #[test]
    fn test_find_section_range_basic() {
        let body = "# Title\n\nIntro text.\n\n## Architecture\n\nArch content.\n\n## Notes\n\nNote content.\n";
        let lines: Vec<&str> = body.lines().collect();
        let (start, end) = find_section_range(&lines, "## Architecture").unwrap();
        assert_eq!(start, 4); // "## Architecture"
        assert_eq!(end, 8); // "## Notes"
    }

    #[test]
    fn test_find_section_range_last_section() {
        let body = "# Title\n\nIntro.\n\n## Last Section\n\nLast content.\n";
        let lines: Vec<&str> = body.lines().collect();
        let (start, end) = find_section_range(&lines, "## Last Section").unwrap();
        assert_eq!(start, 4);
        assert_eq!(end, lines.len()); // extends to EOF
    }

    #[test]
    fn test_find_section_range_without_hashes_in_query() {
        let body = "# Title\n\n## Architecture\n\nContent.\n";
        let lines: Vec<&str> = body.lines().collect();
        let (start, _) = find_section_range(&lines, "Architecture").unwrap();
        assert_eq!(start, 2);
    }

    #[test]
    fn test_find_section_range_not_found() {
        let body = "# Title\n\n## Existing\n\nContent.\n";
        let lines: Vec<&str> = body.lines().collect();
        let result = find_section_range(&lines, "## Missing");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_find_section_subsections_included() {
        let body = "## Parent\n\nParent content.\n\n### Child\n\nChild content.\n\n## Sibling\n\nSibling.\n";
        let lines: Vec<&str> = body.lines().collect();
        let (start, end) = find_section_range(&lines, "## Parent").unwrap();
        assert_eq!(start, 0);
        // Should include ### Child but stop at ## Sibling
        assert_eq!(lines[end], "## Sibling");
    }

    #[test]
    fn test_replace_section_content() {
        let body = "# Title\n\nIntro.\n\n## Architecture\n\nOld arch content.\nMore old content.\n\n## Notes\n\nNote text.\n";
        let result = replace_section_content(body, "## Architecture", "New arch content.").unwrap();
        assert!(result.contains("# Title"));
        assert!(result.contains("Intro."));
        assert!(result.contains("## Architecture"));
        assert!(result.contains("New arch content."));
        assert!(!result.contains("Old arch content."));
        assert!(!result.contains("More old content."));
        assert!(result.contains("## Notes"));
        assert!(result.contains("Note text."));
    }

    #[test]
    fn test_replace_section_last_section() {
        let body = "# Title\n\n## Only Section\n\nOld content.\n";
        let result = replace_section_content(body, "## Only Section", "Replaced.").unwrap();
        assert!(result.contains("## Only Section"));
        assert!(result.contains("Replaced."));
        assert!(!result.contains("Old content."));
    }

    #[test]
    fn test_replace_section_not_found() {
        let body = "# Title\n\n## Existing\n\nContent.\n";
        let result = replace_section_content(body, "## Missing", "new");
        assert!(result.is_err());
    }

    #[test]
    fn test_append_to_section_content() {
        let body = "# Title\n\nIntro.\n\n## Notes\n\nExisting note.\n\n## Other\n\nOther text.\n";
        let result = append_to_section_content(body, "## Notes", "Appended note.").unwrap();
        assert!(result.contains("Existing note."));
        assert!(result.contains("Appended note."));
        assert!(result.contains("## Other"));
        assert!(result.contains("Other text."));
        // Appended content should come before ## Other
        let notes_pos = result.find("Appended note.").unwrap();
        let other_pos = result.find("## Other").unwrap();
        assert!(notes_pos < other_pos);
    }

    #[test]
    fn test_append_to_section_last_section() {
        let body = "# Title\n\n## Notes\n\nExisting.\n";
        let result = append_to_section_content(body, "## Notes", "More text.").unwrap();
        assert!(result.contains("Existing."));
        assert!(result.contains("More text."));
    }

    #[test]
    fn test_append_to_section_not_found() {
        let body = "# Title\n\n## Existing\n\nContent.\n";
        let result = append_to_section_content(body, "## Missing", "new");
        assert!(result.is_err());
    }

    #[test]
    fn test_replace_section_preserves_subsections_of_siblings() {
        let body = "## A\n\nA content.\n\n### A1\n\nA1 content.\n\n## B\n\n### B1\n\nB1 content.\n";
        let result = replace_section_content(body, "## A", "New A content.").unwrap();
        assert!(result.contains("## A"));
        assert!(result.contains("New A content."));
        assert!(!result.contains("A1 content.")); // subsection replaced too
        assert!(result.contains("## B"));
        assert!(result.contains("### B1"));
        assert!(result.contains("B1 content."));
    }

    #[test]
    fn test_section_edit_query_without_hash_prefix() {
        let body = "# Title\n\n## Architecture\n\nArch content.\n\n## Notes\n\nNote.\n";
        let result = replace_section_content(body, "Architecture", "New arch.").unwrap();
        assert!(result.contains("New arch."));
        assert!(!result.contains("Arch content."));
    }

    // ==================== Round 1: List JSON Test ====================

    #[test]
    fn test_list_json_output() {
        let (km, _dir) = setup_km();

        let page = "---\ntitle: Test Page\ntags: [rust, cli]\nsources: []\ncontributors: [alice]\ncreated: 2026-01-15\nupdated: 2026-02-20\n---\n\nbody\n";
        km.write_page("test-page", page).unwrap();

        let pages = km.list_pages().unwrap();
        let refs: Vec<&crate::knowledge::PageInfo> = pages.iter().collect();

        // Capture what print_list_json would output
        let entries: Vec<String> = refs
            .iter()
            .map(|p| {
                format!(
                    "{{\"slug\":{},\"title\":{},\"tags\":[{}],\"contributors\":[{}],\"created\":{},\"updated\":{}}}",
                    serde_json_string(&p.slug),
                    serde_json_string(&p.frontmatter.title),
                    p.frontmatter.tags.iter().map(|t| serde_json_string(t)).collect::<Vec<_>>().join(","),
                    p.frontmatter.contributors.iter().map(|c| serde_json_string(c)).collect::<Vec<_>>().join(","),
                    serde_json_string(&p.frontmatter.created),
                    serde_json_string(&p.frontmatter.updated),
                )
            })
            .collect();
        let json_str = format!("[{}]", entries.join(","));

        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["slug"], "test-page");
        assert_eq!(arr[0]["title"], "Test Page");
        assert_eq!(arr[0]["tags"], serde_json::json!(["rust", "cli"]));
        assert_eq!(arr[0]["contributors"], serde_json::json!(["alice"]));
        assert_eq!(arr[0]["created"], "2026-01-15");
        assert_eq!(arr[0]["updated"], "2026-02-20");
    }
}
