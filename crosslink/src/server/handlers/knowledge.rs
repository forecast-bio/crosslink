//! Handlers for knowledge page endpoints.
//!
//! Implements:
//! - `GET  /api/v1/knowledge`         — list all knowledge pages
//! - `POST /api/v1/knowledge`         — create a new knowledge page
//! - `GET  /api/v1/knowledge/search`  — full-text search across knowledge pages
//! - `GET  /api/v1/knowledge/:slug`   — read a single knowledge page by slug

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
};
use serde_json::Value;

use crate::{
    knowledge::{parse_frontmatter, KnowledgeManager},
    server::{
        state::AppState,
        types::{
            ApiError, CreateKnowledgePageRequest, KnowledgePage, KnowledgePageSummary,
            KnowledgeSearchMatch, KnowledgeSearchQuery, KnowledgeSource,
        },
    },
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn internal_error(context: &str, e: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: context.to_string(),
            detail: Some(e.to_string()),
        }),
    )
}

fn not_found(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: "not found".to_string(),
            detail: Some(msg.into()),
        }),
    )
}

fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "bad request".to_string(),
            detail: Some(msg.into()),
        }),
    )
}

/// Build a `KnowledgeManager` from the app state's crosslink directory.
fn knowledge_manager(state: &AppState) -> Result<KnowledgeManager, (StatusCode, Json<ApiError>)> {
    KnowledgeManager::new(&state.crosslink_dir)
        .map_err(|e| internal_error("Failed to initialize knowledge manager", e))
}

// ---------------------------------------------------------------------------
// GET /api/v1/knowledge
// ---------------------------------------------------------------------------

/// `GET /api/v1/knowledge` — list all knowledge pages with summary metadata.
pub async fn list_knowledge_pages(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    let km = knowledge_manager(&state)?;

    if !km.is_initialized() {
        return Ok(Json(serde_json::json!({ "items": [], "total": 0 })));
    }

    let pages = km
        .list_pages()
        .map_err(|e| internal_error("Failed to list knowledge pages", e))?;

    let items: Vec<KnowledgePageSummary> = pages
        .into_iter()
        .map(|p| KnowledgePageSummary {
            slug: p.slug,
            title: p.frontmatter.title,
            tags: p.frontmatter.tags,
            updated: p.frontmatter.updated,
        })
        .collect();

    let total = items.len();
    Ok(Json(serde_json::json!({ "items": items, "total": total })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/knowledge
// ---------------------------------------------------------------------------

/// `POST /api/v1/knowledge` — create a new knowledge page.
///
/// The request body must contain a `slug`, `title`, `content`, and optional
/// `tags` and `sources`. The handler constructs YAML frontmatter and writes
/// the page to the knowledge cache.
pub async fn create_knowledge_page(
    State(state): State<AppState>,
    Json(body): Json<CreateKnowledgePageRequest>,
) -> Result<(StatusCode, Json<KnowledgePage>), (StatusCode, Json<ApiError>)> {
    if body.slug.is_empty() {
        return Err(bad_request("slug cannot be empty"));
    }
    if body.title.is_empty() {
        return Err(bad_request("title cannot be empty"));
    }

    let km = knowledge_manager(&state)?;

    // Ensure the cache is initialized before writing.
    if !km.is_initialized() {
        km.init_cache()
            .map_err(|e| internal_error("Failed to initialize knowledge cache", e))?;
    }

    if km.page_exists(&body.slug) {
        return Err(bad_request(format!("Page '{}' already exists", body.slug)));
    }

    let now = chrono::Utc::now().format("%Y-%m-%d").to_string();

    // Build YAML frontmatter.
    let sources_yaml = if body.sources.is_empty() {
        "[]".to_string()
    } else {
        let entries: Vec<String> = body
            .sources
            .iter()
            .map(|s| {
                let mut entry = format!("  - url: \"{}\"\n    title: \"{}\"", s.url, s.title);
                if let Some(ref at) = s.accessed_at {
                    entry.push_str(&format!("\n    accessed_at: \"{}\"", at));
                }
                entry
            })
            .collect();
        format!("\n{}", entries.join("\n"))
    };

    let tags_yaml = if body.tags.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            body.tags
                .iter()
                .map(|t| format!("\"{}\"", t))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let page_content = format!(
        "---\ntitle: \"{}\"\ntags: {}\nsources: {}\ncontributors: []\ncreated: \"{}\"\nupdated: \"{}\"\n---\n\n{}",
        body.title, tags_yaml, sources_yaml, now, now, body.content
    );

    km.write_page(&body.slug, &page_content)
        .map_err(|e| internal_error("Failed to write knowledge page", e))?;

    // Commit the new page so it's tracked in git.
    let commit_msg = format!("Add knowledge page: {}", body.slug);
    let _ = km.commit(&commit_msg);

    let response = KnowledgePage {
        slug: body.slug,
        title: body.title,
        tags: body.tags,
        sources: body.sources,
        contributors: vec![],
        created: now.clone(),
        updated: now,
        content: body.content,
    };

    Ok((StatusCode::CREATED, Json(response)))
}

// ---------------------------------------------------------------------------
// GET /api/v1/knowledge/search
// ---------------------------------------------------------------------------

/// `GET /api/v1/knowledge/search?q=<query>` — search knowledge pages by content.
///
/// Returns matching snippets with context lines, ranked by term relevance.
pub async fn search_knowledge(
    State(state): State<AppState>,
    Query(params): Query<KnowledgeSearchQuery>,
) -> Result<Json<Value>, (StatusCode, Json<ApiError>)> {
    if params.q.trim().is_empty() {
        return Err(bad_request("Search query 'q' cannot be empty"));
    }

    let km = knowledge_manager(&state)?;

    if !km.is_initialized() {
        return Ok(Json(serde_json::json!({ "items": [], "total": 0 })));
    }

    // Use 2 lines of context around each match (same default as CLI).
    let matches = km
        .search_content(&params.q, 2)
        .map_err(|e| internal_error("Knowledge search failed", e))?;

    // Enrich each match with the page title from frontmatter.
    let pages = km
        .list_pages()
        .map_err(|e| internal_error("Failed to list pages for title lookup", e))?;

    let title_map: std::collections::HashMap<String, String> = pages
        .into_iter()
        .map(|p| (p.slug.clone(), p.frontmatter.title))
        .collect();

    let items: Vec<KnowledgeSearchMatch> = matches
        .into_iter()
        .map(|m| KnowledgeSearchMatch {
            title: title_map
                .get(&m.slug)
                .cloned()
                .unwrap_or_else(|| m.slug.clone()),
            slug: m.slug,
            line_number: m.line_number,
            context_lines: m.context_lines,
        })
        .collect();

    let total = items.len();
    Ok(Json(serde_json::json!({ "items": items, "total": total })))
}

// ---------------------------------------------------------------------------
// GET /api/v1/knowledge/:slug
// ---------------------------------------------------------------------------

/// `GET /api/v1/knowledge/:slug` — read a single knowledge page by slug.
///
/// Returns the full page content along with parsed frontmatter metadata.
pub async fn get_knowledge_page(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<KnowledgePage>, (StatusCode, Json<ApiError>)> {
    let km = knowledge_manager(&state)?;

    if !km.is_initialized() {
        return Err(not_found(format!("Page '{}' not found", slug)));
    }

    let raw = km.read_page(&slug).map_err(|e| {
        let msg = e.to_string();
        if msg.contains("not found") {
            not_found(format!("Page '{}' not found", slug))
        } else {
            internal_error("Failed to read knowledge page", e)
        }
    })?;

    let frontmatter = parse_frontmatter(&raw);

    let (title, tags, sources, contributors, created, updated) = match frontmatter {
        Some(fm) => {
            let sources: Vec<KnowledgeSource> = fm
                .sources
                .into_iter()
                .map(|s| KnowledgeSource {
                    url: s.url,
                    title: s.title,
                    accessed_at: s.accessed_at,
                })
                .collect();
            (
                fm.title,
                fm.tags,
                sources,
                fm.contributors,
                fm.created,
                fm.updated,
            )
        }
        None => (
            slug.clone(),
            vec![],
            vec![],
            vec![],
            String::new(),
            String::new(),
        ),
    };

    // Strip frontmatter block from content for the `content` field.
    let content = strip_frontmatter(&raw);

    Ok(Json(KnowledgePage {
        slug,
        title,
        tags,
        sources,
        contributors,
        created,
        updated,
        content,
    }))
}

/// Strip the YAML frontmatter block (between `---` delimiters) from raw markdown.
fn strip_frontmatter(raw: &str) -> String {
    let trimmed = raw.trim_start();
    if !trimmed.starts_with("---") {
        return raw.to_string();
    }
    // Find the closing `---` after the opening one.
    if let Some(end) = trimmed[3..].find("\n---") {
        let after = &trimmed[3 + end + 4..]; // skip past "\n---"
        after.trim_start_matches('\n').to_string()
    } else {
        raw.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, Router};
    use serde_json::Value;
    use tower::ServiceExt;

    fn test_state(tmp_dir: &std::path::Path) -> AppState {
        let db_path = tmp_dir.join("test.db");
        let db = crate::db::Database::open(&db_path).unwrap();
        let crosslink_dir = tmp_dir.join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        AppState::new(db, crosslink_dir)
    }

    fn build_router(state: AppState) -> Router {
        use axum::routing::{get, post};
        Router::new()
            .route("/knowledge/search", get(search_knowledge))
            .route(
                "/knowledge",
                get(list_knowledge_pages).post(create_knowledge_page),
            )
            .route("/knowledge/{slug}", get(get_knowledge_page))
            .with_state(state)
    }

    #[tokio::test]
    async fn test_list_empty_knowledge() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(tmp.path());
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/knowledge")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["items"].as_array().unwrap().len(), 0);
        assert_eq!(body["total"], 0);
    }

    #[tokio::test]
    async fn test_create_and_get_knowledge_page() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a minimal knowledge cache directory (skip git init for tests).
        let cache_dir = tmp.path().join(".crosslink").join(".knowledge-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let state = test_state(tmp.path());
        let app = build_router(state);

        // Create a page
        let create_body = serde_json::json!({
            "slug": "test-page",
            "title": "Test Page",
            "content": "Hello, world!",
            "tags": ["test", "example"],
            "sources": [{"url": "https://example.com", "title": "Example"}]
        });

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/knowledge")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&create_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["slug"], "test-page");
        assert_eq!(body["title"], "Test Page");

        // Read the page back
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/knowledge/test-page")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["slug"], "test-page");
        assert_eq!(body["title"], "Test Page");
        assert!(body["content"].as_str().unwrap().contains("Hello, world!"));
    }

    #[tokio::test]
    async fn test_get_nonexistent_page_returns_404() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join(".crosslink").join(".knowledge-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let state = test_state(tmp.path());
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/knowledge/nonexistent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_duplicate_page_returns_400() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join(".crosslink").join(".knowledge-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let state = test_state(tmp.path());
        let app = build_router(state);

        let create_body = serde_json::json!({
            "slug": "dup",
            "title": "Duplicate",
            "content": "First"
        });

        // Create first
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/knowledge")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&create_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // Create duplicate
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/knowledge")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&create_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_search_knowledge_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join(".crosslink").join(".knowledge-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        // Write a page directly to the cache for searching.
        let page = "---\ntitle: \"Rust Notes\"\ntags: [rust]\nsources: []\ncontributors: []\ncreated: \"2026-01-01\"\nupdated: \"2026-01-01\"\n---\n\nRust is a systems programming language.\nIt provides memory safety without garbage collection.\n";
        std::fs::write(cache_dir.join("rust-notes.md"), page).unwrap();

        let state = test_state(tmp.path());
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/knowledge/search?q=memory+safety")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(body["total"].as_u64().unwrap() > 0);
        assert_eq!(body["items"][0]["slug"], "rust-notes");
        assert_eq!(body["items"][0]["title"], "Rust Notes");
    }

    #[tokio::test]
    async fn test_search_empty_query_returns_400() {
        let tmp = tempfile::tempdir().unwrap();
        let state = test_state(tmp.path());
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/knowledge/search?q=")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_list_knowledge_pages_after_create() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_dir = tmp.path().join(".crosslink").join(".knowledge-cache");
        std::fs::create_dir_all(&cache_dir).unwrap();

        // Write two pages directly.
        let page_a = "---\ntitle: \"Alpha\"\ntags: []\nsources: []\ncontributors: []\ncreated: \"2026-01-01\"\nupdated: \"2026-01-01\"\n---\n\nAlpha page.\n";
        let page_b = "---\ntitle: \"Beta\"\ntags: [test]\nsources: []\ncontributors: []\ncreated: \"2026-01-02\"\nupdated: \"2026-01-02\"\n---\n\nBeta page.\n";
        std::fs::write(cache_dir.join("alpha.md"), page_a).unwrap();
        std::fs::write(cache_dir.join("beta.md"), page_b).unwrap();

        let state = test_state(tmp.path());
        let app = build_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/knowledge")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["total"], 2);
        let items = body["items"].as_array().unwrap();
        // Pages are sorted by slug.
        assert_eq!(items[0]["slug"], "alpha");
        assert_eq!(items[1]["slug"], "beta");
    }

    #[test]
    fn test_strip_frontmatter() {
        let raw = "---\ntitle: Test\ntags: []\n---\n\nBody text here.";
        let stripped = strip_frontmatter(raw);
        assert_eq!(stripped, "Body text here.");
    }

    #[test]
    fn test_strip_frontmatter_no_frontmatter() {
        let raw = "Just plain text.";
        let stripped = strip_frontmatter(raw);
        assert_eq!(stripped, "Just plain text.");
    }
}
