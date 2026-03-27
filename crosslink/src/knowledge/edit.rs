use anyhow::Result;

/// Extract the body content after frontmatter.
pub fn extract_body(content: &str) -> &str {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return content;
    }
    let after_first = &trimmed[3..];
    let after_first = after_first.trim_start_matches(['\r', '\n']);
    if let Some(end_idx) = after_first.find("\n---") {
        let after_closing = &after_first[end_idx + 4..];
        // Skip the line ending after the closing --- (handles both \r\n and \n)
        after_closing
            .strip_prefix("\r\n")
            .or_else(|| after_closing.strip_prefix('\n'))
            .unwrap_or(after_closing)
    } else {
        content
    }
}

/// Parse a heading line and return its level (1-6) and text.
/// Returns None if the line is not a markdown heading.
pub fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_end();
    if !trimmed.starts_with('#') {
        return None;
    }
    let hashes = trimmed.bytes().take_while(|&b| b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    // Must be followed by a space (standard markdown heading)
    let rest = &trimmed[hashes..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some((hashes, rest[1..].trim()))
}

/// Find the line range of a section identified by its heading text.
/// Returns (heading_line_idx, section_end_line_idx) where end is exclusive.
/// The section extends from the heading line to the next heading of equal or higher level, or EOF.
pub fn find_section_range(lines: &[&str], heading: &str) -> Result<(usize, usize)> {
    // Normalize the heading query: strip leading '#' chars if the user included them
    let query = heading.trim();
    let (query_level, query_text) = if query.starts_with('#') {
        match parse_heading(query) {
            Some((level, text)) => (Some(level), text),
            None => (None, query),
        }
    } else {
        (None, query)
    };

    // Find the heading line
    let mut heading_idx = None;
    let mut heading_level = 0;
    for (i, line) in lines.iter().enumerate() {
        if let Some((level, text)) = parse_heading(line) {
            let text_matches = text == query_text;
            let level_matches = query_level.is_none() || query_level == Some(level);
            if text_matches && level_matches {
                heading_idx = Some(i);
                heading_level = level;
                break;
            }
        }
    }

    let start = heading_idx
        .ok_or_else(|| anyhow::anyhow!("Section heading '{}' not found in the page", heading))?;

    // Find the end: next heading of equal or higher (lower number) level
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if let Some((level, _)) = parse_heading(line) {
            if level <= heading_level {
                end = i;
                break;
            }
        }
    }

    Ok((start, end))
}

/// Replace the content of a section (everything between the heading and the next same-or-higher-level heading).
/// The heading line itself is preserved.
pub fn replace_section_content(body: &str, heading: &str, new_content: &str) -> Result<String> {
    let lines: Vec<&str> = body.lines().collect();
    let (start, end) = find_section_range(&lines, heading)?;

    let mut result = String::new();
    // Lines before and including the heading
    for line in &lines[..=start] {
        result.push_str(line);
        result.push('\n');
    }
    // New content
    if !new_content.is_empty() {
        result.push('\n');
        result.push_str(new_content);
        if !new_content.ends_with('\n') {
            result.push('\n');
        }
    }
    // Lines after the section
    if end < lines.len() {
        result.push('\n');
        for line in &lines[end..] {
            result.push_str(line);
            result.push('\n');
        }
    }

    Ok(result)
}

/// Append content to the end of a section (before the next same-or-higher-level heading).
pub fn append_to_section_content(body: &str, heading: &str, new_content: &str) -> Result<String> {
    let lines: Vec<&str> = body.lines().collect();
    let (_, end) = find_section_range(&lines, heading)?;

    let mut result = String::new();
    // Lines up to (but not including) the section end
    for line in &lines[..end] {
        result.push_str(line);
        result.push('\n');
    }
    // Append new content
    if !new_content.is_empty() {
        result.push('\n');
        result.push_str(new_content);
        if !new_content.ends_with('\n') {
            result.push('\n');
        }
    }
    // Lines after the section
    if end < lines.len() {
        result.push('\n');
        for line in &lines[end..] {
            result.push_str(line);
            result.push('\n');
        }
    }

    Ok(result)
}
