use anyhow::{Context, Result};
use std::process::Command;

use crate::db::sentinel::SentinelDispatch;

use super::config::NotificationConfig;

/// Send a notification for a completed dispatch.
///
/// Supports Slack incoming webhooks and generic HTTP POST endpoints.
/// Does nothing if notifications are disabled.
pub fn notify_dispatch_completed(
    config: &NotificationConfig,
    dispatch: &SentinelDispatch,
    outcome: &str,
    findings_summary: &str,
) {
    if !config.enabled {
        return;
    }

    let message = build_notification_message(dispatch, outcome, findings_summary);

    for url in &config.webhook_urls {
        if let Err(e) = send_webhook(url, &message, NotificationConfig::is_slack_url(url)) {
            tracing::warn!("notification to {} failed: {e}", mask_url(url));
        }
    }
}

/// Build the notification message payload.
fn build_notification_message(
    dispatch: &SentinelDispatch,
    outcome: &str,
    findings_summary: &str,
) -> NotificationMessage {
    let status_emoji = match outcome {
        "success" => "white_check_mark",
        "failure" => "x",
        "exhausted" => "no_entry_sign",
        "orphaned" => "ghost",
        _ => "question",
    };

    let status_text = match outcome {
        "success" if dispatch.label.contains("fix") => "Fixed",
        "success" => "Reproduced",
        "failure" if dispatch.label.contains("fix") => "Could not fix",
        "failure" => "Could not reproduce",
        "exhausted" => "All attempts exhausted",
        "orphaned" => "Orphaned (worktree removed)",
        other => other,
    };

    let model = dispatch.model_used.as_deref().unwrap_or("unknown");
    let gh_link = dispatch
        .gh_issue_number
        .map_or_else(|| dispatch.signal_ref.clone(), |n| format!("GH#{n}"));

    let summary = if findings_summary.len() > 300 {
        format!("{}...", &findings_summary[..300])
    } else {
        findings_summary.to_string()
    };

    NotificationMessage {
        status_emoji: status_emoji.to_string(),
        status_text: status_text.to_string(),
        signal_ref: dispatch.signal_ref.clone(),
        gh_link,
        title: dispatch.signal_title.clone(),
        model: model.to_string(),
        attempt: dispatch.attempt_number,
        summary,
    }
}

struct NotificationMessage {
    status_emoji: String,
    status_text: String,
    signal_ref: String,
    gh_link: String,
    title: String,
    model: String,
    attempt: i32,
    summary: String,
}

impl NotificationMessage {
    /// Format as a Slack Block Kit message.
    fn to_slack_json(&self) -> serde_json::Value {
        serde_json::json!({
            "blocks": [
                {
                    "type": "header",
                    "text": {
                        "type": "plain_text",
                        "text": format!(":{}: Sentinel: {}", self.status_emoji, self.status_text),
                        "emoji": true,
                    }
                },
                {
                    "type": "section",
                    "fields": [
                        { "type": "mrkdwn", "text": format!("*Signal:* {}", self.signal_ref) },
                        { "type": "mrkdwn", "text": format!("*Issue:* {}", self.gh_link) },
                        { "type": "mrkdwn", "text": format!("*Model:* {}", self.model) },
                        { "type": "mrkdwn", "text": format!("*Attempt:* {} of 2", self.attempt) },
                    ]
                },
                {
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": format!("*{}*\n{}", self.title, self.summary),
                    }
                }
            ]
        })
    }

    /// Format as a generic JSON payload for non-Slack webhooks.
    fn to_generic_json(&self) -> serde_json::Value {
        serde_json::json!({
            "event": "sentinel.dispatch.completed",
            "status": self.status_text,
            "signal_ref": self.signal_ref,
            "title": self.title,
            "model": self.model,
            "attempt": self.attempt,
            "summary": self.summary,
        })
    }
}

/// Send a JSON payload to a webhook URL via curl.
///
/// Uses `curl` instead of a Rust HTTP client to avoid adding another async
/// dependency — sentinel notifications are fire-and-forget, not latency-critical.
fn send_webhook(url: &str, message: &NotificationMessage, is_slack: bool) -> Result<()> {
    let payload = if is_slack {
        message.to_slack_json()
    } else {
        message.to_generic_json()
    };

    let body = serde_json::to_string(&payload)?;

    let output = Command::new("curl")
        .args([
            "-s",
            "-o",
            "/dev/null",
            "-w",
            "%{http_code}",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            &body,
            url,
        ])
        .output()
        .context("Failed to run curl for webhook notification")?;

    let status_code = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !status_code.starts_with('2') {
        anyhow::bail!("webhook returned HTTP {status_code}");
    }

    Ok(())
}

/// Mask a URL for safe logging (hide everything after the host).
fn mask_url(url: &str) -> String {
    url.find("//")
        .and_then(|p| url[p + 2..].find('/').map(|q| p + 2 + q))
        .map_or_else(|| "***".to_string(), |pos| format!("{}/*****", &url[..pos]))
}
