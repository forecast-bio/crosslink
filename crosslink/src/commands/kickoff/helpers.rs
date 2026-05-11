// E-ana tablet — kickoff helpers: pure utility functions
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use super::types::*;

/// Maximum slug length: 64 (`agent_id` limit) - 4 (repo) - 1 (-) - 4 (agent) - 1 (-) = 54.
pub(crate) const MAX_SLUG_LEN: usize = 54;

/// Slugify a feature description into a branch-safe name.
pub(crate) fn slugify(description: &str) -> String {
    slugify_with_max(description, MAX_SLUG_LEN)
}

/// Slugify with a custom max length.
fn slugify_with_max(description: &str, max_len: usize) -> String {
    let slug: String = description
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse multiple hyphens and trim
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in slug.chars() {
        if c == '-' {
            if !prev_hyphen && !result.is_empty() {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }

    // Trim trailing hyphens and truncate
    let trimmed = result.trim_end_matches('-');
    if trimmed.len() > max_len {
        // Cut at the last hyphen before max_len chars to avoid mid-word
        trimmed[..max_len].rfind('-').map_or_else(
            || trimmed[..max_len].to_string(),
            |pos| trimmed[..pos].to_string(),
        )
    } else {
        trimmed.to_string()
    }
}

/// Parse an optional `AC-N:` prefix from a criterion string.
///
/// Returns `(id, remaining_text)`. If no prefix found, id is empty.
pub(super) fn parse_criterion_id(text: &str) -> (String, String) {
    let trimmed = text.trim();
    let upper = trimmed.to_uppercase();
    if let Some(rest) = upper.strip_prefix("AC-") {
        if let Some(colon_pos) = rest.find(':') {
            let digits = &rest[..colon_pos];
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                let id = format!("AC-{digits}");
                let remaining = trimmed[3 + colon_pos + 1..].trim().to_string();
                return (id, remaining);
            }
        }
    }
    (String::new(), trimmed.to_string())
}

/// Extract acceptance criteria from a parsed design doc into a structured format.
///
/// Criteria with `AC-N:` prefixes keep their explicit IDs; others get
/// sequential IDs assigned, skipping any numbers already claimed by explicit IDs.
pub(crate) fn extract_criteria(
    doc: &super::super::design_doc::DesignDoc,
    source_filename: &str,
) -> CriteriaFile {
    let explicit_ids: HashSet<String> = doc
        .acceptance_criteria
        .iter()
        .filter_map(|raw| {
            let (id, _) = parse_criterion_id(raw);
            if id.is_empty() {
                None
            } else {
                Some(id)
            }
        })
        .collect();

    let mut auto_counter = 0u32;
    let mut criteria = Vec::new();

    for raw in &doc.acceptance_criteria {
        let (parsed_id, text) = parse_criterion_id(raw);
        let id = if parsed_id.is_empty() {
            loop {
                auto_counter += 1;
                let candidate = format!("AC-{auto_counter}");
                if !explicit_ids.contains(&candidate) {
                    break candidate;
                }
            }
        } else {
            parsed_id
        };
        criteria.push(Criterion {
            id,
            text,
            criterion_type: "functional".to_string(),
        });
    }

    CriteriaFile {
        source_doc: source_filename.to_string(),
        extracted_at: chrono::Utc::now().to_rfc3339(),
        criteria,
    }
}

/// Subdirectories skipped by [`has_manifest`] when scanning one level deep.
///
/// These contain vendored / build / cache artifacts whose manifests should
/// never light up toolchain support for the parent project (e.g. a stray
/// `Cargo.toml` deep inside `node_modules/` is not signal). The list also
/// includes infra dotdirs so we don't pull in `.crosslink/`'s own config.
const SKIP_SCAN_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    "env",
    "__pycache__",
    ".git",
    ".worktrees",
    ".crosslink",
    ".claude",
    ".venv",
    ".env",
    ".cache",
    ".idea",
    ".vscode",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".cargo",
    ".rustup",
];

/// Return `true` when `repo_root` contains `filename` at the root or exactly
/// one directory level deep (skipping hidden and vendored/build dirs).
///
/// Catches the common monorepo layout where the canonical build manifest
/// lives in a named subdirectory -- e.g. `crosslink/Cargo.toml` here, or
/// `<repo>/santana-core/Cargo.toml` in santana. Without this, kickoff
/// agents in such repos see no Rust/Python/etc. tools in `--allowedTools`
/// and end up sandbox-denied for `cargo`, `uv`, etc. See GH#584.
fn has_manifest(repo_root: &Path, filename: &str) -> bool {
    if repo_root.join(filename).is_file() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(repo_root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Skip hidden dirs except the few explicitly listed in SKIP_SCAN_DIRS
        // (those are listed so future readers see why they're excluded).
        if name.starts_with('.') {
            continue;
        }
        if SKIP_SCAN_DIRS.contains(&name) {
            continue;
        }
        if path.join(filename).is_file() {
            return true;
        }
    }
    false
}

/// Read additional `--allowedTools` patterns from
/// `hook-config.json`'s `kickoff.allowed_tools` array.
///
/// Returns an empty vector when the file is missing, unparseable, or has no
/// such key. Project owners use this to extend the kickoff agent's tool
/// surface beyond what convention detection picks up automatically -- e.g.
/// when the project's manifests live two or more levels deep, or when the
/// agent needs a tool the auto-detect doesn't know about. See GH#584.
pub(crate) fn read_kickoff_allowed_tools(crosslink_dir: &Path) -> Vec<String> {
    let config_path = crosslink_dir.join("hook-config.json");
    let Ok(content) = std::fs::read_to_string(&config_path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) else {
        return Vec::new();
    };
    parsed
        .get("kickoff")
        .and_then(|k| k.get("allowed_tools"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Detect project conventions from the repo root.
pub(crate) fn detect_conventions(repo_root: &Path) -> ProjectConventions {
    let mut conv = ProjectConventions {
        test_command: None,
        lint_commands: Vec::new(),
        allowed_tools: Vec::new(),
    };

    // Rust
    if has_manifest(repo_root, "Cargo.toml") {
        conv.test_command = Some("cargo test".to_string());
        conv.lint_commands
            .push("cargo clippy -- -D warnings".to_string());
        conv.lint_commands.push("cargo fmt --check".to_string());
        conv.allowed_tools.push("Bash(cargo *)".to_string());
    }

    // Node/TypeScript
    if has_manifest(repo_root, "package.json") {
        if conv.test_command.is_none() {
            conv.test_command = Some("npm test".to_string());
        }
        conv.allowed_tools.push("Bash(npm *)".to_string());
        conv.allowed_tools.push("Bash(npx *)".to_string());
    }

    // Python
    if has_manifest(repo_root, "pyproject.toml") || has_manifest(repo_root, "requirements.txt") {
        if conv.test_command.is_none() {
            conv.test_command = Some("uv run pytest".to_string());
        }
        conv.lint_commands.push("ruff check .".to_string());
        conv.allowed_tools.push("Bash(uv *)".to_string());
        conv.allowed_tools.push("Bash(python3 *)".to_string());
        conv.allowed_tools.push("Bash(pytest *)".to_string());
    }

    // Go
    if has_manifest(repo_root, "go.mod") {
        if conv.test_command.is_none() {
            conv.test_command = Some("go test ./...".to_string());
        }
        conv.lint_commands.push("go vet ./...".to_string());
        conv.allowed_tools.push("Bash(go *)".to_string());
    }

    // Just
    if has_manifest(repo_root, "justfile") || has_manifest(repo_root, "Justfile") {
        conv.allowed_tools.push("Bash(just *)".to_string());
    }

    // Make
    if has_manifest(repo_root, "Makefile") || has_manifest(repo_root, "makefile") {
        conv.allowed_tools.push("Bash(make *)".to_string());
    }

    // Shell: detect via .shellcheckrc or .sh files in root/scripts/bin
    let has_shell = repo_root.join(".shellcheckrc").is_file()
        || ["", "scripts", "bin"].iter().any(|sub| {
            let dir = if sub.is_empty() {
                repo_root.to_path_buf()
            } else {
                repo_root.join(sub)
            };
            dir.is_dir()
                && std::fs::read_dir(&dir).ok().is_some_and(|entries| {
                    entries.filter_map(std::result::Result::ok).any(|e| {
                        let n = e.file_name().to_string_lossy().to_string();
                        std::path::Path::new(&n).extension().is_some_and(|ext| {
                            ext.eq_ignore_ascii_case("sh") || ext.eq_ignore_ascii_case("bash")
                        })
                    })
                })
        });
    if has_shell {
        conv.lint_commands.push("shellcheck **/*.sh".to_string());
        conv.allowed_tools.push("Bash(shellcheck *)".to_string());
        conv.allowed_tools.push("Bash(bash *)".to_string());
        conv.allowed_tools.push("Bash(bats *)".to_string());
    }

    // Elixir
    if repo_root.join("mix.exs").is_file() {
        if conv.test_command.is_none() {
            conv.test_command = Some("mix test".to_string());
        }
        conv.lint_commands
            .push("mix format --check-formatted".to_string());
        conv.allowed_tools.push("Bash(mix compile *)".to_string());
        conv.allowed_tools.push("Bash(mix test *)".to_string());
        conv.allowed_tools.push("Bash(mix format *)".to_string());
        conv.allowed_tools.push("Bash(mix deps.get *)".to_string());
        conv.allowed_tools.push("Bash(mix deps.tree *)".to_string());
        conv.allowed_tools
            .push("Bash(mix deps.compile *)".to_string());
        conv.allowed_tools
            .push("Bash(mix ecto.migrate *)".to_string());
        conv.allowed_tools
            .push("Bash(mix gettext.extract *)".to_string());
        conv.allowed_tools
            .push("Bash(mix gettext.merge *)".to_string());
        conv.allowed_tools.push("Bash(mix help *)".to_string());
        conv.allowed_tools.push("Bash(mix hex.info *)".to_string());
        conv.allowed_tools.push("Bash(mix xref *)".to_string());
        conv.allowed_tools
            .push("Bash(mix phx.routes *)".to_string());
        conv.allowed_tools.push("Bash(mix dialyzer *)".to_string());

        // Credo (check if it's a dep)
        if let Ok(content) = std::fs::read_to_string(repo_root.join("mix.exs")) {
            if content.contains(":credo") {
                conv.lint_commands.push("mix credo --strict".to_string());
                conv.allowed_tools.push("Bash(mix credo *)".to_string());
            }
            if content.contains(":sobelow") {
                conv.lint_commands.push("mix sobelow --config".to_string());
                conv.allowed_tools.push("Bash(mix sobelow *)".to_string());
            }
            // Tidewave MCP tools (if :tidewave is a dep and a local dev server is running)
            // NOTE: subagent support for starting mix phx.server is TBD — for now
            // these tools are available but require a running dev server
            if content.contains(":tidewave") {
                conv.allowed_tools
                    .push("mcp__tidewave__get_logs".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__get_source_location".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__get_docs".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__get_ecto_schemas".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__search_package_docs".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__list_project_files".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__read_project_file".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__grep_project_files".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__execute_sql_query".to_string());
                conv.allowed_tools
                    .push("mcp__tidewave__project_eval".to_string());
            }
        }
    }

    conv
}

/// Format the verification level as a display string.
pub(crate) const fn verify_level_name(level: &VerifyLevel) -> &'static str {
    match level {
        VerifyLevel::Local => "local",
        VerifyLevel::Ci => "ci",
        VerifyLevel::Thorough => "thorough",
    }
}

/// Check a kickoff report for missing recommended fields.
pub(crate) fn validate_kickoff_report(report: &KickoffReport) -> Vec<String> {
    let mut warnings = Vec::new();
    if report.schema_version.is_none() {
        warnings.push("Missing schema_version field".to_string());
    }
    if report.agent_id.is_none() {
        warnings.push("Missing agent_id field".to_string());
    }
    if report.issue_id.is_none() {
        warnings.push("Missing issue_id field".to_string());
    }
    if report.criteria.is_empty() {
        warnings.push("No criteria results in report".to_string());
    }
    warnings
}

/// Compute which patterns need adding to a git exclude file.
///
/// Given the existing exclude file content, returns only the patterns
/// from `KICKOFF_EXCLUDE_PATTERNS` that are not already present.
pub(crate) const KICKOFF_EXCLUDE_PATTERNS: &[&str] = &[
    "KICKOFF.md",
    ".kickoff-status",
    ".kickoff-slug",
    ".kickoff-metadata.json",
    ".kickoff-doc.json",
    "PLAN_KICKOFF.md",
    ".kickoff-plan.json",
    ".kickoff-criteria.json",
    ".kickoff-report.json",
];

pub(crate) fn missing_exclude_patterns(existing_content: &str) -> Vec<&'static str> {
    KICKOFF_EXCLUDE_PATTERNS
        .iter()
        .filter(|pattern| !existing_content.lines().any(|l| l.trim() == **pattern))
        .copied()
        .collect()
}

/// Outcome of comparing the on-disk design doc against the launch-time hash.
///
/// Returned by [`verify_protected_doc`]; consumed by `monitor::report` /
/// `monitor::status` so they can warn loudly when the agent rewrote the
/// canonical input it was given via `--doc`. See GH#580.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DocIntegrity {
    /// No `.kickoff-doc.json` breadcrumb — `--doc` wasn't used. Nothing to check.
    NotProtected,
    /// SHA-256 of the current file matches the recorded launch-time hash.
    Match { rel_path: String },
    /// The current file's SHA-256 differs from the recorded hash. The agent
    /// — or some other writer — modified the canonical design doc.
    Mismatch {
        rel_path: String,
        expected: String,
        actual: String,
    },
    /// The breadcrumb exists but the on-disk doc has gone missing or could
    /// not be read. Indicates an outright deletion or rename.
    Missing { rel_path: String, reason: String },
}

/// Compare the worktree's design doc against the hash recorded at launch.
///
/// Reads `.kickoff-doc.json` (written by `kickoff run` when `--doc` was used),
/// re-hashes the file it points at, and returns a structured verdict. Any I/O
/// or parse failure short of "breadcrumb missing entirely" surfaces as
/// `DocIntegrity::Missing` so callers can render a clear message.
pub(crate) fn verify_protected_doc(worktree_dir: &Path) -> DocIntegrity {
    let breadcrumb_path = worktree_dir.join(".kickoff-doc.json");
    let Ok(raw) = std::fs::read_to_string(&breadcrumb_path) else {
        return DocIntegrity::NotProtected;
    };
    let breadcrumb: KickoffDocBreadcrumb = match serde_json::from_str(&raw) {
        Ok(b) => b,
        Err(e) => {
            return DocIntegrity::Missing {
                rel_path: ".kickoff-doc.json".to_string(),
                reason: format!("malformed breadcrumb: {e}"),
            };
        }
    };

    let doc_path = worktree_dir.join(&breadcrumb.rel_path);
    let content = match std::fs::read_to_string(&doc_path) {
        Ok(c) => c,
        Err(e) => {
            return DocIntegrity::Missing {
                rel_path: breadcrumb.rel_path,
                reason: format!("cannot read on-disk doc: {e}"),
            };
        }
    };
    let actual = super::pipeline::compute_doc_hash(&content);

    if actual == breadcrumb.doc_hash {
        DocIntegrity::Match {
            rel_path: breadcrumb.rel_path,
        }
    } else {
        DocIntegrity::Mismatch {
            rel_path: breadcrumb.rel_path,
            expected: breadcrumb.doc_hash,
            actual,
        }
    }
}

/// Derive a tmux session name from a compact name (or legacy slug).
///
/// New format: uses the compact name directly (already ≤64 chars).
/// Legacy format: `feat-{slug}` capped at 50 chars.
pub(crate) fn tmux_session_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| if c == '.' || c == ':' { '-' } else { c })
        .collect();
    if sanitized.len() > 64 {
        sanitized[..64].to_string()
    } else {
        sanitized
    }
}

/// Check if a tmux session with the given name already exists.
pub(super) fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Check if a command is available on PATH.
pub(crate) fn command_available(cmd: &str) -> bool {
    #[cfg(target_os = "windows")]
    let lookup = Command::new("where.exe").arg(cmd).output();
    #[cfg(not(target_os = "windows"))]
    let lookup = Command::new("which").arg(cmd).output();

    lookup.is_ok_and(|o| o.status.success())
}

/// Detect the current platform and Linux distribution (if applicable).
pub(super) fn detect_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOS
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux(detect_linux_distro())
    }
}

/// Detect the Linux distribution by reading /etc/os-release.
pub(super) fn detect_linux_distro() -> LinuxDistro {
    let content = match std::fs::read_to_string("/etc/os-release") {
        Ok(c) => c.to_lowercase(),
        Err(_) => return LinuxDistro::Other,
    };
    if content.contains("id=debian")
        || content.contains("id=ubuntu")
        || content.contains("id_like=debian")
        || content.contains("id_like=\"debian")
    {
        LinuxDistro::Debian
    } else if content.contains("id=fedora")
        || content.contains("id=rhel")
        || content.contains("id=centos")
        || content.contains("id_like=fedora")
        || content.contains("id_like=\"fedora")
        || content.contains("id_like=\"rhel")
    {
        LinuxDistro::Fedora
    } else if content.contains("id=arch")
        || content.contains("id_like=arch")
        || content.contains("id_like=\"arch")
    {
        LinuxDistro::Arch
    } else if content.contains("id=alpine") {
        LinuxDistro::Alpine
    } else {
        LinuxDistro::Other
    }
}

/// Build a platform-specific install hint for a given command.
pub(super) fn install_hint(cmd: &str, platform: &Platform) -> String {
    match cmd {
        "timeout" | "gtimeout" => match platform {
            Platform::MacOS => "On macOS, install GNU coreutils:\n\
                 \n  brew install coreutils\n\
                 \nThis provides `gtimeout` which crosslink will use automatically."
                .to_string(),
            Platform::Linux(LinuxDistro::Debian) => {
                "Install coreutils (provides `timeout`):\n\n  sudo apt install coreutils"
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Fedora) => {
                "Install coreutils (provides `timeout`):\n\n  sudo dnf install coreutils"
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Arch) => {
                "Install coreutils (provides `timeout`):\n\n  sudo pacman -S coreutils".to_string()
            }
            Platform::Linux(LinuxDistro::Alpine) => {
                "Install coreutils (provides `timeout`):\n\n  apk add coreutils".to_string()
            }
            Platform::Linux(LinuxDistro::Other) => {
                "Install GNU coreutils to get the `timeout` command.\n\
                 Use your distribution's package manager (e.g. apt, dnf, pacman)."
                    .to_string()
            }
            Platform::Windows => "Install GNU coreutils via scoop or chocolatey:\n\
                 \n  scoop install coreutils\n  choco install gnuwin32-coreutils.install"
                .to_string(),
        },
        "tmux" => match platform {
            Platform::MacOS => "`tmux` is not installed.\n\n  brew install tmux\n\
                 \nAlternatively, use --container docker to avoid tmux."
                .to_string(),
            Platform::Linux(LinuxDistro::Debian) => {
                "`tmux` is not installed.\n\n  sudo apt install tmux\n\
                 \nAlternatively, use --container docker to avoid tmux."
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Fedora) => {
                "`tmux` is not installed.\n\n  sudo dnf install tmux\n\
                 \nAlternatively, use --container docker to avoid tmux."
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Arch) => {
                "`tmux` is not installed.\n\n  sudo pacman -S tmux\n\
                 \nAlternatively, use --container docker to avoid tmux."
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Alpine) => "`tmux` is not installed.\n\n  apk add tmux\n\
                 \nAlternatively, use --container docker to avoid tmux."
                .to_string(),
            Platform::Linux(LinuxDistro::Other) => "`tmux` is not installed.\n\
                 Install with your distribution's package manager (e.g. apt, dnf, pacman).\n\
                 \nAlternatively, use --container docker to avoid tmux."
                .to_string(),
            Platform::Windows => "`tmux` is not available on Windows.\n\
                 Use --container docker instead for containerized agent mode."
                .to_string(),
        },
        "claude" => match platform {
            Platform::MacOS => "`claude` CLI is not installed.\n\n  brew install claude-code\n\
                 \nOr install via npm:\n\n  npm install -g @anthropic-ai/claude-code"
                .to_string(),
            Platform::Windows | Platform::Linux(_) => {
                "`claude` CLI is not installed.\n\n  npm install -g @anthropic-ai/claude-code"
                    .to_string()
            }
        },
        "gh" => match platform {
            Platform::MacOS => {
                "`gh` (GitHub CLI) is required for --verify ci/thorough.\n\n  brew install gh"
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Debian) => {
                "`gh` (GitHub CLI) is required for --verify ci/thorough.\n\
                 \nInstall via apt (official repo):\n\
                 \n  sudo mkdir -p /etc/apt/keyrings\n  \
                 curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
                 | sudo tee /etc/apt/keyrings/githubcli-archive-keyring.gpg > /dev/null\n  \
                 echo \"deb [arch=$(dpkg --print-architecture) \
                 signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] \
                 https://cli.github.com/packages stable main\" \
                 | sudo tee /etc/apt/sources.list.d/github-cli.list > /dev/null\n  \
                 sudo apt update && sudo apt install gh\n\
                 \nOr install a single binary from: https://cli.github.com"
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Fedora) => {
                "`gh` (GitHub CLI) is required for --verify ci/thorough.\
                 \n\n  sudo dnf install gh"
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Arch) => {
                "`gh` (GitHub CLI) is required for --verify ci/thorough.\
                 \n\n  sudo pacman -S github-cli"
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Alpine) => {
                "`gh` (GitHub CLI) is required for --verify ci/thorough.\
                 \n\n  apk add github-cli"
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Other) => {
                "`gh` (GitHub CLI) is required for --verify ci/thorough.\n\
                 Install from: https://cli.github.com"
                    .to_string()
            }
            Platform::Windows => "`gh` (GitHub CLI) is required for --verify ci/thorough.\
                 \n\n  winget install GitHub.cli\n\
                 \nOr: scoop install gh"
                .to_string(),
        },
        "docker" => match platform {
            Platform::MacOS => "`docker` is not installed.\n\n  brew install --cask docker\n\
                 \nOr install Docker Desktop from: https://docs.docker.com/get-docker/\n\
                 \nAlternatively, use --container none for local mode."
                .to_string(),
            Platform::Linux(LinuxDistro::Debian) => "`docker` is not installed.\n\
                 \nInstall Docker Engine:\n\
                 \n  curl -fsSL https://get.docker.com | sh\n  sudo usermod -aG docker $USER\n\
                 \nOr see: https://docs.docker.com/engine/install/\n\
                 \nAlternatively, use --container none for local mode."
                .to_string(),
            Platform::Linux(LinuxDistro::Fedora) => "`docker` is not installed.\n\
                 \n  sudo dnf install docker-ce docker-ce-cli containerd.io\n\
                 \nOr: curl -fsSL https://get.docker.com | sh\n\
                 \nAlternatively, use --container none for local mode."
                .to_string(),
            Platform::Linux(LinuxDistro::Arch) => "`docker` is not installed.\n\
                 \n  sudo pacman -S docker\n  sudo systemctl enable --now docker\n\
                 \nAlternatively, use --container none for local mode."
                .to_string(),
            Platform::Linux(LinuxDistro::Alpine) => "`docker` is not installed.\n\
                 \n  apk add docker\n  rc-update add docker default\n  service docker start\n\
                 \nAlternatively, use --container none for local mode."
                .to_string(),
            Platform::Linux(LinuxDistro::Other) | Platform::Windows => {
                "`docker` is not installed.\n\
                 Install from: https://docs.docker.com/get-docker/\n\
                 \nAlternatively, use --container none for local mode."
                    .to_string()
            }
        },
        "podman" => match platform {
            Platform::MacOS => "`podman` is not installed.\n\n  brew install podman\n\
                 \nAlternatively, use --container none for local mode."
                .to_string(),
            Platform::Linux(LinuxDistro::Debian) => {
                "`podman` is not installed.\n\n  sudo apt install podman\n\
                 \nAlternatively, use --container none for local mode."
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Fedora) => {
                "`podman` is not installed.\n\n  sudo dnf install podman\n\
                 \nAlternatively, use --container none for local mode."
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Arch) => {
                "`podman` is not installed.\n\n  sudo pacman -S podman\n\
                 \nAlternatively, use --container none for local mode."
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Alpine) => {
                "`podman` is not installed.\n\n  apk add podman\n\
                 \nAlternatively, use --container none for local mode."
                    .to_string()
            }
            Platform::Linux(LinuxDistro::Other) => "`podman` is not installed.\n\
                 Install from: https://podman.io/getting-started/installation\n\
                 \nAlternatively, use --container none for local mode."
                .to_string(),
            Platform::Windows => "`podman` is not installed.\n\
                 \n  winget install RedHat.Podman\n\
                 \nAlternatively, use --container none for local mode."
                .to_string(),
        },
        other => {
            format!("`{other}` is not installed. Install it using your system package manager.")
        }
    }
}

/// Format seconds as a human-readable duration string.
pub(crate) fn format_duration(secs: u64) -> String {
    if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        if m > 0 {
            format!("{h}h {m}m")
        } else {
            format!("{h}h")
        }
    } else if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        if s > 0 {
            format!("{m}m {s}s")
        } else {
            format!("{m}m")
        }
    } else {
        format!("{secs}s")
    }
}

/// Truncate a string to `max` characters (char-boundary safe).
pub(super) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        s.chars().take(max).collect()
    } else {
        s.to_string()
    }
}

/// Normalize raw status file content to a canonical status string.
pub(super) fn normalize_status(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower == "done" {
        "done".to_string()
    } else if lower.contains("fail") || lower.contains("error") {
        "failed".to_string()
    } else if lower.contains("running") || raw.is_empty() {
        "running".to_string()
    } else {
        raw.to_string()
    }
}

/// Read the timeout metadata for display purposes.
pub(super) fn read_timeout_metadata(wt_path: &Path) -> Option<KickoffMetadata> {
    let meta_path = wt_path.join(".kickoff-metadata.json");
    let content = std::fs::read_to_string(&meta_path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Read the agent ID from the worktree's .crosslink/agent.json.
pub(super) fn read_agent_id(wt_path: &Path, _crosslink_dir: &Path) -> Option<String> {
    let agent_json = wt_path.join(".crosslink").join("agent.json");
    if agent_json.exists() {
        if let Ok(content) = std::fs::read_to_string(&agent_json) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                return val
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
        }
    }
    None
}

/// Try to read the associated issue from kickoff metadata.
pub(super) fn read_agent_issue(wt_path: &Path, _crosslink_dir: &Path) -> Option<String> {
    // Try .kickoff-criteria.json first (has issue ID from kickoff)
    let criteria_path = wt_path.join(".kickoff-criteria.json");
    if criteria_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&criteria_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                // The criteria file might have issue_id in extracted metadata
                if let Some(id) = val.get("issue_id").and_then(serde_json::Value::as_i64) {
                    return Some(crate::utils::format_issue_id(id));
                }
            }
        }
    }
    // Try .crosslink/agent.json
    let agent_json = wt_path.join(".crosslink").join("agent.json");
    if agent_json.exists() {
        if let Ok(content) = std::fs::read_to_string(&agent_json) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(id) = val.get("issue_id").and_then(serde_json::Value::as_i64) {
                    return Some(crate::utils::format_issue_id(id));
                }
            }
        }
    }
    None
}

/// Generate a small random numeric suffix (no external crate needed).
pub(super) fn rand_suffix() -> u32 {
    use std::time::SystemTime;
    let seed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    seed % 10000
}

/// Generate a 4-character hex suffix for worktree directory uniqueness.
///
/// Combines nanosecond timestamp with process ID to avoid collisions
/// when two processes start in the same nanosecond window.
pub(super) fn rand_hex_suffix() -> String {
    use std::time::SystemTime;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let pid = std::process::id();
    let mixed = nanos.wrapping_mul(31).wrapping_add(pid);
    format!("{:04x}", mixed & 0xFFFF)
}

/// Classify an agent for cleanup purposes.
pub(super) fn classify_agent(agent: &AgentInfo) -> CleanupClass {
    match agent.status.as_str() {
        // "done" and "failed" agents are safe to clean up (terminal states)
        "done" | "failed" => CleanupClass::Done,
        "running" => CleanupClass::Active,
        // "stopped", "timed-out", and anything else — potentially stale
        _ => CleanupClass::Stale,
    }
}
