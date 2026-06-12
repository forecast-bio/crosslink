//! AC-10 of the hub v3 design (`.design/hub-v3-per-agent-refs.md`):
//! the v2 conflict/recovery machinery is DELETED, not deprecated.
//!
//! This test greps the production source tree for the symbols of the
//! deleted machinery and fails if any reappear. It is the permanent
//! regression guard for the 754b teardown: the rebase-retry/recovery
//! era must never silently return.

use std::path::Path;

/// Symbols of deleted v2 machinery that must not exist in production code.
///
/// Each entry is a function/struct/key name unique enough that its presence
/// anywhere under `src/` (outside this allowlist's own mention in docs)
/// indicates the machinery returned.
const FORBIDDEN_SYMBOLS: &[&str] = &[
    // shared_writer write-path machinery (B1)
    "recover_from_push_conflict",
    "reconcile_display_counter",
    "promote_offline_issues",
    "rewrite_as_offline",
    "set_issue_created_claim_in_log",
    // dual-write bridge (B1)
    "ShadowStats",
    "dual_write_enabled",
    "check_hubv3_parity",
    "hub_v3.dual_write",
    // sync repair machinery (B2)
    "clean_dirty_state",
    "hub_health_check",
    "verify_cache_worktree",
    "push_hub_if_ahead",
    "rebase_preserving_local",
    "commit_and_push_locks",
    "upgrade_to_v2",
    "ensure_hub_gitignore",
];

fn scan_dir(dir: &Path, hits: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).expect("readable src dir");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, hits);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let content = std::fs::read_to_string(&path).expect("readable source file");
        for symbol in FORBIDDEN_SYMBOLS {
            for (lineno, line) in content.lines().enumerate() {
                if line.contains(symbol) {
                    hits.push(format!("{}:{}: {}", path.display(), lineno + 1, symbol));
                }
            }
        }
    }
}

#[test]
fn deleted_v2_machinery_never_returns() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut hits = Vec::new();
    scan_dir(&src, &mut hits);
    assert!(
        hits.is_empty(),
        "AC-10 violation: deleted v2 machinery symbols found in production source:\n{}",
        hits.join("\n")
    );
}
