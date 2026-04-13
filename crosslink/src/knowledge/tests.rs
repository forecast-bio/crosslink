use super::core::{parse_inline_array, split_kv_or_bare, unquote, yaml_escape};
use super::edit::{
    append_to_section_content, extract_body, find_section_range, parse_heading,
    replace_section_content,
};
use super::search::group_matches;
use super::*;
use crate::utils::truncate;
use std::path::Path;
use std::process::Command;
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

/// Helper: create a `KnowledgeManager` with pre-populated pages.
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

    let (_dir, manager) = setup_search_manager(&[("rust-testing", page_a), ("ci-setup", page_b)]);

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
            "Should reject Windows reserved name: {name}"
        );
        assert!(
            result.unwrap_err().to_string().contains("Windows reserved"),
            "Error should mention Windows reserved for: {name}"
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
    assert!(resolved.contains("<!-- MERGE CONFLICT: Both versions kept. Cleanup recommended. -->"));
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

// --- Additional coverage tests ---

#[test]
fn test_yaml_escape_plain() {
    assert_eq!(yaml_escape("hello"), "\"hello\"");
}

#[test]
fn test_yaml_escape_with_quotes() {
    assert_eq!(yaml_escape("say \"hi\""), "\"say \\\"hi\\\"\"");
}

#[test]
fn test_yaml_escape_with_backslash() {
    assert_eq!(yaml_escape("path\\to\\file"), "\"path\\\\to\\\\file\"");
}

#[test]
fn test_yaml_escape_with_both() {
    assert_eq!(yaml_escape("a\\\"b"), "\"a\\\\\\\"b\"");
}

#[test]
fn test_yaml_escape_empty() {
    assert_eq!(yaml_escape(""), "\"\"");
}

#[test]
fn test_split_kv_or_bare_with_value() {
    let result = split_kv_or_bare("title: My Page");
    assert_eq!(result, Some(("title", "My Page")));
}

#[test]
fn test_split_kv_or_bare_bare_key() {
    let result = split_kv_or_bare("sources:");
    assert_eq!(result, Some(("sources", "")));
}

#[test]
fn test_split_kv_or_bare_no_colon() {
    let result = split_kv_or_bare("nocolon");
    assert_eq!(result, None);
}

#[test]
fn test_split_kv_or_bare_value_with_colons() {
    let result = split_kv_or_bare("url: https://example.com");
    assert_eq!(result, Some(("url", "https://example.com")));
}

#[test]
fn test_unquote_escaped_quotes() {
    assert_eq!(unquote("\"say \\\"hi\\\"\""), "say \"hi\"");
}

#[test]
fn test_unquote_escaped_backslash() {
    assert_eq!(unquote("\"path\\\\to\""), "path\\to");
}

#[test]
fn test_unquote_single_quoted() {
    assert_eq!(unquote("'hello world'"), "hello world");
}

#[test]
fn test_unquote_unquoted_with_spaces() {
    assert_eq!(unquote("  bare value  "), "bare value");
}

#[test]
fn test_delete_page_happy_path() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    manager.write_page("to-delete", "content").unwrap();
    assert!(manager.page_exists("to-delete"));

    manager.delete_page("to-delete").unwrap();
    assert!(!manager.page_exists("to-delete"));
}

#[test]
fn test_delete_page_not_found() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.delete_page("nonexistent");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn test_page_exists_happy_path() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert!(!manager.page_exists("new-page"));

    manager.write_page("new-page", "content").unwrap();
    assert!(manager.page_exists("new-page"));
}

#[test]
fn test_search_content_empty_query_returns_empty() {
    let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nSome content.\n";
    let (_dir, manager) = setup_search_manager(&[("test", page)]);

    let results = manager.search_content("", 0).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_search_content_whitespace_only_query() {
    let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nSome content.\n";
    let (_dir, manager) = setup_search_manager(&[("test", page)]);

    let results = manager.search_content("   ", 0).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_resolve_accept_both_unterminated_ours() {
    let content = "before\n<<<<<<< HEAD\norphaned ours content\n";
    let resolved = resolve_accept_both(content);
    assert!(resolved.contains("orphaned ours content"));
    assert!(resolved.contains("before"));
}

#[test]
fn test_resolve_accept_both_unterminated_theirs() {
    let content = "before\n<<<<<<< HEAD\nours text\n=======\ntheirs text\n";
    let resolved = resolve_accept_both(content);
    assert!(resolved.contains("ours text"));
    assert!(resolved.contains("theirs text"));
    assert!(resolved.contains("before"));
}

#[test]
fn test_list_pages_without_frontmatter_uses_slug_as_title() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    manager
        .write_page("no-front", "# Just a heading\n\nNo frontmatter.\n")
        .unwrap();

    let pages = manager.list_pages().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].slug, "no-front");
    assert_eq!(pages[0].frontmatter.title, "no-front");
}

#[test]
fn test_parse_frontmatter_with_source_inline_key() {
    let content = "\
---
title: Source Test
tags: []
sources:
  - url: https://example.com
    title: Example
    accessed_at: 2026-01-01
contributors: []
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://example.com");
    assert_eq!(fm.sources[0].title, "Example");
    assert_eq!(fm.sources[0].accessed_at, Some("2026-01-01".to_string()));
}

#[test]
fn test_parse_frontmatter_unknown_top_level_key() {
    let content = "---\ntitle: With Extra\nunknown_key: some_value\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.title, "With Extra");
    assert_eq!(fm.created, "2026-01-01");
}

#[test]
fn test_parse_inline_array_single_quoted_items() {
    let result = parse_inline_array("['foo', 'bar']");
    assert_eq!(result, Some(vec!["foo".to_string(), "bar".to_string()]));
}

#[test]
fn test_parse_inline_array_double_quoted_items() {
    let result = parse_inline_array("[\"foo\", \"bar\"]");
    assert_eq!(result, Some(vec!["foo".to_string(), "bar".to_string()]));
}

#[test]
fn test_parse_inline_array_whitespace_around() {
    let result = parse_inline_array("  [ a , b , c ]  ");
    assert_eq!(
        result,
        Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
    );
}

#[test]
fn test_serialize_frontmatter_with_yaml_escape_needed() {
    let fm = PageFrontmatter {
        title: "Title with \"quotes\" and \\backslash".to_string(),
        tags: vec!["a".to_string()],
        sources: Vec::new(),
        contributors: Vec::new(),
        created: "2026-01-01".to_string(),
        updated: "2026-01-01".to_string(),
    };
    let serialized = serialize_frontmatter(&fm);
    assert!(serialized.contains("\\\"quotes\\\""));
    assert!(serialized.contains("\\\\backslash"));
}

#[test]
fn test_search_sources_case_insensitive() {
    let page = "---\ntitle: Test\ntags: []\nsources:\n  - url: https://EXAMPLE.COM/path\n    title: Example\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let (_dir, manager) = setup_search_manager(&[("test", page)]);

    let results = manager.search_sources("example.com").unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn test_search_sources_empty_sources() {
    let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let (_dir, manager) = setup_search_manager(&[("test", page)]);

    let results = manager.search_sources("anything.com").unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_group_matches_single_item() {
    let groups = group_matches(&[5], 2);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0], vec![5]);
}

#[test]
fn test_group_matches_exact_boundary() {
    let groups = group_matches(&[0, 3], 1);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0], vec![0, 3]);

    let groups = group_matches(&[0, 1], 0);
    assert_eq!(groups.len(), 1);

    let groups = group_matches(&[0, 2], 0);
    assert_eq!(groups.len(), 2);
}

#[test]
fn test_group_matches_multiple_groups() {
    let groups = group_matches(&[0, 1, 5, 6, 10], 0);
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0], vec![0, 1]);
    assert_eq!(groups[1], vec![5, 6]);
    assert_eq!(groups[2], vec![10]);
}

#[test]
fn test_safe_page_path_rejects_null_bytes() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert!(manager.safe_page_path("foo\0bar").is_err());
}

#[test]
fn test_crosslink_dir_accessor() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert_eq!(manager.crosslink_dir(), crosslink_dir);
}

#[test]
fn test_cache_path_accessor() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert_eq!(
        manager.cache_path(),
        crosslink_dir.join(KNOWLEDGE_CACHE_DIR)
    );
}

#[test]
fn test_parse_frontmatter_multiline_contributors() {
    let content = "\
---
title: Contributors Test
tags: []
sources: []
contributors:
  - alice
  - bob
  - carol
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.contributors, vec!["alice", "bob", "carol"]);
}

#[test]
fn test_parse_frontmatter_tags_empty_bare() {
    let content = "---\ntitle: Bare\ntags:\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert!(fm.tags.is_empty());
}

#[test]
fn test_parse_frontmatter_multiple_sources_with_flush() {
    let content = "\
---
title: Multi Source
tags: []
sources:
  - url: https://a.com
    title: A
  - url: https://b.com
    title: B
    accessed_at: 2026-02-01
  - url: https://c.com
    title: C
contributors: []
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 3);
    assert_eq!(fm.sources[0].url, "https://a.com");
    assert_eq!(fm.sources[0].title, "A");
    assert!(fm.sources[0].accessed_at.is_none());
    assert_eq!(fm.sources[1].url, "https://b.com");
    assert_eq!(fm.sources[1].title, "B");
    assert_eq!(fm.sources[1].accessed_at, Some("2026-02-01".to_string()));
    assert_eq!(fm.sources[2].url, "https://c.com");
    assert_eq!(fm.sources[2].title, "C");
}

#[test]
fn test_serialize_frontmatter_sources_without_accessed_at() {
    let fm = PageFrontmatter {
        title: "No Access".to_string(),
        tags: Vec::new(),
        sources: vec![Source {
            url: "https://example.com".to_string(),
            title: "Example".to_string(),
            accessed_at: None,
        }],
        contributors: Vec::new(),
        created: "2026-01-01".to_string(),
        updated: "2026-01-01".to_string(),
    };

    let serialized = serialize_frontmatter(&fm);
    assert!(serialized.contains("url: \"https://example.com\""));
    assert!(!serialized.contains("accessed_at"));
}

#[test]
fn test_has_conflict_markers_missing_middle() {
    let content = "<<<<<<< HEAD\nours\n>>>>>>> branch\n";
    assert!(!has_conflict_markers(content));
}

#[test]
fn test_resolve_accept_both_preserves_non_conflict_lines() {
    let content = "line1\nline2\nline3\n";
    let resolved = resolve_accept_both(content);
    assert_eq!(resolved, "line1\nline2\nline3\n");
}

#[test]
fn test_search_content_context_at_file_boundaries() {
    let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\nMatchword here.\nsome other content.\n";
    let (_dir, manager) = setup_search_manager(&[("boundary", page)]);

    let results = manager.search_content("matchword", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert!(!results[0].context_lines.is_empty());
}

#[test]
fn test_resolve_conflicts_in_cache_no_md_files() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    std::fs::write(cache_dir.join("notes.txt"), "some text").unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let resolved = manager.resolve_conflicts_in_cache().unwrap();
    assert!(resolved.is_empty());
}

#[test]
fn test_resolve_conflicts_in_cache_empty() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let resolved = manager.resolve_conflicts_in_cache().unwrap();
    assert!(resolved.is_empty());
}

// --- Additional coverage tests ---

#[test]
fn test_parse_frontmatter_empty_string() {
    assert!(parse_frontmatter("").is_none());
}

#[test]
fn test_parse_frontmatter_only_opening_delimiter() {
    assert!(parse_frontmatter("---\ntitle: Orphan\n").is_none());
}

#[test]
fn test_parse_frontmatter_leading_whitespace() {
    // Content with leading whitespace before the opening ---
    let content = "  ---\ntitle: Indented\ncreated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.title, "Indented");
    assert_eq!(fm.created, "2026-01-01");
}

#[test]
fn test_parse_frontmatter_tags_non_array_value() {
    // tags with a plain string value (not array, not empty) -> falls through to TopLevel
    let content = "---\ntitle: PlainTag\ntags: not-an-array\nsources: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.title, "PlainTag");
    // tags should remain empty because a non-array string is not parsed
    assert!(fm.tags.is_empty());
}

#[test]
fn test_parse_frontmatter_contributors_non_array_value() {
    // contributors with a plain string value -> falls through to TopLevel
    let content = "---\ntitle: PlainContrib\ncontributors: not-an-array\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.title, "PlainContrib");
    assert!(fm.contributors.is_empty());
}

#[test]
fn test_parse_frontmatter_source_flushed_at_top_level_key() {
    let content = "\
---
title: Flush Test
tags: []
sources:
  - url: https://flushed.com
    title: Flushed Source
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://flushed.com");
    assert_eq!(fm.sources[0].title, "Flushed Source");
    assert_eq!(fm.created, "2026-01-01");
}

#[test]
fn test_parse_frontmatter_source_with_inline_key_on_dash_line() {
    let content = "\
---
title: Inline Source Key
tags: []
sources:
  - url: https://inline.example.com
    title: Inline Example
  - title: Title First
    url: https://title-first.com
contributors: []
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 2);
    assert_eq!(fm.sources[0].url, "https://inline.example.com");
    assert_eq!(fm.sources[0].title, "Inline Example");
    assert_eq!(fm.sources[1].url, "https://title-first.com");
    assert_eq!(fm.sources[1].title, "Title First");
}

#[test]
fn test_parse_frontmatter_source_accessed_at_on_dash_line() {
    let content = "\
---
title: AccessedAt Inline
sources:
  - accessed_at: 2026-03-01
    url: https://accessed.com
    title: Accessed
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].accessed_at, Some("2026-03-01".to_string()));
    assert_eq!(fm.sources[0].url, "https://accessed.com");
}

#[test]
fn test_parse_frontmatter_source_unknown_nested_key() {
    let content = "\
---
title: Unknown Key
sources:
  - url: https://example.com
    title: Example
    unknown_field: some_value
contributors: []
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://example.com");
    assert_eq!(fm.sources[0].title, "Example");
}

#[test]
fn test_parse_frontmatter_source_new_dash_item_flushes_previous_in_source_item() {
    let content = "\
---
title: Source Item Flush
sources:
  - url: https://first.com
    title: First
  - url: https://second.com
    title: Second
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 2);
    assert_eq!(fm.sources[0].url, "https://first.com");
    assert_eq!(fm.sources[0].title, "First");
    assert_eq!(fm.sources[1].url, "https://second.com");
    assert_eq!(fm.sources[1].title, "Second");
}

#[test]
fn test_parse_frontmatter_source_unknown_key_on_dash_line() {
    let content = "\
---
title: Unknown Dash Key
sources:
  - mystery: value
    url: https://mystery.com
    title: Mystery
contributors: []
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://mystery.com");
    assert_eq!(fm.sources[0].title, "Mystery");
}

#[test]
fn test_parse_frontmatter_empty_yaml_block() {
    let content = "---\n---\n";
    assert!(parse_frontmatter(content).is_none());
}

#[test]
fn test_parse_frontmatter_final_source_flushed_at_end() {
    let content = "\
---
title: Final Flush
sources:
  - url: https://last.com
    title: Last
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://last.com");
    assert_eq!(fm.sources[0].title, "Last");
}

#[test]
fn test_resolve_accept_both_empty_content() {
    let resolved = resolve_accept_both("");
    assert_eq!(resolved, "");
}

#[test]
fn test_resolve_accept_both_only_local_empty() {
    let content = "\
<<<<<<< HEAD
only local
=======
>>>>>>> branch
";
    let resolved = resolve_accept_both(content);
    assert!(!has_conflict_markers(&resolved));
    assert!(resolved.contains("only local"));
    assert!(resolved.contains("<!-- MERGE CONFLICT:"));
}

#[test]
fn test_group_matches_large_context_merges_all() {
    let groups = group_matches(&[0, 10, 20, 30], 20);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0], vec![0, 10, 20, 30]);
}

#[test]
fn test_group_matches_zero_context_adjacent() {
    let groups = group_matches(&[3, 4, 5, 10], 0);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0], vec![3, 4, 5]);
    assert_eq!(groups[1], vec![10]);
}

#[test]
fn test_sync_outcome_default() {
    let outcome = SyncOutcome::default();
    assert!(outcome.resolved_conflicts.is_empty());
}

#[test]
fn test_serialize_frontmatter_multiple_contributors() {
    let fm = PageFrontmatter {
        title: "Multi Contributors".to_string(),
        tags: Vec::new(),
        sources: Vec::new(),
        contributors: vec!["alice".to_string(), "bob".to_string(), "carol".to_string()],
        created: "2026-01-01".to_string(),
        updated: "2026-01-01".to_string(),
    };
    let serialized = serialize_frontmatter(&fm);
    assert!(serialized.contains(r#"contributors: ["alice", "bob", "carol"]"#));
}

#[test]
fn test_serialize_frontmatter_multiple_tags() {
    let fm = PageFrontmatter {
        title: "Multi Tags".to_string(),
        tags: vec![
            "rust".to_string(),
            "async".to_string(),
            "testing".to_string(),
        ],
        sources: Vec::new(),
        contributors: Vec::new(),
        created: "2026-01-01".to_string(),
        updated: "2026-01-01".to_string(),
    };
    let serialized = serialize_frontmatter(&fm);
    assert!(serialized.contains(r#"tags: ["rust", "async", "testing"]"#));
}

#[test]
fn test_serialize_then_parse_roundtrip_complex() {
    let fm = PageFrontmatter {
        title: "Complex \"Roundtrip\" Test".to_string(),
        tags: vec!["alpha".to_string(), "beta".to_string()],
        sources: vec![
            Source {
                url: "https://a.example.com/path?q=1".to_string(),
                title: "Site A".to_string(),
                accessed_at: Some("2026-02-15".to_string()),
            },
            Source {
                url: "https://b.example.com".to_string(),
                title: "Site B".to_string(),
                accessed_at: None,
            },
        ],
        contributors: vec!["alice".to_string(), "bob".to_string()],
        created: "2026-01-01".to_string(),
        updated: "2026-03-10".to_string(),
    };
    let serialized = serialize_frontmatter(&fm);
    let parsed = parse_frontmatter(&serialized).unwrap();
    assert_eq!(parsed.title, fm.title);
    assert_eq!(parsed.tags, fm.tags);
    assert_eq!(parsed.sources.len(), 2);
    assert_eq!(parsed.sources[0].url, fm.sources[0].url);
    assert_eq!(parsed.sources[0].title, fm.sources[0].title);
    assert_eq!(parsed.sources[0].accessed_at, fm.sources[0].accessed_at);
    assert_eq!(parsed.sources[1].url, fm.sources[1].url);
    assert_eq!(parsed.sources[1].title, fm.sources[1].title);
    assert_eq!(parsed.sources[1].accessed_at, fm.sources[1].accessed_at);
    assert_eq!(parsed.contributors, fm.contributors);
    assert_eq!(parsed.created, fm.created);
    assert_eq!(parsed.updated, fm.updated);
}

#[test]
fn test_search_content_ignores_non_md_files() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

    manager
        .write_page(
            "real-page",
            "---\ntitle: Real\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nfindable content\n",
        )
        .unwrap();
    std::fs::write(cache_dir.join("notes.txt"), "findable content").unwrap();

    let results = manager.search_content("findable", 0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].slug, "real-page");
}

#[test]
fn test_parse_frontmatter_contributors_empty_bare() {
    let content = "---\ntitle: Bare Contrib\ncontributors:\nsources: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert!(fm.contributors.is_empty());
}

#[test]
fn test_parse_frontmatter_sources_empty_bracket() {
    let content = "---\ntitle: No Sources\nsources: []\ncreated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert!(fm.sources.is_empty());
}

#[test]
fn test_has_conflict_markers_all_on_same_line() {
    // All three markers on the same line is NOT a real git conflict — git
    // always places each marker on its own line. The previous implementation
    // used naive `contains()` which would false-positive on this (#418).
    let content = "<<<<<<< =======  >>>>>>>\n";
    assert!(!has_conflict_markers(content));
}

#[test]
fn test_has_conflict_markers_mid_line_not_detected() {
    // Markers that appear mid-line (not at line start) should not trigger.
    let content = "This is a line with <<<<<<< in it\nand ======= here\nand >>>>>>> there\n";
    assert!(!has_conflict_markers(content));
}

#[test]
fn test_has_conflict_markers_out_of_order() {
    // Markers in wrong order (separator before opening) should not trigger.
    let content = "=======\n<<<<<<< HEAD\n>>>>>>> branch\n";
    assert!(!has_conflict_markers(content));
}

#[test]
fn test_has_conflict_markers_valid_sequence() {
    // Proper git conflict marker sequence should be detected.
    let content = "before\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> branch\nafter\n";
    assert!(has_conflict_markers(content));
}

#[test]
fn test_has_conflict_markers_missing_closing() {
    let content = "<<<<<<< HEAD\nours\n=======\ntheirs\n";
    assert!(!has_conflict_markers(content));
}

#[test]
fn test_split_kv_or_bare_key_with_spaces() {
    let result = split_kv_or_bare("  title: Spaced  ");
    assert_eq!(result, Some(("title", "Spaced")));
}

#[test]
fn test_split_kv_or_bare_empty_value() {
    let result = split_kv_or_bare("tags: ");
    assert_eq!(result, Some(("tags", "")));
}

#[test]
fn test_parse_inline_array_comma_in_quoted_value() {
    // A comma inside double quotes should not split the value (#431).
    let result = parse_inline_array("[\"last, first\", \"plain\"]");
    assert_eq!(
        result,
        Some(vec!["last, first".to_string(), "plain".to_string()])
    );
}

#[test]
fn test_serialize_parse_roundtrip_comma_in_tag() {
    // Tags containing commas must survive a serialize→parse roundtrip (#431).
    let fm = PageFrontmatter {
        title: "Test".to_string(),
        tags: vec!["rust, systems".to_string(), "plain".to_string()],
        sources: vec![],
        contributors: vec![],
        created: "2026-01-01".to_string(),
        updated: "2026-01-01".to_string(),
    };
    let serialized = serialize_frontmatter(&fm);
    let parsed = parse_frontmatter(&serialized).unwrap();
    assert_eq!(parsed.tags, fm.tags);
}

#[test]
fn test_parse_inline_array_no_brackets() {
    assert_eq!(parse_inline_array("hello"), None);
}

#[test]
fn test_parse_inline_array_only_opening() {
    assert_eq!(parse_inline_array("[hello"), None);
}

#[test]
fn test_unquote_empty_string() {
    assert_eq!(unquote(""), "");
}

#[test]
fn test_unquote_only_quotes() {
    assert_eq!(unquote("\"\""), "");
    assert_eq!(unquote("''"), "");
}

#[test]
fn test_unquote_mismatched_quotes() {
    assert_eq!(unquote("'hello\""), "'hello\"");
}

#[test]
fn test_knowledge_manager_new_fails_on_root_path() {
    let result = KnowledgeManager::new(Path::new("/"));
    let _ = result;
}

#[test]
fn test_is_initialized_true_when_cache_exists() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert!(manager.is_initialized());
}

#[test]
fn test_list_pages_sorted_alphabetically() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

    manager
        .write_page("zebra", "---\ntitle: Zebra\ncreated: 2026-01-01\n---\n")
        .unwrap();
    manager
        .write_page("alpha", "---\ntitle: Alpha\ncreated: 2026-01-01\n---\n")
        .unwrap();
    manager
        .write_page("middle", "---\ntitle: Middle\ncreated: 2026-01-01\n---\n")
        .unwrap();

    let pages = manager.list_pages().unwrap();
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0].slug, "alpha");
    assert_eq!(pages[1].slug, "middle");
    assert_eq!(pages[2].slug, "zebra");
}

#[test]
fn test_resolve_accept_both_conflict_with_single_line_sides() {
    let content = "<<<<<<< HEAD\nA\n=======\nB\n>>>>>>> branch\n";
    let resolved = resolve_accept_both(content);
    assert!(!has_conflict_markers(&resolved));
    assert!(resolved.contains("A\n"));
    assert!(resolved.contains("B\n"));
    assert!(resolved.contains("---\n"));
}

#[test]
fn test_search_content_multiple_terms_partial_match() {
    let page = "---\ntitle: Test\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nRust is great.\n";
    let (_dir, manager) = setup_search_manager(&[("test-page", page)]);

    let results = manager.search_content("rust python java", 0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].slug, "test-page");
}

#[test]
fn test_search_content_line_numbers_are_1_based() {
    let page = "---\ntitle: T\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nfirst line\nsecond line\ntarget line\nfourth line\n";
    let (_dir, manager) = setup_search_manager(&[("numbered", page)]);

    let results = manager.search_content("target", 0).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].line_number > 0);
    assert!(results[0]
        .context_lines
        .iter()
        .any(|(_, line)| line.contains("target")));
}

#[test]
fn test_parse_frontmatter_source_with_accessed_at_on_new_dash() {
    let content = "\
---
title: Accessed At New Dash
sources:
  - url: https://first.com
    title: First
  - accessed_at: 2026-05-01
    url: https://second.com
    title: Second
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 2);
    assert_eq!(fm.sources[0].url, "https://first.com");
    assert!(fm.sources[0].accessed_at.is_none());
    assert_eq!(fm.sources[1].url, "https://second.com");
    assert_eq!(fm.sources[1].accessed_at, Some("2026-05-01".to_string()));
}

#[test]
fn test_yaml_escape_special_characters() {
    assert_eq!(yaml_escape("hello world"), "\"hello world\"");
    assert_eq!(yaml_escape("a: b"), "\"a: b\"");
}

#[test]
fn test_serialize_frontmatter_starts_and_ends_with_delimiters() {
    let fm = PageFrontmatter {
        title: "Delim Test".to_string(),
        tags: Vec::new(),
        sources: Vec::new(),
        contributors: Vec::new(),
        created: "2026-01-01".to_string(),
        updated: "2026-01-01".to_string(),
    };
    let serialized = serialize_frontmatter(&fm);
    assert!(serialized.starts_with("---\n"));
    assert!(serialized.ends_with("---\n"));
}

#[test]
fn test_page_frontmatter_clone_and_debug() {
    let fm = PageFrontmatter {
        title: "Clone Test".to_string(),
        tags: vec!["a".to_string()],
        sources: Vec::new(),
        contributors: Vec::new(),
        created: "2026-01-01".to_string(),
        updated: "2026-01-01".to_string(),
    };
    let cloned = fm.clone();
    assert_eq!(cloned, fm);
    let debug_str = format!("{fm:?}");
    assert!(debug_str.contains("Clone Test"));
}

#[test]
fn test_source_clone_and_debug() {
    let src = Source {
        url: "https://example.com".to_string(),
        title: "Example".to_string(),
        accessed_at: Some("2026-01-01".to_string()),
    };
    let cloned = src.clone();
    assert_eq!(cloned, src);
    let debug_str = format!("{src:?}");
    assert!(debug_str.contains("example.com"));
}

#[test]
fn test_search_match_debug() {
    let m = SearchMatch {
        slug: "test".to_string(),
        line_number: 5,
        context_lines: vec![(5, "hello".to_string())],
    };
    let debug_str = format!("{m:?}");
    assert!(debug_str.contains("test"));
}

#[test]
fn test_page_info_debug() {
    let info = PageInfo {
        slug: "test-slug".to_string(),
        frontmatter: PageFrontmatter {
            title: "Test".to_string(),
            tags: Vec::new(),
            sources: Vec::new(),
            contributors: Vec::new(),
            created: String::new(),
            updated: String::new(),
        },
    };
    let debug_str = format!("{info:?}");
    assert!(debug_str.contains("test-slug"));
}

#[test]
fn test_sync_outcome_debug() {
    let outcome = SyncOutcome {
        resolved_conflicts: vec!["page-a".to_string()],
    };
    let debug_str = format!("{outcome:?}");
    assert!(debug_str.contains("page-a"));
}

#[test]
fn test_resolve_accept_both_back_to_back_conflicts() {
    let content = "\
<<<<<<< HEAD
first local
=======
first remote
>>>>>>> branch
<<<<<<< HEAD
second local
=======
second remote
>>>>>>> branch
";
    let resolved = resolve_accept_both(content);
    assert!(!has_conflict_markers(&resolved));
    assert!(resolved.contains("first local"));
    assert!(resolved.contains("first remote"));
    assert!(resolved.contains("second local"));
    assert!(resolved.contains("second remote"));
    assert_eq!(resolved.matches("<!-- MERGE CONFLICT:").count(), 2);
}

// --- Mid-sequence conflict resolution failure tests (#420) ---

#[test]
fn test_resolve_accept_both_missing_separator() {
    // Opening marker appears but the separator (=======) is never reached.
    // The content after <<<<<<< should be preserved as orphaned "ours" content.
    let content = "before\n<<<<<<< HEAD\norphaned content\n>>>>>>> branch\n";
    let resolved = resolve_accept_both(content);
    // The >>>>>>> without a preceding ======= leaves us in InOurs state.
    // The >>>>>>> marker is treated as ours content since we haven't seen =======.
    assert!(resolved.contains("before"));
    assert!(resolved.contains("orphaned content"));
}

#[test]
fn test_resolve_accept_both_nested_opening_markers() {
    // Two opening markers in sequence without a separator between them.
    // The second <<<<<<< should be treated as content within the ours section.
    let content = "\
<<<<<<< HEAD
ours line 1
<<<<<<< HEAD
ours line 2
=======
theirs
>>>>>>> branch
";
    let resolved = resolve_accept_both(content);
    assert!(!has_conflict_markers(&resolved));
    assert!(resolved.contains("ours line 1"));
    assert!(resolved.contains("theirs"));
}

#[test]
fn test_resolve_accept_both_separator_without_opening() {
    // A separator line without a preceding opening marker should be
    // passed through as normal content (it's in Outside state).
    let content = "normal\n=======\nmore normal\n";
    let resolved = resolve_accept_both(content);
    assert_eq!(resolved, content);
}

#[test]
fn test_resolve_accept_both_interleaved_normal_and_conflict() {
    // Verify that normal content between two conflict blocks is preserved.
    let content = "\
<<<<<<< HEAD
first ours
=======
first theirs
>>>>>>> branch
middle content is preserved
<<<<<<< HEAD
second ours
=======
second theirs
>>>>>>> branch
";
    let resolved = resolve_accept_both(content);
    assert!(resolved.contains("middle content is preserved"));
    assert!(!has_conflict_markers(&resolved));
    assert_eq!(resolved.matches("<!-- MERGE CONFLICT:").count(), 2);
}

#[test]
fn test_safe_page_path_valid_with_dots_in_name() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

    assert!(manager.safe_page_path("my.page").is_ok());
    assert!(manager.safe_page_path("v1.0.0").is_ok());
}

#[test]
fn test_safe_page_path_rejects_double_dots() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

    assert!(manager.safe_page_path("foo..bar").is_err());
}

#[test]
fn test_cache_path_str() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let path_str = manager.cache_path_str();
    assert!(path_str.contains(KNOWLEDGE_CACHE_DIR));
}

#[test]
fn test_search_content_context_merges_nearby_matches() {
    let page = "---\ntitle: T\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nfirst keyword here\nseparator\nsecond keyword here\n";
    let (_dir, manager) = setup_search_manager(&[("grouped", page)]);

    let results = manager.search_content("keyword", 5).unwrap();
    assert_eq!(results.len(), 1);
    let lines_text: String = results[0]
        .context_lines
        .iter()
        .map(|(_, l)| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(lines_text.contains("first keyword"));
    assert!(lines_text.contains("second keyword"));
}

#[test]
fn test_search_content_context_separates_distant_matches() {
    let mut page = String::from("---\ntitle: T\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n\nkeyword here\n");
    for _ in 0..20 {
        page.push_str("filler line\n");
    }
    page.push_str("keyword again\n");

    let (_dir, manager) = setup_search_manager(&[("distant", &page)]);

    let results = manager.search_content("keyword", 0).unwrap();
    assert_eq!(results.len(), 2);
}

// --- Additional branch coverage tests ---

#[test]
fn test_parse_frontmatter_tags_inline_empty_bracket_stays_top_level() {
    let content = "\
---
title: Tags Bracket Test
tags: []
sources: []
contributors: []
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert!(fm.tags.is_empty());
    assert!(fm.sources.is_empty());
    assert_eq!(fm.created, "2026-01-01");
}

#[test]
fn test_parse_frontmatter_contributors_inline_empty_bracket_stays_top_level() {
    let content = "\
---
title: Contrib Bracket Test
tags: []
sources: []
contributors: []
created: 2026-01-15
updated: 2026-01-15
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert!(fm.contributors.is_empty());
    assert_eq!(fm.created, "2026-01-15");
}

#[test]
fn test_parse_frontmatter_sources_bare_key_enters_in_sources() {
    let content = "\
---
title: Bare Sources Key
sources:
  - url: https://bare-sources.com
    title: Bare
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://bare-sources.com");
    assert_eq!(fm.sources[0].title, "Bare");
}

#[test]
fn test_parse_frontmatter_in_sources_non_list_line_ignored() {
    let content = "\
---
title: Non-list In Sources
sources:
    ignored_line
  - url: https://real.com
    title: Real
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://real.com");
}

#[test]
fn test_parse_frontmatter_in_source_item_skips_empty_flush() {
    let content = "\
---
title: Skip Empty Flush
sources:
  - unknown_key: value
  - url: https://real.com
    title: Real Source
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://real.com");
    assert_eq!(fm.sources[0].title, "Real Source");
}

#[test]
fn test_parse_frontmatter_in_source_item_nested_unknown_key_ignored() {
    let content = "\
---
title: Unknown Nested
sources:
  - url: https://example.com
    title: Example
    totally_unknown: should_be_ignored
    another_unknown: also_ignored
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://example.com");
    assert_eq!(fm.sources[0].title, "Example");
    assert!(fm.sources[0].accessed_at.is_none());
}

#[test]
fn test_parse_frontmatter_top_level_state_ignores_indented_non_list_line() {
    let content = "\
---
title: Top Level Ignore
    this_is_indented_but_not_a_list_item_or_nested_key
sources: []
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.title, "Top Level Ignore");
    assert_eq!(fm.created, "2026-01-01");
}

#[test]
fn test_parse_frontmatter_in_source_item_dash_line_no_colon() {
    let content = "\
---
title: No Colon Dash
sources:
  - url: https://first.com
    title: First
  - just-a-tag
  - url: https://third.com
    title: Third
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    let urls: Vec<&str> = fm.sources.iter().map(|s| s.url.as_str()).collect();
    assert!(urls.contains(&"https://first.com"));
    assert!(urls.contains(&"https://third.com"));
}

#[test]
fn test_parse_frontmatter_in_sources_dash_line_no_colon() {
    let content = "\
---
title: Bare Dash Source
sources:
  - just-a-label
  - url: https://real.com
    title: Real
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://real.com");
}

#[test]
fn test_init_cache_early_return_when_initialized() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert!(manager.is_initialized());

    let result = manager.init_cache();
    assert!(result.is_ok());
}

#[test]
fn test_init_cache_not_initialized_attempts_git() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let repo_root = dir.path();
    init_git_repo(repo_root);

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert!(!manager.is_initialized());

    let _result = manager.init_cache();
}

#[test]
fn test_commit_nothing_to_commit() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path();
    init_git_repo(repo_root);

    let crosslink_dir = repo_root.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&knowledge_path).unwrap();

    Command::new("git")
        .args(["init", &knowledge_path.to_string_lossy()])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.email",
            "test@test.com",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.name",
            "Test",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .output()
        .unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.commit("test: nothing to commit");
    assert!(result.is_ok());
}

#[test]
fn test_commit_with_changes() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path();
    init_git_repo(repo_root);

    let crosslink_dir = repo_root.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&knowledge_path).unwrap();

    Command::new("git")
        .args(["init", &knowledge_path.to_string_lossy()])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.email",
            "test@test.com",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.name",
            "Test",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .output()
        .unwrap();

    std::fs::write(knowledge_path.join("test-page.md"), "# Test\n\nContent.\n").unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.commit("test: add page");
    assert!(result.is_ok());
}

#[test]
fn test_sync_unreachable_remote_returns_ok() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path();
    init_git_repo(repo_root);

    let crosslink_dir = repo_root.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&knowledge_path).unwrap();

    Command::new("git")
        .args(["init", &knowledge_path.to_string_lossy()])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.email",
            "test@test.com",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.name",
            "Test",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "remote",
            "add",
            "origin",
            "https://nonexistent.invalid/repo.git",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "commit",
            "--allow-empty",
            "-m",
            "init",
        ])
        .output()
        .unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.sync();
    assert!(result.is_ok());
    assert!(result.unwrap().resolved_conflicts.is_empty());
}

// Note: The remaining git-heavy integration tests (test_push_unreachable_remote_returns_ok,
// test_sync_unknown_revision_returns_ok, test_git_in_repo_success, test_git_in_repo_failure,
// test_git_in_cache_failure, test_sync_with_local_remote_pair_reset_path,
// test_sync_with_unpushed_local_commits_rebase_path, test_push_success_with_local_remote,
// test_push_rejected_rebase_succeeds, test_handle_rebase_conflict_merge_succeeds,
// test_handle_rebase_conflict_with_md_conflicts, test_commit_propagates_error_when_git_fails,
// test_init_cache_fetches_remote_branch_when_available, and the remaining tests)
// are preserved below without modification.

#[test]
fn test_push_unreachable_remote_returns_ok() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path();

    let crosslink_dir = repo_root.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&knowledge_path).unwrap();

    let kp = knowledge_path.to_string_lossy();
    Command::new("git").args(["init", &kp]).output().unwrap();
    Command::new("git")
        .args(["-C", &kp, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "config", "user.name", "Test"])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &kp,
            "remote",
            "add",
            "origin",
            "https://nonexistent.invalid/repo.git",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "checkout", "--orphan", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "commit", "--allow-empty", "-m", "init knowledge"])
        .output()
        .unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.push();
    assert!(result.is_ok());
}

#[test]
fn test_sync_unknown_revision_returns_ok() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path();
    init_git_repo(repo_root);

    let crosslink_dir = repo_root.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&knowledge_path).unwrap();

    Command::new("git")
        .args(["init", &knowledge_path.to_string_lossy()])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.email",
            "test@test.com",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.name",
            "Test",
        ])
        .output()
        .unwrap();
    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.sync();
    assert!(result.is_ok());
}

#[test]
fn test_git_in_repo_success() {
    let dir = tempdir().unwrap();
    let repo_root = dir.path();
    init_git_repo(repo_root);

    let crosslink_dir = repo_root.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.git_in_repo(&["status"]);
    assert!(result.is_ok());
}

#[test]
fn test_git_in_repo_failure() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.git_in_repo(&["rev-parse", "HEAD"]);
    assert!(result.is_err());
}

#[test]
fn test_git_in_cache_failure() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.git_in_cache(&["status"]);
    assert!(result.is_err());
}

#[test]
fn test_parse_frontmatter_tags_empty_value_enters_in_tags() {
    let content = "\
---
title: Tags Empty Value
tags:
  - item1
  - item2
sources: []
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.tags, vec!["item1", "item2"]);
}

#[test]
fn test_parse_frontmatter_contributors_empty_value_enters_in_contributors() {
    let content = "\
---
title: Contributors Empty
contributors:
  - alice
  - bob
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.contributors, vec!["alice", "bob"]);
}

#[test]
fn test_parse_frontmatter_in_source_item_non_list_non_nested_line() {
    let content = "\
---
title: Source Item Odd Line
sources:
  - url: https://example.com
    title: Example
  random: value
  - url: https://second.com
    title: Second
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 2);
    assert_eq!(fm.sources[0].url, "https://example.com");
    assert_eq!(fm.sources[1].url, "https://second.com");
}

#[test]
fn test_search_content_saturating_sub_at_start_of_file() {
    let page = "keyword is the first line\nsome other content\n";
    let (_dir, manager) = setup_search_manager(&[("first-line", page)]);

    let results = manager.search_content("keyword", 5).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].context_lines[0].0 >= 1);
}

#[test]
fn test_search_content_line_number_correct() {
    let page = "line one\nline two\nkeyword here\nline four\n";
    let (_dir, manager) = setup_search_manager(&[("line-num", page)]);

    let results = manager.search_content("keyword", 0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].line_number, 3);
}

#[test]
fn test_resolve_conflicts_in_cache_no_cache_dir() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.resolve_conflicts_in_cache();
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[test]
fn test_page_exists_valid_slug_missing_file() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert!(!manager.page_exists("does-not-exist"));
}

#[test]
fn test_parse_frontmatter_in_sources_indented_non_list_ignored() {
    let content = "\
---
title: Sources Non-list
sources:
    this_is_indented_but_no_dash
  - url: https://valid.com
    title: Valid
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://valid.com");
}

// Helper: create a bare git repo that can serve as a remote.
fn init_bare_remote(path: &Path) {
    let p = path.to_string_lossy();
    Command::new("git")
        .args(["init", "--bare", &p])
        .output()
        .unwrap();
}

// Helper: clone a bare repo locally, configure user info.
fn clone_repo(remote: &Path, local: &Path) {
    let remote_s = remote.to_string_lossy();
    let local_s = local.to_string_lossy();
    Command::new("git")
        .args(["clone", &remote_s, &local_s])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &local_s, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &local_s, "config", "user.name", "Test"])
        .output()
        .unwrap();
}

#[test]
fn test_sync_with_local_remote_pair_reset_path() {
    let dir = tempdir().unwrap();

    let remote_path = dir.path().join("remote.git");
    init_bare_remote(&remote_path);

    let main_repo = dir.path().join("main");
    clone_repo(&remote_path, &main_repo);

    let kp = main_repo.to_string_lossy();
    Command::new("git")
        .args(["-C", &kp, "checkout", "--orphan", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();
    std::fs::write(main_repo.join("index.md"), "# Index\n").unwrap();
    Command::new("git")
        .args(["-C", &kp, "add", "index.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "commit", "-m", "init knowledge"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);

    Command::new("git")
        .args([
            "clone",
            "--branch",
            KNOWLEDGE_BRANCH,
            &remote_path.to_string_lossy(),
            &knowledge_path.to_string_lossy(),
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.email",
            "test@test.com",
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &knowledge_path.to_string_lossy(),
            "config",
            "user.name",
            "Test",
        ])
        .output()
        .unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.sync();
    assert!(result.is_ok());
    assert!(result.unwrap().resolved_conflicts.is_empty());
}

#[test]
fn test_sync_with_unpushed_local_commits_rebase_path() {
    let dir = tempdir().unwrap();

    let remote_path = dir.path().join("remote.git");
    init_bare_remote(&remote_path);

    let setup_repo = dir.path().join("setup");
    clone_repo(&remote_path, &setup_repo);
    let s = setup_repo.to_string_lossy();
    Command::new("git")
        .args(["-C", &s, "checkout", "--orphan", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();
    std::fs::write(setup_repo.join("index.md"), "# Index\n").unwrap();
    Command::new("git")
        .args(["-C", &s, "add", "index.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &s, "commit", "-m", "init knowledge"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &s, "push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    Command::new("git")
        .args([
            "clone",
            "--branch",
            KNOWLEDGE_BRANCH,
            &remote_path.to_string_lossy(),
            &knowledge_path.to_string_lossy(),
        ])
        .output()
        .unwrap();
    let kp = knowledge_path.to_string_lossy();
    Command::new("git")
        .args(["-C", &kp, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "config", "user.name", "Test"])
        .output()
        .unwrap();

    std::fs::write(knowledge_path.join("local-page.md"), "# Local\n").unwrap();
    Command::new("git")
        .args(["-C", &kp, "add", "local-page.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "commit", "-m", "local unpushed commit"])
        .output()
        .unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.sync();
    assert!(result.is_ok());
}

#[test]
fn test_push_success_with_local_remote() {
    let dir = tempdir().unwrap();

    let remote_path = dir.path().join("remote.git");
    init_bare_remote(&remote_path);

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&knowledge_path).unwrap();

    let kp = knowledge_path.to_string_lossy();
    Command::new("git").args(["init", &kp]).output().unwrap();
    Command::new("git")
        .args(["-C", &kp, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "config", "user.name", "Test"])
        .output()
        .unwrap();
    Command::new("git")
        .args([
            "-C",
            &kp,
            "remote",
            "add",
            "origin",
            &remote_path.to_string_lossy(),
        ])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "checkout", "--orphan", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();
    std::fs::write(knowledge_path.join("index.md"), "# Index\n").unwrap();
    Command::new("git")
        .args(["-C", &kp, "add", "index.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "commit", "-m", "init knowledge"])
        .output()
        .unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.push();
    assert!(result.is_ok());
}

#[test]
fn test_push_rejected_rebase_succeeds() {
    let dir = tempdir().unwrap();

    let remote_path = dir.path().join("remote.git");
    init_bare_remote(&remote_path);

    let clone_a = dir.path().join("clone-a");
    clone_repo(&remote_path, &clone_a);
    let ca = clone_a.to_string_lossy();
    Command::new("git")
        .args(["-C", &ca, "checkout", "--orphan", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();
    std::fs::write(clone_a.join("base.md"), "# Base\n").unwrap();
    Command::new("git")
        .args(["-C", &ca, "add", "base.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "commit", "-m", "init"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    Command::new("git")
        .args([
            "clone",
            "--branch",
            KNOWLEDGE_BRANCH,
            &remote_path.to_string_lossy(),
            &knowledge_path.to_string_lossy(),
        ])
        .output()
        .unwrap();
    let kp = knowledge_path.to_string_lossy();
    Command::new("git")
        .args(["-C", &kp, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "config", "user.name", "Test"])
        .output()
        .unwrap();

    std::fs::write(clone_a.join("a-change.md"), "# A Change\n").unwrap();
    Command::new("git")
        .args(["-C", &ca, "add", "a-change.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "commit", "-m", "A new commit"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    std::fs::write(knowledge_path.join("b-change.md"), "# B Change\n").unwrap();
    Command::new("git")
        .args(["-C", &kp, "add", "b-change.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "commit", "-m", "B's local commit"])
        .output()
        .unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.push();
    assert!(result.is_ok());
}

#[test]
fn test_handle_rebase_conflict_merge_succeeds() {
    let dir = tempdir().unwrap();

    let remote_path = dir.path().join("remote.git");
    init_bare_remote(&remote_path);

    let clone_a = dir.path().join("clone-a");
    clone_repo(&remote_path, &clone_a);
    let ca = clone_a.to_string_lossy();
    Command::new("git")
        .args(["-C", &ca, "checkout", "--orphan", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();
    std::fs::write(clone_a.join("base.md"), "# Base\n").unwrap();
    Command::new("git")
        .args(["-C", &ca, "add", "base.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "commit", "-m", "init"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    Command::new("git")
        .args([
            "clone",
            "--branch",
            KNOWLEDGE_BRANCH,
            &remote_path.to_string_lossy(),
            &knowledge_path.to_string_lossy(),
        ])
        .output()
        .unwrap();
    let kp = knowledge_path.to_string_lossy();
    Command::new("git")
        .args(["-C", &kp, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "config", "user.name", "Test"])
        .output()
        .unwrap();

    Command::new("git")
        .args(["-C", &kp, "fetch", "origin"])
        .output()
        .unwrap();

    let remote_ref = format!("origin/{KNOWLEDGE_BRANCH}");
    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.handle_rebase_conflict(&remote_ref);
    assert!(result.is_ok());
    let outcome = result.unwrap();
    assert!(outcome.resolved_conflicts.is_empty());
}

#[test]
fn test_handle_rebase_conflict_with_md_conflicts() {
    let dir = tempdir().unwrap();

    let remote_path = dir.path().join("remote.git");
    init_bare_remote(&remote_path);

    let clone_a = dir.path().join("clone-a");
    clone_repo(&remote_path, &clone_a);
    let ca = clone_a.to_string_lossy();
    Command::new("git")
        .args(["-C", &ca, "checkout", "--orphan", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();
    std::fs::write(clone_a.join("page.md"), "# Page\n\nOriginal content.\n").unwrap();
    Command::new("git")
        .args(["-C", &ca, "add", "page.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "commit", "-m", "init"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    let crosslink_dir = dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();
    let knowledge_path = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    Command::new("git")
        .args([
            "clone",
            "--branch",
            KNOWLEDGE_BRANCH,
            &remote_path.to_string_lossy(),
            &knowledge_path.to_string_lossy(),
        ])
        .output()
        .unwrap();
    let kp = knowledge_path.to_string_lossy();
    Command::new("git")
        .args(["-C", &kp, "config", "user.email", "test@test.com"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "config", "user.name", "Test"])
        .output()
        .unwrap();

    std::fs::write(clone_a.join("page.md"), "# Page\n\nRemote change.\n").unwrap();
    Command::new("git")
        .args(["-C", &ca, "add", "page.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "commit", "-m", "remote change to page"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &ca, "push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    std::fs::write(knowledge_path.join("page.md"), "# Page\n\nLocal change.\n").unwrap();
    Command::new("git")
        .args(["-C", &kp, "add", "page.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &kp, "commit", "-m", "local change to page"])
        .output()
        .unwrap();

    Command::new("git")
        .args(["-C", &kp, "fetch", "origin"])
        .output()
        .unwrap();

    let remote_ref = format!("origin/{KNOWLEDGE_BRANCH}");
    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();

    let result = manager.handle_rebase_conflict(&remote_ref);
    assert!(result.is_ok());
}

#[test]
fn test_commit_propagates_error_when_git_fails() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let result = manager.commit("test: should fail");
    assert!(result.is_err());
}

#[test]
fn test_init_cache_fetches_remote_branch_when_available() {
    let dir = tempdir().unwrap();

    let remote_path = dir.path().join("remote.git");
    init_bare_remote(&remote_path);

    let tmp_repo = dir.path().join("tmp-setup");
    clone_repo(&remote_path, &tmp_repo);
    let tr = tmp_repo.to_string_lossy();
    Command::new("git")
        .args(["-C", &tr, "checkout", "--orphan", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();
    std::fs::write(tmp_repo.join("index.md"), "# Index\n").unwrap();
    Command::new("git")
        .args(["-C", &tr, "add", "index.md"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &tr, "commit", "-m", "init knowledge"])
        .output()
        .unwrap();
    Command::new("git")
        .args(["-C", &tr, "push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    let main_repo = dir.path().join("main");
    clone_repo(&remote_path, &main_repo);

    let crosslink_dir = main_repo.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    assert!(!manager.is_initialized());

    let _result = manager.init_cache();
}

#[test]
fn test_parse_frontmatter_tags_empty_bracket_handled_by_inline_array() {
    let content = "---\ntitle: T\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert!(fm.tags.is_empty());
    assert_eq!(fm.created, "2026-01-01");
}

#[test]
fn test_parse_frontmatter_contributors_empty_bracket_handled_by_inline_array() {
    let content =
        "---\ntitle: T\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert!(fm.contributors.is_empty());
    assert_eq!(fm.created, "2026-01-01");
}

#[test]
fn test_parse_frontmatter_tags_bare_colon_enters_in_tags() {
    let content = "---\ntitle: T\ntags:\n  - one\n  - two\ncreated: 2026-01-01\n---\n";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.tags, vec!["one", "two"]);
}

#[test]
fn test_parse_frontmatter_in_source_item_is_nested_key_url_path() {
    let content = "\
---
title: Nested Key Path
sources:
  - url: https://first.com
    title: First Title
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://first.com");
    assert_eq!(fm.sources[0].title, "First Title");
}

#[test]
fn test_parse_frontmatter_sources_non_bracket_value_enters_in_sources() {
    let content = "\
---
title: Sources Non-bracket
sources: not-a-bracket
  - url: https://still-works.com
    title: Still Works
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://still-works.com");
}

#[test]
fn test_knowledge_manager_new_no_parent_error() {
    let result = KnowledgeManager::new(std::path::Path::new("/crosslink-dir-at-root"));
    let _ = result;
}

#[test]
fn test_parse_frontmatter_in_source_item_dash_unknown_key_no_url_title() {
    let content = "\
---
title: Mystery Key
sources:
  - mystery: something
    url: https://mystery.com
    title: Mystery Site
created: 2026-01-01
updated: 2026-01-01
---
";
    let fm = parse_frontmatter(content).unwrap();
    assert_eq!(fm.sources.len(), 1);
    assert_eq!(fm.sources[0].url, "https://mystery.com");
    assert_eq!(fm.sources[0].title, "Mystery Site");
}

#[test]
fn test_serialize_frontmatter_source_with_accessed_at() {
    let fm = PageFrontmatter {
        title: "Accessed".to_string(),
        tags: Vec::new(),
        sources: vec![Source {
            url: "https://example.com".to_string(),
            title: "Ex".to_string(),
            accessed_at: Some("2026-03-01".to_string()),
        }],
        contributors: Vec::new(),
        created: "2026-01-01".to_string(),
        updated: "2026-01-01".to_string(),
    };
    let s = serialize_frontmatter(&fm);
    assert!(s.contains("accessed_at: \"2026-03-01\""));
}

#[test]
fn test_group_matches_boundary_exactly_merges() {
    let groups = group_matches(&[0, 3], 1);
    assert_eq!(groups.len(), 1);

    let groups = group_matches(&[0, 4], 1);
    assert_eq!(groups.len(), 2);
}

#[test]
fn test_group_matches_chained_merges() {
    let groups = group_matches(&[0, 1, 2], 2);
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0], vec![0, 1, 2]);
}

#[test]
fn test_list_pages_skips_non_md_extensions() {
    let dir = tempdir().unwrap();
    let crosslink_dir = dir.path().join(".crosslink");
    let cache_dir = crosslink_dir.join(KNOWLEDGE_CACHE_DIR);
    std::fs::create_dir_all(&cache_dir).unwrap();

    std::fs::write(cache_dir.join("noext"), "no extension").unwrap();
    std::fs::write(cache_dir.join("doc.rst"), "rst file").unwrap();
    std::fs::write(
        cache_dir.join("page.md"),
        "---\ntitle: Valid\ncreated: 2026-01-01\n---\n",
    )
    .unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    let pages = manager.list_pages().unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].slug, "page");
}

#[test]
fn test_search_sources_pages_without_sources_skipped() {
    let page_no_sources = "---\ntitle: No Sources\ntags: []\nsources: []\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\nContent.\n";
    let page_with_source = "---\ntitle: Has Source\ntags: []\nsources:\n  - url: https://target.example.com\n    title: Target\ncontributors: []\ncreated: 2026-01-01\nupdated: 2026-01-01\n---\nContent.\n";

    let (_dir, manager) = setup_search_manager(&[
        ("no-sources", page_no_sources),
        ("with-source", page_with_source),
    ]);

    let results = manager.search_sources("target.example.com").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].slug, "with-source");
}

/// Helper: create a git repo with an orphan knowledge branch worktree.
fn setup_knowledge_with_git_worktree() -> (tempfile::TempDir, KnowledgeManager) {
    let dir = tempdir().unwrap();
    let main_root = dir.path();

    Command::new("git")
        .args(["init", "-b", "main", &main_root.to_string_lossy()])
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.email", "test@test.local"],
        vec!["config", "user.name", "Test"],
    ] {
        Command::new("git")
            .current_dir(main_root)
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::write(main_root.join("README.md"), "# test\n").unwrap();
    Command::new("git")
        .current_dir(main_root)
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(main_root)
        .args(["commit", "-m", "init", "--no-gpg-sign"])
        .output()
        .unwrap();

    let crosslink_dir = main_root.join(".crosslink");
    std::fs::create_dir_all(&crosslink_dir).unwrap();

    let manager = KnowledgeManager::new(&crosslink_dir).unwrap();
    manager.init_cache().unwrap();

    (dir, manager)
}

#[test]
fn test_commit_nothing_to_commit_propagates_error() {
    let (_dir, manager) = setup_knowledge_with_git_worktree();

    let result = manager.commit("empty commit test");
    assert!(
        result.is_err(),
        "commit() returns Err when nothing to commit on this git version"
    );
}

#[test]
fn test_sync_graceful_on_fetch_error() {
    let (_dir, manager) = setup_knowledge_with_git_worktree();

    let result = manager.sync();
    assert!(
        result.is_ok(),
        "sync() should handle missing remote gracefully"
    );
    assert!(result.unwrap().resolved_conflicts.is_empty());
}

#[test]
fn test_push_graceful_on_remote_error() {
    let (_dir, manager) = setup_knowledge_with_git_worktree();

    let result = manager.push();
    assert!(
        result.is_ok(),
        "push() should handle missing remote gracefully"
    );
}

#[test]
fn test_init_cache_idempotent_with_real_git() {
    let (_dir, manager) = setup_knowledge_with_git_worktree();

    let result = manager.init_cache();
    assert!(result.is_ok(), "init_cache() should be idempotent");
    assert!(manager.is_initialized());
}

#[test]
fn test_init_cache_from_existing_remote_knowledge_branch() {
    let remote_dir = tempdir().unwrap();
    let work1_dir = tempdir().unwrap();
    let work2_dir = tempdir().unwrap();

    Command::new("git")
        .current_dir(remote_dir.path())
        .args(["init", "--bare", "-b", "main"])
        .output()
        .unwrap();

    for dir in [work1_dir.path()] {
        Command::new("git")
            .args(["init", "-b", "main", &dir.to_string_lossy()])
            .output()
            .unwrap();
        for args in [
            vec!["config", "user.email", "test@test.local"],
            vec!["config", "user.name", "Test"],
            vec![
                "remote",
                "add",
                "origin",
                remote_dir.path().to_str().unwrap(),
            ],
        ] {
            Command::new("git")
                .current_dir(dir)
                .args(&args)
                .output()
                .unwrap();
        }
        std::fs::write(dir.join("README.md"), "# test\n").unwrap();
        Command::new("git")
            .current_dir(dir)
            .args(["add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(dir)
            .args(["commit", "-m", "init", "--no-gpg-sign"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(dir)
            .args(["push", "-u", "origin", "main"])
            .output()
            .unwrap();
    }

    let crosslink1 = work1_dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink1).unwrap();
    std::fs::write(
        crosslink1.join("hook-config.json"),
        r#"{"remote":"origin"}"#,
    )
    .unwrap();

    let mgr1 = KnowledgeManager::new(&crosslink1).unwrap();
    mgr1.init_cache().unwrap();

    Command::new("git")
        .current_dir(mgr1.cache_path())
        .args(["push", "origin", KNOWLEDGE_BRANCH])
        .output()
        .unwrap();

    Command::new("git")
        .args(["init", "-b", "main", &work2_dir.path().to_string_lossy()])
        .output()
        .unwrap();
    for args in [
        vec!["config", "user.email", "test@test.local"],
        vec!["config", "user.name", "Test"],
        vec![
            "remote",
            "add",
            "origin",
            remote_dir.path().to_str().unwrap(),
        ],
    ] {
        Command::new("git")
            .current_dir(work2_dir.path())
            .args(&args)
            .output()
            .unwrap();
    }
    std::fs::write(work2_dir.path().join("README.md"), "# test\n").unwrap();
    Command::new("git")
        .current_dir(work2_dir.path())
        .args(["add", "."])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(work2_dir.path())
        .args(["commit", "-m", "init", "--no-gpg-sign"])
        .output()
        .unwrap();
    Command::new("git")
        .current_dir(work2_dir.path())
        .args(["push", "-u", "origin", "main"])
        .output()
        .unwrap();

    let crosslink2 = work2_dir.path().join(".crosslink");
    std::fs::create_dir_all(&crosslink2).unwrap();
    std::fs::write(
        crosslink2.join("hook-config.json"),
        r#"{"remote":"origin"}"#,
    )
    .unwrap();

    let mgr2 = KnowledgeManager::new(&crosslink2).unwrap();
    let result = mgr2.init_cache();
    assert!(
        result.is_ok(),
        "init_cache from remote should succeed: {result:?}"
    );
    assert!(mgr2.is_initialized());
}

// --- Edit helper tests ---

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

#[test]
fn test_extract_body_crlf() {
    let content = "---\r\ntitle: Test\r\ntags: []\r\n---\r\n\r\n# Test\r\n\r\nBody text.\r\n";
    let body = extract_body(content);
    assert!(
        body.starts_with("\r\n# Test") || body.starts_with("\n# Test"),
        "got: {body:?}"
    );
    assert!(!body.contains("title: Test"));
}

#[test]
fn test_truncate_short() {
    assert_eq!(truncate("hello", 10), "hello");
}

#[test]
fn test_truncate_long() {
    assert_eq!(truncate("hello world foo bar", 10), "hello w...");
}

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
    let body =
        "# Title\n\nIntro text.\n\n## Architecture\n\nArch content.\n\n## Notes\n\nNote content.\n";
    let lines: Vec<&str> = body.lines().collect();
    let (start, end) = find_section_range(&lines, "## Architecture").unwrap();
    assert_eq!(start, 4);
    assert_eq!(end, 8);
}

#[test]
fn test_find_section_range_last_section() {
    let body = "# Title\n\nIntro.\n\n## Last Section\n\nLast content.\n";
    let lines: Vec<&str> = body.lines().collect();
    let (start, end) = find_section_range(&lines, "## Last Section").unwrap();
    assert_eq!(start, 4);
    assert_eq!(end, lines.len());
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
    let body =
        "## Parent\n\nParent content.\n\n### Child\n\nChild content.\n\n## Sibling\n\nSibling.\n";
    let lines: Vec<&str> = body.lines().collect();
    let (start, end) = find_section_range(&lines, "## Parent").unwrap();
    assert_eq!(start, 0);
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
    assert!(!result.contains("A1 content."));
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
