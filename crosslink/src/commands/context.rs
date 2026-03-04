use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use crate::commands::init;
use crate::ContextCommands;

/// Language detection: maps manifest files to (language name, rule filename).
const LANGUAGE_MANIFESTS: &[(&str, &str, &str)] = &[
    ("Cargo.toml", "Rust", "rust.md"),
    ("package.json", "JavaScript", "javascript.md"),
    ("tsconfig.json", "TypeScript", "typescript.md"),
    ("pyproject.toml", "Python", "python.md"),
    ("requirements.txt", "Python", "python.md"),
    ("go.mod", "Go", "go.md"),
    ("pom.xml", "Java", "java.md"),
    ("build.gradle", "Java", "java.md"),
    ("Gemfile", "Ruby", "ruby.md"),
    ("composer.json", "PHP", "php.md"),
    ("Package.swift", "Swift", "swift.md"),
    ("CMakeLists.txt", "C/C++", "cpp.md"),
    ("Makefile", "C/C++", "c.md"),
];

/// Expected hook files that should exist in .claude/hooks/.
const EXPECTED_HOOKS: &[&str] = &[
    "prompt-guard.py",
    "post-edit-check.py",
    "session-start.py",
    "pre-web-check.py",
    "work-check.py",
    "crosslink_config.py",
];

/// Expected command files that should exist in .claude/commands/.
const EXPECTED_COMMANDS: &[&str] = &[
    "workflow.md",
    "feature.md",
    "featree.md",
    "kickoff.md",
    "check.md",
    "commit.md",
    "preflight.md",
    "review.md",
    "audit.md",
];

/// Expected rule files in .crosslink/rules/.
const EXPECTED_RULES: &[&str] = &[
    "global.md",
    "project.md",
    "tracking-strict.md",
    "tracking-normal.md",
    "tracking-relaxed.md",
];

pub fn run(command: ContextCommands, crosslink_dir: &Path) -> Result<()> {
    match command {
        ContextCommands::Measure { verbose } => measure(crosslink_dir, verbose),
        ContextCommands::Check => {
            let claude_dir = crosslink_dir
                .parent()
                .context("Cannot determine project root")?
                .join(".claude");
            check(crosslink_dir, &claude_dir)
        }
    }
}

// ---------------------------------------------------------------------------
// measure — report context injection sizes
// ---------------------------------------------------------------------------

fn measure(crosslink_dir: &Path, verbose: bool) -> Result<()> {
    let project_root = crosslink_dir
        .parent()
        .context("Cannot determine project root")?;

    println!("Context injection measurement");
    println!("{}", "=".repeat(60));

    // 1. Rule files
    let rules_dir = crosslink_dir.join("rules");
    let mut total_rules: usize = 0;
    let mut active_rules: usize = 0;
    let mut dormant_rules: usize = 0;

    // Detect active languages
    let active_langs = detect_active_languages(project_root);

    let rules_local_dir = crosslink_dir.join("rules.local");

    println!("\n## Rule files (.crosslink/rules/)");
    println!("{:<35} {:>8} {:>8}  STATUS", "FILE", "BYTES", "~TOKENS");
    println!("{}", "-".repeat(65));

    // Collect overridden filenames from rules.local/
    let local_overrides: std::collections::HashSet<String> = if rules_local_dir.is_dir() {
        fs::read_dir(&rules_local_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    if rules_dir.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(&rules_dir)
            .context("Failed to read rules directory")?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "md" || ext == "txt")
                    .unwrap_or(false)
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in &entries {
            let path = entry.path();
            let filename = entry.file_name().to_string_lossy().to_string();

            // If overridden by rules.local/, show the local version's size
            let (size, suffix) = if local_overrides.contains(&filename) {
                let local_path = rules_local_dir.join(&filename);
                let s = fs::metadata(&local_path)
                    .map(|m| m.len() as usize)
                    .unwrap_or(0);
                (s, " (local)")
            } else {
                let s = fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
                (s, "")
            };
            let tokens = size / 4;
            total_rules += size;

            let is_active = is_rule_active(&filename, &active_langs);
            let status = if is_active {
                active_rules += size;
                "active"
            } else {
                dormant_rules += size;
                "dormant"
            };

            println!(
                "{:<35} {:>8} {:>8}  {}{}",
                filename, size, tokens, status, suffix
            );
        }
    }

    // Show additive rules from rules.local/ (files not present in rules/)
    if rules_local_dir.is_dir() {
        let base_files: std::collections::HashSet<String> = if rules_dir.is_dir() {
            fs::read_dir(&rules_dir)
                .ok()
                .into_iter()
                .flatten()
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        } else {
            std::collections::HashSet::new()
        };

        let mut local_entries: Vec<_> = fs::read_dir(&rules_local_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                !base_files.contains(&name)
                    && e.path()
                        .extension()
                        .map(|ext| ext == "md" || ext == "txt")
                        .unwrap_or(false)
            })
            .collect();
        local_entries.sort_by_key(|e| e.file_name());

        for entry in &local_entries {
            let path = entry.path();
            let filename = entry.file_name().to_string_lossy().to_string();
            let size = fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
            let tokens = size / 4;
            total_rules += size;
            active_rules += size;

            println!("{:<35} {:>8} {:>8}  active (local)", filename, size, tokens);
        }
    }

    println!();
    println!(
        "  Total rules:   {:>8} bytes ({} tokens)",
        total_rules,
        total_rules / 4
    );
    println!(
        "  Active rules:  {:>8} bytes ({} tokens)",
        active_rules,
        active_rules / 4
    );
    println!(
        "  Dormant rules: {:>8} bytes ({} tokens)",
        dormant_rules,
        dormant_rules / 4
    );

    // 2. Active languages
    println!("\n## Detected languages");
    if active_langs.is_empty() {
        println!("  (none detected)");
    } else {
        for lang in &active_langs {
            println!("  - {}", lang);
        }
    }

    // 3. CLAUDE.md
    let claude_md = project_root.join("CLAUDE.md");
    let claude_md_size = if claude_md.is_file() {
        fs::metadata(&claude_md)
            .map(|m| m.len() as usize)
            .unwrap_or(0)
    } else {
        0
    };

    println!("\n## CLAUDE.md");
    if claude_md_size > 0 {
        println!(
            "  {:>8} bytes ({} tokens)",
            claude_md_size,
            claude_md_size / 4
        );
    } else {
        println!("  (not found)");
    }

    // 4. Skill files (.claude/commands/)
    let commands_dir = project_root.join(".claude/commands");
    let mut total_skills: usize = 0;

    println!("\n## Skill files (.claude/commands/)");
    if commands_dir.is_dir() {
        println!("{:<35} {:>8} {:>8}", "FILE", "BYTES", "~TOKENS");
        println!("{}", "-".repeat(55));

        let mut entries: Vec<_> = fs::read_dir(&commands_dir)
            .context("Failed to read commands directory")?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|ext| ext == "md").unwrap_or(false))
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in &entries {
            let path = entry.path();
            let filename = entry.file_name().to_string_lossy().to_string();
            let size = fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
            total_skills += size;
            println!("{:<35} {:>8} {:>8}", filename, size, size / 4);
        }
        println!();
        println!(
            "  Total skills: {:>8} bytes ({} tokens)",
            total_skills,
            total_skills / 4
        );
    } else {
        println!("  (not found)");
    }

    // 5. Estimated behavioral guard size (first prompt)
    let tree_est: usize = 2000;
    let deps_est: usize = 1200;
    let wrapper_est: usize = 500;
    let full_guard = tree_est + deps_est + active_rules + wrapper_est;

    println!("\n## Estimated first-prompt injection");
    println!("  Project tree:    ~{:>6} bytes", tree_est);
    println!("  Dependencies:    ~{:>6} bytes", deps_est);
    println!("  Active rules:     {:>6} bytes", active_rules);
    println!("  Wrapper/headers: ~{:>6} bytes", wrapper_est);
    println!("  ─────────────────────────");
    println!(
        "  Total:           ~{:>6} bytes (~{} tokens)",
        full_guard,
        full_guard / 4
    );

    // 6. Condensed reminder estimate
    let condensed_est: usize = 500;
    println!("\n## Condensed reminder (subsequent prompts)");
    println!(
        "  Estimated:       ~{:>6} bytes (~{} tokens)",
        condensed_est,
        condensed_est / 4
    );

    // 7. Savings comparison
    println!("\n## Adaptive reminder savings (over 50 prompts)");
    let always_total = full_guard + condensed_est * 49;
    // With adaptive (threshold=5): full guard + ~9 condensed reminders
    let adaptive_reminders = 49 / 5; // roughly 9 reminders
    let adaptive_total = full_guard + condensed_est * adaptive_reminders;
    let saved = always_total.saturating_sub(adaptive_total);
    println!(
        "  Always-inject:   ~{:>8} bytes ({} tokens)",
        always_total,
        always_total / 4
    );
    println!(
        "  Adaptive (t=5):  ~{:>8} bytes ({} tokens)",
        adaptive_total,
        adaptive_total / 4
    );
    println!(
        "  Saved:           ~{:>8} bytes ({} tokens, {:.0}%)",
        saved,
        saved / 4,
        if always_total > 0 {
            saved as f64 / always_total as f64 * 100.0
        } else {
            0.0
        }
    );

    if verbose {
        println!("\n## Hook config");
        let config_path = crosslink_dir.join("hook-config.json");
        if config_path.is_file() {
            let content =
                fs::read_to_string(&config_path).context("Failed to read hook-config.json")?;
            println!("{}", content);
        } else {
            println!("  (not found)");
        }
    }

    Ok(())
}

fn detect_active_languages(project_root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Check project root and immediate subdirs
    let mut check_dirs = vec![project_root.to_path_buf()];
    if let Ok(entries) = fs::read_dir(project_root) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with('.') {
                    check_dirs.push(path);
                }
            }
        }
    }

    for dir in &check_dirs {
        for &(manifest, lang, _rule_file) in LANGUAGE_MANIFESTS {
            if dir.join(manifest).exists() && seen.insert(lang.to_string()) {
                found.push(lang.to_string());
            }
        }
    }

    found
}

fn is_rule_active(filename: &str, active_langs: &[String]) -> bool {
    // Always-active files
    if matches!(
        filename,
        "global.md"
            | "project.md"
            | "tracking-strict.md"
            | "tracking-normal.md"
            | "tracking-relaxed.md"
            | "sanitize-patterns.txt"
            | "knowledge.md"
            | "web.md"
    ) {
        return true;
    }

    // Check if the rule matches a detected language
    for &(_manifest, lang, rule_file) in LANGUAGE_MANIFESTS {
        if filename == rule_file && active_langs.iter().any(|l| l == lang) {
            return true;
        }
    }

    false
}

// ---------------------------------------------------------------------------
// check — verify crosslink deployment integrity
// ---------------------------------------------------------------------------

fn check(crosslink_dir: &Path, claude_dir: &Path) -> Result<()> {
    let mut problems = 0;

    println!("Crosslink deployment check");
    println!("{}", "=".repeat(40));

    // 1. Rule files
    println!("\n## Rule files");
    let rules_dir = crosslink_dir.join("rules");
    for &name in EXPECTED_RULES {
        let path = rules_dir.join(name);
        if path.is_file() {
            println!("  OK  {}", name);
        } else {
            println!("  MISSING  {}", name);
            problems += 1;
        }
    }

    // Also check language rule files
    for &(_name, _content) in init::RULE_FILES {
        let path = rules_dir.join(_name);
        if path.is_file() {
            // verbose: could print OK
        } else {
            println!("  MISSING  {}", _name);
            problems += 1;
        }
    }

    // 2. Hook files
    println!("\n## Hook files");
    let hooks_dir = claude_dir.join("hooks");
    for &name in EXPECTED_HOOKS {
        let path = hooks_dir.join(name);
        if path.is_file() {
            println!("  OK  {}", name);
        } else {
            println!("  MISSING  {}", name);
            problems += 1;
        }
    }

    // 3. Command files
    println!("\n## Command files");
    let commands_dir = claude_dir.join("commands");
    for &name in EXPECTED_COMMANDS {
        let path = commands_dir.join(name);
        if path.is_file() {
            println!("  OK  {}", name);
        } else {
            println!("  MISSING  {}", name);
            problems += 1;
        }
    }

    // 4. Hook config
    println!("\n## Configuration");
    let config_path = crosslink_dir.join("hook-config.json");
    if config_path.is_file() {
        match fs::read_to_string(&config_path) {
            Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(_) => println!("  OK  hook-config.json (valid JSON)"),
                Err(e) => {
                    println!("  INVALID  hook-config.json: {}", e);
                    problems += 1;
                }
            },
            Err(e) => {
                println!("  ERROR  hook-config.json: {}", e);
                problems += 1;
            }
        }
    } else {
        println!("  MISSING  hook-config.json");
        problems += 1;
    }

    // Summary
    println!();
    if problems == 0 {
        println!("All checks passed.");
    } else {
        println!(
            "{} problem(s) found. Run `crosslink init --force` to repair.",
            problems
        );
        std::process::exit(1);
    }

    Ok(())
}
