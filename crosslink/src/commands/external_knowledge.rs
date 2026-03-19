//! External knowledge query commands.
//!
//! Handles `--repo` flag for `crosslink knowledge search/show/list`.

use anyhow::{bail, Result};
use std::path::Path;

use crate::external::{
    read_data_ttl, read_url_ttl, resolve_repo, ExternalCache, ExternalKnowledgeReader, RepoSource,
};
use crate::knowledge::parse_frontmatter;

/// Get an ExternalKnowledgeReader for the given repo value.
fn get_reader(
    crosslink_dir: &Path,
    repo_value: &str,
    refresh: bool,
) -> Result<(ExternalKnowledgeReader, String)> {
    let source = resolve_repo(repo_value, crosslink_dir)?;
    match source {
        RepoSource::Local(path) => {
            let reader = ExternalKnowledgeReader::for_local(&path);
            Ok((reader, repo_value.to_string()))
        }
        RepoSource::Remote(_) => {
            let cache = ExternalCache::new(crosslink_dir, repo_value);
            let data_ttl = read_data_ttl(crosslink_dir);
            let url_ttl = read_url_ttl(crosslink_dir);
            let knowledge_dir = cache.ensure_knowledge(data_ttl, url_ttl, refresh)?;
            let reader = ExternalKnowledgeReader::new(knowledge_dir);
            Ok((reader, repo_value.to_string()))
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::fn_params_excessive_bools)]
pub fn search(
    crosslink_dir: &Path,
    repo_value: &str,
    query: Option<&str>,
    context: usize,
    source_domain: Option<&str>,
    refresh: bool,
    json: bool,
    quiet: bool,
    tag: Option<&str>,
    since: Option<&str>,
    contributor: Option<&str>,
) -> Result<()> {
    if query.is_none() && source_domain.is_none() {
        bail!("Provide a search query or --source domain");
    }

    let (reader, label) = get_reader(crosslink_dir, repo_value, refresh)?;

    // Source-domain search
    if let Some(domain) = source_domain {
        let mut matches = reader.search_sources(domain)?;

        // If --repo + --source + query, also filter by content
        if let Some(q) = query {
            let q_lower = q.to_lowercase();
            matches.retain(|page| {
                reader
                    .read_page(&page.slug)
                    .map(|content| content.to_lowercase().contains(&q_lower))
                    .unwrap_or(false)
            });
        }

        if json {
            print_sources_json(&matches, &label);
        } else if !quiet {
            println!("--- Results from {} ---\n", label);
            if matches.is_empty() {
                println!("No knowledge pages cite \"{}\".", domain);
            } else {
                for page in &matches {
                    let matching_sources: Vec<_> = page
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
            }
            println!("\n--- End external results ---");
        } else {
            // Quiet: just data
            for page in &matches {
                println!("{}", page.slug);
            }
        }
        return Ok(());
    }

    // Content search — query guaranteed Some by the guard at top of function
    let Some(query) = query else {
        bail!("Provide a search query or --source domain");
    };
    let mut matches = reader.search_content(query, context)?;

    // Apply metadata filters
    if tag.is_some() || since.is_some() || contributor.is_some() {
        matches.retain(|m| {
            let content = match reader.read_page(&m.slug) {
                Ok(c) => c,
                Err(_) => return false,
            };
            let fm = match parse_frontmatter(&content) {
                Some(fm) => fm,
                None => return false,
            };
            if let Some(tag) = tag {
                if !fm.tags.iter().any(|t| t == tag) {
                    return false;
                }
            }
            if let Some(since) = since {
                if fm.updated.as_str() < since {
                    return false;
                }
            }
            if let Some(contributor) = contributor {
                if !fm.contributors.iter().any(|c| c == contributor) {
                    return false;
                }
            }
            true
        });
    }

    if json {
        print_content_json(&matches, &label);
    } else if !quiet {
        println!("--- Results from {} ---\n", label);
        if matches.is_empty() {
            println!("No knowledge pages match \"{}\".", query);
        } else {
            for (i, m) in matches.iter().enumerate() {
                if i > 0 {
                    println!();
                }
                println!("  {}.md (line {}):", m.slug, m.line_number);
                for (line_num, line) in &m.context_lines {
                    println!("    {:>4} | {}", line_num, line);
                }
            }
        }
        println!("\n--- End external results ---");
    } else {
        for m in &matches {
            println!("{}", m.slug);
        }
    }

    Ok(())
}

pub fn show(
    crosslink_dir: &Path,
    repo_value: &str,
    slug: &str,
    refresh: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let (reader, label) = get_reader(crosslink_dir, repo_value, refresh)?;
    let content = reader.read_page(slug)?;

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
                "source": label,
            });
            println!("{}", serde_json::to_string_pretty(&json_obj)?);
        } else {
            bail!("Page '{}' has no valid frontmatter", slug);
        }
    } else {
        if !quiet {
            println!("--- Results from {} ---\n", label);
        }
        print!("{}", content);
        if !quiet {
            println!("\n--- End external results ---");
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn list(
    crosslink_dir: &Path,
    repo_value: &str,
    tag_filter: Option<&str>,
    contributor_filter: Option<&str>,
    since: Option<&str>,
    refresh: bool,
    json: bool,
    quiet: bool,
) -> Result<()> {
    let (reader, label) = get_reader(crosslink_dir, repo_value, refresh)?;
    let pages = reader.list_pages()?;

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
        let entries: Vec<serde_json::Value> = filtered
            .iter()
            .map(|p| {
                serde_json::json!({
                    "slug": p.slug,
                    "title": p.frontmatter.title,
                    "tags": p.frontmatter.tags,
                    "updated": p.frontmatter.updated,
                    "source": label,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(());
    }

    if !quiet {
        println!("--- Results from {} ---\n", label);
    }

    if filtered.is_empty() {
        if !quiet {
            println!("No knowledge pages found.");
        }
    } else {
        if !quiet {
            println!("{:<30} {:<30} {:<20} UPDATED", "SLUG", "TITLE", "TAGS");
            println!("{}", "-".repeat(90));
        }
        for page in &filtered {
            let tags_str = if page.frontmatter.tags.is_empty() {
                String::new()
            } else {
                page.frontmatter.tags.join(", ")
            };
            println!(
                "{:<30} {:<30} {:<20} {}",
                page.slug,
                crate::utils::truncate(&page.frontmatter.title, 28),
                crate::utils::truncate(&tags_str, 18),
                page.frontmatter.updated,
            );
        }
        if !quiet {
            println!("\n{} page(s)", filtered.len());
        }
    }

    if !quiet {
        println!("\n--- End external results ---");
    }

    Ok(())
}

// ───────────────────────────────────────────────────────────────────────────
// JSON formatting helpers
// ───────────────────────────────────────────────────────────────────────────

fn print_content_json(matches: &[crate::knowledge::SearchMatch], source: &str) {
    let entries: Vec<serde_json::Value> = matches
        .iter()
        .map(|m| {
            let lines: Vec<serde_json::Value> = m
                .context_lines
                .iter()
                .map(|(num, text)| serde_json::json!({"line": num, "text": text}))
                .collect();
            serde_json::json!({
                "slug": m.slug,
                "line_number": m.line_number,
                "context_lines": lines,
                "source": source,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
    );
}

fn print_sources_json(matches: &[crate::knowledge::PageInfo], source: &str) {
    let entries: Vec<serde_json::Value> = matches
        .iter()
        .map(|p| {
            serde_json::json!({
                "slug": p.slug,
                "title": p.frontmatter.title,
                "tags": p.frontmatter.tags,
                "sources": p.frontmatter.sources.iter().map(|s| {
                    serde_json::json!({
                        "url": s.url,
                        "title": s.title,
                    })
                }).collect::<Vec<_>>(),
                "source": source,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&entries).unwrap_or_else(|_| "[]".to_string())
    );
}
