use anyhow::Result;
use chrono::Utc;
use std::path::Path;
use std::process::Command;

use super::{Signal, SignalKind, Source, SourceKind};

/// Configuration for the maintenance sweep source.
pub struct MaintenanceSweepConfig {
    pub lint_enabled: bool,
    pub test_coverage_enabled: bool,
    pub lint_warning_threshold: u64,
}

impl Default for MaintenanceSweepConfig {
    fn default() -> Self {
        Self {
            lint_enabled: true,
            test_coverage_enabled: false,
            lint_warning_threshold: 10,
        }
    }
}

/// Runs local maintenance commands and emits signals when quality drifts.
///
/// Checks:
/// - Lint warnings above threshold (`cargo clippy` / `npm run lint`)
/// - Test failures (`cargo test` / `npm test`)
pub struct MaintenanceSweepSource {
    config: MaintenanceSweepConfig,
    repo_root: std::path::PathBuf,
}

impl MaintenanceSweepSource {
    pub fn new(repo_root: &Path, config: MaintenanceSweepConfig) -> Self {
        Self {
            config,
            repo_root: repo_root.to_path_buf(),
        }
    }

    /// Count lint warnings from `cargo clippy`.
    fn check_clippy_warnings(&self) -> Option<Signal> {
        let Ok(output) = Command::new("cargo")
            .args(["clippy", "--message-format=short", "--quiet"])
            .current_dir(&self.repo_root)
            .output()
        else {
            return None; // cargo not available
        };

        let stderr = String::from_utf8_lossy(&output.stderr);
        let warning_count = stderr.lines().filter(|l| l.contains("warning")).count() as u64;

        if warning_count >= self.config.lint_warning_threshold {
            let now = Utc::now();
            Some(Signal {
                source: SourceKind::Internal,
                kind: SignalKind::StaleIssue,
                reference: format!("MAINT:clippy:{}", now.format("%Y%m%d")),
                title: format!("Lint drift: {warning_count} clippy warnings"),
                body: format!(
                    "{warning_count} clippy warnings detected (threshold: {}). \
                     Run `cargo clippy` for details.",
                    self.config.lint_warning_threshold
                ),
                metadata: serde_json::json!({
                    "type": "lint_drift",
                    "tool": "clippy",
                    "warning_count": warning_count,
                    "threshold": self.config.lint_warning_threshold,
                }),
                detected_at: now,
            })
        } else {
            None
        }
    }

    /// Run `cargo test` and check for failures.
    fn check_test_failures(&self) -> Option<Signal> {
        let Ok(output) = Command::new("cargo")
            .args(["test", "--no-fail-fast", "--quiet"])
            .current_dir(&self.repo_root)
            .env("RUST_BACKTRACE", "0")
            .output()
        else {
            return None;
        };

        if output.status.success() {
            return None;
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let failure_lines: Vec<&str> = stderr
            .lines()
            .filter(|l| l.contains("FAILED") || l.contains("test result: FAILED"))
            .collect();

        let failure_count = failure_lines.len();
        if failure_count > 0 {
            let now = Utc::now();
            let details = failure_lines.join("\n");
            Some(Signal {
                source: SourceKind::Internal,
                kind: SignalKind::StaleIssue,
                reference: format!("MAINT:test-fail:{}", now.format("%Y%m%d")),
                title: format!("Test regression: {failure_count} failure(s)"),
                body: format!(
                    "{failure_count} test failure(s) detected:\n```\n{}\n```",
                    &details[..details.len().min(2000)]
                ),
                metadata: serde_json::json!({
                    "type": "test_regression",
                    "failure_count": failure_count,
                }),
                detected_at: now,
            })
        } else {
            None
        }
    }
}

impl Source for MaintenanceSweepSource {
    fn name(&self) -> &'static str {
        "maintenance-sweep"
    }

    fn poll(&mut self) -> Result<Vec<Signal>> {
        let mut signals = Vec::new();

        if self.config.lint_enabled {
            if let Some(s) = self.check_clippy_warnings() {
                signals.push(s);
            }
        }

        if self.config.test_coverage_enabled {
            if let Some(s) = self.check_test_failures() {
                signals.push(s);
            }
        }

        Ok(signals)
    }
}
