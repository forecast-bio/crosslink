use axum::{extract::State, response::Json};
use serde_json::{json, Value};

use crate::server::state::AppState;

/// `GET /api/v1/health` — liveness check.
///
/// Returns `{"status": "ok", "version": "<crate version>"}`.
pub async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "version": state.version,
    }))
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request, Router};
    use tower::util::ServiceExt;

    use crate::db::Database;
    use crate::server::{routes::build_router, state::AppState};

    #[tokio::test]
    async fn test_health_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(&dir.path().join("test.db")).unwrap();
        let state = AppState::new(db, dir.path().join(".crosslink"));
        let app: Router = build_router(state, None);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["status"], "ok");
        assert!(body["version"].is_string());
    }
}
