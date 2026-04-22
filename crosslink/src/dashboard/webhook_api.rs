//! Webhook configuration REST surface (design doc §14 Phase 5 —
//! webhook alerting).
//!
//! Exposes the dashboard's outbound-webhook URL list. The poll loop
//! (see [`super::poll`]) reads this list on every tick; the operator
//! edits it through the `/settings/webhooks` page.
//!
//! Endpoints (all under `/api/v1/dashboard`):
//! - `GET /webhooks` — list configured URLs (returned plaintext — the
//!   user typed them and may need to verify/edit).
//! - `PUT /webhooks` — replace the full list. Validates each URL
//!   before writing; on any failure, no change is persisted.
//!
//! We deliberately do not offer a partial update (POST one URL) — the
//! list is short enough that round-tripping the whole thing per edit
//! is simpler than tracking per-URL IDs.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::db::DashboardDb;
use super::webhook;
use crate::server::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/webhooks", get(get_webhooks).put(put_webhooks))
}

#[derive(Debug, Serialize)]
struct WebhooksView {
    urls: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SetWebhooksBody {
    urls: Vec<String>,
}

#[derive(Debug)]
enum WebhookApiError {
    BadRequest(String),
    Internal(String),
}

impl IntoResponse for WebhookApiError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, m),
        };
        (status, Json(serde_json::json!({"error": msg}))).into_response()
    }
}

fn require_db_path(state: &AppState) -> Result<std::path::PathBuf, WebhookApiError> {
    state
        .dashboard_db_path
        .clone()
        .ok_or_else(|| WebhookApiError::BadRequest("dashboard DB not configured".into()))
}

async fn get_webhooks(
    State(state): State<AppState>,
) -> Result<Json<WebhooksView>, WebhookApiError> {
    let db_path = require_db_path(&state)?;
    let urls = tokio::task::spawn_blocking(move || -> Result<Vec<String>, WebhookApiError> {
        let db = DashboardDb::open(&db_path)
            .map_err(|e| WebhookApiError::Internal(format!("open db: {e}")))?;
        webhook::load_urls(&db).map_err(|e| WebhookApiError::Internal(format!("load urls: {e}")))
    })
    .await
    .map_err(|e| WebhookApiError::Internal(format!("task panicked: {e}")))??;
    Ok(Json(WebhooksView { urls }))
}

async fn put_webhooks(
    State(state): State<AppState>,
    Json(body): Json<SetWebhooksBody>,
) -> Result<Json<WebhooksView>, WebhookApiError> {
    let db_path = require_db_path(&state)?;

    // Validate up front so a single bad URL rejects the whole batch
    // without a partial write.
    let mut cleaned = Vec::with_capacity(body.urls.len());
    for raw in body.urls {
        let trimmed = raw.trim().to_string();
        if trimmed.is_empty() {
            continue;
        }
        if let Err(reason) = webhook::validate_url(&trimmed) {
            return Err(WebhookApiError::BadRequest(format!("{trimmed}: {reason}")));
        }
        cleaned.push(trimmed);
    }

    let urls = tokio::task::spawn_blocking(move || -> Result<Vec<String>, WebhookApiError> {
        let db = DashboardDb::open(&db_path)
            .map_err(|e| WebhookApiError::Internal(format!("open db: {e}")))?;
        webhook::save_urls(&db, &cleaned)
            .map_err(|e| WebhookApiError::Internal(format!("save urls: {e}")))?;
        webhook::load_urls(&db).map_err(|e| WebhookApiError::Internal(format!("reload urls: {e}")))
    })
    .await
    .map_err(|e| WebhookApiError::Internal(format!("task panicked: {e}")))??;

    Ok(Json(WebhooksView { urls }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_state(dashboard_db: Option<std::path::PathBuf>) -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let main_db_path = dir.path().join("crosslink.db");
        let main_db = crate::db::Database::open(&main_db_path).unwrap();
        let mut state = AppState::new(main_db, dir.path().join(".crosslink"));
        if let Some(p) = dashboard_db {
            state = state.with_dashboard_db(p);
        }
        (state, dir)
    }

    fn mk_dashboard_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dashboard.db");
        DashboardDb::open(&path).unwrap();
        (dir, path)
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_get_webhooks_without_db_returns_400() {
        let (state, _tmp) = test_state(None);
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/webhooks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_get_webhooks_empty_default() {
        let (_tmp, db_path) = mk_dashboard_db();
        let (state, _tmp2) = test_state(Some(db_path));
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/webhooks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["urls"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_put_then_get_roundtrip() {
        let (_tmp, db_path) = mk_dashboard_db();
        let (state, _tmp2) = test_state(Some(db_path));
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);

        let payload = serde_json::json!({
            "urls": [
                "https://hooks.slack.com/services/T/B/XYZ",
                "  https://discord.com/api/webhooks/123/abc  ",
                ""
            ]
        });
        let put_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/dashboard/webhooks")
                    .header("content-type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(put_resp.status(), StatusCode::OK);
        let put_body = body_json(put_resp).await;
        let urls = put_body["urls"].as_array().unwrap();
        assert_eq!(urls.len(), 2, "empty/whitespace entries dropped");
        assert_eq!(urls[0], "https://hooks.slack.com/services/T/B/XYZ");
        assert_eq!(urls[1], "https://discord.com/api/webhooks/123/abc");

        let get_resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/dashboard/webhooks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_resp.status(), StatusCode::OK);
        let get_body = body_json(get_resp).await;
        assert_eq!(get_body["urls"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_put_rejects_invalid_url() {
        let (_tmp, db_path) = mk_dashboard_db();
        let (state, _tmp2) = test_state(Some(db_path.clone()));
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);

        let payload = serde_json::json!({ "urls": ["ftp://example.com/hook"] });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/dashboard/webhooks")
                    .header("content-type", "application/json")
                    .body(Body::from(payload.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Nothing persisted: the DB still holds an empty list.
        let db = DashboardDb::open(&db_path).unwrap();
        assert!(webhook::load_urls(&db).unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_put_empty_list_clears_config() {
        let (_tmp, db_path) = mk_dashboard_db();
        {
            let db = DashboardDb::open(&db_path).unwrap();
            webhook::save_urls(&db, &["https://example.com/hook".to_string()]).unwrap();
        }
        let (state, _tmp2) = test_state(Some(db_path.clone()));
        let app = axum::Router::new()
            .nest("/api/v1/dashboard", router())
            .with_state(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/v1/dashboard/webhooks")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::json!({ "urls": [] }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["urls"].as_array().unwrap().len(), 0);
    }
}
