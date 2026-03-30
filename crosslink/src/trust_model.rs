use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Trust model configuration, loaded from `.crosslink/swarm.toml`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrustConfig {
    #[serde(default)]
    pub trust: TrustProfile,
    #[serde(default)]
    pub ignore: IgnoreRules,
    #[serde(default)]
    pub boundaries: BoundaryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustProfile {
    /// Trust model type: "local-only", "multi-tenant", "public-api", "custom"
    #[serde(default = "default_model")]
    pub model: String,
    /// Description of the trust model for agent context
    #[serde(default)]
    pub description: String,
}

fn default_model() -> String {
    "local-only".to_string()
}

impl Default for TrustProfile {
    fn default() -> Self {
        Self {
            model: default_model(),
            description: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IgnoreRules {
    /// Patterns to match against finding titles — matched findings get "by-design" annotation
    #[serde(default)]
    pub patterns: Vec<String>,
    /// Reason shown when a finding is triaged as by-design
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BoundaryConfig {
    /// External interfaces: "http", "ws", "grpc", "cli", "file"
    #[serde(default)]
    pub external: Vec<String>,
    /// Internal/trusted interfaces
    #[serde(default)]
    pub internal: Vec<String>,
}

/// Result of applying trust model to a finding
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriageResult {
    /// Finding is valid and should be reported
    Valid,
    /// Finding is by-design per trust model
    ByDesign { reason: String },
    /// Finding severity should be adjusted
    Downgraded {
        original_severity: String,
        new_severity: String,
        reason: String,
    },
}

/// Load trust configuration from `.crosslink/swarm.toml`.
///
/// Returns a default configuration if the file does not exist.
///
/// # Errors
///
/// Returns an error if the config file exists but cannot be read or parsed.
pub fn load_trust_config(crosslink_dir: &Path) -> Result<TrustConfig> {
    let config_path = crosslink_dir.join("swarm.toml");
    if !config_path.exists() {
        return Ok(TrustConfig::default());
    }
    let contents = std::fs::read_to_string(&config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let config: TrustConfig =
        toml::from_str(&contents).with_context(|| "failed to parse swarm.toml")?;
    Ok(config)
}

/// Check if a finding matches any ignore pattern (case-insensitive substring match).
///
/// Returns `ByDesign` if the finding title matches an ignore pattern, `Valid` otherwise.
#[must_use]
pub fn triage_finding(config: &TrustConfig, title: &str, description: &str) -> TriageResult {
    let title_lower = title.to_lowercase();
    let description_lower = description.to_lowercase();

    // Check ignore patterns — matched findings are triaged as by-design.
    for pattern in &config.ignore.patterns {
        if title_lower.contains(&pattern.to_lowercase()) {
            return TriageResult::ByDesign {
                reason: if config.ignore.reason.is_empty() {
                    format!("matched ignore pattern: {pattern}")
                } else {
                    config.ignore.reason.clone()
                },
            };
        }
    }

    // Check boundary-based downgrade — findings about internal boundaries get
    // severity reduced because internal interfaces have implicit trust.
    for boundary in &config.boundaries.internal {
        let boundary_lower = boundary.to_lowercase();
        if title_lower.contains(&boundary_lower) || description_lower.contains(&boundary_lower) {
            return TriageResult::Downgraded {
                original_severity: "high".to_string(),
                new_severity: "low".to_string(),
                reason: format!(
                    "finding relates to internal boundary '{boundary}' which has implicit trust"
                ),
            };
        }
    }

    TriageResult::Valid
}

/// Apply triage to a list of findings.
///
/// Each finding is a `(title, description, severity)` tuple. Returns each finding
/// annotated with its `TriageResult`. Findings are never silently dropped.
#[must_use]
pub fn apply_trust_model(
    config: &TrustConfig,
    findings: Vec<(String, String, String)>,
) -> Vec<(String, String, String, TriageResult)> {
    findings
        .into_iter()
        .map(|(title, description, severity)| {
            let result = triage_finding(config, &title, &description);
            (title, description, severity, result)
        })
        .collect()
}

/// Generate a sensible default config for common trust models.
#[must_use]
pub fn generate_default_config(model: &str) -> TrustConfig {
    match model {
        "local-only" => TrustConfig {
            trust: TrustProfile {
                model: "local-only".to_string(),
                description: "Single-user local tool — no network auth required".to_string(),
            },
            ignore: IgnoreRules {
                patterns: vec![
                    "authentication".to_string(),
                    "authorization".to_string(),
                    "session management".to_string(),
                ],
                reason: "Local-only tool with no network exposure".to_string(),
            },
            boundaries: BoundaryConfig {
                external: vec!["cli".to_string(), "file".to_string()],
                internal: vec![],
            },
        },
        "multi-tenant" => TrustConfig {
            trust: TrustProfile {
                model: "multi-tenant".to_string(),
                description: "Multi-tenant service with tenant isolation requirements".to_string(),
            },
            ignore: IgnoreRules::default(),
            boundaries: BoundaryConfig {
                external: vec!["http".to_string(), "ws".to_string(), "grpc".to_string()],
                internal: vec![],
            },
        },
        "public-api" => TrustConfig {
            trust: TrustProfile {
                model: "public-api".to_string(),
                description: "Public-facing API with untrusted input".to_string(),
            },
            ignore: IgnoreRules::default(),
            boundaries: BoundaryConfig {
                external: vec!["http".to_string(), "ws".to_string()],
                internal: vec![],
            },
        },
        _ => TrustConfig {
            trust: TrustProfile {
                model: model.to_string(),
                description: String::new(),
            },
            ignore: IgnoreRules::default(),
            boundaries: BoundaryConfig::default(),
        },
    }
}

/// Write a default `swarm.toml` configuration for the given trust model.
///
/// # Errors
///
/// Returns an error if serialization or file writing fails.
pub fn write_default_config(crosslink_dir: &Path, model: &str) -> Result<()> {
    let config = generate_default_config(model);
    let contents =
        toml::to_string_pretty(&config).with_context(|| "failed to serialize trust config")?;
    let config_path = crosslink_dir.join("swarm.toml");
    std::fs::write(&config_path, contents)
        .with_context(|| format!("failed to write {}", config_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn load_returns_default_when_file_missing() {
        let dir = TempDir::new().unwrap();
        let config = load_trust_config(dir.path()).unwrap();
        assert_eq!(config.trust.model, "local-only");
        assert!(config.ignore.patterns.is_empty());
        assert!(config.boundaries.external.is_empty());
    }

    #[test]
    fn load_parses_valid_toml() {
        let dir = TempDir::new().unwrap();
        let toml_content = r#"
[trust]
model = "multi-tenant"
description = "My service"

[ignore]
patterns = ["authentication", "CSRF"]
reason = "Handled by gateway"

[boundaries]
external = ["http", "grpc"]
internal = ["db"]
"#;
        std::fs::write(dir.path().join("swarm.toml"), toml_content).unwrap();
        let config = load_trust_config(dir.path()).unwrap();
        assert_eq!(config.trust.model, "multi-tenant");
        assert_eq!(config.trust.description, "My service");
        assert_eq!(config.ignore.patterns.len(), 2);
        assert_eq!(config.ignore.reason, "Handled by gateway");
        assert_eq!(config.boundaries.external, vec!["http", "grpc"]);
        assert_eq!(config.boundaries.internal, vec!["db"]);
    }

    #[test]
    fn triage_finding_matches_patterns_case_insensitively() {
        let config = TrustConfig {
            ignore: IgnoreRules {
                patterns: vec!["Authentication".to_string()],
                reason: "Not applicable".to_string(),
            },
            ..Default::default()
        };
        let result = triage_finding(&config, "Missing AUTHENTICATION check", "details");
        assert_eq!(
            result,
            TriageResult::ByDesign {
                reason: "Not applicable".to_string()
            }
        );
    }

    #[test]
    fn triage_finding_returns_valid_when_no_patterns_match() {
        let config = TrustConfig {
            ignore: IgnoreRules {
                patterns: vec!["authentication".to_string()],
                reason: "Not applicable".to_string(),
            },
            ..Default::default()
        };
        let result = triage_finding(&config, "SQL injection in query builder", "details");
        assert_eq!(result, TriageResult::Valid);
    }

    #[test]
    fn generate_default_config_local_only() {
        let config = generate_default_config("local-only");
        assert_eq!(config.trust.model, "local-only");
        assert!(!config.ignore.patterns.is_empty());
        assert!(config
            .ignore
            .patterns
            .contains(&"authentication".to_string()));
        assert!(config
            .ignore
            .patterns
            .contains(&"authorization".to_string()));
        assert_eq!(
            config.boundaries.external,
            vec!["cli".to_string(), "file".to_string()]
        );
    }

    #[test]
    fn generate_default_config_multi_tenant() {
        let config = generate_default_config("multi-tenant");
        assert_eq!(config.trust.model, "multi-tenant");
        assert!(config.ignore.patterns.is_empty());
        assert_eq!(
            config.boundaries.external,
            vec!["http".to_string(), "ws".to_string(), "grpc".to_string()]
        );
    }

    #[test]
    fn generate_default_config_public_api() {
        let config = generate_default_config("public-api");
        assert_eq!(config.trust.model, "public-api");
        assert!(config.ignore.patterns.is_empty());
        assert_eq!(
            config.boundaries.external,
            vec!["http".to_string(), "ws".to_string()]
        );
    }

    #[test]
    fn serde_roundtrip_toml() {
        let config = generate_default_config("local-only");
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: TrustConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.trust.model, config.trust.model);
        assert_eq!(deserialized.trust.description, config.trust.description);
        assert_eq!(deserialized.ignore.patterns, config.ignore.patterns);
        assert_eq!(deserialized.ignore.reason, config.ignore.reason);
        assert_eq!(deserialized.boundaries.external, config.boundaries.external);
        assert_eq!(deserialized.boundaries.internal, config.boundaries.internal);
    }

    #[test]
    fn by_design_includes_configured_reason() {
        let config = TrustConfig {
            ignore: IgnoreRules {
                patterns: vec!["auth".to_string()],
                reason: "Local-only tool with no network exposure".to_string(),
            },
            ..Default::default()
        };
        let result = triage_finding(&config, "Missing auth middleware", "desc");
        match result {
            TriageResult::ByDesign { reason } => {
                assert_eq!(reason, "Local-only tool with no network exposure");
            }
            other => panic!("Expected ByDesign, got {:?}", other),
        }
    }

    #[test]
    fn by_design_uses_default_reason_when_empty() {
        let config = TrustConfig {
            ignore: IgnoreRules {
                patterns: vec!["auth".to_string()],
                reason: String::new(),
            },
            ..Default::default()
        };
        let result = triage_finding(&config, "Missing auth middleware", "desc");
        match result {
            TriageResult::ByDesign { reason } => {
                assert!(reason.contains("matched ignore pattern"));
                assert!(reason.contains("auth"));
            }
            other => panic!("Expected ByDesign, got {:?}", other),
        }
    }

    #[test]
    fn apply_trust_model_annotates_all_findings() {
        let config = TrustConfig {
            ignore: IgnoreRules {
                patterns: vec!["authentication".to_string()],
                reason: "Not applicable".to_string(),
            },
            ..Default::default()
        };
        let findings = vec![
            (
                "Missing authentication".to_string(),
                "desc1".to_string(),
                "high".to_string(),
            ),
            (
                "SQL injection".to_string(),
                "desc2".to_string(),
                "critical".to_string(),
            ),
        ];
        let results = apply_trust_model(&config, findings);
        assert_eq!(results.len(), 2);
        assert_eq!(
            results[0].3,
            TriageResult::ByDesign {
                reason: "Not applicable".to_string()
            }
        );
        assert_eq!(results[1].3, TriageResult::Valid);
    }

    #[test]
    fn write_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        write_default_config(dir.path(), "multi-tenant").unwrap();
        let loaded = load_trust_config(dir.path()).unwrap();
        assert_eq!(loaded.trust.model, "multi-tenant");
        assert!(loaded.trust.description.contains("Multi-tenant"));
    }

    #[test]
    fn triage_finding_downgrades_internal_boundary_in_title() {
        let config = TrustConfig {
            boundaries: BoundaryConfig {
                internal: vec!["internal-db".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        let result = triage_finding(&config, "SQL injection via internal-db", "some details");
        match result {
            TriageResult::Downgraded {
                original_severity,
                new_severity,
                reason,
            } => {
                assert_eq!(original_severity, "high");
                assert_eq!(new_severity, "low");
                assert!(reason.contains("internal-db"));
                assert!(reason.contains("implicit trust"));
            }
            other => panic!("Expected Downgraded, got {:?}", other),
        }
    }

    #[test]
    fn triage_finding_downgrades_internal_boundary_in_description() {
        let config = TrustConfig {
            boundaries: BoundaryConfig {
                internal: vec!["message-bus".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };
        // Pattern only in description, not title
        let result = triage_finding(&config, "Unvalidated input", "goes through message-bus");
        match result {
            TriageResult::Downgraded { .. } => {}
            other => panic!("Expected Downgraded, got {:?}", other),
        }
    }

    #[test]
    fn generate_default_config_custom_model() {
        let config = generate_default_config("my-custom-model");
        assert_eq!(config.trust.model, "my-custom-model");
        assert!(config.trust.description.is_empty());
        assert!(config.ignore.patterns.is_empty());
        assert!(config.boundaries.external.is_empty());
        assert!(config.boundaries.internal.is_empty());
    }
}
