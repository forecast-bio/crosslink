use axum::{routing::get, Router};

use crate::server::{
    handlers::{
        agents::{get_agent, get_agent_status, list_agents, list_locks, list_stale_locks},
        health::health,
    },
    state::AppState,
};

/// Build the full axum router with all API routes and static file serving.
pub fn build_router(state: AppState, dashboard_dir: Option<std::path::PathBuf>) -> Router {
    let api = Router::new()
        .route("/health", get(health))
        // Agent monitoring
        .route("/agents", get(list_agents))
        .route("/agents/{id}", get(get_agent))
        .route("/agents/{id}/status", get(get_agent_status))
        // Locks
        .route("/locks", get(list_locks))
        .route("/locks/stale", get(list_stale_locks));

    let mut app = Router::new().nest("/api/v1", api).with_state(state);

    // Serve static dashboard files if a directory was provided.
    if let Some(dir) = dashboard_dir {
        use tower_http::services::ServeDir;
        app = app.fallback_service(ServeDir::new(dir));
    }

    app
}
