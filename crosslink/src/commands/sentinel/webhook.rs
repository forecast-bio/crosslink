use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::post,
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::sources::{Signal, SignalKind, SourceKind};

/// Webhook event pushed into the signal channel for the sentinel engine.
#[derive(Debug, Clone)]
pub struct WebhookEvent {
    pub signal: Signal,
}

/// Shared state for the webhook server.
struct WebhookState {
    sender: mpsc::Sender<WebhookEvent>,
    webhook_secret: Option<String>,
}

/// GitHub webhook payload for issue events.
#[derive(Debug, Deserialize)]
struct GitHubIssueEvent {
    action: String,
    issue: GitHubIssue,
    label: Option<GitHubLabel>,
}

#[derive(Debug, Deserialize)]
struct GitHubIssue {
    number: i64,
    title: String,
    body: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubLabel {
    name: String,
}

#[derive(Debug, Serialize)]
struct WebhookResponse {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal_ref: Option<String>,
}

/// Configuration for the webhook server.
#[derive(Debug, Clone)]
pub struct WebhookConfig {
    pub port: u16,
    pub secret: Option<String>,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            port: 9876,
            secret: None,
        }
    }
}

/// Start the webhook server. Returns a channel receiver for incoming signals.
///
/// The server runs in the background on a tokio runtime. The caller reads
/// signals from the returned receiver and feeds them into the sentinel engine.
pub async fn start_webhook_server(config: &WebhookConfig) -> Result<mpsc::Receiver<WebhookEvent>> {
    let (sender, receiver) = mpsc::channel(100);

    let state = Arc::new(WebhookState {
        sender,
        webhook_secret: config.secret.clone(),
    });

    let app = Router::new()
        .route("/webhook/github", post(handle_github_webhook))
        .route("/health", axum::routing::get(health))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", config.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind webhook server to {addr}"))?;

    tracing::info!("Sentinel webhook server listening on {addr}");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!("Webhook server error: {e}");
        }
    });

    Ok(receiver)
}

/// Health check endpoint.
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "service": "sentinel-webhook" }))
}

/// Handle incoming GitHub webhook events.
async fn handle_github_webhook(
    State(state): State<Arc<WebhookState>>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<WebhookResponse>, StatusCode> {
    // Verify webhook signature if secret is configured
    if let Some(ref secret) = state.webhook_secret {
        let signature = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !verify_signature(secret, &body, signature) {
            tracing::warn!("Webhook signature verification failed");
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    // Determine event type
    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown");

    if event_type == "issues" {
        handle_issue_event(&state, &body).await
    } else {
        tracing::debug!("Ignoring webhook event type: {event_type}");
        Ok(Json(WebhookResponse {
            status: "ignored".into(),
            signal_ref: None,
        }))
    }
}

/// Process a GitHub issue event (labeled, unlabeled, opened, etc.)
async fn handle_issue_event(
    state: &WebhookState,
    body: &str,
) -> Result<Json<WebhookResponse>, StatusCode> {
    let event: GitHubIssueEvent = serde_json::from_str(body).map_err(|e| {
        tracing::warn!("Failed to parse issue event: {e}");
        StatusCode::BAD_REQUEST
    })?;

    // We only care about "labeled" events with agent-todo: prefixes
    if event.action != "labeled" {
        return Ok(Json(WebhookResponse {
            status: "ignored".into(),
            signal_ref: None,
        }));
    }

    let Some(label) = &event.label else {
        return Ok(Json(WebhookResponse {
            status: "ignored".into(),
            signal_ref: None,
        }));
    };

    if !label.name.starts_with("agent-todo:") {
        return Ok(Json(WebhookResponse {
            status: "ignored".into(),
            signal_ref: None,
        }));
    }

    let label_suffix = label
        .name
        .strip_prefix("agent-todo: ")
        .unwrap_or(&label.name);
    let signal_ref = format!("GH#{}:{}", event.issue.number, label_suffix);

    let signal = Signal {
        source: SourceKind::GitHub,
        kind: SignalKind::LabelAdded,
        reference: signal_ref.clone(),
        title: event.issue.title,
        body: event.issue.body.unwrap_or_default(),
        metadata: serde_json::json!({
            "label": &label.name,
            "number": event.issue.number,
            "via": "webhook",
        }),
        detected_at: chrono::Utc::now(),
    };

    let event = WebhookEvent { signal };
    if let Err(e) = state.sender.send(event).await {
        tracing::error!("Failed to queue webhook signal: {e}");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }

    tracing::info!("Webhook received: {signal_ref}");
    Ok(Json(WebhookResponse {
        status: "queued".into(),
        signal_ref: Some(signal_ref),
    }))
}

/// Verify GitHub webhook HMAC-SHA256 signature.
fn verify_signature(secret: &str, body: &str, signature: &str) -> bool {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    let Some(hex_sig) = signature.strip_prefix("sha256=") else {
        return false;
    };

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };

    mac.update(body.as_bytes());

    let Ok(expected) = hex::decode(hex_sig) else {
        return false;
    };

    mac.verify_slice(&expected).is_ok()
}
