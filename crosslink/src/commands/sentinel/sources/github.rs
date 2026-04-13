use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::Deserialize;
use std::process::Command;

use super::{Signal, SignalKind, Source, SourceKind};
use crate::commands::sentinel::config::SentinelConfig;

/// A GitHub issue as returned by `gh issue list --json`.
#[derive(Debug, Deserialize)]
struct GhIssue {
    number: i64,
    title: String,
    body: Option<String>,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
}

/// Polls GitHub for issues with `agent-todo:*` labels via the `gh` CLI.
pub struct GitHubLabelSource {
    labels: Vec<String>,
    repo: Option<String>,
}

impl GitHubLabelSource {
    pub fn new(config: &SentinelConfig) -> Self {
        Self {
            labels: config.sources.github_labels.labels.clone(),
            repo: None,
        }
    }

    /// Detect the current repo's owner/name via `gh repo view`.
    fn detect_repo(&mut self) -> Result<String> {
        if let Some(ref repo) = self.repo {
            return Ok(repo.clone());
        }
        let output = Command::new("gh")
            .args([
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "-q",
                ".nameWithOwner",
            ])
            .output()
            .context("Failed to run `gh repo view`. Is `gh` installed?")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("authentication") || stderr.contains("login") {
                bail!("GitHub CLI not authenticated. Run `gh auth login` first.");
            }
            bail!("Failed to detect repository: {}", stderr.trim());
        }
        let repo = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if repo.is_empty() {
            bail!("Could not detect repository. Are you in a git repo with a GitHub remote?");
        }
        self.repo = Some(repo.clone());
        Ok(repo)
    }

    /// Poll GitHub for issues matching a single label.
    fn poll_label(repo: &str, label: &str) -> Result<Vec<Signal>> {
        let output = Command::new("gh")
            .args([
                "issue",
                "list",
                "--repo",
                repo,
                "--label",
                label,
                "--json",
                "number,title,body,labels,createdAt",
                "--state",
                "open",
                "--limit",
                "50",
            ])
            .output()
            .context("Failed to run `gh issue list`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("rate limit") || stderr.contains("403") || stderr.contains("429") {
                bail!("GitHub API rate limit exceeded");
            }
            bail!("gh issue list failed: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() || stdout.trim() == "[]" {
            return Ok(Vec::new());
        }

        let issues: Vec<GhIssue> = serde_json::from_str(&stdout)
            .with_context(|| format!("Failed to parse gh output for label '{label}'"))?;

        let label_suffix = label.strip_prefix("agent-todo: ").unwrap_or(label);
        let now = Utc::now();

        let signals = issues
            .into_iter()
            .filter_map(|issue| {
                // Defensive: verify the label we asked for is actually present in the
                // response. Protects against future `gh` API changes or filter bugs.
                let all_label_names: Vec<String> =
                    issue.labels.iter().map(|l| l.name.clone()).collect();
                if !all_label_names.iter().any(|name| name == label) {
                    tracing::warn!(
                        "GH#{} returned for label '{}' but doesn't have it (has: {:?}); skipping",
                        issue.number,
                        label,
                        all_label_names
                    );
                    return None;
                }

                Some(Signal {
                    source: SourceKind::GitHub,
                    kind: SignalKind::LabelAdded,
                    reference: format!("GH#{}:{}", issue.number, label_suffix),
                    title: issue.title,
                    body: issue.body.unwrap_or_default(),
                    metadata: serde_json::json!({
                        "label": label,
                        "number": issue.number,
                        "created_at": issue.created_at,
                        // Include all labels so triage engine can route on label combinations
                        "all_labels": all_label_names,
                    }),
                    detected_at: now,
                })
            })
            .collect();

        Ok(signals)
    }
}

impl Source for GitHubLabelSource {
    fn name(&self) -> &'static str {
        "github-labels"
    }

    fn poll(&mut self) -> Result<Vec<Signal>> {
        let repo = self.detect_repo()?;
        let mut all_signals = Vec::new();

        for label in self.labels.clone() {
            match Self::poll_label(&repo, &label) {
                Ok(signals) => all_signals.extend(signals),
                Err(e) => {
                    tracing::warn!("failed to poll label '{label}': {e}");
                }
            }
        }

        Ok(all_signals)
    }
}
