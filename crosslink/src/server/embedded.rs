//! Embedded dashboard assets.
//!
//! The React dashboard lives at `<repo>/dashboard/`; its built output
//! (`<repo>/dashboard/dist/`) is embedded into the binary at compile
//! time via [`rust_embed::RustEmbed`]. `crosslink dashboard` serves
//! these assets as a fallback for any request that doesn't match an
//! API or WebSocket route.
//!
//! The embed lives in this binary so `cargo install crosslink` ships a
//! working dashboard (GH #429). For development, the `--dashboard-dir`
//! override on `crosslink dashboard` bypasses the embed and serves
//! from disk instead.
//!
//! See `DESIGN-CROSSLINK-DASHBOARD.md` §12 for the distribution story.

use axum::{
    body::Body,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../dashboard/dist/"]
struct DashboardAssets;

/// Fallback handler that serves the embedded SPA bundle.
///
/// - Exact-path hits (e.g. `/assets/index-abc.js`) return the matching
///   asset with its MIME type guessed from the path extension.
/// - Any unmatched path falls through to `index.html` (SPA fallback —
///   the React router handles client-side routes).
/// - If even `index.html` is missing (impossible after a successful
///   build, but defended against anyway), return a 404.
pub async fn serve_embedded(uri: Uri) -> Response {
    let raw_path = uri.path().trim_start_matches('/');

    // The fallback handler runs whenever no nested route matched. For
    // `/api/*` and `/ws/*` that means "unknown API/WS endpoint" — those
    // must return 404, NOT fall through to the SPA. Only non-API paths
    // get the index.html fallback so client-side routing (e.g. `/alerts`,
    // `/project/foo`) works on direct navigation.
    if raw_path.starts_with("api/") || raw_path == "api" || raw_path.starts_with("ws") {
        return StatusCode::NOT_FOUND.into_response();
    }

    let path = if raw_path.is_empty() {
        "index.html"
    } else {
        raw_path
    };

    if let Some(asset) = DashboardAssets::get(path) {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        return Response::builder()
            .header(header::CONTENT_TYPE, mime.as_ref())
            .body(Body::from(asset.data.into_owned()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    // SPA fallback: any unmatched non-API path serves index.html so
    // client-side routes work on direct navigation.
    if let Some(index) = DashboardAssets::get("index.html") {
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .body(Body::from(index.data.into_owned()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    StatusCode::NOT_FOUND.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_html_is_embedded() {
        // After `cargo build` runs, dashboard/dist/index.html exists
        // (either the real build output or build.rs's placeholder).
        // Either way, rust-embed must have picked it up.
        assert!(
            DashboardAssets::get("index.html").is_some(),
            "dashboard/dist/index.html must be embedded — run \
             `npm --prefix dashboard run build` before `cargo build`"
        );
    }

    #[tokio::test]
    async fn test_serve_embedded_returns_index_for_root() {
        let uri: Uri = "/".parse().unwrap();
        let resp = serve_embedded(uri).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            ct.starts_with("text/html"),
            "root should serve HTML, got: {ct}"
        );
    }

    #[tokio::test]
    async fn test_serve_embedded_spa_fallback_for_unknown_path() {
        // Unknown paths fall back to index.html (SPA routing).
        let uri: Uri = "/some/deep/client/route".parse().unwrap();
        let resp = serve_embedded(uri).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(ct.starts_with("text/html"));
    }

    #[tokio::test]
    async fn test_serve_embedded_returns_404_for_unknown_api_path() {
        // Crucial: the SPA fallback MUST NOT intercept unknown API paths.
        // Those should reach the caller as a clean 404 so API consumers
        // don't mistake an HTML index.html for a JSON API response.
        let uri: Uri = "/api/v1/nonexistent".parse().unwrap();
        let resp = serve_embedded(uri).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_serve_embedded_returns_404_for_unknown_ws_path() {
        let uri: Uri = "/ws/unknown".parse().unwrap();
        let resp = serve_embedded(uri).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
