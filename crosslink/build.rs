//! Build script to track include_str! dependencies and inject git metadata.
//! This ensures cargo rebuilds when template files change.

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
            println!("cargo:rustc-env=CROSSLINK_VERSION={}", suffix);
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

    // Track crosslink config and rules files
    println!("cargo:rerun-if-changed=resources/crosslink/hook-config.json");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/global.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/project.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/rust.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/python.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/javascript.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/typescript.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/typescript-react.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/javascript-react.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/go.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/java.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/c.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/cpp.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/csharp.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/ruby.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/php.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/swift.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/kotlin.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/scala.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/zig.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/odin.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/elixir.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/elixir-phoenix.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/web.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/sanitize-patterns.txt");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/knowledge.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/tracking-strict.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/tracking-normal.md");
    println!("cargo:rerun-if-changed=resources/crosslink/rules/tracking-relaxed.md");
}
