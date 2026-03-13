//! Handlers for token usage tracking endpoints.
//!
//! Implements:
//! - `GET  /api/v1/usage`         — list token usage records with optional filters
//! - `POST /api/v1/usage`         — record a new token usage entry
//! - `GET  /api/v1/usage/summary` — aggregated usage grouped by agent and model

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::Json,
};

use crate::server::{
    state::AppState,
    types::{
        ApiError, CreateTokenUsageRequest, TokenUsageListQuery, TokenUsageListResponse,
        TokenUsageSummaryQuery, TokenUsageSummaryResponse,
    },
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn internal_error(context: &str, e: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: context.to_string(),
            detail: Some(e.to_string()),
        }),
    )
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/usage` — list token usage records.
///
/// Supports optional query parameters: `agent_id`, `session_id`, `model`,
/// `from`, `to` (ISO 8601 timestamps), and `limit`.
pub async fn list_usage(
    State(state): State<AppState>,
    Query(params): Query<TokenUsageListQuery>,
) -> Result<Json<TokenUsageListResponse>, (StatusCode, Json<ApiError>)> {
    let db = state.db();

    let items = db
        .list_token_usage(
            params.agent_id.as_deref(),
            params.session_id,
            params.model.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
            params.limit,
        )
        .map_err(|e| internal_error("Failed to list token usage", e))?;

    let total = items.len();
    Ok(Json(TokenUsageListResponse { items, total }))
}

/// `POST /api/v1/usage` — record a new token usage entry.
///
/// Body: `CreateTokenUsageRequest` JSON.
/// Returns the created `TokenUsage` record.
pub async fn create_usage(
    State(state): State<AppState>,
    Json(body): Json<CreateTokenUsageRequest>,
) -> Result<(StatusCode, Json<crate::models::TokenUsage>), (StatusCode, Json<ApiError>)> {
    let db = state.db();

    let id = db
        .create_token_usage(
            &body.agent_id,
            body.session_id,
            body.input_tokens,
            body.output_tokens,
            body.cache_read_tokens,
            body.cache_creation_tokens,
            &body.model,
            body.cost_estimate,
        )
        .map_err(|e| internal_error("Failed to create token usage", e))?;

    let usage = db
        .get_token_usage(id)
        .map_err(|e| internal_error("Failed to fetch new token usage", e))?
        .ok_or_else(|| internal_error("Token usage created but not found", format!("id={id}")))?;

    Ok((StatusCode::CREATED, Json(usage)))
}

/// `GET /api/v1/usage/summary` — aggregated usage grouped by agent and model.
///
/// Supports optional query parameters: `agent_id`, `from`, `to`.
pub async fn usage_summary(
    State(state): State<AppState>,
    Query(params): Query<TokenUsageSummaryQuery>,
) -> Result<Json<TokenUsageSummaryResponse>, (StatusCode, Json<ApiError>)> {
    let db = state.db();

    let items = db
        .get_usage_summary(
            params.agent_id.as_deref(),
            params.from.as_deref(),
            params.to.as_deref(),
        )
        .map_err(|e| internal_error("Failed to get usage summary", e))?;

    let total_input_tokens: i64 = items.iter().map(|r| r.total_input_tokens).sum();
    let total_output_tokens: i64 = items.iter().map(|r| r.total_output_tokens).sum();
    let total_cost: f64 = items.iter().map(|r| r.total_cost).sum();

    Ok(Json(TokenUsageSummaryResponse {
        items,
        total_input_tokens,
        total_output_tokens,
        total_cost,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
        Router,
    };
    use serde_json::{json, Value};
    use tower::util::ServiceExt;

    use crate::db::Database;
    use crate::server::{routes::build_router, state::AppState};

    fn test_app() -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");
        let state = AppState::new(db, dir.path().join(".crosslink"));
        (build_router(state, None), dir)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_list_usage_empty() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 0);
        assert_eq!(body["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_create_and_list_usage() {
        let (app, _dir) = test_app();

        // Create a usage record
        let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/usage")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agent_id": "test-agent",
                            "input_tokens": 1000,
                            "output_tokens": 500,
                            "model": "claude-sonnet-4-20250514",
                            "cost_estimate": 0.0045
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let created = body_json(create_resp).await;
        assert_eq!(created["agent_id"], "test-agent");
        assert_eq!(created["input_tokens"], 1000);
        assert_eq!(created["output_tokens"], 500);
        assert_eq!(created["model"], "claude-sonnet-4-20250514");

        // List and verify
        let list_resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_resp.status(), StatusCode::OK);
        let body = body_json(list_resp).await;
        assert_eq!(body["total"], 1);
        assert_eq!(body["items"][0]["agent_id"], "test-agent");
    }

    #[tokio::test]
    async fn test_list_usage_with_agent_filter() {
        let (app, _dir) = test_app();

        // Create two records for different agents
        for agent in &["agent-a", "agent-b"] {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/usage")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "agent_id": agent,
                                "input_tokens": 100,
                                "output_tokens": 50,
                                "model": "claude-sonnet-4-20250514"
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
        }

        // Filter by agent-a
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage?agent_id=agent-a")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 1);
        assert_eq!(body["items"][0]["agent_id"], "agent-a");
    }

    #[tokio::test]
    async fn test_usage_summary_empty() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total_input_tokens"], 0);
        assert_eq!(body["total_output_tokens"], 0);
        assert_eq!(body["total_cost"], 0.0);
        assert_eq!(body["items"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_usage_summary_aggregation() {
        let (app, _dir) = test_app();

        // Create multiple records for same agent + model
        for _ in 0..3 {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/usage")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "agent_id": "worker-1",
                                "input_tokens": 1000,
                                "output_tokens": 200,
                                "model": "claude-sonnet-4-20250514",
                                "cost_estimate": 0.003
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total_input_tokens"], 3000);
        assert_eq!(body["total_output_tokens"], 600);
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        let item = &body["items"][0];
        assert_eq!(item["agent_id"], "worker-1");
        assert_eq!(item["request_count"], 3);
        assert_eq!(item["total_input_tokens"], 3000);
    }

    #[tokio::test]
    async fn test_create_usage_with_cache_tokens() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/usage")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agent_id": "cache-agent",
                            "input_tokens": 500,
                            "output_tokens": 100,
                            "cache_read_tokens": 2000,
                            "cache_creation_tokens": 300,
                            "model": "claude-opus-4-20250514"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert_eq!(body["cache_read_tokens"], 2000);
        assert_eq!(body["cache_creation_tokens"], 300);
    }

    #[tokio::test]
    async fn test_create_usage_default_model() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/usage")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "agent_id": "no-model-agent",
                            "input_tokens": 100,
                            "output_tokens": 50
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body = body_json(resp).await;
        assert_eq!(body["model"], "unknown");
    }

    #[tokio::test]
    async fn test_list_usage_with_model_filter() {
        let (app, _dir) = test_app();

        // Create records for two different models.
        for model in &["claude-sonnet-4-20250514", "claude-opus-4-20250514"] {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/usage")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "agent_id": "test-agent",
                                "input_tokens": 100,
                                "output_tokens": 50,
                                "model": model
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
        }

        // Filter by sonnet model only.
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage?model=claude-sonnet-4-20250514")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 1);
        assert_eq!(body["items"][0]["model"], "claude-sonnet-4-20250514");
    }

    #[tokio::test]
    async fn test_usage_summary_with_agent_filter() {
        let (app, _dir) = test_app();

        // Create records for two agents.
        for agent in &["alpha-agent", "beta-agent"] {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/usage")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "agent_id": agent,
                                "input_tokens": 500,
                                "output_tokens": 100,
                                "model": "claude-sonnet-4-20250514",
                                "cost_estimate": 0.001
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
        }

        // Summary filtered to alpha-agent only.
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage/summary?agent_id=alpha-agent")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["items"].as_array().unwrap().len(), 1);
        assert_eq!(body["items"][0]["agent_id"], "alpha-agent");
        assert_eq!(body["total_input_tokens"], 500);
    }

    #[tokio::test]
    async fn test_list_usage_with_limit() {
        let (app, _dir) = test_app();

        // Create three records.
        for _ in 0..3 {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/usage")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "agent_id": "limit-agent",
                                "input_tokens": 100,
                                "output_tokens": 50,
                                "model": "claude-sonnet-4-20250514"
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
        }

        // Limit to 2.
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage?limit=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["total"], 2);
    }

    #[test]
    fn test_internal_error_helper() {
        let (status, json) = super::internal_error("ctx", "detail msg");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "ctx");
        assert_eq!(json.detail.as_deref(), Some("detail msg"));
    }

    #[tokio::test]
    async fn test_usage_summary_total_cost() {
        let (app, _dir) = test_app();

        // Create two records with known costs.
        for cost in &[0.002_f64, 0.003_f64] {
            app.clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/api/v1/usage")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "agent_id": "cost-agent",
                                "input_tokens": 100,
                                "output_tokens": 50,
                                "model": "claude-sonnet-4-20250514",
                                "cost_estimate": cost
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
        }

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/usage/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // total_cost should be ~0.005.
        let total_cost = body["total_cost"].as_f64().unwrap();
        assert!((total_cost - 0.005).abs() < 1e-9);
    }
}
