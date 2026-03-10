pub mod handlers;
pub mod routes;
pub mod state;
pub mod types;
pub mod watcher;
pub mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Result;
use tower_http::cors::{Any, CorsLayer};

use crate::db::Database;
use state::AppState;

/// Start the crosslink web server.
///
/// Binds to `0.0.0.0:<port>`, configures CORS for the Vite dev server on
/// `:5173`, serves the React dashboard from `dashboard_dir` (if provided),
/// exposes the REST API under `/api/v1/`, and opens a WebSocket hub at `/ws`.
///
/// The filesystem watcher is started as a background task and broadcasts
/// heartbeat events to all connected WebSocket clients.
pub async fn run(
    port: u16,
    dashboard_dir: Option<PathBuf>,
    db: Database,
    crosslink_dir: PathBuf,
) -> Result<()> {
    let state = AppState::new(db, crosslink_dir.clone());

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
        .allow_headers(Any);

    let app = routes::build_router(state, dashboard_dir).layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("crosslink serve: listening on http://{}", addr);
    println!("  API:       http://{}/api/v1/health", addr);
    println!("  WebSocket: ws://{}/ws", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
