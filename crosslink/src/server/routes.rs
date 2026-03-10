use axum::{routing::get, Router};

use crate::server::{
    handlers::{
        agents::{get_agent, get_agent_status, list_agents, list_locks, list_stale_locks},
        health::health,
        issues::{
            add_blocker, add_comment, add_label, close_issue, create_issue, create_subissue,
            delete_issue, get_issue, list_blocked, list_comments, list_issues, list_ready,
            remove_blocker, remove_label, reopen_issue, update_issue,
        },
        knowledge::{
            create_knowledge_page, get_knowledge_page, list_knowledge_pages, search_knowledge,
        },
        milestones::{
            assign_milestone, close_milestone, create_milestone, get_milestone, list_milestones,
        },
        search::global_search,
        sessions::{end_session, get_current_session, start_session, work_on_issue},
    },
    state::AppState,
    ws::ws_handler,
};

/// Build the full axum router with all API routes and static file serving.
pub fn build_router(state: AppState, dashboard_dir: Option<std::path::PathBuf>) -> Router {
    use axum::routing::{delete, post};

    let api = Router::new()
        .route("/health", get(health))
        // Agent monitoring
        .route("/agents", get(list_agents))
        .route("/agents/{id}", get(get_agent))
        .route("/agents/{id}/status", get(get_agent_status))
        // Locks
        .route("/locks", get(list_locks))
        .route("/locks/stale", get(list_stale_locks))
        // Issues — static paths first to avoid conflict with /{id}
        .route("/issues/blocked", get(list_blocked))
        .route("/issues/ready", get(list_ready))
        // Issues — CRUD
        .route("/issues", get(list_issues).post(create_issue))
        .route(
            "/issues/{id}",
            get(get_issue).patch(update_issue).delete(delete_issue),
        )
        .route("/issues/{id}/close", post(close_issue))
        .route("/issues/{id}/reopen", post(reopen_issue))
        .route("/issues/{id}/subissue", post(create_subissue))
        // Comments
        .route(
            "/issues/{id}/comments",
            get(list_comments).post(add_comment),
        )
        // Labels
        .route("/issues/{id}/labels", post(add_label))
        .route("/issues/{id}/labels/{label}", delete(remove_label))
        // Blockers / dependencies
        .route("/issues/{id}/block", post(add_blocker))
        .route("/issues/{id}/block/{blocker_id}", delete(remove_blocker))
        // Sessions
        .route("/sessions/current", get(get_current_session))
        .route("/sessions/start", post(start_session))
        .route("/sessions/end", post(end_session))
        .route("/sessions/work/{id}", post(work_on_issue))
        // Milestones
        .route("/milestones", get(list_milestones).post(create_milestone))
        .route("/milestones/{id}", get(get_milestone))
        .route("/milestones/{id}/assign", post(assign_milestone))
        .route("/milestones/{id}/close", post(close_milestone))
        // Knowledge — static path first to avoid conflict with {slug}
        .route("/knowledge/search", get(search_knowledge))
        .route(
            "/knowledge",
            get(list_knowledge_pages).post(create_knowledge_page),
        )
        .route("/knowledge/{slug}", get(get_knowledge_page))
        // Unified search
        .route("/search", get(global_search));

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
