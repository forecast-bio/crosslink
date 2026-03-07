// E-ana tablet — design document parser for kickoff prompts

/// A parsed design document providing structured requirements for kickoff agents.
pub(crate) struct DesignDoc {
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) requirements: Vec<String>,
    pub(crate) acceptance_criteria: Vec<String>,
    pub(crate) architecture: String,
    pub(crate) open_questions: Vec<String>,
    pub(crate) out_of_scope: Vec<String>,
    pub(crate) unknown_sections: Vec<(String, String)>,
}

/// Which section the parser is currently accumulating into.
enum Section {
    Title,
    Summary,
    Requirements,
    AcceptanceCriteria,
    Architecture,
    OpenQuestions,
    OutOfScope,
    Unknown(String),
}

/// Parse a markdown design document into a `DesignDoc`.
///
/// Never fails — missing sections produce empty fields. Follows the
/// line-scanning state machine pattern from `knowledge.rs::parse_frontmatter()`.
pub(crate) fn parse_design_doc(content: &str) -> DesignDoc {
    let mut doc = DesignDoc {
        title: String::new(),
        summary: String::new(),
        requirements: Vec::new(),
        acceptance_criteria: Vec::new(),
        architecture: String::new(),
        open_questions: Vec::new(),
        out_of_scope: Vec::new(),
        unknown_sections: Vec::new(),
    };

    let mut section = Section::Title;
    let mut current_block = String::new();
    let mut in_code_fence = false;

    for line in content.lines() {
        // Track code fences so we don't treat comments as headings
        if line.starts_with("```") {
            in_code_fence = !in_code_fence;
            current_block.push_str(line);
            current_block.push('\n');
            continue;
        }

        if in_code_fence {
            current_block.push_str(line);
            current_block.push('\n');
            continue;
        }

        // H1: extract title
        if let Some(rest) = line.strip_prefix("# ") {
            let rest = rest.trim();
            if !rest.starts_with('#') {
                doc.title = rest
                    .strip_prefix("Feature:")
                    .or_else(|| rest.strip_prefix("feature:"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| rest.to_string());
                section = Section::Title;
                current_block.clear();
                continue;
            }
        }

        // H2: switch section
        if let Some(rest) = line.strip_prefix("## ") {
            // Flush previous section
            flush_block(&mut doc, &section, &current_block);
            current_block.clear();

            section = match rest.trim().to_lowercase().as_str() {
                "summary" => Section::Summary,
                "requirements" => Section::Requirements,
                "acceptance criteria" => Section::AcceptanceCriteria,
                "architecture" => Section::Architecture,
                "open questions" => Section::OpenQuestions,
                "out of scope" => Section::OutOfScope,
                other => Section::Unknown(other.to_string()),
            };
            continue;
        }

        // Accumulate content for the current section
        current_block.push_str(line);
        current_block.push('\n');
    }

    // Flush final section
    flush_block(&mut doc, &section, &current_block);

    doc
}

/// Flush accumulated block text into the appropriate section of the doc.
fn flush_block(doc: &mut DesignDoc, section: &Section, block: &str) {
    match section {
        Section::Title => {} // title already extracted from H1
        Section::Summary => doc.summary = block.trim().to_string(),
        Section::Requirements => doc.requirements = parse_list_items(block),
        Section::AcceptanceCriteria => doc.acceptance_criteria = parse_list_items(block),
        Section::Architecture => doc.architecture = block.trim().to_string(),
        Section::OpenQuestions => doc.open_questions = parse_list_items(block),
        Section::OutOfScope => doc.out_of_scope = parse_list_items(block),
        Section::Unknown(name) => {
            let trimmed = block.trim();
            if !trimmed.is_empty() {
                doc.unknown_sections
                    .push((name.clone(), trimmed.to_string()));
            }
        }
    }
}

/// Parse list items from a block of text. Supports `- `, `* `, `- [ ] `, `- [x] ` prefixes.
fn parse_list_items(block: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current_item: Option<String> = None;

    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Try to strip a list prefix
        let content = strip_list_prefix(trimmed);
        if let Some(text) = content {
            // New list item — flush previous
            if let Some(prev) = current_item.take() {
                items.push(prev);
            }
            current_item = Some(text.to_string());
        } else if let Some(ref mut item) = current_item {
            // Continuation line — append to current item
            item.push(' ');
            item.push_str(trimmed);
        }
        // Non-list text before the first item is ignored
    }

    if let Some(item) = current_item {
        items.push(item);
    }

    items
}

/// Strip a markdown list prefix, returning the content after it.
fn strip_list_prefix(line: &str) -> Option<&str> {
    // Checkbox variants: `- [ ] `, `- [x] `, `* [ ] `, `* [x] `
    for prefix in &["- [x] ", "- [X] ", "- [ ] ", "* [x] ", "* [X] ", "* [ ] "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    // Plain bullet: `- ` or `* `
    if let Some(rest) = line.strip_prefix("- ") {
        return Some(rest);
    }
    if let Some(rest) = line.strip_prefix("* ") {
        return Some(rest);
    }
    None
}

/// Validate a design doc and return warnings for missing sections.
pub(crate) fn validate_design_doc(doc: &DesignDoc) -> Vec<String> {
    let mut warnings = Vec::new();
    if doc.summary.is_empty() {
        warnings.push("Design doc has no ## Summary section".to_string());
    }
    if doc.requirements.is_empty() {
        warnings.push("Design doc has no ## Requirements section".to_string());
    }
    if doc.acceptance_criteria.is_empty() {
        warnings.push("Design doc has no ## Acceptance Criteria section".to_string());
    }
    warnings
}

/// Render a parsed design doc as a `## Design Specification` markdown block for KICKOFF.md.
pub(crate) fn build_design_doc_section(doc: &DesignDoc) -> String {
    let mut out = String::from("\n## Design Specification\n\n");

    if !doc.summary.is_empty() {
        out.push_str("### Summary\n\n");
        out.push_str(&doc.summary);
        out.push_str("\n\n");
    }

    if !doc.requirements.is_empty() {
        out.push_str("### Requirements\n\n");
        for req in &doc.requirements {
            out.push_str(&format!("- {}\n", req));
        }
        out.push('\n');
    }

    if !doc.acceptance_criteria.is_empty() {
        out.push_str("### Acceptance Criteria\n\n");
        for ac in &doc.acceptance_criteria {
            out.push_str(&format!("- [ ] {}\n", ac));
        }
        out.push('\n');
    }

    if !doc.architecture.is_empty() {
        out.push_str("### Architecture\n\n");
        out.push_str(&doc.architecture);
        out.push_str("\n\n");
    }

    if !doc.out_of_scope.is_empty() {
        out.push_str("### Out of Scope\n\n");
        for item in &doc.out_of_scope {
            out.push_str(&format!("- {}\n", item));
        }
        out.push('\n');
    }

    for (name, body) in &doc.unknown_sections {
        out.push_str(&format!("### {}\n\n", name));
        out.push_str(body);
        out.push_str("\n\n");
    }

    out
}

/// Build an escalation instructions block when there are open questions.
///
/// Returns `None` if there are no open questions.
pub(crate) fn build_open_questions_escalation(doc: &DesignDoc) -> Option<String> {
    if doc.open_questions.is_empty() {
        return None;
    }

    let mut out = String::from(
        "\n## Open Questions — Escalation Required\n\n\
         The design document contains unresolved questions. **Before implementing anything \
         affected by these questions**, escalate to the user:\n\n",
    );

    for (i, q) in doc.open_questions.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, q));
    }

    out.push_str(
        "\n**Action**: For each question that affects your implementation, add a comment:\n\
         `crosslink comment <issue_id> \"Blocker: <question>\" --kind blocker`\n\
         Then proceed with the parts of the feature that are NOT blocked by these questions.\n",
    );

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== parse_design_doc Tests ====================

    #[test]
    fn test_parse_empty_input() {
        let doc = parse_design_doc("");
        assert!(doc.title.is_empty());
        assert!(doc.summary.is_empty());
        assert!(doc.requirements.is_empty());
        assert!(doc.acceptance_criteria.is_empty());
        assert!(doc.architecture.is_empty());
        assert!(doc.open_questions.is_empty());
        assert!(doc.out_of_scope.is_empty());
        assert!(doc.unknown_sections.is_empty());
    }

    #[test]
    fn test_parse_title_plain() {
        let doc = parse_design_doc("# My Great Feature\n");
        assert_eq!(doc.title, "My Great Feature");
    }

    #[test]
    fn test_parse_title_with_feature_prefix() {
        let doc = parse_design_doc("# Feature: User Authentication\n");
        assert_eq!(doc.title, "User Authentication");
    }

    #[test]
    fn test_parse_title_feature_prefix_lowercase() {
        let doc = parse_design_doc("# feature: lower case prefix\n");
        assert_eq!(doc.title, "lower case prefix");
    }

    #[test]
    fn test_parse_summary() {
        let input = "# Title\n\n## Summary\n\nThis is a summary\nwith multiple lines.\n";
        let doc = parse_design_doc(input);
        assert_eq!(doc.summary, "This is a summary\nwith multiple lines.");
    }

    #[test]
    fn test_parse_requirements_dash() {
        let input = "## Requirements\n- REQ-1: First\n- REQ-2: Second\n";
        let doc = parse_design_doc(input);
        assert_eq!(doc.requirements, vec!["REQ-1: First", "REQ-2: Second"]);
    }

    #[test]
    fn test_parse_requirements_asterisk() {
        let input = "## Requirements\n* First requirement\n* Second requirement\n";
        let doc = parse_design_doc(input);
        assert_eq!(
            doc.requirements,
            vec!["First requirement", "Second requirement"]
        );
    }

    #[test]
    fn test_parse_acceptance_criteria_checkboxes() {
        let input = "## Acceptance Criteria\n- [ ] AC-1: Not done\n- [x] AC-2: Already done\n";
        let doc = parse_design_doc(input);
        assert_eq!(
            doc.acceptance_criteria,
            vec!["AC-1: Not done", "AC-2: Already done"]
        );
    }

    #[test]
    fn test_parse_architecture() {
        let input = "## Architecture\n\nUse a layered approach.\n\nDatabase -> Service -> API\n";
        let doc = parse_design_doc(input);
        assert_eq!(
            doc.architecture,
            "Use a layered approach.\n\nDatabase -> Service -> API"
        );
    }

    #[test]
    fn test_parse_open_questions() {
        let input = "## Open Questions\n- Q1: Should we use Redis?\n- Q2: What about auth?\n";
        let doc = parse_design_doc(input);
        assert_eq!(
            doc.open_questions,
            vec!["Q1: Should we use Redis?", "Q2: What about auth?"]
        );
    }

    #[test]
    fn test_parse_out_of_scope() {
        let input = "## Out of Scope\n- Not doing X\n- Not doing Y\n";
        let doc = parse_design_doc(input);
        assert_eq!(doc.out_of_scope, vec!["Not doing X", "Not doing Y"]);
    }

    #[test]
    fn test_parse_unknown_sections() {
        let input = "## References\n\nSee RFC 1234.\n";
        let doc = parse_design_doc(input);
        assert_eq!(doc.unknown_sections.len(), 1);
        assert_eq!(doc.unknown_sections[0].0, "references");
        assert_eq!(doc.unknown_sections[0].1, "See RFC 1234.");
    }

    #[test]
    fn test_parse_case_insensitive_headings() {
        let input = "## SUMMARY\n\nUpper case heading.\n\n## REQUIREMENTS\n- Item one\n";
        let doc = parse_design_doc(input);
        assert_eq!(doc.summary, "Upper case heading.");
        assert_eq!(doc.requirements, vec!["Item one"]);
    }

    #[test]
    fn test_parse_mixed_bullet_styles() {
        let input = "## Requirements\n- Dash item\n* Star item\n- [ ] Checkbox item\n";
        let doc = parse_design_doc(input);
        assert_eq!(
            doc.requirements,
            vec!["Dash item", "Star item", "Checkbox item"]
        );
    }

    #[test]
    fn test_parse_full_document() {
        let input = "\
# Feature: Batch Retry Logic

## Summary

Add retry logic for batch operations.

## Requirements
- REQ-1: Retry up to 3 times
- REQ-2: Exponential backoff

## Acceptance Criteria
- [ ] AC-1: Retries work
- [x] AC-2: Logs show attempts

## Architecture

Use a middleware pattern.

## Open Questions
- Q1: Max retry count?

## Out of Scope
- Not handling network partitions
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.title, "Batch Retry Logic");
        assert_eq!(doc.summary, "Add retry logic for batch operations.");
        assert_eq!(doc.requirements.len(), 2);
        assert_eq!(doc.acceptance_criteria.len(), 2);
        assert_eq!(doc.architecture, "Use a middleware pattern.");
        assert_eq!(doc.open_questions.len(), 1);
        assert_eq!(doc.out_of_scope.len(), 1);
    }

    #[test]
    fn test_parse_multiline_list_item() {
        let input = "## Requirements\n- First requirement\n  which continues here\n- Second\n";
        let doc = parse_design_doc(input);
        assert_eq!(doc.requirements.len(), 2);
        assert_eq!(
            doc.requirements[0],
            "First requirement which continues here"
        );
        assert_eq!(doc.requirements[1], "Second");
    }

    #[test]
    fn test_parse_uppercase_x_checkbox() {
        let input = "## Acceptance Criteria\n- [X] Done with uppercase X\n";
        let doc = parse_design_doc(input);
        assert_eq!(doc.acceptance_criteria, vec!["Done with uppercase X"]);
    }

    // ==================== validate_design_doc Tests ====================

    #[test]
    fn test_validate_complete_doc() {
        let doc = DesignDoc {
            title: "Test".to_string(),
            summary: "A summary".to_string(),
            requirements: vec!["REQ-1".to_string()],
            acceptance_criteria: vec!["AC-1".to_string()],
            architecture: String::new(),
            open_questions: Vec::new(),
            out_of_scope: Vec::new(),
            unknown_sections: Vec::new(),
        };
        assert!(validate_design_doc(&doc).is_empty());
    }

    #[test]
    fn test_validate_missing_all() {
        let doc = parse_design_doc("# Just a title\n");
        let warnings = validate_design_doc(&doc);
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0].contains("Summary"));
        assert!(warnings[1].contains("Requirements"));
        assert!(warnings[2].contains("Acceptance Criteria"));
    }

    #[test]
    fn test_validate_partial_missing() {
        let doc = DesignDoc {
            title: "Test".to_string(),
            summary: "Present".to_string(),
            requirements: Vec::new(),
            acceptance_criteria: vec!["AC-1".to_string()],
            architecture: String::new(),
            open_questions: Vec::new(),
            out_of_scope: Vec::new(),
            unknown_sections: Vec::new(),
        };
        let warnings = validate_design_doc(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Requirements"));
    }

    // ==================== build_design_doc_section Tests ====================

    #[test]
    fn test_build_section_full() {
        let doc = DesignDoc {
            title: "Test".to_string(),
            summary: "A summary".to_string(),
            requirements: vec!["REQ-1: Do thing".to_string()],
            acceptance_criteria: vec!["AC-1: Verify thing".to_string()],
            architecture: "Layered".to_string(),
            open_questions: Vec::new(),
            out_of_scope: vec!["Not X".to_string()],
            unknown_sections: Vec::new(),
        };
        let section = build_design_doc_section(&doc);
        assert!(section.contains("## Design Specification"));
        assert!(section.contains("### Summary"));
        assert!(section.contains("A summary"));
        assert!(section.contains("### Requirements"));
        assert!(section.contains("- REQ-1: Do thing"));
        assert!(section.contains("### Acceptance Criteria"));
        assert!(section.contains("- [ ] AC-1: Verify thing"));
        assert!(section.contains("### Architecture"));
        assert!(section.contains("Layered"));
        assert!(section.contains("### Out of Scope"));
        assert!(section.contains("- Not X"));
    }

    #[test]
    fn test_build_section_empty_doc() {
        let doc = parse_design_doc("");
        let section = build_design_doc_section(&doc);
        assert!(section.contains("## Design Specification"));
        assert!(!section.contains("### Summary"));
        assert!(!section.contains("### Requirements"));
    }

    #[test]
    fn test_build_section_includes_unknown_sections() {
        let doc = DesignDoc {
            title: String::new(),
            summary: String::new(),
            requirements: Vec::new(),
            acceptance_criteria: Vec::new(),
            architecture: String::new(),
            open_questions: Vec::new(),
            out_of_scope: Vec::new(),
            unknown_sections: vec![("notes".to_string(), "Some notes here.".to_string())],
        };
        let section = build_design_doc_section(&doc);
        assert!(section.contains("### notes"));
        assert!(section.contains("Some notes here."));
    }

    // ==================== build_open_questions_escalation Tests ====================

    #[test]
    fn test_escalation_none_when_no_questions() {
        let doc = parse_design_doc("");
        assert!(build_open_questions_escalation(&doc).is_none());
    }

    #[test]
    fn test_escalation_present_with_questions() {
        let doc = DesignDoc {
            title: String::new(),
            summary: String::new(),
            requirements: Vec::new(),
            acceptance_criteria: Vec::new(),
            architecture: String::new(),
            open_questions: vec![
                "Q1: Should we use Redis?".to_string(),
                "Q2: Auth strategy?".to_string(),
            ],
            out_of_scope: Vec::new(),
            unknown_sections: Vec::new(),
        };
        let escalation = build_open_questions_escalation(&doc).unwrap();
        assert!(escalation.contains("Open Questions"));
        assert!(escalation.contains("Escalation Required"));
        assert!(escalation.contains("1. Q1: Should we use Redis?"));
        assert!(escalation.contains("2. Q2: Auth strategy?"));
        assert!(escalation.contains("crosslink comment"));
        assert!(escalation.contains("blocker"));
    }

    // ==================== strip_list_prefix Tests ====================

    #[test]
    fn test_strip_list_prefix_dash() {
        assert_eq!(strip_list_prefix("- hello"), Some("hello"));
    }

    #[test]
    fn test_strip_list_prefix_asterisk() {
        assert_eq!(strip_list_prefix("* hello"), Some("hello"));
    }

    #[test]
    fn test_strip_list_prefix_checkbox_unchecked() {
        assert_eq!(strip_list_prefix("- [ ] todo"), Some("todo"));
    }

    #[test]
    fn test_strip_list_prefix_checkbox_checked() {
        assert_eq!(strip_list_prefix("- [x] done"), Some("done"));
    }

    #[test]
    fn test_strip_list_prefix_no_prefix() {
        assert_eq!(strip_list_prefix("plain text"), None);
    }

    // ==================== Code fence handling Tests ====================

    #[test]
    fn test_parse_h1_inside_code_fence_ignored() {
        let input = "\
# Real Title

## Summary

Some summary.

```bash
# This is a shell comment, not a heading
echo hello
```
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.title, "Real Title");
        assert_eq!(
            doc.summary,
            "Some summary.\n\n```bash\n# This is a shell comment, not a heading\necho hello\n```"
        );
    }

    #[test]
    fn test_parse_h2_inside_code_fence_ignored() {
        let input = "\
# Real Title

## Requirements
- REQ-1: First

## Summary

Some summary.

```markdown
## This is not a section switch
```

Still in summary.
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.title, "Real Title");
        assert_eq!(doc.requirements, vec!["REQ-1: First"]);
        // The ## inside the code fence should NOT switch to a new section
        assert!(doc.summary.contains("## This is not a section switch"));
        assert!(doc.summary.contains("Still in summary."));
    }

    #[test]
    fn test_parse_multiple_code_fences() {
        let input = "\
# Real Title

## Summary

Before code.

```
# Not a title
## Not a section
```

After first fence.

```python
# Python comment
```

Still in summary.
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.title, "Real Title");
        assert!(doc.summary.contains("Before code."));
        assert!(doc.summary.contains("# Not a title"));
        assert!(doc.summary.contains("After first fence."));
        assert!(doc.summary.contains("# Python comment"));
        assert!(doc.summary.contains("Still in summary."));
    }

    #[test]
    fn test_parse_code_fence_does_not_affect_content_after() {
        let input = "\
# Real Title

## Summary

Summary text.

```
# shell comment
```

## Requirements
- REQ-1: After the fence
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.title, "Real Title");
        assert_eq!(doc.requirements, vec!["REQ-1: After the fence"]);
    }
}
