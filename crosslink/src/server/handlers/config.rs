//! Handlers for project configuration endpoints.
//!
//! Implements:
//! - `GET /api/v1/config` — read the current hook-config.json
//! - `PATCH /api/v1/config` — merge-update hook-config.json fields

use axum::{extract::State, http::StatusCode, response::Json};

use crate::server::{
    errors::{bad_request, internal_error},
    state::AppState,
    types::{ApiError, ConfigResponse, UpdateConfigRequest},
};

/// Extract a `ConfigResponse` from a raw JSON Value.
///
/// Field name mapping between `hook-config.json` and the API response:
///
/// | hook-config.json key         | API response field           |
/// |------------------------------|------------------------------|
/// | `tracking_mode`              | `tracking_mode`              |
/// | `stale_lock_timeout_minutes` | `stale_lock_timeout_minutes` |
/// | `tracker_remote`             | `remote`                     |
/// | `signing_enforcement`        | `signing_enforcement`        |
/// | `intervention_tracking`      | `intervention_tracking`      |
/// | `auto_steal_stale_locks`     | `auto_steal_stale_locks`     |
///
/// Note: the file uses `tracker_remote` while the API exposes `remote`.
/// The `PATCH /api/v1/config` request body also uses `remote`.
fn config_from_value(v: &serde_json::Value) -> ConfigResponse {
    ConfigResponse {
        tracking_mode: v
            .get("tracking_mode")
            .and_then(|x| x.as_str())
            .unwrap_or("normal")
            .to_string(),
        stale_lock_timeout_minutes: v
            .get("stale_lock_timeout_minutes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(30),
        remote: v
            .get("tracker_remote")
            .and_then(|x| x.as_str())
            .unwrap_or("origin")
            .to_string(),
        signing_enforcement: v
            .get("signing_enforcement")
            .and_then(|x| x.as_str())
            .unwrap_or("off")
            .to_string(),
        intervention_tracking: v
            .get("intervention_tracking")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        auto_steal_stale_locks: v
            .get("auto_steal_stale_locks")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/v1/config` — return the current project configuration.
///
/// # Errors
/// Returns an error if the config file cannot be read or parsed.
pub async fn get_config(
    State(state): State<AppState>,
) -> Result<Json<ConfigResponse>, (StatusCode, Json<ApiError>)> {
    let config_path = state.crosslink_dir.join("hook-config.json");

    let result = tokio::task::spawn_blocking(
        move || -> Result<ConfigResponse, (StatusCode, Json<ApiError>)> {
            // If no config file exists, return sensible defaults
            if !config_path.exists() {
                return Ok(ConfigResponse {
                    tracking_mode: "normal".to_string(),
                    stale_lock_timeout_minutes: 30,
                    remote: "origin".to_string(),
                    signing_enforcement: "off".to_string(),
                    intervention_tracking: false,
                    auto_steal_stale_locks: false,
                });
            }

            let content = std::fs::read_to_string(&config_path)
                .map_err(|e| internal_error("Failed to read config file", e))?;
            let value: serde_json::Value = serde_json::from_str(&content)
                .map_err(|e| internal_error("Failed to parse config file", e))?;

            Ok(config_from_value(&value))
        },
    )
    .await
    .map_err(|e| internal_error("Task join error", e))?;

    result.map(Json)
}

/// `PATCH /api/v1/config` — merge-update configuration fields.
///
/// Only provided (non-null) fields are updated; all others are left unchanged.
/// The full updated config is written back to `hook-config.json` and returned.
///
/// # Errors
/// Returns an error if validation fails, or the config file cannot be read or written.
pub async fn update_config(
    State(state): State<AppState>,
    Json(body): Json<UpdateConfigRequest>,
) -> Result<Json<ConfigResponse>, (StatusCode, Json<ApiError>)> {
    // Validate before entering spawn_blocking (these are pure checks).
    if let Some(ref mode) = body.tracking_mode {
        let valid_modes = ["strict", "normal", "relaxed"];
        if !valid_modes.contains(&mode.as_str()) {
            return Err(bad_request(format!(
                "Invalid tracking_mode '{}'. Must be one of: {}",
                mode,
                valid_modes.join(", ")
            )));
        }
    }
    if let Some(ref enforcement) = body.signing_enforcement {
        let valid = ["off", "audit", "warn", "enforce"];
        if !valid.contains(&enforcement.as_str()) {
            return Err(bad_request(format!(
                "Invalid signing_enforcement '{}'. Must be one of: {}",
                enforcement,
                valid.join(", ")
            )));
        }
    }

    let config_path = state.crosslink_dir.join("hook-config.json");

    let result = tokio::task::spawn_blocking(
        move || -> Result<ConfigResponse, (StatusCode, Json<ApiError>)> {
            // Read existing config (or start from empty object)
            let mut value: serde_json::Value = if config_path.exists() {
                let content = std::fs::read_to_string(&config_path)
                    .map_err(|e| internal_error("Failed to read config file", e))?;
                serde_json::from_str(&content)
                    .map_err(|e| internal_error("Failed to parse config file", e))?
            } else {
                serde_json::json!({})
            };

            let obj = value
                .as_object_mut()
                .ok_or_else(|| internal_error("Config file is not a JSON object", "expected {}"))?;

            if let Some(ref mode) = body.tracking_mode {
                obj.insert(
                    "tracking_mode".to_string(),
                    serde_json::Value::String(mode.clone()),
                );
            }

            if let Some(timeout) = body.stale_lock_timeout_minutes {
                obj.insert(
                    "stale_lock_timeout_minutes".to_string(),
                    serde_json::json!(timeout),
                );
            }

            if let Some(ref enforcement) = body.signing_enforcement {
                obj.insert(
                    "signing_enforcement".to_string(),
                    serde_json::Value::String(enforcement.clone()),
                );
            }

            if let Some(ref remote) = body.remote {
                obj.insert(
                    "tracker_remote".to_string(),
                    serde_json::Value::String(remote.clone()),
                );
            }

            if let Some(intervention) = body.intervention_tracking {
                obj.insert(
                    "intervention_tracking".to_string(),
                    serde_json::Value::Bool(intervention),
                );
            }

            if let Some(auto_steal) = body.auto_steal_stale_locks {
                obj.insert(
                    "auto_steal_stale_locks".to_string(),
                    serde_json::Value::Bool(auto_steal),
                );
            }

            // Write updated config back to disk
            let pretty = serde_json::to_string_pretty(&value)
                .map_err(|e| internal_error("Serialization failed", e))?;

            // Ensure .crosslink directory exists
            if let Some(parent) = config_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| internal_error("Failed to create config directory", e))?;
            }

            std::fs::write(&config_path, format!("{pretty}\n"))
                .map_err(|e| internal_error("Failed to write config file", e))?;

            Ok(config_from_value(&value))
        },
    )
    .await
    .map_err(|e| internal_error("Task join error", e))?;

    result.map(Json)
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

    fn test_app_with_config(config: &Value) -> (Router, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("test db");

        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();
        let config_path = crosslink_dir.join("hook-config.json");
        std::fs::write(&config_path, serde_json::to_string_pretty(config).unwrap()).unwrap();

        let state = AppState::new(db, crosslink_dir);
        (build_router(state, None), dir)
    }

    async fn body_json(resp: axum::response::Response) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn test_get_config_no_file_returns_defaults() {
        let (app, _dir) = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["tracking_mode"], "normal");
        assert_eq!(body["remote"], "origin");
        assert_eq!(body["intervention_tracking"], false);
        assert_eq!(body["auto_steal_stale_locks"], false);
    }

    #[tokio::test]
    async fn test_get_config_reads_existing_file() {
        let config = json!({
            "tracking_mode": "strict",
            "signing_enforcement": "audit",
            "intervention_tracking": true,
            "auto_steal_stale_locks": false,
            "tracker_remote": "upstream"
        });
        let (app, _dir) = test_app_with_config(&config);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["tracking_mode"], "strict");
        assert_eq!(body["signing_enforcement"], "audit");
        assert_eq!(body["intervention_tracking"], true);
        assert_eq!(body["remote"], "upstream");
    }

    #[tokio::test]
    async fn test_update_config_creates_file_if_missing() {
        let (app, dir) = test_app();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/config")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"tracking_mode": "strict"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["tracking_mode"], "strict");

        // Verify file was written
        let written = std::fs::read_to_string(crosslink_dir.join("hook-config.json")).unwrap();
        let parsed: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed["tracking_mode"], "strict");
    }

    #[tokio::test]
    async fn test_update_config_merges_fields() {
        let config = json!({
            "tracking_mode": "normal",
            "intervention_tracking": false,
            "extra_field": "preserved"
        });
        let (app, dir) = test_app_with_config(&config);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"tracking_mode": "strict", "intervention_tracking": true})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["tracking_mode"], "strict");
        assert_eq!(body["intervention_tracking"], true);

        // Verify that extra_field was preserved in the file
        let written =
            std::fs::read_to_string(dir.path().join(".crosslink/hook-config.json")).unwrap();
        let parsed: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed["extra_field"], "preserved");
    }

    #[tokio::test]
    async fn test_update_config_invalid_tracking_mode() {
        let (app, dir) = test_app();
        std::fs::create_dir_all(dir.path().join(".crosslink")).unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/config")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"tracking_mode": "invalid"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("Invalid tracking_mode"));
    }

    #[tokio::test]
    async fn test_update_config_invalid_signing_enforcement() {
        let (app, dir) = test_app();
        std::fs::create_dir_all(dir.path().join(".crosslink")).unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"signing_enforcement": "yolo"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = body_json(resp).await;
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("Invalid signing_enforcement"));
    }

    #[tokio::test]
    async fn test_update_config_empty_body_is_noop() {
        let config = json!({
            "tracking_mode": "strict",
            "intervention_tracking": true
        });
        let (app, _dir) = test_app_with_config(&config);

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/config")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        // Values should remain unchanged
        assert_eq!(body["tracking_mode"], "strict");
        assert_eq!(body["intervention_tracking"], true);
    }

    #[tokio::test]
    async fn test_update_config_all_fields() {
        let (app, dir) = test_app();
        std::fs::create_dir_all(dir.path().join(".crosslink")).unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "tracking_mode": "strict",
                            "stale_lock_timeout_minutes": 45,
                            "signing_enforcement": "enforce",
                            "remote": "upstream",
                            "intervention_tracking": true,
                            "auto_steal_stale_locks": true
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["tracking_mode"], "strict");
        assert_eq!(body["signing_enforcement"], "enforce");
        assert_eq!(body["remote"], "upstream");
        assert_eq!(body["intervention_tracking"], true);
        assert_eq!(body["auto_steal_stale_locks"], true);

        // Verify written file has stale_lock_timeout_minutes
        let written =
            std::fs::read_to_string(dir.path().join(".crosslink/hook-config.json")).unwrap();
        let parsed: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed["stale_lock_timeout_minutes"], 45);
    }

    #[test]
    fn test_helper_functions() {
        let (status, json) = crate::server::errors::internal_error("ctx", "err");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "ctx");

        let (status, json) = crate::server::errors::bad_request("invalid");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json.detail.as_deref(), Some("invalid"));
    }

    #[test]
    fn test_config_from_value_defaults() {
        // Verify config_from_value returns sensible defaults when fields are missing.
        let empty = serde_json::json!({});
        let cfg = super::config_from_value(&empty);
        assert_eq!(cfg.tracking_mode, "normal");
        assert_eq!(cfg.stale_lock_timeout_minutes, 30);
        assert_eq!(cfg.remote, "origin");
        assert_eq!(cfg.signing_enforcement, "off");
        assert!(!cfg.intervention_tracking);
        assert!(!cfg.auto_steal_stale_locks);
    }

    #[test]
    fn test_config_from_value_with_all_fields() {
        let value = serde_json::json!({
            "tracking_mode": "strict",
            "stale_lock_timeout_minutes": 60,
            "tracker_remote": "upstream",
            "signing_enforcement": "enforce",
            "intervention_tracking": true,
            "auto_steal_stale_locks": true
        });
        let cfg = super::config_from_value(&value);
        assert_eq!(cfg.tracking_mode, "strict");
        assert_eq!(cfg.stale_lock_timeout_minutes, 60);
        assert_eq!(cfg.remote, "upstream");
        assert_eq!(cfg.signing_enforcement, "enforce");
        assert!(cfg.intervention_tracking);
        assert!(cfg.auto_steal_stale_locks);
    }

    #[tokio::test]
    async fn test_update_config_sets_remote() {
        let (app, dir) = test_app();
        std::fs::create_dir_all(dir.path().join(".crosslink")).unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"remote": "upstream"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["remote"], "upstream");
    }

    #[tokio::test]
    async fn test_update_config_sets_auto_steal() {
        let (app, dir) = test_app();
        std::fs::create_dir_all(dir.path().join(".crosslink")).unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::PATCH)
                    .uri("/api/v1/config")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({"auto_steal_stale_locks": true}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["auto_steal_stale_locks"], true);
    }
}
