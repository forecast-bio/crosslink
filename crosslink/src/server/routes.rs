use axum::{routing::get, Router};

use crate::server::{handlers::health::health, state::AppState, ws::ws_handler};

/// Build the full axum router with all API routes and static file serving.
pub fn build_router(state: AppState, dashboard_dir: Option<std::path::PathBuf>) -> Router {
    let api = Router::new().route("/health", get(health));

    let mut app = Router::new()
        .nest("/api/v1", api)
        .route("/ws", get(ws_handler))
        .with_state(state);

    // Serve static dashboard files if a directory was provided.
    if let Some(dir) = dashboard_dir {
        use tower_http::services::ServeDir;
        app = app.fallback_service(ServeDir::new(dir));
    }

    app
}
