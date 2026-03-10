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
