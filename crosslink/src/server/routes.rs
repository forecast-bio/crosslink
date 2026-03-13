use axum::{routing::get, Router};

use crate::server::{
    handlers::{
        agents::{
            get_agent, get_agent_status, list_agents, list_locks, list_stale_locks,
            notify_lock_changed,
        },
        config::{get_config, update_config},
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
        orchestrator::{
            decompose_handler, execute, get_plan, get_plan_by_id, get_snapshot, get_status,
            list_plans_handler, mark_stage_done_handler, mark_stage_failed_handler,
            mark_stage_running_handler, pause, poll_agents, resume_execution, retry_stage,
            skip_stage,
        },
        search::global_search,
        sessions::{end_session, get_current_session, start_session, work_on_issue},
        sync::{sync_fetch, sync_push, sync_status},
        usage::{create_usage, list_usage, usage_summary},
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
        .route("/locks/notify", post(notify_lock_changed))
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
        .route("/search", get(global_search))
        // Sync
        .route("/sync/status", get(sync_status))
        .route("/sync/fetch", post(sync_fetch))
        .route("/sync/push", post(sync_push))
        // Config
        .route("/config", get(get_config).patch(update_config))
        // Token usage — static path first to avoid conflict with future /{id}
        .route("/usage/summary", get(usage_summary))
        .route("/usage", get(list_usage).post(create_usage))
        // Orchestrator — static paths first
        .route("/orchestrator/plans", get(list_plans_handler))
        .route("/orchestrator/plans/{id}", get(get_plan_by_id))
        .route("/orchestrator/plan", get(get_plan))
        .route("/orchestrator/status", get(get_status))
        .route("/orchestrator/snapshot", get(get_snapshot))
        .route("/orchestrator/agents/poll", get(poll_agents))
        .route("/orchestrator/decompose", post(decompose_handler))
        .route("/orchestrator/execute", post(execute))
        .route("/orchestrator/pause", post(pause))
        .route("/orchestrator/resume", post(resume_execution))
        .route("/orchestrator/stages/{id}/retry", post(retry_stage))
        .route("/orchestrator/stages/{id}/skip", post(skip_stage))
        .route(
            "/orchestrator/stages/{id}/running",
            post(mark_stage_running_handler),
        )
        .route(
            "/orchestrator/stages/{id}/done",
            post(mark_stage_done_handler),
        )
        .route(
            "/orchestrator/stages/{id}/failed",
            post(mark_stage_failed_handler),
        );

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn test_build_router_with_dashboard_dir() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        let state = AppState::new(db, dir.path().join(".crosslink"));
        let dashboard = dir.path().join("dashboard");
        std::fs::create_dir_all(&dashboard).unwrap();
        // Should not panic
        let _router = build_router(state, Some(dashboard));
    }
}
