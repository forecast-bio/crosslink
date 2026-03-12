use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::process::Command;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// A ready-to-file GitHub issue built from a review finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueTemplate {
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
}

/// The outcome of a batch filing run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilingResult {
    pub filed: Vec<FiledIssue>,
    pub skipped: Vec<SkippedIssue>,
}

/// An issue that was successfully created on GitHub.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FiledIssue {
    pub number: u64,
    pub title: String,
    pub url: String,
}

/// An issue that was not filed (e.g. duplicate detected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedIssue {
    pub title: String,
    pub reason: String,
}

/// Consolidated review finding ready for filing. Self-contained so this
/// module can be developed independently of findings.rs — the types will be
/// unified later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingForFiling {
    pub title: String,
    pub severity: String,
    pub file: String,
    pub line: Option<usize>,
    pub description: String,
    pub suggested_fix: Option<String>,
    pub consensus_count: usize,
}

// ---------------------------------------------------------------------------
// Template building
// ---------------------------------------------------------------------------

/// Build a GitHub issue template from a review finding.
///
/// - Title is prefixed with the severity in brackets.
/// - Body is structured markdown with Description, Location, Suggested Fix,
///   and Metadata sections.
/// - Labels are derived from severity: critical/high -> "bug", medium ->
///   "enhancement", low/info -> "tech-debt". "review-finding" is always added.
pub fn build_issue_template(finding: &FindingForFiling) -> IssueTemplate {
    let severity_upper = finding.severity.to_uppercase();
    let title = format!("[{}] {}", severity_upper, finding.title);

    let location = match finding.line {
        Some(line) => format!("`{}:{}`", finding.file, line),
        None => format!("`{}`", finding.file),
    };

    let suggested_fix_section = match &finding.suggested_fix {
        Some(fix) => format!("## Suggested Fix\n\n{}\n", fix),
        None => String::new(),
    };

    let body = format!(
        "## Description\n\n{}\n\n## Location\n\n{}\n\n{}## Metadata\n\n- **Severity**: {}\n- **Consensus count**: {}\n- **Source**: automated swarm review\n",
        finding.description,
        location,
        suggested_fix_section,
        severity_upper,
        finding.consensus_count,
    );

    let severity_label = match severity_upper.as_str() {
        "CRITICAL" | "HIGH" => "bug",
        "MEDIUM" => "enhancement",
        _ => "tech-debt", // LOW, INFO, or anything else
    };

    let labels = vec![severity_label.to_string(), "review-finding".to_string()];

    IssueTemplate {
        title,
        body,
        labels,
    }
}

// ---------------------------------------------------------------------------
// Duplicate detection
// ---------------------------------------------------------------------------

/// Normalize a title for comparison: lowercase, strip leading `[SEVERITY]`
/// prefix brackets, collapse whitespace.
fn normalize_title(title: &str) -> String {
    let trimmed = title.trim().to_lowercase();
    // Strip a leading "[…]" severity prefix if present.
    let without_prefix = if trimmed.starts_with('[') {
        match trimmed.find(']') {
            Some(idx) => trimmed[idx + 1..].trim_start().to_string(),
            None => trimmed,
        }
    } else {
        trimmed
    };
    // Collapse multiple spaces.
    without_prefix
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract the set of words from a string.
fn word_set(s: &str) -> HashSet<String> {
    s.split_whitespace().map(|w| w.to_string()).collect()
}

/// Jaccard similarity between two word sets.
fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// Check whether `title` is a likely duplicate of any entry in
/// `existing_issues`. Uses normalized comparison and word-overlap Jaccard
/// similarity with a threshold of 0.7.
pub fn check_duplicate(title: &str, existing_issues: &[String]) -> bool {
    let norm = normalize_title(title);
    let norm_words = word_set(&norm);

    for existing in existing_issues {
        let existing_norm = normalize_title(existing);

        // Exact match after normalization.
        if norm == existing_norm {
            return true;
        }

        // Substring containment (either direction).
        if norm.contains(&existing_norm) || existing_norm.contains(&norm) {
            return true;
        }

        // Jaccard word-overlap.
        let existing_words = word_set(&existing_norm);
        if jaccard(&norm_words, &existing_words) > 0.7 {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// Filing via gh CLI
// ---------------------------------------------------------------------------

/// Fetch the titles of currently open issues from GitHub via `gh issue list`.
fn fetch_existing_issue_titles() -> Result<Vec<String>> {
    let output = Command::new("gh")
        .args(["issue", "list", "--json", "title", "--limit", "500"])
        .output()
        .context("failed to run `gh issue list`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh issue list failed: {}", stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(&stdout).context("failed to parse gh issue list JSON")?;

    let titles: Vec<String> = entries
        .iter()
        .filter_map(|v| v.get("title").and_then(|t| t.as_str()).map(String::from))
        .collect();

    Ok(titles)
}

/// Create a single issue via `gh issue create` and return `(number, url)`.
fn create_issue_via_gh(template: &IssueTemplate) -> Result<(u64, String)> {
    let labels_arg = template.labels.join(",");

    let output = Command::new("gh")
        .args([
            "issue",
            "create",
            "--title",
            &template.title,
            "--body",
            &template.body,
            "--label",
            &labels_arg,
        ])
        .output()
        .context("failed to run `gh issue create`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("gh issue create failed: {}", stderr);
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();

    // gh issue create prints the URL — extract the issue number from the
    // trailing path segment (e.g. …/issues/42).
    let number = url
        .rsplit('/')
        .next()
        .and_then(|seg| seg.parse::<u64>().ok())
        .unwrap_or(0);

    Ok((number, url))
}

/// File issues for the given findings. Existing open issues are fetched first
/// so duplicates can be detected and skipped.
///
/// When `dry_run` is true no issues are actually created — the result shows
/// what *would* happen.
pub fn file_issues(findings: &[FindingForFiling], dry_run: bool) -> Result<FilingResult> {
    let existing_titles = if dry_run {
        // In dry-run mode we still try to fetch existing issues so we can
        // report duplicates accurately, but we don't fail if gh is
        // unavailable.
        fetch_existing_issue_titles().unwrap_or_default()
    } else {
        fetch_existing_issue_titles()?
    };

    let mut filed: Vec<FiledIssue> = Vec::new();
    let mut skipped: Vec<SkippedIssue> = Vec::new();

    for finding in findings {
        let template = build_issue_template(finding);

        if check_duplicate(&template.title, &existing_titles) {
            skipped.push(SkippedIssue {
                title: template.title,
                reason: "duplicate of existing issue".to_string(),
            });
            continue;
        }

        if dry_run {
            filed.push(FiledIssue {
                number: 0,
                title: template.title,
                url: "(dry run)".to_string(),
            });
        } else {
            let (number, url) = create_issue_via_gh(&template)?;
            filed.push(FiledIssue {
                number,
                title: template.title,
                url,
            });
        }
    }

    Ok(FilingResult { filed, skipped })
}

/// File issues with a summary table printed to stdout afterwards.
pub fn file_issues_batch(findings: &[FindingForFiling], dry_run: bool) -> Result<FilingResult> {
    let result = file_issues(findings, dry_run)?;

    // -- Summary table -------------------------------------------------------
    if dry_run {
        println!("=== Dry Run Summary ===");
    } else {
        println!("=== Filing Summary ===");
    }

    if !result.filed.is_empty() {
        println!("\nFiled ({}):", result.filed.len());
        for issue in &result.filed {
            if dry_run {
                println!("  - {}", issue.title);
            } else {
                println!("  - #{} {} ({})", issue.number, issue.title, issue.url);
            }
        }
    }

    if !result.skipped.is_empty() {
        println!("\nSkipped ({}):", result.skipped.len());
        for issue in &result.skipped {
            println!("  - {} ({})", issue.title, issue.reason);
        }
    }

    println!(
        "\nTotal: {} filed, {} skipped",
        result.filed.len(),
        result.skipped.len()
    );

    Ok(result)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- build_issue_template ------------------------------------------------

    fn make_finding(severity: &str) -> FindingForFiling {
        FindingForFiling {
            title: "Buffer overflow in parser".to_string(),
            severity: severity.to_string(),
            file: "src/parser.rs".to_string(),
            line: Some(42),
            description: "Unchecked index into byte slice.".to_string(),
            suggested_fix: Some("Add bounds check before indexing.".to_string()),
            consensus_count: 3,
        }
    }

    #[test]
    fn template_title_includes_severity_prefix() {
        let tmpl = build_issue_template(&make_finding("high"));
        assert_eq!(tmpl.title, "[HIGH] Buffer overflow in parser");
    }

    #[test]
    fn template_body_contains_description_section() {
        let tmpl = build_issue_template(&make_finding("high"));
        assert!(tmpl.body.contains("## Description"));
        assert!(tmpl.body.contains("Unchecked index into byte slice."));
    }

    #[test]
    fn template_body_contains_location_with_line() {
        let tmpl = build_issue_template(&make_finding("high"));
        assert!(tmpl.body.contains("`src/parser.rs:42`"));
    }

    #[test]
    fn template_body_location_without_line() {
        let mut f = make_finding("medium");
        f.line = None;
        let tmpl = build_issue_template(&f);
        assert!(tmpl.body.contains("`src/parser.rs`"));
        assert!(!tmpl.body.contains(":42"));
    }

    #[test]
    fn template_body_contains_suggested_fix() {
        let tmpl = build_issue_template(&make_finding("high"));
        assert!(tmpl.body.contains("## Suggested Fix"));
        assert!(tmpl.body.contains("Add bounds check before indexing."));
    }

    #[test]
    fn template_body_omits_suggested_fix_when_none() {
        let mut f = make_finding("high");
        f.suggested_fix = None;
        let tmpl = build_issue_template(&f);
        assert!(!tmpl.body.contains("## Suggested Fix"));
    }

    #[test]
    fn template_body_contains_metadata() {
        let tmpl = build_issue_template(&make_finding("high"));
        assert!(tmpl.body.contains("**Severity**: HIGH"));
        assert!(tmpl.body.contains("**Consensus count**: 3"));
    }

    // -- label mapping -------------------------------------------------------

    #[test]
    fn labels_critical_maps_to_bug() {
        let tmpl = build_issue_template(&make_finding("critical"));
        assert!(tmpl.labels.contains(&"bug".to_string()));
        assert!(tmpl.labels.contains(&"review-finding".to_string()));
    }

    #[test]
    fn labels_high_maps_to_bug() {
        let tmpl = build_issue_template(&make_finding("high"));
        assert!(tmpl.labels.contains(&"bug".to_string()));
    }

    #[test]
    fn labels_medium_maps_to_enhancement() {
        let tmpl = build_issue_template(&make_finding("medium"));
        assert!(tmpl.labels.contains(&"enhancement".to_string()));
    }

    #[test]
    fn labels_low_maps_to_tech_debt() {
        let tmpl = build_issue_template(&make_finding("low"));
        assert!(tmpl.labels.contains(&"tech-debt".to_string()));
    }

    #[test]
    fn labels_info_maps_to_tech_debt() {
        let tmpl = build_issue_template(&make_finding("info"));
        assert!(tmpl.labels.contains(&"tech-debt".to_string()));
    }

    #[test]
    fn labels_always_include_review_finding() {
        for sev in &["critical", "high", "medium", "low", "info"] {
            let tmpl = build_issue_template(&make_finding(sev));
            assert!(
                tmpl.labels.contains(&"review-finding".to_string()),
                "missing review-finding label for severity {}",
                sev
            );
        }
    }

    // -- check_duplicate -----------------------------------------------------

    #[test]
    fn duplicate_exact_match() {
        let existing = vec!["[HIGH] Buffer overflow in parser".to_string()];
        assert!(check_duplicate(
            "[HIGH] Buffer overflow in parser",
            &existing
        ));
    }

    #[test]
    fn duplicate_ignores_severity_prefix() {
        let existing = vec!["Buffer overflow in parser".to_string()];
        assert!(check_duplicate(
            "[HIGH] Buffer overflow in parser",
            &existing
        ));
    }

    #[test]
    fn duplicate_case_insensitive() {
        let existing = vec!["buffer overflow in parser".to_string()];
        assert!(check_duplicate(
            "[HIGH] Buffer Overflow In Parser",
            &existing
        ));
    }

    #[test]
    fn duplicate_close_match_via_jaccard() {
        // Same core words, minor variation — should exceed 0.7 Jaccard.
        let existing = vec!["Buffer overflow found in the parser module".to_string()];
        assert!(check_duplicate(
            "[HIGH] Buffer overflow in parser module",
            &existing
        ));
    }

    #[test]
    fn no_false_positive_on_unrelated_titles() {
        let existing = vec!["Fix typo in README".to_string()];
        assert!(!check_duplicate(
            "[HIGH] Buffer overflow in parser",
            &existing
        ));
    }

    #[test]
    fn no_false_positive_on_partial_word_overlap() {
        let existing = vec!["Update parser documentation".to_string()];
        assert!(!check_duplicate(
            "[HIGH] Buffer overflow in parser",
            &existing
        ));
    }

    #[test]
    fn duplicate_with_different_severity_prefix() {
        let existing = vec!["[MEDIUM] Buffer overflow in parser".to_string()];
        assert!(check_duplicate(
            "[HIGH] Buffer overflow in parser",
            &existing
        ));
    }

    #[test]
    fn duplicate_empty_existing_list() {
        let existing: Vec<String> = vec![];
        assert!(!check_duplicate("[HIGH] Something", &existing));
    }

    // -- serde roundtrip -----------------------------------------------------

    #[test]
    fn issue_template_serde_roundtrip() {
        let tmpl = build_issue_template(&make_finding("high"));
        let json = serde_json::to_string(&tmpl).unwrap();
        let parsed: IssueTemplate = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, tmpl.title);
        assert_eq!(parsed.body, tmpl.body);
        assert_eq!(parsed.labels, tmpl.labels);
    }

    #[test]
    fn filing_result_serde_roundtrip() {
        let result = FilingResult {
            filed: vec![FiledIssue {
                number: 42,
                title: "[HIGH] Test issue".to_string(),
                url: "https://github.com/org/repo/issues/42".to_string(),
            }],
            skipped: vec![SkippedIssue {
                title: "[LOW] Skipped issue".to_string(),
                reason: "duplicate of existing issue".to_string(),
            }],
        };
        let json = serde_json::to_string(&result).unwrap();
        let parsed: FilingResult = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.filed.len(), 1);
        assert_eq!(parsed.filed[0].number, 42);
        assert_eq!(parsed.skipped.len(), 1);
        assert_eq!(parsed.skipped[0].reason, "duplicate of existing issue");
    }

    #[test]
    fn finding_for_filing_serde_roundtrip() {
        let finding = make_finding("critical");
        let json = serde_json::to_string(&finding).unwrap();
        let parsed: FindingForFiling = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, finding.title);
        assert_eq!(parsed.severity, finding.severity);
        assert_eq!(parsed.line, Some(42));
    }

    // -- normalize_title (internal) ------------------------------------------

    #[test]
    fn normalize_strips_prefix_and_lowercases() {
        assert_eq!(normalize_title("[HIGH] Buffer Overflow"), "buffer overflow");
    }

    #[test]
    fn normalize_handles_no_prefix() {
        assert_eq!(normalize_title("Buffer Overflow"), "buffer overflow");
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize_title("[HIGH]   extra   spaces  "), "extra spaces");
    }
}
