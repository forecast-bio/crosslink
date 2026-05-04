//! Build script to track `include_str`! dependencies, inject git metadata,
//! and auto-generate rule file includes from resources/crosslink/rules/.

use std::fs;
use std::io::Write;
use std::path::Path;

fn main() {
    // Inject git commit hash into the binary for `crosslink --version`
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/");
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    {
        if output.status.success() {
            let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let dirty = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            let suffix = if dirty {
                format!("{}+{}-dirty", env!("CARGO_PKG_VERSION"), hash)
            } else {
                format!("{}+{}", env!("CARGO_PKG_VERSION"), hash)
            };
            println!("cargo:rustc-env=CROSSLINK_VERSION={suffix}");
        }
    }

    // Track claude resource files
    println!("cargo:rerun-if-changed=resources/claude/settings.json");
    println!("cargo:rerun-if-changed=resources/claude/hooks/prompt-guard.py");
    println!("cargo:rerun-if-changed=resources/claude/hooks/post-edit-check.py");
    println!("cargo:rerun-if-changed=resources/claude/hooks/session-start.py");
    println!("cargo:rerun-if-changed=resources/claude/hooks/pre-web-check.py");
    println!("cargo:rerun-if-changed=resources/claude/hooks/work-check.py");
    println!("cargo:rerun-if-changed=resources/claude/mcp/safe-fetch-server.py");
    println!("cargo:rerun-if-changed=resources/mcp.json");
    println!("cargo:rerun-if-changed=resources/claude/commands/workflow.md");

    // Track crosslink config
    println!("cargo:rerun-if-changed=resources/crosslink/hook-config.json");

    // Auto-discover and track all rule files in resources/crosslink/rules/
    println!("cargo:rerun-if-changed=resources/crosslink/rules/");
    let rules_dir = Path::new("resources/crosslink/rules");
    if rules_dir.is_dir() {
        if let Err(e) = generate_rules_file(rules_dir) {
            eprintln!("cargo:warning=Failed to generate rules_gen.rs: {e}");
        }
    }

    // Auto-discover and track all command files in resources/claude/commands/
    println!("cargo:rerun-if-changed=resources/claude/commands/");
    let commands_dir = Path::new("resources/claude/commands");
    if commands_dir.is_dir() {
        if let Err(e) = generate_commands_file(commands_dir) {
            eprintln!("cargo:warning=Failed to generate commands_gen.rs: {e}");
        }
    }

    // Auto-discover and track all skill files in resources/claude/skills/
    // Skills are directories (each with a SKILL.md plus optional reference files)
    // bundled into projects so collaborators get a consistent skill surface.
    println!("cargo:rerun-if-changed=resources/claude/skills/");
    let skills_dir = Path::new("resources/claude/skills");
    if skills_dir.is_dir() {
        if let Err(e) = generate_skills_file(skills_dir) {
            eprintln!("cargo:warning=Failed to generate skills_gen.rs: {e}");
        }
    }

    // Track dashboard build output for rust-embed (GH #429 / #689).
    // The frontend lives at ../dashboard/ and emits a built bundle to
    // ../dashboard/dist/. rust-embed requires the folder to exist at
    // compile time — if the developer hasn't run `npm --prefix dashboard
    // run build` yet, create a minimal placeholder so `cargo build` still
    // succeeds. CI runs the npm build first, so CI always embeds the
    // real assets.
    println!("cargo:rerun-if-changed=../dashboard/dist/");
    let dashboard_dist = Path::new("../dashboard/dist");
    let dashboard_index = dashboard_dist.join("index.html");
    if !dashboard_index.exists() {
        let _ = fs::create_dir_all(dashboard_dist);
        let placeholder = r#"<!doctype html>
<html><head><title>crosslink dashboard — not built</title></head>
<body style="font-family: system-ui; max-width: 42rem; margin: 4rem auto; padding: 0 1rem; color: #222;">
<h1>crosslink dashboard — frontend not built</h1>
<p>The Rust binary was compiled without a built dashboard. To build the
frontend assets and embed them in the next <code>cargo build</code>:</p>
<pre>npm --prefix dashboard run build
cargo build</pre>
<p>See <code>DESIGN-CROSSLINK-DASHBOARD.md</code> at the repo root for
the design; GH #429 tracks the broader feature.</p>
</body></html>"#;
        if let Err(e) = fs::write(&dashboard_index, placeholder) {
            eprintln!(
                "cargo:warning=Failed to write dashboard placeholder at {}: {e}",
                dashboard_index.display()
            );
        }
    }
}

fn generate_commands_file(commands_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut cmd_entries: Vec<(String, String)> = Vec::new();

    let mut entries: Vec<_> = fs::read_dir(commands_dir)?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".md"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let filename = entry.file_name().to_string_lossy().to_string();
        let rel_path = format!("resources/claude/commands/{filename}");
        println!("cargo:rerun-if-changed={rel_path}");

        // Generate a const name: crosslink-guide.md -> CMD_CROSSLINK_GUIDE
        let const_name = filename
            .trim_end_matches(".md")
            .to_uppercase()
            .replace('-', "_");
        let const_name = format!("CMD_{const_name}");

        cmd_entries.push((filename, const_name));
    }

    let out_dir = std::env::var("OUT_DIR")?;
    let gen_path = Path::new(&out_dir).join("commands_gen.rs");
    let mut gen_file = fs::File::create(&gen_path)?;

    writeln!(
        gen_file,
        "// Auto-generated by build.rs — do not edit manually"
    )?;
    writeln!(gen_file, "// Generated from resources/claude/commands/")?;
    writeln!(gen_file)?;

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
    let abs_commands_dir = Path::new(&manifest_dir).join("resources/claude/commands");

    for (filename, const_name) in &cmd_entries {
        let abs_path = abs_commands_dir.join(filename);
        // Use forward slashes for include_str! paths — backslashes on Windows
        // are interpreted as escape sequences inside string literals.
        let abs_path_str = abs_path.to_string_lossy().replace('\\', "/");
        writeln!(
            gen_file,
            "pub(crate) const {const_name}: &str = include_str!(\"{abs_path_str}\");"
        )?;
    }

    writeln!(gen_file)?;
    writeln!(
        gen_file,
        "pub(crate) const COMMAND_FILES: &[(&str, &str)] = &["
    )?;
    for (filename, const_name) in &cmd_entries {
        writeln!(gen_file, "    (\"{filename}\", {const_name}),")?;
    }
    writeln!(gen_file, "];")?;

    Ok(())
}

fn generate_skills_file(skills_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Walk skill subdirectories and collect (relative_path_within_skills, const_name) pairs.
    // Relative path is e.g. "architect/SKILL.md" or "rust-gpu-discipline/anti-patterns.md".
    let mut skill_entries: Vec<(String, String)> = Vec::new();

    let mut skill_dirs: Vec<_> = fs::read_dir(skills_dir)?
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    skill_dirs.sort_by_key(std::fs::DirEntry::file_name);

    for skill_dir in skill_dirs {
        let skill_name = skill_dir.file_name().to_string_lossy().to_string();

        let mut files: Vec<_> = fs::read_dir(skill_dir.path())?
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .collect();
        files.sort_by_key(std::fs::DirEntry::file_name);

        for file in files {
            let filename = file.file_name().to_string_lossy().to_string();
            let rel_path = format!("{skill_name}/{filename}");
            let track_path = format!("resources/claude/skills/{rel_path}");
            println!("cargo:rerun-if-changed={track_path}");

            // include_str! requires UTF-8. Skill bundles like `.skill` (zip) or
            // any future binary asset would otherwise break the build. Validate
            // up front and skip with a warning so the rest of the skill still ships.
            if fs::read_to_string(file.path()).is_err() {
                println!(
                    "cargo:warning=Skipping non-UTF-8 skill asset: {track_path} \
                     (skill bundles like .skill archives are redundant with the unpacked directory)"
                );
                continue;
            }

            // Generate a unique const name from the relative path.
            // architect/SKILL.md           -> SKILL_ARCHITECT__SKILL_MD
            // rust-gpu-discipline/SKILL.md -> SKILL_RUST_GPU_DISCIPLINE__SKILL_MD
            let const_name = rel_path
                .replace('/', "__")
                .replace(['-', '.'], "_")
                .to_uppercase();
            let const_name = format!("SKILL_{const_name}");

            skill_entries.push((rel_path, const_name));
        }
    }

    let out_dir = std::env::var("OUT_DIR")?;
    let gen_path = Path::new(&out_dir).join("skills_gen.rs");
    let mut gen_file = fs::File::create(&gen_path)?;

    writeln!(
        gen_file,
        "// Auto-generated by build.rs — do not edit manually"
    )?;
    writeln!(gen_file, "// Generated from resources/claude/skills/")?;
    writeln!(gen_file)?;

    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
    let abs_skills_dir = Path::new(&manifest_dir).join("resources/claude/skills");

    for (rel_path, const_name) in &skill_entries {
        let abs_path = abs_skills_dir.join(rel_path);
        // Use forward slashes for include_str! paths — backslashes on Windows
        // are interpreted as escape sequences inside string literals.
        let abs_path_str = abs_path.to_string_lossy().replace('\\', "/");
        writeln!(
            gen_file,
            "pub(crate) const {const_name}: &str = include_str!(\"{abs_path_str}\");"
        )?;
    }

    writeln!(gen_file)?;
    writeln!(
        gen_file,
        "pub(crate) const SKILL_FILES: &[(&str, &str)] = &["
    )?;
    for (rel_path, const_name) in &skill_entries {
        writeln!(gen_file, "    (\"{rel_path}\", {const_name}),")?;
    }
    writeln!(gen_file, "];")?;

    Ok(())
}

fn generate_rules_file(rules_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut rule_entries: Vec<(String, String)> = Vec::new();

    let mut entries: Vec<_> = fs::read_dir(rules_dir)?
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            std::path::Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
                || std::path::Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
        })
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let filename = entry.file_name().to_string_lossy().to_string();
        let rel_path = format!("resources/crosslink/rules/{filename}");
        println!("cargo:rerun-if-changed={rel_path}");

        // Generate a const name from filename: quality.md -> RULE_QUALITY
        let const_name = filename
            .trim_end_matches(".md")
            .trim_end_matches(".txt")
            .to_uppercase()
            .replace('-', "_");
        let const_name = format!("RULE_{const_name}");

        rule_entries.push((filename, const_name));
    }

    // Generate rules_gen.rs with all includes and the RULE_FILES array
    let out_dir = std::env::var("OUT_DIR")?;
    let gen_path = Path::new(&out_dir).join("rules_gen.rs");
    let mut gen_file = fs::File::create(&gen_path)?;

    writeln!(
        gen_file,
        "// Auto-generated by build.rs — do not edit manually"
    )?;
    writeln!(gen_file, "// Generated from resources/crosslink/rules/")?;
    writeln!(gen_file)?;

    // Use absolute path for include_str! since OUT_DIR is deep in target/
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
    let abs_rules_dir = Path::new(&manifest_dir).join("resources/crosslink/rules");

    // Write include_str! constants
    for (filename, const_name) in &rule_entries {
        let abs_path = abs_rules_dir.join(filename);
        // Use forward slashes for include_str! paths — backslashes on Windows
        // are interpreted as escape sequences inside string literals.
        let abs_path_str = abs_path.to_string_lossy().replace('\\', "/");
        writeln!(
            gen_file,
            "pub(crate) const {const_name}: &str = include_str!(\"{abs_path_str}\");"
        )?;
    }

    writeln!(gen_file)?;

    // Write the RULE_FILES array
    writeln!(
        gen_file,
        "pub(crate) const RULE_FILES: &[(&str, &str)] = &["
    )?;
    for (filename, const_name) in &rule_entries {
        writeln!(gen_file, "    (\"{filename}\", {const_name}),")?;
    }
    writeln!(gen_file, "];")?;

    Ok(())
}
