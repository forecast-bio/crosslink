pub mod embedded;
pub mod errors;
pub mod handlers;
pub mod routes;
pub mod state;
pub mod types;
pub mod watcher;
pub mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use axum::extract::DefaultBodyLimit;
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use tower_http::cors::CorsLayer;

use crate::db::Database;
use state::AppState;

/// Maximum allowed request body size (10 MB).
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

/// Bearer token authentication middleware.
///
/// Exempts `/api/v1/health` (read-only status) and `/ws` (WebSocket, uses
/// its own protocol-level auth if needed). All other `/api/` routes require
/// a valid `Authorization: Bearer <token>` header.
async fn auth_middleware(
    axum::extract::State(state): axum::extract::State<AppState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = request.uri().path();

    // Exempt health check and WebSocket from auth
    if path == "/api/v1/health" || path == "/ws" || !path.starts_with("/api/") {
        return Ok(next.run(request).await);
    }

    let authorized = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|token| token == state.auth_token);

    if authorized {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Start the crosslink web server.
///
/// Binds to `127.0.0.1:<port>`, configures CORS for the Vite dev server on
/// `:5173`, serves the React dashboard from `dashboard_dir` (if provided),
/// exposes the REST API under `/api/v1/`, and opens a WebSocket hub at `/ws`.
///
/// The filesystem watcher is started as a background task and broadcasts
/// heartbeat events to all connected WebSocket clients.
///
/// # Errors
///
/// Returns an error if the server fails to bind or encounters a runtime error.
pub async fn run(
    port: u16,
    dashboard_dir: Option<PathBuf>,
    db: Database,
    crosslink_dir: PathBuf,
) -> Result<()> {
    run_with_dashboard_db(port, dashboard_dir, db, crosslink_dir, None).await
}

/// Variant of [`run`] that additionally registers a per-user dashboard
/// `SQLite` path with `AppState`, enabling the `/api/v1/dashboard` API
/// routes (GH #429). `crosslink dashboard serve` uses this variant;
/// the deprecated `crosslink serve` passes `None` via [`run`].
///
/// # Errors
/// As [`run`].
pub async fn run_with_dashboard_db(
    port: u16,
    dashboard_dir: Option<PathBuf>,
    db: Database,
    crosslink_dir: PathBuf,
    dashboard_db_path: Option<PathBuf>,
) -> Result<()> {
    let mut state = AppState::new(db, crosslink_dir.clone());

    // When a dashboard DB is configured, spawn the 5-second poll loop
    // alongside the server and wire its broadcast sender to the same
    // channel the WebSocket hub fanouts from (state.ws_tx). That way
    // `WsEvent::DashboardProjectUpdated` events emitted by the poll
    // loop reach connected WS clients without any extra plumbing.
    let poll_handle = if let Some(path) = dashboard_db_path {
        state = state.with_dashboard_db(path.clone());
        let cancel = tokio_util::sync::CancellationToken::new();
        let cancel_clone = cancel.clone();
        let tx = state.ws_tx.clone();
        let handle = tokio::spawn(async move {
            crate::dashboard::poll::run(path, cancel_clone, Some(tx)).await;
        });
        Some((cancel, handle))
    } else {
        None
    };

    // Start the heartbeat watcher in the background.
    watcher::start_watcher(crosslink_dir, state.ws_tx.clone());

    // Allow the Vite dev server (port 5173) and same-origin requests in
    // development. In production the dashboard is served from the same origin
    // so only the same-origin case matters, but permitting all origins here
    // keeps the dev-only setup simple (this server is localhost-only by design).
    let localhost: axum::http::HeaderValue = "http://localhost:5173".parse()?;
    let loopback: axum::http::HeaderValue = "http://127.0.0.1:5173".parse()?;
    let cors = CorsLayer::new()
        .allow_origin([localhost, loopback])
        .allow_methods(tower_http::cors::Any)
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::header::ACCEPT,
        ]);

    // Remember whether a dashboard is being served so we can print a
    // clickable URL (with the bearer token baked in) at startup.
    let has_dashboard = dashboard_dir.is_some();

    let app = routes::build_router(state.clone(), dashboard_dir)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE))
        .layer(cors);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("crosslink dashboard: listening on http://{addr}");
    // The dashboard reads `?token=<value>` on first load, persists it
    // to sessionStorage, and strips it from the URL (see
    // `dashboard/src/auth/bootstrap.ts`). Subsequent reloads in the
    // same tab reuse the stored token.
    if has_dashboard {
        // --dashboard-dir override in effect: frontend served from disk.
        println!(
            "  Dashboard: http://{addr}/?token={}  (from --dashboard-dir)",
            state.auth_token
        );
    } else {
        // Default path: serve the embedded bundle from the binary.
        println!("  Dashboard: http://{addr}/?token={}", state.auth_token);
    }
    println!("  API:       http://{addr}/api/v1/health");
    println!("  WebSocket: ws://{addr}/ws");
    println!("  Auth:      Bearer {}", state.auth_token);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let serve_result = axum::serve(listener, app).await;

    // Shut down the poll loop cleanly when the server exits.
    if let Some((cancel, handle)) = poll_handle {
        cancel.cancel();
        let _ = handle.await;
    }

    serve_result?;
    Ok(())
}
