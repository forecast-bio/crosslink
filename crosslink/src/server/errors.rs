//! Shared error-response helpers for axum handlers.
//!
//! Centralises the `internal_error`, `not_found`, and `bad_request` constructors
//! that were previously copy-pasted across every handler module.

use axum::{http::StatusCode, response::Json};

use crate::server::types::ApiError;

/// Build a 500 Internal Server Error response.
pub fn internal_error(context: &str, e: impl std::fmt::Display) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiError {
            error: context.to_string(),
            detail: Some(e.to_string()),
        }),
    )
}

/// Build a 404 Not Found response.
pub fn not_found(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::NOT_FOUND,
        Json(ApiError {
            error: "not found".to_string(),
            detail: Some(msg.into()),
        }),
    )
}

/// Build a 400 Bad Request response.
pub fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "bad request".to_string(),
            detail: Some(msg.into()),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_internal_error_helper() {
        let (status, json) = internal_error("ctx", "detail");
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json.error, "ctx");
        assert_eq!(json.detail.as_deref(), Some("detail"));
    }

    #[test]
    fn test_not_found_helper() {
        let (status, json) = not_found("not there");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json.error, "not found");
        assert_eq!(json.detail.as_deref(), Some("not there"));
    }

    #[test]
    fn test_bad_request_helper() {
        let (status, json) = bad_request("invalid");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(json.error, "bad request");
        assert_eq!(json.detail.as_deref(), Some("invalid"));
    }
}
