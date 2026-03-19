// E-ana tablet — design document parser for kickoff prompts

/// A group of requirements under a layer/phase header.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RequirementGroup {
    /// The group name (e.g., "Foundation", "Backends + Integration").
    pub(crate) name: String,
    /// Execution hint parsed from parenthetical annotations: "parallel", "sequential", or empty.
    pub(crate) execution_hint: String,
    /// The requirements in this group.
    pub(crate) items: Vec<String>,
}

/// A parsed design document providing structured requirements for kickoff agents.
pub(crate) struct DesignDoc {
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) requirements: Vec<String>,
    /// Structured requirement groups from `### Layer N:` or `### Phase N:` headers.
    /// Empty when no layer headers are detected (flat requirements).
    pub(crate) requirement_groups: Vec<RequirementGroup>,
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
        requirement_groups: Vec::new(),
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
        Section::Requirements => {
            let (flat, groups) = parse_requirements_block(block);
            doc.requirements = flat;
            doc.requirement_groups = groups;
        }
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

/// Parse a requirements block, detecting `### Layer N:` / `### Phase N:` headers.
///
/// Returns (flat_requirements, groups). If no layer headers are found, groups is empty
/// and flat_requirements contains all items. Sub-bullets (indented `- ` or `* `) are
/// collapsed into their parent item rather than becoming separate entries.
fn parse_requirements_block(block: &str) -> (Vec<String>, Vec<RequirementGroup>) {
    let mut groups: Vec<RequirementGroup> = Vec::new();
    let mut current_group: Option<RequirementGroup> = None;
    let mut current_chunk = String::new();
    let mut has_layer_headers = false;

    for line in block.lines() {
        // Detect H3 layer/phase headers: `### Layer 1: Foundation (sequential — ...)`
        if let Some(rest) = line.strip_prefix("### ") {
            let rest = rest.trim();
            // Check if it matches Layer/Phase pattern
            let is_layer = rest.starts_with("Layer ")
                || rest.starts_with("Phase ")
                || rest.starts_with("layer ")
                || rest.starts_with("phase ");
            if is_layer {
                has_layer_headers = true;

                // Flush previous group
                if let Some(mut group) = current_group.take() {
                    group.items = parse_list_items_collapsing_sub_bullets(&current_chunk);
                    groups.push(group);
                }
                current_chunk.clear();

                // Parse the header: strip "Layer N:" or "Phase N:" prefix, extract name and hint
                let (name, hint) = parse_layer_header(rest);
                current_group = Some(RequirementGroup {
                    name,
                    execution_hint: hint,
                    items: Vec::new(),
                });
                continue;
            }
        }
        current_chunk.push_str(line);
        current_chunk.push('\n');
    }

    // Flush final group/chunk
    if let Some(mut group) = current_group.take() {
        group.items = parse_list_items_collapsing_sub_bullets(&current_chunk);
        groups.push(group);
    }

    // Build flat requirements list (always populated for backward compat)
    let flat = if has_layer_headers {
        groups.iter().flat_map(|g| g.items.clone()).collect()
    } else {
        parse_list_items_collapsing_sub_bullets(block)
    };

    let groups = if has_layer_headers {
        groups
    } else {
        Vec::new()
    };
    (flat, groups)
}

/// Parse a layer/phase header, returning (name, execution_hint).
///
/// Input examples:
/// - `"Layer 1: Foundation (sequential — everything else depends on these)"`
/// - `"Phase 2: Backends + Integration (parallel — each agent independent)"`
/// - `"Layer 3: End-to-end delivery"`
fn parse_layer_header(header: &str) -> (String, String) {
    // Strip "Layer N:" or "Phase N:" prefix
    let after_prefix = header
        .find(':')
        .map(|i| header[i + 1..].trim())
        .unwrap_or(header);

    // Extract parenthetical hint
    let (name, hint) = if let Some(paren_start) = after_prefix.find('(') {
        let name = after_prefix[..paren_start].trim().to_string();
        let paren_content = after_prefix[paren_start + 1..].trim_end_matches(')').trim();
        let hint = if paren_content.starts_with("parallel") {
            "parallel".to_string()
        } else if paren_content.starts_with("sequential") {
            "sequential".to_string()
        } else {
            paren_content.to_string()
        };
        (name, hint)
    } else {
        (after_prefix.to_string(), String::new())
    };

    (name, hint)
}

/// Parse list items, collapsing sub-bullets into their parent item.
///
/// A sub-bullet is a line indented 2+ spaces that starts with `- ` or `* `.
/// These are appended to the parent item's text rather than becoming separate entries.
fn parse_list_items_collapsing_sub_bullets(block: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut current_item: Option<String> = None;

    for line in block.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();

        if indent >= 2 {
            // Indented line — check if it's a sub-bullet
            if let Some(text) = strip_list_prefix(trimmed) {
                // Sub-bullet: collapse into parent
                if let Some(ref mut item) = current_item {
                    item.push_str("; ");
                    item.push_str(text);
                } else {
                    // No parent — treat as top-level
                    current_item = Some(text.to_string());
                }
            } else if let Some(ref mut item) = current_item {
                // Continuation line
                item.push(' ');
                item.push_str(trimmed);
            }
        } else if let Some(text) = strip_list_prefix(trimmed) {
            // Top-level bullet — flush previous
            if let Some(prev) = current_item.take() {
                items.push(prev);
            }
            current_item = Some(text.to_string());
        } else if let Some(ref mut item) = current_item {
            // Continuation line
            item.push(' ');
            item.push_str(trimmed);
        }
    }

    if let Some(item) = current_item {
        items.push(item);
    }

    items
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
            requirement_groups: Vec::new(),
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
            requirement_groups: Vec::new(),
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
            requirement_groups: Vec::new(),
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
            requirement_groups: Vec::new(),
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
            requirement_groups: Vec::new(),
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

    // ==================== Layer Header Parsing Tests ====================

    #[test]
    fn test_parse_layer_headers_creates_groups() {
        let input = "\
# Feature: Secrets

## Requirements

### Layer 1: Foundation (sequential — everything depends on these)
- REQ-1: SecretBackend trait
- REQ-2: Extension traits

### Layer 2: Backends (parallel — each agent independent)
- REQ-3: EnvBackend
- REQ-4: FileBackend

### Layer 3: Delivery (sequential — depends on Layer 2)
- REQ-5: Container delivery
- REQ-6: E2E test
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.requirement_groups.len(), 3);

        assert_eq!(doc.requirement_groups[0].name, "Foundation");
        assert_eq!(doc.requirement_groups[0].execution_hint, "sequential");
        assert_eq!(doc.requirement_groups[0].items.len(), 2);

        assert_eq!(doc.requirement_groups[1].name, "Backends");
        assert_eq!(doc.requirement_groups[1].execution_hint, "parallel");
        assert_eq!(doc.requirement_groups[1].items.len(), 2);

        assert_eq!(doc.requirement_groups[2].name, "Delivery");
        assert_eq!(doc.requirement_groups[2].execution_hint, "sequential");
        assert_eq!(doc.requirement_groups[2].items.len(), 2);

        // Flat requirements should contain all 6
        assert_eq!(doc.requirements.len(), 6);
    }

    #[test]
    fn test_parse_no_layer_headers_no_groups() {
        let input = "\
## Requirements
- REQ-1: First
- REQ-2: Second
";
        let doc = parse_design_doc(input);
        assert!(doc.requirement_groups.is_empty());
        assert_eq!(doc.requirements.len(), 2);
    }

    #[test]
    fn test_parse_layer_header_no_hint() {
        let input = "\
## Requirements

### Layer 1: Foundation
- REQ-1: Thing
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.requirement_groups.len(), 1);
        assert_eq!(doc.requirement_groups[0].name, "Foundation");
        assert_eq!(doc.requirement_groups[0].execution_hint, "");
    }

    #[test]
    fn test_parse_phase_header_variant() {
        let input = "\
## Requirements

### Phase 1: Setup
- REQ-1: Init
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.requirement_groups.len(), 1);
        assert_eq!(doc.requirement_groups[0].name, "Setup");
    }

    #[test]
    fn test_sub_bullets_collapsed_into_parent() {
        let input = "\
## Requirements
- REQ-1: Error enum
  - SecretNotProvided
  - SecretNotFound
  - BackendError
- REQ-2: Config section
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.requirements.len(), 2);
        assert!(doc.requirements[0].contains("SecretNotProvided"));
        assert!(doc.requirements[0].contains("SecretNotFound"));
        assert!(doc.requirements[0].contains("BackendError"));
        assert_eq!(doc.requirements[1], "REQ-2: Config section");
    }

    #[test]
    fn test_sub_bullets_in_layer_groups() {
        let input = "\
## Requirements

### Layer 1: Foundation (sequential)
- REQ-1: Error enum
  - SecretNotProvided
  - SecretNotFound
- REQ-2: Config
";
        let doc = parse_design_doc(input);
        assert_eq!(doc.requirement_groups[0].items.len(), 2);
        assert!(doc.requirement_groups[0].items[0].contains("SecretNotProvided"));
    }

    #[test]
    fn test_parse_layer_header_details() {
        let (name, hint) =
            parse_layer_header("Layer 1: Foundation (sequential — everything depends on these)");
        assert_eq!(name, "Foundation");
        assert_eq!(hint, "sequential");
    }

    #[test]
    fn test_parse_layer_header_parallel() {
        let (name, hint) =
            parse_layer_header("Phase 2: Backends + Integration (parallel — each independent)");
        assert_eq!(name, "Backends + Integration");
        assert_eq!(hint, "parallel");
    }

    #[test]
    fn test_parse_layer_header_no_parens() {
        let (name, hint) = parse_layer_header("Layer 3: End-to-end delivery");
        assert_eq!(name, "End-to-end delivery");
        assert_eq!(hint, "");
    }
}
