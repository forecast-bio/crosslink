// E-ana tablet — design command: launch foreground Claude session for design doc authoring
use anyhow::{bail, Context, Result};
use std::process::{Command, Stdio};

/// Run `crosslink design` — launch a foreground Claude session with the /design skill prompt.
///
/// If called from inside Claude Code (detected via `CLAUDE_CODE` env var), prints
/// a message directing the user to `/design` and exits with code 1.
pub fn run(
    description: Option<&str>,
    issue: Option<i64>,
    gh_issue: Option<i64>,
    continue_slug: Option<&str>,
) -> Result<()> {
    // 1. Claude Code detection
    if std::env::var("CLAUDE_CODE").is_ok() || std::env::var("CLAUDECODE").is_ok() {
        eprintln!("Already inside Claude Code \u{2014} use /design instead.");
        std::process::exit(1);
    }

    // 2. Verify `claude` CLI is on PATH
    let claude_available = Command::new("which")
        .arg("claude")
        .output()
        .is_ok_and(|o| o.status.success());

    if !claude_available {
        bail!(
            "`claude` CLI not found. Install it:\n\n  \
             npm install -g @anthropic-ai/claude-code\n\n  \
             Or: brew install claude-code"
        );
    }

    // 3. Build the prompt arguments line
    let mut args_parts = Vec::new();

    if let Some(slug) = continue_slug {
        args_parts.push(format!("--continue {slug}"));
    } else if let Some(desc) = description {
        args_parts.push(format!("\"{desc}\""));
    }

    if let Some(id) = issue {
        args_parts.push(format!("--issue {id}"));
    }
    if let Some(id) = gh_issue {
        args_parts.push(format!("--gh-issue {id}"));
    }

    let arguments = args_parts.join(" ");

    // 4. Read the /design skill template
    let skill_prompt = include_str!("../../resources/claude/commands/design.md");

    // Strip the YAML frontmatter (everything between first --- and second ---)
    let prompt_body = strip_frontmatter(skill_prompt);

    // 5. Build the full prompt with arguments substituted
    let full_prompt = if arguments.is_empty() {
        prompt_body.to_string()
    } else {
        format!("ARGUMENTS: {arguments}\n\n{prompt_body}")
    };

    // 6. Launch foreground Claude session
    let status = Command::new("claude")
        .arg("--prompt")
        .arg(&full_prompt)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Failed to launch claude session")?;

    if !status.success() {
        let code = status.code().unwrap_or(1);
        std::process::exit(code);
    }

    Ok(())
}

/// Strip YAML frontmatter (---\n...\n---) from the beginning of a markdown document.
fn strip_frontmatter(content: &str) -> &str {
    if !content.starts_with("---") {
        return content;
    }

    // Find the closing --- (skip the opening one)
    content[3..].find("\n---").map_or(content, |end| {
        let after_frontmatter = &content[3 + end + 4..];
        after_frontmatter.trim_start_matches('\n')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_frontmatter_with_frontmatter() {
        let input = "---\nallowed-tools: Read\ndescription: test\n---\n\n## Context\nBody here";
        let result = strip_frontmatter(input);
        assert!(result.starts_with("## Context"));
    }

    #[test]
    fn test_strip_frontmatter_without_frontmatter() {
        let input = "## Context\nBody here";
        let result = strip_frontmatter(input);
        assert_eq!(result, input);
    }

    #[test]
    fn test_strip_frontmatter_empty() {
        let result = strip_frontmatter("");
        assert_eq!(result, "");
    }
}
