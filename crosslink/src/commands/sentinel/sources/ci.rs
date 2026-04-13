use anyhow::{Context, Result};
use chrono::Utc;
use serde::Deserialize;
use std::process::Command;

use super::{Signal, SignalKind, Source, SourceKind};

/// A GitHub Actions workflow run as returned by `gh run list --json`.
#[derive(Debug, Deserialize)]
struct GhRun {
    #[serde(rename = "databaseId")]
    database_id: i64,
    #[serde(rename = "headBranch")]
    head_branch: String,
    name: String,
    conclusion: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
    url: Option<String>,
}

/// Polls GitHub Actions for failed workflow runs on the default branch.
pub struct GitHubCISource {
    default_branch: Option<String>,
}

impl GitHubCISource {
    pub const fn new() -> Self {
        Self {
            default_branch: None,
        }
    }

    /// Detect the default branch via `gh repo view`.
    fn detect_default_branch(&mut self) -> Result<String> {
        if let Some(ref branch) = self.default_branch {
            return Ok(branch.clone());
        }
        let output = Command::new("gh")
            .args([
                "repo",
                "view",
                "--json",
                "defaultBranchRef",
                "-q",
                ".defaultBranchRef.name",
            ])
            .output()
            .context("Failed to detect default branch")?;
        if !output.status.success() {
            anyhow::bail!(
                "gh repo view failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if branch.is_empty() {
            anyhow::bail!("Could not detect default branch");
        }
        self.default_branch = Some(branch.clone());
        Ok(branch)
    }
}

impl Source for GitHubCISource {
    fn name(&self) -> &'static str {
        "github-ci"
    }

    fn poll(&mut self) -> Result<Vec<Signal>> {
        let branch = self.detect_default_branch()?;

        let output = Command::new("gh")
            .args([
                "run",
                "list",
                "--branch",
                &branch,
                "--status",
                "failure",
                "--json",
                "databaseId,headBranch,name,conclusion,createdAt,url",
                "--limit",
                "10",
            ])
            .output()
            .context("Failed to run `gh run list`")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("gh run list failed: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() || stdout.trim() == "[]" {
            return Ok(Vec::new());
        }

        let runs: Vec<GhRun> =
            serde_json::from_str(&stdout).context("Failed to parse gh run list output")?;

        let now = Utc::now();
        let signals = runs
            .into_iter()
            .filter(|r| r.conclusion.as_deref() == Some("failure"))
            .map(|run| Signal {
                source: SourceKind::CI,
                kind: SignalKind::CIFailure,
                reference: format!("CI:run/{}", run.database_id),
                title: format!("CI failure: {} on {}", run.name, run.head_branch),
                body: format!(
                    "Workflow '{}' failed on branch '{}'. Run URL: {}",
                    run.name,
                    run.head_branch,
                    run.url.as_deref().unwrap_or("unknown")
                ),
                metadata: serde_json::json!({
                    "run_id": run.database_id,
                    "workflow": run.name,
                    "branch": run.head_branch,
                    "created_at": run.created_at,
                    "url": run.url,
                }),
                detected_at: now,
            })
            .collect();

        Ok(signals)
    }
}
