//! Build script to track include_str! dependencies.
//! This ensures cargo rebuilds when template files change.

fn main() {
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
