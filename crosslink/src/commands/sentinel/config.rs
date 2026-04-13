use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Top-level sentinel configuration from `.crosslink/hook-config.json`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SentinelConfig {
    pub enabled: bool,
    pub interval_minutes: u64,
    pub max_concurrent_agents: u32,
    pub sources: SourcesConfig,
    pub default_agent: DefaultAgentConfig,
    pub escalation: EscalationConfig,
    pub webhook: WebhookServerConfig,
    pub notifications: NotificationConfig,
}

impl Default for SentinelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval_minutes: 10,
            max_concurrent_agents: 3,
            sources: SourcesConfig::default(),
            default_agent: DefaultAgentConfig::default(),
            escalation: EscalationConfig::default(),
            webhook: WebhookServerConfig::default(),
            notifications: NotificationConfig::default(),
        }
    }
}

/// Source adapter configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SourcesConfig {
    pub github_labels: GitHubLabelsConfig,
    pub internal_hygiene: InternalHygieneConfig,
    pub github_ci: GitHubCIConfig,
    pub maintenance_sweep: MaintenanceSweepSourceConfig,
}

/// GitHub label polling configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GitHubLabelsConfig {
    pub enabled: bool,
    pub labels: Vec<String>,
}

impl Default for GitHubLabelsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            labels: vec![
                "agent-todo: replicate".to_string(),
                "agent-todo: fix".to_string(),
            ],
        }
    }
}

/// Internal hygiene source configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct InternalHygieneConfig {
    pub enabled: bool,
    pub stale_threshold_days: i64,
}

impl Default for InternalHygieneConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            stale_threshold_days: 30,
        }
    }
}

/// Maintenance sweep source configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MaintenanceSweepSourceConfig {
    pub enabled: bool,
    pub lint_enabled: bool,
    pub test_coverage_enabled: bool,
    pub lint_warning_threshold: u64,
}

impl Default for MaintenanceSweepSourceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lint_enabled: true,
            test_coverage_enabled: false,
            lint_warning_threshold: 10,
        }
    }
}

/// GitHub CI failure source configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GitHubCIConfig {
    pub enabled: bool,
}

/// Default agent settings for dispatched agents.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DefaultAgentConfig {
    pub model: String,
    pub timeout_minutes: u64,
    /// Verify level as a string ("local", "ci", "thorough"). Parse via `verify_level()`.
    pub verify: String,
}

impl DefaultAgentConfig {
    /// Parse the verify string into a `VerifyLevel`, falling back to `Local` on invalid input.
    pub fn verify_level(&self) -> crate::commands::kickoff::VerifyLevel {
        crate::commands::kickoff::parse_verify_level(&self.verify)
            .unwrap_or(crate::commands::kickoff::VerifyLevel::Local)
    }
}

impl Default for DefaultAgentConfig {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-6".to_string(),
            timeout_minutes: 30,
            verify: "local".to_string(),
        }
    }
}

/// Webhook server configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WebhookServerConfig {
    pub enabled: bool,
    pub port: u16,
    /// GitHub webhook secret for HMAC-SHA256 signature verification.
    /// If None, signatures are not verified (not recommended for production).
    pub secret: Option<String>,
}

impl Default for WebhookServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            port: 9876,
            secret: None,
        }
    }
}

/// Outbound notification configuration.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct NotificationConfig {
    pub enabled: bool,
    /// Webhook URLs to POST dispatch results to. Supports Slack incoming
    /// webhooks (auto-detected by URL pattern) and generic JSON endpoints.
    pub webhook_urls: Vec<String>,
}

impl NotificationConfig {
    /// Check if a URL looks like a Slack incoming webhook.
    pub fn is_slack_url(url: &str) -> bool {
        url.contains("hooks.slack.com")
    }
}

/// Automatic model escalation configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EscalationConfig {
    pub enabled: bool,
    pub model: String,
    pub cooldown_minutes: u64,
    pub max_attempts: u32,
    /// Stored as integer percentage (150 = 1.5x) to avoid float in config.
    pub timeout_multiplier_pct: u32,
}

impl Default for EscalationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            model: "claude-opus-4-6".to_string(),
            cooldown_minutes: 30,
            max_attempts: 2,
            timeout_multiplier_pct: 150,
        }
    }
}

impl SentinelConfig {
    /// Load sentinel config from hook-config.json.
    /// Returns default config if the sentinel key is absent.
    pub fn load(crosslink_dir: &Path) -> Result<Self> {
        let config_path = crosslink_dir.join("hook-config.json");
        if !config_path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read {}", config_path.display()))?;
        let root: serde_json::Value = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", config_path.display()))?;
        match root.get("sentinel") {
            Some(sentinel_val) => {
                let config: SentinelConfig = serde_json::from_value(sentinel_val.clone())
                    .context("Failed to parse sentinel config")?;
                Ok(config)
            }
            None => Ok(Self::default()),
        }
    }
}
