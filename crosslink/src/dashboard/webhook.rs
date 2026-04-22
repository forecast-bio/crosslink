//! Outbound webhook delivery for dashboard alert fires (design doc §14
//! Phase 5 — Polish / webhook alerting).
//!
//! When an alert transitions from "not derived" → "derived" during the
//! poll loop's reconcile pass, we emit a POST to each configured
//! webhook URL with a payload tailored to the destination:
//!
//! - `hooks.slack.com/...`   → Slack Block Kit JSON
//! - `*.discord.com/...`     → Discord native `{content, embeds}` JSON
//! - anything else           → generic `{event, severity, ...}` JSON
//!
//! Discord webhooks do also accept Slack-formatted payloads on a
//! `/slack` suffix, but we prefer the native shape so users don't have
//! to hand-edit their URL.
//!
//! Delivery is fire-and-forget: the poll loop spawns a task per URL,
//! failures are logged via `tracing::warn` with the host portion of
//! the URL masked. A stuck webhook endpoint doesn't stall the polling
//! cadence for other projects.
//!
//! Configuration lives in the dashboard DB's `config` table under
//! `webhook.urls`, stored as a JSON array of strings. The GET/PUT REST
//! surface (see `webhook_api`) edits this value; the poll loop reads
//! it once per tick.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde_json::{json, Value};

use super::alerts::{DerivedAlert, Severity};
use super::db::DashboardDb;

/// Config key under which the JSON-encoded URL list lives.
pub const KEY_WEBHOOK_URLS: &str = "webhook.urls";

/// One alert-fire event, carried in the dispatch payload. Owns its
/// strings so callers can freely move it into a spawned task.
#[derive(Debug, Clone)]
pub struct AlertNotification {
    pub kind: String,
    pub severity: Severity,
    pub project_slug: String,
    pub subject_ref: String,
    pub detail: String,
    pub opened_at: DateTime<Utc>,
}

impl AlertNotification {
    /// Build a notification from a derived alert + the project it
    /// belongs to. The `opened_at` is taken as "now" — the caller that
    /// observes the fire is responsible for supplying the timestamp.
    #[must_use]
    pub fn new(
        alert: &DerivedAlert,
        project_slug: impl Into<String>,
        opened_at: DateTime<Utc>,
    ) -> Self {
        Self {
            kind: alert.kind.to_string(),
            severity: alert.severity,
            project_slug: project_slug.into(),
            subject_ref: alert.subject_ref.clone(),
            detail: alert.detail.clone(),
            opened_at,
        }
    }

    /// Format for Slack incoming-webhook endpoints (Block Kit).
    #[must_use]
    pub fn to_slack_json(&self) -> Value {
        let emoji = severity_emoji(self.severity);
        let header = format!(
            ":{emoji}: Crosslink alert: {kind}",
            kind = self.kind,
            emoji = emoji,
        );
        json!({
            "blocks": [
                {
                    "type": "header",
                    "text": { "type": "plain_text", "text": header, "emoji": true }
                },
                {
                    "type": "section",
                    "fields": [
                        { "type": "mrkdwn", "text": format!("*Project:* {}", self.project_slug) },
                        { "type": "mrkdwn", "text": format!("*Severity:* {}", self.severity.as_str()) },
                        { "type": "mrkdwn", "text": format!("*Subject:* {}", self.subject_ref) },
                        { "type": "mrkdwn", "text": format!("*Opened:* {}", self.opened_at.to_rfc3339()) },
                    ]
                },
                {
                    "type": "section",
                    "text": { "type": "mrkdwn", "text": format!("_{}_", self.detail) }
                }
            ]
        })
    }

    /// Format for Discord webhooks. Uses the native `content` + single
    /// `embed` shape with a coloured side bar by severity.
    #[must_use]
    pub fn to_discord_json(&self) -> Value {
        let color = match self.severity {
            Severity::Critical => 0x00E6_1E4Cu32, // rose
            Severity::Warning => 0x00F5_9E0Bu32,  // amber
            Severity::Info => 0x0038_BDF8u32,     // sky
        };
        json!({
            "content": format!(
                "Crosslink alert — **{}** on `{}`",
                self.kind, self.project_slug
            ),
            "embeds": [{
                "title": self.kind,
                "description": self.detail,
                "color": color,
                "fields": [
                    { "name": "Project",  "value": self.project_slug, "inline": true },
                    { "name": "Severity", "value": self.severity.as_str(), "inline": true },
                    { "name": "Subject",  "value": self.subject_ref, "inline": false },
                ],
                "timestamp": self.opened_at.to_rfc3339(),
            }]
        })
    }

    /// Format for unknown / generic HTTP endpoints. Predictable key set
    /// for bridges and custom consumers.
    #[must_use]
    pub fn to_generic_json(&self) -> Value {
        json!({
            "event": "crosslink.alert.opened",
            "kind": self.kind,
            "severity": self.severity.as_str(),
            "project_slug": self.project_slug,
            "subject_ref": self.subject_ref,
            "detail": self.detail,
            "opened_at": self.opened_at.to_rfc3339(),
        })
    }

    /// Pick the payload shape that matches `url`.
    #[must_use]
    pub fn payload_for(&self, url: &str) -> Value {
        if is_slack_url(url) {
            self.to_slack_json()
        } else if is_discord_url(url) {
            self.to_discord_json()
        } else {
            self.to_generic_json()
        }
    }
}

const fn severity_emoji(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "rotating_light",
        Severity::Warning => "warning",
        Severity::Info => "information_source",
    }
}

/// True if `url` points to Slack's incoming-webhook domain.
#[must_use]
pub fn is_slack_url(url: &str) -> bool {
    url.contains("://hooks.slack.com/") || url.contains("://slack.com/api/webhooks/")
}

/// True if `url` points to Discord's webhook domain (both historical
/// and current hostnames).
#[must_use]
pub fn is_discord_url(url: &str) -> bool {
    url.contains("://discord.com/api/webhooks/")
        || url.contains("://discordapp.com/api/webhooks/")
        || url.contains("://canary.discord.com/api/webhooks/")
}

/// Mask a URL down to its scheme + host for safe logging. Secrets in
/// Slack/Discord webhook URLs live in the path, which we drop.
#[must_use]
pub fn mask_url(url: &str) -> String {
    url.find("://").map_or_else(
        || "<invalid-url>".to_string(),
        |scheme_end| {
            let rest = &url[scheme_end + 3..];
            let host_end = rest.find('/').unwrap_or(rest.len());
            format!("{}://{}/…", &url[..scheme_end], &rest[..host_end])
        },
    )
}

/// Minimal URL validation: must use https, or http pointed at a
/// loopback host (for dev/testing and local bridges). We don't parse
/// the full URL grammar — we only enforce scheme + host prefix, which
/// is enough to keep a fat-fingered "example.com" out of the store.
///
/// # Errors
/// Returns `Err` on unsupported schemes, or http URLs targeting a
/// non-loopback host.
pub fn validate_url(url: &str) -> Result<(), String> {
    if let Some(rest) = url.strip_prefix("https://") {
        if rest.is_empty() || rest.starts_with('/') {
            return Err("https URL missing host".into());
        }
        return Ok(());
    }
    if let Some(rest) = url.strip_prefix("http://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let host_only = host.rsplit_once('@').map_or(host, |(_, h)| h);
        let host_no_port = host_only.rsplit_once(':').map_or(host_only, |(h, _)| h);
        match host_no_port {
            "localhost" | "127.0.0.1" | "[::1]" | "::1" => return Ok(()),
            _ => return Err("http webhooks are only allowed for loopback; use https".into()),
        }
    }
    Err("unsupported scheme; expected https (or http on loopback)".into())
}

/// Load the configured webhook URLs from the `config` table. Returns
/// an empty vec when the key is missing, so first-run behaves as "no
/// webhooks configured".
///
/// # Errors
/// Propagates `SQLite` errors other than missing rows.
pub fn load_urls(db: &DashboardDb) -> Result<Vec<String>> {
    let value: Option<String> = db
        .conn
        .query_row(
            "SELECT value FROM config WHERE key = ?1",
            params![KEY_WEBHOOK_URLS],
            |row| row.get(0),
        )
        .map_or_else(
            |e| {
                if matches!(e, rusqlite::Error::QueryReturnedNoRows) {
                    Ok(None)
                } else {
                    Err(e)
                }
            },
            |v: String| Ok(Some(v)),
        )?;

    let Some(raw) = value else {
        return Ok(Vec::new());
    };
    let urls: Vec<String> =
        serde_json::from_str(&raw).context("decoding stored webhook.urls JSON")?;
    Ok(urls)
}

/// Persist the given URL list, replacing whatever was there. An empty
/// list deletes the row so `load_urls` short-circuits next time.
///
/// # Errors
/// Propagates any `SQLite` error.
pub fn save_urls(db: &DashboardDb, urls: &[String]) -> Result<()> {
    if urls.is_empty() {
        db.conn.execute(
            "DELETE FROM config WHERE key = ?1",
            params![KEY_WEBHOOK_URLS],
        )?;
        return Ok(());
    }
    let payload = serde_json::to_string(urls).context("encoding webhook.urls JSON")?;
    db.conn.execute(
        "INSERT INTO config (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![KEY_WEBHOOK_URLS, payload],
    )?;
    Ok(())
}

/// HTTP timeout for a single webhook dispatch. Keeps a stuck endpoint
/// from holding open the fire-and-forget task for longer than one
/// poll tick.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(5);

/// POST the appropriate payload shape to `url`. Returns `Ok(())` on a
/// 2xx response and an error describing the failure otherwise.
///
/// # Errors
/// Network error, non-2xx response, or client-construction failure.
pub async fn dispatch(url: &str, notification: &AlertNotification) -> Result<()> {
    let payload = notification.payload_for(url);
    let client = reqwest::Client::builder()
        .timeout(DISPATCH_TIMEOUT)
        .build()
        .context("building reqwest client")?;
    let resp = client
        .post(url)
        .json(&payload)
        .send()
        .await
        .with_context(|| format!("POST to {}", mask_url(url)))?;
    let status = resp.status();
    if !status.is_success() {
        let body_snippet = resp.text().await.unwrap_or_default();
        let trimmed = body_snippet.chars().take(200).collect::<String>();
        anyhow::bail!("webhook {} returned {status}: {trimmed}", mask_url(url));
    }
    Ok(())
}

/// Fire one notification at every configured URL. Errors are logged,
/// not returned — the caller is in the poll loop and must not abort
/// other work.
pub async fn dispatch_all(urls: &[String], notification: &AlertNotification) {
    for url in urls {
        if let Err(e) = dispatch(url, notification).await {
            tracing::warn!("webhook dispatch failed for {}: {e:#}", mask_url(url),);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_notification(severity: Severity) -> AlertNotification {
        AlertNotification {
            kind: "stale_lock".into(),
            severity,
            project_slug: "owner/repo".into(),
            subject_ref: "lock:42".into(),
            detail: "held > 60 minutes".into(),
            opened_at: DateTime::parse_from_rfc3339("2026-04-21T00:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        }
    }

    #[test]
    fn test_is_slack_url() {
        assert!(is_slack_url("https://hooks.slack.com/services/T00/B00/xxx"));
        assert!(!is_slack_url("https://hooks.slack.evil.com/services/..."));
        assert!(!is_slack_url("https://example.com/slack"));
    }

    #[test]
    fn test_is_discord_url() {
        assert!(is_discord_url("https://discord.com/api/webhooks/123/abc"));
        assert!(is_discord_url(
            "https://discordapp.com/api/webhooks/123/abc"
        ));
        assert!(!is_discord_url("https://example.com/discord"));
    }

    #[test]
    fn test_validate_url_https_ok() {
        assert!(validate_url("https://hooks.slack.com/services/a/b/c").is_ok());
    }

    #[test]
    fn test_validate_url_rejects_unsupported_scheme() {
        assert!(validate_url("ftp://example.com").is_err());
    }

    #[test]
    fn test_validate_url_allows_http_loopback_only() {
        assert!(validate_url("http://127.0.0.1:8080/hook").is_ok());
        assert!(validate_url("http://localhost:8080/hook").is_ok());
        assert!(validate_url("http://example.com/hook").is_err());
    }

    #[test]
    fn test_mask_url_drops_path() {
        assert_eq!(
            mask_url("https://hooks.slack.com/services/T00/B00/secret"),
            "https://hooks.slack.com/…"
        );
        assert_eq!(
            mask_url("http://127.0.0.1:8080/hook/token"),
            "http://127.0.0.1:8080/…"
        );
    }

    #[test]
    fn test_slack_payload_shape() {
        let n = sample_notification(Severity::Critical);
        let v = n.to_slack_json();
        let blocks = v["blocks"].as_array().unwrap();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0]["type"], "header");
        let header_text = blocks[0]["text"]["text"].as_str().unwrap();
        assert!(header_text.contains("stale_lock"));
        assert!(header_text.contains(":rotating_light:"));
    }

    #[test]
    fn test_discord_payload_shape() {
        let n = sample_notification(Severity::Warning);
        let v = n.to_discord_json();
        assert!(v["content"].as_str().unwrap().contains("owner/repo"));
        let embed = &v["embeds"][0];
        assert_eq!(embed["title"], "stale_lock");
        assert_eq!(embed["color"].as_u64().unwrap(), 0x00F5_9E0B);
    }

    #[test]
    fn test_generic_payload_shape() {
        let n = sample_notification(Severity::Info);
        let v = n.to_generic_json();
        assert_eq!(v["event"], "crosslink.alert.opened");
        assert_eq!(v["kind"], "stale_lock");
        assert_eq!(v["severity"], "info");
        assert_eq!(v["project_slug"], "owner/repo");
    }

    #[test]
    fn test_payload_for_routes_by_url() {
        let n = sample_notification(Severity::Warning);
        let slack = n.payload_for("https://hooks.slack.com/services/a/b/c");
        assert!(slack.get("blocks").is_some());
        let discord = n.payload_for("https://discord.com/api/webhooks/1/abc");
        assert!(discord.get("embeds").is_some());
        let generic = n.payload_for("https://example.com/hook");
        assert_eq!(generic["event"], "crosslink.alert.opened");
    }

    #[test]
    fn test_load_save_urls_roundtrip() {
        let dir = tempdir().unwrap();
        let db = DashboardDb::open(&dir.path().join("d.db")).unwrap();
        assert!(load_urls(&db).unwrap().is_empty());

        let urls = vec![
            "https://hooks.slack.com/services/a/b/c".to_string(),
            "https://discord.com/api/webhooks/1/xyz".to_string(),
        ];
        save_urls(&db, &urls).unwrap();
        assert_eq!(load_urls(&db).unwrap(), urls);

        // Empty list clears the row.
        save_urls(&db, &[]).unwrap();
        assert!(load_urls(&db).unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_dispatch_success_on_2xx() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = std::sync::Arc::new(tokio::sync::Mutex::new(None::<String>));
        let received_clone = std::sync::Arc::clone(&received);

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let n = socket.read(&mut buf).await.unwrap();
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            *received_clone.lock().await = Some(request);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            let _ = socket.shutdown().await;
        });

        let n = sample_notification(Severity::Critical);
        let url = format!("http://{addr}/hook");
        dispatch(&url, &n).await.unwrap();

        let got = received.lock().await.clone().unwrap();
        assert!(got.starts_with("POST /hook "));
        assert!(got.contains("content-type: application/json"));
        assert!(got.contains("crosslink.alert.opened"));
    }

    #[tokio::test]
    async fn test_dispatch_error_on_non_2xx() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let _ = socket.read(&mut buf).await.unwrap();
            socket
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\n\
                      Content-Length: 13\r\n\r\n\
                      upstream oops",
                )
                .await
                .unwrap();
            let _ = socket.shutdown().await;
        });

        let n = sample_notification(Severity::Warning);
        let url = format!("http://{addr}/hook");
        let err = dispatch(&url, &n).await.unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("500"), "expected 500 in error, got {msg}");
    }
}
