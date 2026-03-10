pub mod handlers;
pub mod routes;
pub mod state;

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
/// and exposes the REST API under `/api/v1/`.
pub async fn run(
    port: u16,
    dashboard_dir: Option<PathBuf>,
    db: Database,
    crosslink_dir: PathBuf,
) -> Result<()> {
    let state = AppState::new(db, crosslink_dir);

    // Allow the Vite dev server (port 5173) and same-origin requests in
    // development. In production the dashboard is served from the same origin
    // so only the same-origin case matters, but permitting all origins here
    // keeps the dev-only setup simple (this server is localhost-only by design).
    let cors = CorsLayer::new()
        .allow_origin([
            "http://localhost:5173".parse().unwrap(),
            "http://127.0.0.1:5173".parse().unwrap(),
        ])
        .allow_methods(tower_http::cors::Any)
        .allow_headers(Any);

    let app = routes::build_router(state, dashboard_dir).layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("crosslink serve: listening on http://{}", addr);
    println!("  API: http://{}/api/v1/health", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
