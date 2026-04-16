// Finding consolidation and deduplication for swarm review results.
//
// When multiple agents review a codebase in parallel, they often surface
// overlapping issues.  This module collects per-agent review reports,
// deduplicates them via lightweight word-overlap similarity, and produces a
// single consolidated report with consensus-boosted severities.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

/// A single finding from a review agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub title: String,
    pub severity: FindingSeverity,
    pub file: String,
    pub line: Option<usize>,
    pub description: String,
    pub suggested_fix: Option<String>,
    pub agent: String,
}

/// Severity levels for findings, ordered from most to least severe.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl std::fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Critical => write!(f, "critical"),
            Self::High => write!(f, "high"),
            Self::Medium => write!(f, "medium"),
            Self::Low => write!(f, "low"),
            Self::Info => write!(f, "info"),
        }
    }
}

impl FindingSeverity {
    /// Bump severity up one level (towards Critical).  Critical cannot be
    /// bumped further and stays as-is.
    const fn bumped(self) -> Self {
        match self {
            Self::Info => Self::Low,
            Self::Low => Self::Medium,
            Self::Medium => Self::High,
            Self::High | Self::Critical => Self::Critical,
        }
    }
}

/// A complete review report from a single agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewReport {
    pub agent: String,
    pub partition_label: String,
    pub mandate: String,
    pub findings: Vec<Finding>,
    pub completed_at: Option<String>,
}

/// The consolidated, deduplicated output of `consolidate()`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidatedReport {
    pub title: String,
    pub generated_at: String,
    pub agent_count: usize,
    pub total_findings: usize,
    pub deduplicated_findings: usize,
    pub groups: Vec<FindingGroup>,
}

/// A group of findings deemed to be the same underlying issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingGroup {
    /// The best single description chosen from the group.
    pub canonical: Finding,
    /// Other findings that were merged into this group.
    pub duplicates: Vec<Finding>,
    /// How many distinct agents reported this issue.
    pub consensus_count: usize,
    /// Severity after consensus boosting.
    pub effective_severity: FindingSeverity,
}

// ---------------------------------------------------------------------------
// Similarity
// ---------------------------------------------------------------------------

/// Compute a similarity score in `[0.0, 1.0]` between two findings.
///
/// Components:
/// - Same file: +0.4
/// - Same severity: +0.1
/// - Title word overlap (Jaccard): +0.3 * jaccard
/// - Description word overlap (Jaccard): +0.2 * jaccard
#[must_use]
pub fn similarity_score(a: &Finding, b: &Finding) -> f64 {
    let mut score = 0.0;

    // File match
    if a.file == b.file {
        score += 0.4;
    }

    // Severity match
    if a.severity == b.severity {
        score += 0.1;
    }

    // Title Jaccard
    score += 0.3 * jaccard_similarity(&a.title, &b.title);

    // Description Jaccard
    score += 0.2 * jaccard_similarity(&a.description, &b.description);

    score
}

/// Word-level Jaccard similarity: |intersection| / |union|.
///
/// Words are lowercased and split on whitespace.  Returns 0.0 when both
/// strings are empty.
fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let set_a: HashSet<String> = a.split_whitespace().map(str::to_lowercase).collect();
    let set_b: HashSet<String> = b.split_whitespace().map(str::to_lowercase).collect();

    if set_a.is_empty() && set_b.is_empty() {
        return 0.0;
    }

    let intersection =
        f64::from(u32::try_from(set_a.intersection(&set_b).count()).unwrap_or(u32::MAX));
    let union = f64::from(u32::try_from(set_a.union(&set_b).count()).unwrap_or(u32::MAX));

    intersection / union
}

/// Threshold above which two findings are considered duplicates.
const SIMILARITY_THRESHOLD: f64 = 0.5;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Read all `review-findings-*.json` files from `dir` and deserialize them.
///
/// # Errors
///
/// Returns an error if the directory cannot be read, a matching file cannot be
/// read from disk, or a file contains invalid JSON.
pub fn parse_reports(dir: &Path) -> Result<Vec<ReviewReport>> {
    let mut reports = Vec::new();

    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };

        if file_name.starts_with("review-findings-")
            && std::path::Path::new(&file_name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("json"))
        {
            let content = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let report: ReviewReport = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            reports.push(report);
        }
    }

    Ok(reports)
}

// ---------------------------------------------------------------------------
// Consolidation
// ---------------------------------------------------------------------------

/// Consolidate multiple review reports into a single deduplicated report.
///
/// 1. Collect all findings from all reports.
/// 2. Group findings by similarity (same file + similar title = likely same).
/// 3. For each group, pick the longest description as canonical.
/// 4. Calculate consensus count (unique agents per group).
/// 5. Boost effective severity when 3+ agents report the same issue.
/// 6. Sort groups by effective severity (critical first), then consensus.
pub fn consolidate(reports: Vec<ReviewReport>) -> ConsolidatedReport {
    // Unique agent names.
    let agent_count = reports
        .iter()
        .map(|r| r.agent.as_str())
        .collect::<HashSet<_>>()
        .len();

    // Flatten all findings.
    let all_findings: Vec<Finding> = reports
        .into_iter()
        .flat_map(|r| r.findings.into_iter())
        .collect();
    let total_findings = all_findings.len();

    // Greedy clustering: assign each finding to the first group it matches,
    // or start a new group.
    let mut groups: Vec<Vec<Finding>> = Vec::new();

    for finding in all_findings {
        let mut merged = false;
        for group in &mut groups {
            // Compare against the first member (the proto-canonical).
            if similarity_score(&group[0], &finding) >= SIMILARITY_THRESHOLD {
                group.push(finding.clone());
                merged = true;
                break;
            }
        }
        if !merged {
            groups.push(vec![finding]);
        }
    }

    // Convert raw groups into FindingGroup structs.
    let mut finding_groups: Vec<FindingGroup> =
        groups.into_iter().map(build_finding_group).collect();

    // Sort: effective severity ascending (Critical < High < … is the Ord),
    // then descending consensus count.
    finding_groups.sort_by(|a, b| {
        a.effective_severity
            .cmp(&b.effective_severity)
            .then_with(|| b.consensus_count.cmp(&a.consensus_count))
    });

    let deduplicated_findings = finding_groups.len();

    ConsolidatedReport {
        title: "Consolidated Review Findings".to_string(),
        generated_at: chrono::Utc::now().to_rfc3339(),
        agent_count,
        total_findings,
        deduplicated_findings,
        groups: finding_groups,
    }
}

/// Build a `FindingGroup` from a non-empty vec of similar findings.
fn build_finding_group(mut members: Vec<Finding>) -> FindingGroup {
    assert!(!members.is_empty());

    // Canonical = longest description (richest detail).
    members.sort_by_key(|b| std::cmp::Reverse(b.description.len()));
    let canonical = members.remove(0);

    // Consensus: count distinct agents.
    let mut agents: HashSet<&str> = HashSet::new();
    agents.insert(&canonical.agent);
    for m in &members {
        agents.insert(&m.agent);
    }
    let consensus_count = agents.len();

    // Severity boosting: 3+ agents → bump one level.
    let effective_severity = if consensus_count >= 3 {
        canonical.severity.bumped()
    } else {
        canonical.severity
    };

    FindingGroup {
        canonical,
        duplicates: members,
        consensus_count,
        effective_severity,
    }
}

// ---------------------------------------------------------------------------
// Markdown rendering
// ---------------------------------------------------------------------------

/// Render a `ConsolidatedReport` as a Markdown string.
#[must_use]
pub fn generate_markdown_report(report: &ConsolidatedReport) -> String {
    let mut md = String::new();

    // Header
    let _ = writeln!(md, "# {}\n", report.title);
    let _ = writeln!(md, "Generated: {}\n", report.generated_at);
    md.push_str("## Summary\n\n");
    md.push_str("| Metric | Value |\n");
    md.push_str("|--------|-------|\n");
    let _ = writeln!(md, "| Agents | {} |", report.agent_count);
    let _ = writeln!(md, "| Total findings | {} |", report.total_findings);
    let _ = writeln!(
        md,
        "| After deduplication | {} |",
        report.deduplicated_findings
    );
    md.push('\n');

    // Group findings by severity for rendering.
    let severity_order = [
        FindingSeverity::Critical,
        FindingSeverity::High,
        FindingSeverity::Medium,
        FindingSeverity::Low,
        FindingSeverity::Info,
    ];

    let grouped: HashMap<FindingSeverity, Vec<&FindingGroup>> = {
        let mut map: HashMap<FindingSeverity, Vec<&FindingGroup>> = HashMap::new();
        for g in &report.groups {
            map.entry(g.effective_severity).or_default().push(g);
        }
        map
    };

    for severity in &severity_order {
        let Some(groups) = grouped.get(severity) else {
            continue;
        };

        let _ = writeln!(md, "## {} Findings\n", severity_header(*severity));

        for (i, group) in groups.iter().enumerate() {
            let f = &group.canonical;
            let location = f
                .line
                .map_or_else(|| f.file.clone(), |line| format!("{}:{}", f.file, line));

            let _ = writeln!(
                md,
                "### {}. {} ({})\n",
                i + 1,
                f.title,
                group.effective_severity
            );
            let _ = writeln!(md, "**File:** `{location}`\n");
            let _ = writeln!(
                md,
                "**Consensus:** {}/{} agents\n",
                group.consensus_count,
                // We don't know the total agent count here, but it's in the
                // report; callers can cross-reference.  Just show the raw
                // consensus count.
                group.consensus_count
            );
            let _ = writeln!(md, "{}\n", f.description);

            if let Some(fix) = &f.suggested_fix {
                let _ = writeln!(md, "**Suggested fix:** {fix}\n");
            }

            if !group.duplicates.is_empty() {
                md.push_str("<details>\n<summary>Duplicate reports</summary>\n\n");
                for dup in &group.duplicates {
                    let _ = writeln!(
                        md,
                        "- **{}** (agent: {}, severity: {}): {}",
                        dup.title, dup.agent, dup.severity, dup.description
                    );
                }
                md.push_str("\n</details>\n\n");
            }
        }
    }

    md
}

/// Human-friendly header for a severity level.
const fn severity_header(s: FindingSeverity) -> &'static str {
    match s {
        FindingSeverity::Critical => "Critical",
        FindingSeverity::High => "High",
        FindingSeverity::Medium => "Medium",
        FindingSeverity::Low => "Low",
        FindingSeverity::Info => "Informational",
    }
}

// ---------------------------------------------------------------------------
// Cross-referencing
// ---------------------------------------------------------------------------

/// Filter out finding groups whose canonical title matches an existing issue
/// title (case-insensitive).
#[must_use]
pub fn cross_reference_issues(
    findings: &[FindingGroup],
    existing_issues: &[String],
) -> Vec<FindingGroup> {
    let existing_lower: HashSet<String> =
        existing_issues.iter().map(|t| t.to_lowercase()).collect();

    findings
        .iter()
        .filter(|g| !existing_lower.contains(&g.canonical.title.to_lowercase()))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Helper: build a finding with the given fields.
    fn make_finding(
        title: &str,
        severity: FindingSeverity,
        file: &str,
        description: &str,
        agent: &str,
    ) -> Finding {
        Finding {
            title: title.to_string(),
            severity,
            file: file.to_string(),
            line: Some(42),
            description: description.to_string(),
            suggested_fix: Some("Fix it".to_string()),
            agent: agent.to_string(),
        }
    }

    fn make_report(agent: &str, findings: Vec<Finding>) -> ReviewReport {
        ReviewReport {
            agent: agent.to_string(),
            partition_label: "partition-a".to_string(),
            mandate: "review everything".to_string(),
            findings,
            completed_at: Some("2026-01-01T00:00:00Z".to_string()),
        }
    }

    // -- similarity_score ---------------------------------------------------

    #[test]
    fn similarity_identical_findings_high_score() {
        let a = make_finding(
            "SQL injection risk",
            FindingSeverity::High,
            "src/db.rs",
            "User input passed directly to SQL query",
            "agent-1",
        );
        let b = make_finding(
            "SQL injection risk",
            FindingSeverity::High,
            "src/db.rs",
            "User input passed directly to SQL query",
            "agent-2",
        );
        let score = similarity_score(&a, &b);
        assert!(
            score > 0.9,
            "identical findings should have score > 0.9, got {score}"
        );
    }

    #[test]
    fn similarity_same_file_similar_title_above_threshold() {
        let a = make_finding(
            "SQL injection vulnerability",
            FindingSeverity::High,
            "src/db.rs",
            "Unsanitized input in query builder",
            "agent-1",
        );
        let b = make_finding(
            "Potential SQL injection",
            FindingSeverity::Medium,
            "src/db.rs",
            "Input not sanitized before database query",
            "agent-2",
        );
        let score = similarity_score(&a, &b);
        assert!(
            score >= SIMILARITY_THRESHOLD,
            "same-file + similar-title should be >= threshold, got {score}"
        );
    }

    #[test]
    fn similarity_different_findings_low_score() {
        let a = make_finding(
            "SQL injection risk",
            FindingSeverity::High,
            "src/db.rs",
            "User input passed directly to SQL query",
            "agent-1",
        );
        let b = make_finding(
            "Missing error handling",
            FindingSeverity::Low,
            "src/server.rs",
            "unwrap() called on network response",
            "agent-2",
        );
        let score = similarity_score(&a, &b);
        assert!(
            score < SIMILARITY_THRESHOLD,
            "unrelated findings should be < threshold, got {score}"
        );
    }

    #[test]
    fn similarity_empty_strings() {
        let a = make_finding("", FindingSeverity::Info, "", "", "agent-1");
        let b = make_finding("", FindingSeverity::Info, "", "", "agent-2");
        // Same file ("") and same severity, so at least 0.5.
        let score = similarity_score(&a, &b);
        assert!(score >= 0.5, "empty-string findings: got {score}");
    }

    // -- consolidation ------------------------------------------------------

    #[test]
    fn consolidation_deduplicates_overlapping_findings() {
        let f1 = make_finding(
            "SQL injection risk",
            FindingSeverity::High,
            "src/db.rs",
            "User input passed directly to SQL query without sanitization",
            "agent-1",
        );
        let f2 = make_finding(
            "SQL injection risk",
            FindingSeverity::High,
            "src/db.rs",
            "Direct SQL interpolation of user input",
            "agent-2",
        );
        let f3 = make_finding(
            "Missing error handling",
            FindingSeverity::Low,
            "src/server.rs",
            "unwrap() called on network response",
            "agent-3",
        );

        let reports = vec![
            make_report("agent-1", vec![f1]),
            make_report("agent-2", vec![f2]),
            make_report("agent-3", vec![f3]),
        ];

        let consolidated = consolidate(reports);

        assert_eq!(consolidated.total_findings, 3);
        assert_eq!(
            consolidated.deduplicated_findings, 2,
            "should merge the two SQL findings into one group"
        );
        assert_eq!(consolidated.agent_count, 3);
    }

    #[test]
    fn consolidation_severity_boost_three_agents() {
        // Three agents report the same Medium finding → should boost to High.
        let reports: Vec<ReviewReport> = (1..=3)
            .map(|i| {
                make_finding(
                    "Hardcoded secret",
                    FindingSeverity::Medium,
                    "src/config.rs",
                    "API key is hardcoded in source code which is a security concern",
                    &format!("agent-{i}"),
                )
            })
            .map(|f| {
                let agent = f.agent.clone();
                make_report(&agent, vec![f])
            })
            .collect();

        let consolidated = consolidate(reports);

        assert_eq!(consolidated.deduplicated_findings, 1);
        let group = &consolidated.groups[0];
        assert_eq!(group.consensus_count, 3);
        assert_eq!(
            group.effective_severity,
            FindingSeverity::High,
            "Medium + 3 agents should boost to High"
        );
    }

    #[test]
    fn consolidation_no_boost_two_agents() {
        let f1 = make_finding(
            "Hardcoded secret",
            FindingSeverity::Medium,
            "src/config.rs",
            "API key is hardcoded in source code",
            "agent-1",
        );
        let f2 = make_finding(
            "Hardcoded secret",
            FindingSeverity::Medium,
            "src/config.rs",
            "API key is hardcoded in source code",
            "agent-2",
        );

        let reports = vec![
            make_report("agent-1", vec![f1]),
            make_report("agent-2", vec![f2]),
        ];

        let consolidated = consolidate(reports);
        let group = &consolidated.groups[0];
        assert_eq!(group.consensus_count, 2);
        assert_eq!(
            group.effective_severity,
            FindingSeverity::Medium,
            "only 2 agents should not boost"
        );
    }

    #[test]
    fn consolidation_critical_stays_critical_on_boost() {
        let reports: Vec<ReviewReport> = (1..=3)
            .map(|i| {
                make_finding(
                    "RCE via deserialization",
                    FindingSeverity::Critical,
                    "src/api.rs",
                    "Untrusted data deserialized without validation allowing remote code execution",
                    &format!("agent-{i}"),
                )
            })
            .map(|f| {
                let agent = f.agent.clone();
                make_report(&agent, vec![f])
            })
            .collect();

        let consolidated = consolidate(reports);
        let group = &consolidated.groups[0];
        assert_eq!(group.effective_severity, FindingSeverity::Critical);
    }

    #[test]
    fn consolidation_sort_order() {
        let critical = make_finding(
            "RCE",
            FindingSeverity::Critical,
            "src/api.rs",
            "Remote code execution",
            "agent-1",
        );
        let low = make_finding(
            "Style issue",
            FindingSeverity::Low,
            "src/fmt.rs",
            "Inconsistent formatting",
            "agent-1",
        );
        let high = make_finding(
            "Auth bypass",
            FindingSeverity::High,
            "src/auth.rs",
            "Authentication bypass",
            "agent-1",
        );

        let reports = vec![make_report("agent-1", vec![low, critical, high])];

        let consolidated = consolidate(reports);
        let severities: Vec<FindingSeverity> = consolidated
            .groups
            .iter()
            .map(|g| g.effective_severity)
            .collect();

        // Should be sorted Critical, High, Low.
        assert_eq!(
            severities,
            vec![
                FindingSeverity::Critical,
                FindingSeverity::High,
                FindingSeverity::Low,
            ]
        );
    }

    // -- markdown generation ------------------------------------------------

    #[test]
    fn markdown_report_contains_expected_sections() {
        let f = make_finding(
            "SQL injection",
            FindingSeverity::High,
            "src/db.rs",
            "Unsanitized input",
            "agent-1",
        );

        let report = ConsolidatedReport {
            title: "Test Report".to_string(),
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            agent_count: 1,
            total_findings: 1,
            deduplicated_findings: 1,
            groups: vec![FindingGroup {
                canonical: f,
                duplicates: vec![],
                consensus_count: 1,
                effective_severity: FindingSeverity::High,
            }],
        };

        let md = generate_markdown_report(&report);

        assert!(md.contains("# Test Report"), "should have title");
        assert!(md.contains("## Summary"), "should have summary section");
        assert!(md.contains("| Agents | 1 |"), "should show agent count");
        assert!(
            md.contains("## High Findings"),
            "should have severity section"
        );
        assert!(md.contains("SQL injection"), "should contain finding title");
        assert!(md.contains("`src/db.rs:42`"), "should contain file:line");
        assert!(
            md.contains("Unsanitized input"),
            "should contain description"
        );
        assert!(
            md.contains("**Suggested fix:**"),
            "should show suggested fix"
        );
    }

    #[test]
    fn markdown_report_shows_duplicates() {
        let canonical = make_finding(
            "Bug",
            FindingSeverity::Medium,
            "src/lib.rs",
            "A bug was found here",
            "agent-1",
        );
        let dup = make_finding(
            "Bug",
            FindingSeverity::Medium,
            "src/lib.rs",
            "Same bug",
            "agent-2",
        );

        let report = ConsolidatedReport {
            title: "Test".to_string(),
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            agent_count: 2,
            total_findings: 2,
            deduplicated_findings: 1,
            groups: vec![FindingGroup {
                canonical,
                duplicates: vec![dup],
                consensus_count: 2,
                effective_severity: FindingSeverity::Medium,
            }],
        };

        let md = generate_markdown_report(&report);
        assert!(
            md.contains("Duplicate reports"),
            "should show duplicates section"
        );
        assert!(
            md.contains("agent-2"),
            "should mention the duplicate's agent"
        );
    }

    // -- serde roundtrip ----------------------------------------------------

    #[test]
    fn serde_roundtrip_finding() {
        let f = make_finding(
            "Test",
            FindingSeverity::High,
            "src/main.rs",
            "A test finding",
            "agent-1",
        );
        let json = serde_json::to_string(&f).unwrap();
        let deserialized: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.title, f.title);
        assert_eq!(deserialized.severity, f.severity);
        assert_eq!(deserialized.file, f.file);
        assert_eq!(deserialized.line, f.line);
        assert_eq!(deserialized.description, f.description);
        assert_eq!(deserialized.agent, f.agent);
    }

    #[test]
    fn serde_roundtrip_review_report() {
        let report = make_report(
            "agent-1",
            vec![make_finding(
                "Bug",
                FindingSeverity::Low,
                "f.rs",
                "desc",
                "agent-1",
            )],
        );
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: ReviewReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.agent, report.agent);
        assert_eq!(deserialized.findings.len(), 1);
    }

    #[test]
    fn serde_roundtrip_consolidated_report() {
        let f = make_finding("T", FindingSeverity::Info, "f.rs", "d", "a");
        let report = ConsolidatedReport {
            title: "R".to_string(),
            generated_at: "2026-01-01T00:00:00Z".to_string(),
            agent_count: 1,
            total_findings: 1,
            deduplicated_findings: 1,
            groups: vec![FindingGroup {
                canonical: f,
                duplicates: vec![],
                consensus_count: 1,
                effective_severity: FindingSeverity::Info,
            }],
        };
        let json = serde_json::to_string(&report).unwrap();
        let deserialized: ConsolidatedReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.title, "R");
        assert_eq!(deserialized.groups.len(), 1);
    }

    #[test]
    fn serde_severity_rename_all() {
        let json = serde_json::to_string(&FindingSeverity::Critical).unwrap();
        assert_eq!(json, "\"critical\"");
        let deser: FindingSeverity = serde_json::from_str("\"high\"").unwrap();
        assert_eq!(deser, FindingSeverity::High);
    }

    // -- parse_reports ------------------------------------------------------

    #[test]
    fn parse_reports_reads_matching_files() {
        let dir = tempfile::tempdir().unwrap();

        let report = make_report(
            "agent-1",
            vec![make_finding(
                "Bug",
                FindingSeverity::Low,
                "f.rs",
                "desc",
                "agent-1",
            )],
        );

        // Write a matching file.
        let path = dir.path().join("review-findings-agent-1.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(serde_json::to_string(&report).unwrap().as_bytes())
            .unwrap();

        // Write a non-matching file that should be ignored.
        let ignored = dir.path().join("other-file.json");
        std::fs::File::create(&ignored)
            .unwrap()
            .write_all(b"{}")
            .unwrap();

        let reports = parse_reports(dir.path()).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].agent, "agent-1");
    }

    #[test]
    fn parse_reports_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let reports = parse_reports(dir.path()).unwrap();
        assert!(reports.is_empty());
    }

    // -- cross_reference_issues ---------------------------------------------

    #[test]
    fn cross_reference_filters_matching_issues() {
        let g1 = FindingGroup {
            canonical: make_finding(
                "SQL injection risk",
                FindingSeverity::High,
                "src/db.rs",
                "desc",
                "a",
            ),
            duplicates: vec![],
            consensus_count: 1,
            effective_severity: FindingSeverity::High,
        };
        let g2 = FindingGroup {
            canonical: make_finding(
                "Missing tests",
                FindingSeverity::Low,
                "src/lib.rs",
                "desc",
                "a",
            ),
            duplicates: vec![],
            consensus_count: 1,
            effective_severity: FindingSeverity::Low,
        };

        let existing = vec!["SQL Injection Risk".to_string()]; // case-insensitive match
        let filtered = cross_reference_issues(&[g1, g2], &existing);

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].canonical.title, "Missing tests");
    }

    #[test]
    fn cross_reference_no_existing_issues() {
        let g = FindingGroup {
            canonical: make_finding("Bug", FindingSeverity::Low, "f.rs", "d", "a"),
            duplicates: vec![],
            consensus_count: 1,
            effective_severity: FindingSeverity::Low,
        };

        let filtered = cross_reference_issues(&[g], &[]);
        assert_eq!(filtered.len(), 1);
    }

    // -- FindingSeverity::bumped --------------------------------------------

    #[test]
    fn severity_bump_chain() {
        assert_eq!(FindingSeverity::Info.bumped(), FindingSeverity::Low);
        assert_eq!(FindingSeverity::Low.bumped(), FindingSeverity::Medium);
        assert_eq!(FindingSeverity::Medium.bumped(), FindingSeverity::High);
        assert_eq!(FindingSeverity::High.bumped(), FindingSeverity::Critical);
        assert_eq!(
            FindingSeverity::Critical.bumped(),
            FindingSeverity::Critical
        );
    }

    #[test]
    fn severity_display_all_variants() {
        assert_eq!(format!("{}", FindingSeverity::Critical), "critical");
        assert_eq!(format!("{}", FindingSeverity::High), "high");
        assert_eq!(format!("{}", FindingSeverity::Medium), "medium");
        assert_eq!(format!("{}", FindingSeverity::Low), "low");
        assert_eq!(format!("{}", FindingSeverity::Info), "info");
    }

    #[test]
    fn severity_header_all_variants() {
        assert_eq!(severity_header(FindingSeverity::Critical), "Critical");
        assert_eq!(severity_header(FindingSeverity::High), "High");
        assert_eq!(severity_header(FindingSeverity::Medium), "Medium");
        assert_eq!(severity_header(FindingSeverity::Low), "Low");
        assert_eq!(severity_header(FindingSeverity::Info), "Informational");
    }

    #[test]
    fn generate_markdown_report_no_line_location() {
        let findings = vec![Finding {
            title: "Test finding".to_string(),
            description: "A test".to_string(),
            severity: FindingSeverity::Medium,
            file: "src/lib.rs".to_string(),
            line: None,
            suggested_fix: None,
            agent: "agent-1".to_string(),
        }];
        let reports = vec![ReviewReport {
            agent: "agent-1".to_string(),
            partition_label: "test".to_string(),
            mandate: "test mandate".to_string(),
            findings,
            completed_at: None,
        }];
        let report = consolidate(reports);
        let md = generate_markdown_report(&report);
        assert!(md.contains("src/lib.rs"));
    }
}
