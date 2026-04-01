use anyhow::Result;
use std::path::Path;

use anyhow::{bail, Context};
use std::path::PathBuf;

use crate::db::{Database, SCHEMA_VERSION};
use crate::hydration::hydrate_to_sqlite;
use crate::identity::AgentConfig;
use crate::issue_file::{
    read_all_issue_files, read_all_milestone_files, read_comment_files, read_counters,
    read_milestones_file, write_comment_file, write_counters, write_issue_file, Counters,
};
use crate::signing;
use crate::sync::SyncManager;
use crate::IntegrityCommands;

use crate::sync::HUB_CACHE_DIR;

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum CheckStatus {
    Pass,
    Fail(String),
    Repaired(String),
    Skipped(String),
}

#[derive(Debug, Clone)]
struct CheckResult {
    name: String,
    status: CheckStatus,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run(action: Option<&IntegrityCommands>, crosslink_dir: &Path, db: &Database) -> Result<()> {
    match action {
        None => run_all(crosslink_dir, db),
        Some(IntegrityCommands::Schema { repair }) => {
            let result = check_schema(db, *repair)?;
            print_result(&result);
            Ok(())
        }
        Some(IntegrityCommands::Counters { repair }) => {
            let result = check_counters(crosslink_dir, db, *repair)?;
            print_result(&result);
            Ok(())
        }
        Some(IntegrityCommands::Hydration { repair }) => {
            let result = check_hydration(crosslink_dir, db, *repair)?;
            print_result(&result);
            Ok(())
        }
        Some(IntegrityCommands::Locks { repair }) => {
            let result = check_locks(crosslink_dir, *repair)?;
            print_result(&result);
            Ok(())
        }
        Some(IntegrityCommands::Layout { repair }) => {
            let result = check_layout(crosslink_dir, *repair);
            print_result(&result);
            Ok(())
        }
        Some(IntegrityCommands::SignBackfill { confirm, key }) => {
            sign_backfill(crosslink_dir, *confirm, key.as_deref())
        }
    }
}

fn run_all(crosslink_dir: &Path, db: &Database) -> Result<()> {
    println!("Running all integrity checks...\n");

    let results = vec![
        check_schema(db, false)?,
        check_counters(crosslink_dir, db, false)?,
        check_hydration(crosslink_dir, db, false)?,
        check_locks(crosslink_dir, false)?,
        check_layout(crosslink_dir, false),
    ];

    for result in &results {
        print_result(result);
    }
    println!();
    print_summary(&results);
    Ok(())
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

fn check_schema(db: &Database, _repair: bool) -> Result<CheckResult> {
    let version = db.get_schema_version()?;
    let status = if version == SCHEMA_VERSION {
        CheckStatus::Pass
    } else {
        // Database::open() auto-migrates, so if we get here with a mismatch
        // something is genuinely wrong. Report it but there's nothing to repair
        // beyond reopening the DB (which already happened).
        CheckStatus::Fail(format!(
            "version {version} does not match expected {SCHEMA_VERSION}"
        ))
    };
    Ok(CheckResult {
        name: "schema".to_string(),
        status,
    })
}

fn check_counters(crosslink_dir: &Path, db: &Database, repair: bool) -> Result<CheckResult> {
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    if !cache_dir.exists() {
        return Ok(CheckResult {
            name: "counters".to_string(),
            status: CheckStatus::Skipped("sync not configured".to_string()),
        });
    }

    let counters_path = cache_dir.join("meta").join("counters.json");
    let counters = read_counters(&counters_path)?;
    let max_display = db.get_max_display_id()?;
    let max_comment = db.get_max_comment_id()?;
    let expected_display = max_display + 1;
    let expected_comment = max_comment + 1;

    let display_ok = counters.next_display_id >= expected_display;
    let comment_ok = counters.next_comment_id >= expected_comment;

    if display_ok && comment_ok {
        return Ok(CheckResult {
            name: "counters".to_string(),
            status: CheckStatus::Pass,
        });
    }

    let mut issues = Vec::new();
    if !display_ok {
        issues.push(format!(
            "next_display_id is {}, expected >= {}",
            counters.next_display_id, expected_display
        ));
    }
    if !comment_ok {
        issues.push(format!(
            "next_comment_id is {}, expected >= {}",
            counters.next_comment_id, expected_comment
        ));
    }
    let details = issues.join("; ");

    if !repair {
        return Ok(CheckResult {
            name: "counters".to_string(),
            status: CheckStatus::Fail(details),
        });
    }

    let repaired = Counters {
        next_display_id: expected_display.max(counters.next_display_id),
        next_comment_id: expected_comment.max(counters.next_comment_id),
        next_milestone_id: counters.next_milestone_id,
    };
    write_counters(&counters_path, &repaired)?;

    Ok(CheckResult {
        name: "counters".to_string(),
        status: CheckStatus::Repaired(format!("fixed: {details}")),
    })
}

fn check_hydration(crosslink_dir: &Path, db: &Database, repair: bool) -> Result<CheckResult> {
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    if !cache_dir.exists() {
        return Ok(CheckResult {
            name: "hydration".to_string(),
            status: CheckStatus::Skipped("sync not configured".to_string()),
        });
    }

    let issues_dir = cache_dir.join("issues");
    let json_issues = read_all_issue_files(&issues_dir)?;
    let json_issue_count = json_issues
        .iter()
        .filter(|i| i.display_id.is_some())
        .count() as i64;
    let db_issue_count = db.get_issue_count()?;

    // Count milestones: per-file first, fall back to legacy single-file
    let milestones_dir = cache_dir.join("meta").join("milestones");
    let json_milestone_entries = read_all_milestone_files(&milestones_dir)?;
    let json_milestone_count = if json_milestone_entries.is_empty() {
        let legacy_path = cache_dir.join("meta").join("milestones.json");
        let legacy = read_milestones_file(&legacy_path)?;
        legacy.milestones.len() as i64
    } else {
        json_milestone_entries.len() as i64
    };
    let db_milestone_count = db.get_milestone_count()?;

    let issues_ok = json_issue_count == db_issue_count;
    let milestones_ok = json_milestone_count == db_milestone_count;

    if issues_ok && milestones_ok {
        return Ok(CheckResult {
            name: "hydration".to_string(),
            status: CheckStatus::Pass,
        });
    }

    let mut issues = Vec::new();
    if !issues_ok {
        issues.push(format!(
            "{json_issue_count} issues in JSON, {db_issue_count} in SQLite"
        ));
    }
    if !milestones_ok {
        issues.push(format!(
            "{json_milestone_count} milestones in JSON, {db_milestone_count} in SQLite"
        ));
    }
    let details = issues.join("; ");

    if !repair {
        return Ok(CheckResult {
            name: "hydration".to_string(),
            status: CheckStatus::Fail(details),
        });
    }

    db.clear_shared_data()?;
    let stats = hydrate_to_sqlite(&cache_dir, db)?;

    Ok(CheckResult {
        name: "hydration".to_string(),
        status: CheckStatus::Repaired(format!(
            "re-hydrated {} issues, {} comments",
            stats.issues, stats.comments
        )),
    })
}

fn check_locks(crosslink_dir: &Path, repair: bool) -> Result<CheckResult> {
    let Ok(sync) = SyncManager::new(crosslink_dir) else {
        return Ok(CheckResult {
            name: "locks".to_string(),
            status: CheckStatus::Skipped("sync not configured".to_string()),
        });
    };

    if !sync.is_initialized() {
        return Ok(CheckResult {
            name: "locks".to_string(),
            status: CheckStatus::Skipped("sync cache not initialized".to_string()),
        });
    }

    let stale = sync.find_stale_locks()?;

    if stale.is_empty() {
        return Ok(CheckResult {
            name: "locks".to_string(),
            status: CheckStatus::Pass,
        });
    }

    let details = format!(
        "{} stale lock(s): {}",
        stale.len(),
        stale
            .iter()
            .map(|(id, agent)| format!("#{id} ({agent})"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    if !repair {
        return Ok(CheckResult {
            name: "locks".to_string(),
            status: CheckStatus::Fail(details),
        });
    }

    let Some(agent) = AgentConfig::load(crosslink_dir)? else {
        return Ok(CheckResult {
            name: "locks".to_string(),
            status: CheckStatus::Fail(format!("{details}; cannot repair without agent identity")),
        });
    };

    let mut released = 0;
    if sync.is_v2_layout() {
        if let Ok(Some(writer)) = crate::shared_writer::SharedWriter::new(crosslink_dir) {
            for (id, stale_agent_id) in &stale {
                match writer.force_release_lock_v2(*id, stale_agent_id) {
                    Ok(_) => released += 1,
                    Err(e) => tracing::warn!("Could not release stale lock #{}: {}", id, e),
                }
            }
        }
    } else {
        for (id, _) in &stale {
            if sync.release_lock(&agent, *id, crate::sync::LockMode::Steal)? {
                released += 1;
            }
        }
    }

    Ok(CheckResult {
        name: "locks".to_string(),
        status: CheckStatus::Repaired(format!(
            "released {} of {} stale lock(s)",
            released,
            stale.len()
        )),
    })
}

// ---------------------------------------------------------------------------
// Output formatting
// ---------------------------------------------------------------------------

fn print_result(result: &CheckResult) {
    let (tag, detail) = match &result.status {
        CheckStatus::Pass => ("PASS", String::new()),
        CheckStatus::Fail(d) => ("FAIL", d.clone()),
        CheckStatus::Repaired(d) => ("REPAIRED", d.clone()),
        CheckStatus::Skipped(d) => ("SKIPPED", d.clone()),
    };

    let tag_str = format!("[{tag}]");
    if detail.is_empty() {
        println!("{:<12} {}", tag_str, result.name);
    } else {
        println!("{:<12} {:<12} {}", tag_str, result.name, detail);
    }
}

fn print_summary(results: &[CheckResult]) {
    let passed = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Pass))
        .count();
    let failed = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Fail(_)))
        .count();
    let repaired = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Repaired(_)))
        .count();
    let skipped = results
        .iter()
        .filter(|r| matches!(r.status, CheckStatus::Skipped(_)))
        .count();

    let mut parts = Vec::new();
    if passed > 0 {
        parts.push(format!("{passed} passed"));
    }
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if repaired > 0 {
        parts.push(format!("{repaired} repaired"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }

    println!("Integrity: {}", parts.join(", "));
}

// ---------------------------------------------------------------------------
// Layout check: detect mixed V1/V2 issue files
// ---------------------------------------------------------------------------

fn check_layout(crosslink_dir: &Path, repair: bool) -> CheckResult {
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    let issues_dir = cache_dir.join("issues");

    if !issues_dir.exists() {
        return CheckResult {
            name: "layout".to_string(),
            status: CheckStatus::Skipped("no issues directory".to_string()),
        };
    }

    // Scan for V1 flat files and V2 directories
    let mut v1_uuids: Vec<String> = Vec::new();
    let mut v2_uuids: Vec<String> = Vec::new();
    let mut both_uuids: Vec<String> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&issues_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            if path.is_file()
                && std::path::Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                let uuid = name.trim_end_matches(".json").to_string();
                v1_uuids.push(uuid);
            } else if path.is_dir() && path.join("issue.json").exists() {
                v2_uuids.push(name);
            }
        }
    }

    // Find UUIDs that exist in both formats
    let v1_set: std::collections::HashSet<&str> = v1_uuids.iter().map(String::as_str).collect();
    let v2_set: std::collections::HashSet<&str> = v2_uuids.iter().map(String::as_str).collect();
    for uuid in &v1_set {
        if v2_set.contains(uuid) {
            both_uuids.push(uuid.to_string());
        }
    }

    // Check version marker consistency
    let meta_dir = cache_dir.join("meta");
    let version = crate::issue_file::read_layout_version(&meta_dir).unwrap_or(1);
    let v1_only: Vec<&str> = v1_uuids
        .iter()
        .filter(|u| !v2_set.contains(u.as_str()))
        .map(String::as_str)
        .collect();

    let has_problems = !both_uuids.is_empty() || (version >= 2 && !v1_only.is_empty());

    if !has_problems {
        return CheckResult {
            name: "layout".to_string(),
            status: CheckStatus::Pass,
        };
    }

    let mut issues_desc = Vec::new();
    if !both_uuids.is_empty() {
        issues_desc.push(format!(
            "{} UUID(s) have both V1 and V2 files",
            both_uuids.len()
        ));
    }
    if version >= 2 && !v1_only.is_empty() {
        issues_desc.push(format!("{} V1 flat file(s) on a V2 hub", v1_only.len()));
    }

    if !repair {
        return CheckResult {
            name: "layout".to_string(),
            status: CheckStatus::Fail(issues_desc.join("; ")),
        };
    }

    // Repair: migrate V1 → V2 and remove stale V1 duplicates
    let mut migrated = 0;
    let mut cleaned = 0;

    // Remove V1 files that have V2 equivalents (stale duplicates)
    for uuid in &both_uuids {
        let v1_path = issues_dir.join(format!("{uuid}.json"));
        if v1_path.exists() {
            let _ = std::fs::remove_file(&v1_path);
            cleaned += 1;
        }
    }

    // Migrate V1-only files to V2 format (when hub is V2)
    if version >= 2 {
        for uuid in &v1_only {
            let v1_path = issues_dir.join(format!("{uuid}.json"));
            let v2_dir = issues_dir.join(uuid);
            let v2_path = v2_dir.join("issue.json");

            if v1_path.exists() && !v2_path.exists() {
                if let Ok(content) = std::fs::read(&v1_path) {
                    if std::fs::create_dir_all(&v2_dir).is_ok()
                        && std::fs::write(&v2_path, &content).is_ok()
                    {
                        let _ = std::fs::remove_file(&v1_path);
                        migrated += 1;
                    }
                }
            }
        }
    }

    // Ensure version marker exists
    if !meta_dir.join("version.json").exists() {
        let _ = crate::issue_file::write_layout_version(
            &meta_dir,
            crate::issue_file::CURRENT_LAYOUT_VERSION,
        );
    }

    let mut repair_desc = Vec::new();
    if cleaned > 0 {
        repair_desc.push(format!("{cleaned} stale V1 duplicate(s) removed"));
    }
    if migrated > 0 {
        repair_desc.push(format!("{migrated} V1 file(s) migrated to V2"));
    }

    CheckResult {
        name: "layout".to_string(),
        status: CheckStatus::Repaired(repair_desc.join("; ")),
    }
}

// ---------------------------------------------------------------------------
// Sign backfill: retroactively sign unsigned entries with a human key
// ---------------------------------------------------------------------------

/// Signing namespace for backfill attestation — distinct from the original
/// `"crosslink-comment"` namespace so verification can distinguish
/// human-attested entries from agent-signed ones.
const BACKFILL_SIGNING_NAMESPACE: &str = "crosslink-backfill";

/// Principal used for human backfill attestation in `allowed_signers`.
const BACKFILL_PRINCIPAL: &str = "backfill@crosslink";

fn sign_backfill(crosslink_dir: &Path, confirm: bool, key_override: Option<&Path>) -> Result<()> {
    let cache_dir = crosslink_dir.join(HUB_CACHE_DIR);
    if !cache_dir.exists() {
        bail!("Hub cache not found. Run `crosslink sync` first.");
    }

    // ── Resolve signing key ──────────────────────────────────────────
    let private_key = resolve_signing_key(key_override)?;
    let public_key = derive_public_key_path(&private_key)?;
    let fingerprint = signing::get_key_fingerprint(&public_key)?;
    let public_key_line = signing::read_public_key(&public_key)?;

    println!("Signing key: {fingerprint}");
    println!("Public key:  {}", public_key.display());

    // ── Scan for unsigned entries ────────────────────────────────────
    let issues_dir = cache_dir.join("issues");
    let mut issues = read_all_issue_files(&issues_dir)?;

    // V1 inline comments
    let mut v1_unsigned_count = 0usize;
    let mut v1_issue_count = 0usize;
    for issue in &issues {
        let n = issue
            .comments
            .iter()
            .filter(|c| c.signed_by.is_none() || c.signature.is_none())
            .count();
        if n > 0 {
            v1_unsigned_count += n;
            v1_issue_count += 1;
        }
    }

    // V2 standalone comment files
    let mut v2_unsigned: Vec<(PathBuf, crate::issue_file::CommentFile)> = Vec::new();
    for entry in std::fs::read_dir(&issues_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            let comments_dir = path.join("comments");
            if comments_dir.exists() {
                for cf in read_comment_files(&comments_dir)? {
                    if cf.signed_by.is_none() || cf.signature.is_none() {
                        let cf_path = comments_dir.join(format!("{}.json", cf.uuid));
                        v2_unsigned.push((cf_path, cf));
                    }
                }
            }
        }
    }

    let total = v1_unsigned_count + v2_unsigned.len();
    if total == 0 {
        println!("No unsigned entries found. Nothing to do.");
        return Ok(());
    }

    println!();
    println!("Found {total} unsigned entry(ies):");
    if v1_unsigned_count > 0 {
        println!("  {v1_unsigned_count} inline comment(s) across {v1_issue_count} issue(s)");
    }
    if !v2_unsigned.is_empty() {
        println!("  {} standalone comment file(s)", v2_unsigned.len());
    }
    println!();
    println!("These will be signed with your key ({fingerprint}) as attestation");
    println!("that the missing signatures were a system error, not unapproved commits.");

    if !confirm {
        println!();
        println!("Dry run. Re-run with --confirm to apply signatures.");
        return Ok(());
    }

    // ── Sign V1 inline comments ─────────────────────────────────────
    let mut signed_count = 0usize;
    let mut modified_issue_paths: Vec<PathBuf> = Vec::new();

    for issue in &mut issues {
        let mut modified = false;
        for comment in &mut issue.comments {
            if comment.signed_by.is_some() && comment.signature.is_some() {
                continue;
            }
            let canonical = signing::canonicalize_for_signing(&[
                ("author", comment.author.as_str()),
                ("comment_id", &comment.id.to_string()),
                ("content", comment.content.as_str()),
            ]);
            let sig = signing::sign_content(&private_key, &canonical, BACKFILL_SIGNING_NAMESPACE)
                .with_context(|| {
                format!(
                    "Failed to sign comment {} in issue {}",
                    comment.id, issue.uuid
                )
            })?;
            comment.signed_by = Some(fingerprint.clone());
            comment.signature = Some(sig);
            signed_count += 1;
            modified = true;
        }
        if modified {
            // Determine write path: V2 directory takes precedence
            let v2_path = issues_dir.join(issue.uuid.to_string()).join("issue.json");
            let v1_path = issues_dir.join(format!("{}.json", issue.uuid));
            let write_path = if v2_path.exists() { v2_path } else { v1_path };
            write_issue_file(&write_path, issue)?;
            modified_issue_paths.push(write_path);
        }
    }

    // ── Sign V2 standalone comment files ─────────────────────────────
    let mut v2_signed_paths: Vec<PathBuf> = Vec::new();
    for (cf_path, mut cf) in v2_unsigned {
        // V2 comment files don't store a numeric id; use the uuid as the
        // comment_id field for canonical content (matches nothing in the
        // original signing flow, but creates a verifiable attestation).
        let canonical = signing::canonicalize_for_signing(&[
            ("author", cf.author.as_str()),
            ("comment_id", &cf.uuid.to_string()),
            ("content", cf.content.as_str()),
        ]);
        let sig = signing::sign_content(&private_key, &canonical, BACKFILL_SIGNING_NAMESPACE)
            .with_context(|| format!("Failed to sign comment file {}", cf.uuid))?;
        cf.signed_by = Some(fingerprint.clone());
        cf.signature = Some(sig);
        write_comment_file(&cf_path, &cf)?;
        v2_signed_paths.push(cf_path);
        signed_count += 1;
    }

    // ── Register human key in allowed_signers ────────────────────────
    let trust_dir = cache_dir.join("trust");
    let allowed_signers_path = trust_dir.join("allowed_signers");
    let mut signers = signing::AllowedSigners::load(&allowed_signers_path)?;
    let added = signers.add_entry(signing::AllowedSignerEntry {
        principal: BACKFILL_PRINCIPAL.to_string(),
        public_key: public_key_line,
        metadata_comment: Some(format!(
            "approved by human backfill at {}",
            chrono::Utc::now().format("%Y-%m-%d")
        )),
    });
    if added {
        signers.save(&allowed_signers_path)?;
        println!("Registered {fingerprint} as {BACKFILL_PRINCIPAL} in allowed_signers.");
    }

    // ── Commit and push to hub branch ────────────────────────────────
    // Stage all modified files
    let mut rel_paths: Vec<String> = Vec::new();
    for path in modified_issue_paths.iter().chain(v2_signed_paths.iter()) {
        if let Ok(rel) = path.strip_prefix(&cache_dir) {
            rel_paths.push(rel.to_string_lossy().to_string());
        }
    }
    if added {
        if let Ok(rel) = allowed_signers_path.strip_prefix(&cache_dir) {
            rel_paths.push(rel.to_string_lossy().to_string());
        }
    }

    if rel_paths.is_empty() {
        println!("No files to commit.");
        return Ok(());
    }

    // git add
    let mut add_args = vec!["add", "--"];
    let refs: Vec<&str> = rel_paths.iter().map(String::as_str).collect();
    add_args.extend_from_slice(&refs);

    std::process::Command::new("git")
        .current_dir(&cache_dir)
        .args(&add_args)
        .output()
        .context("Failed to git add in hub cache")?;

    // git commit (without gpg signing — this is the hub branch, human
    // attestation is in the entry signatures themselves)
    let commit_msg = format!(
        "integrity: backfill {signed_count} unsigned entry signature(s)\n\n\
         Attested by {fingerprint} ({BACKFILL_PRINCIPAL}).\n\
         These entries lacked signatures due to a system error,\n\
         not unapproved commits."
    );
    let commit_output = std::process::Command::new("git")
        .current_dir(&cache_dir)
        .args(["-c", "commit.gpgsign=false", "commit", "-m", &commit_msg])
        .output()
        .context("Failed to git commit in hub cache")?;

    if !commit_output.status.success() {
        let stderr = String::from_utf8_lossy(&commit_output.stderr);
        bail!("git commit failed: {stderr}");
    }

    // Try to push
    let sync = SyncManager::new(crosslink_dir)?;
    let remote = sync.remote();
    let push_output = std::process::Command::new("git")
        .current_dir(&cache_dir)
        .args(["push", remote, "HEAD:refs/heads/crosslink/hub"])
        .output()
        .context("Failed to push hub branch")?;

    if push_output.status.success() {
        println!("Signed {signed_count} entry(ies) and pushed to {remote}.");
    } else {
        let stderr = String::from_utf8_lossy(&push_output.stderr);
        println!("Signed {signed_count} entry(ies). Committed locally.");
        println!("Push failed (you may need to push manually): {stderr}");
    }

    Ok(())
}

/// Resolve the SSH private key to use for signing.
fn resolve_signing_key(key_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(key) = key_override {
        let path = PathBuf::from(key);
        if !path.exists() {
            bail!("Specified key not found: {}", path.display());
        }
        // If they passed a .pub file, derive the private key
        return Ok(strip_pub_extension(&path));
    }

    // Try git's configured signing key
    if let Some(path) = signing::find_git_signing_key() {
        return Ok(strip_pub_extension(&path));
    }

    bail!(
        "No signing key found. Configure one with:\n  \
         git config --global user.signingkey ~/.ssh/your_key\n\
         or pass --key <path>"
    );
}

/// If the path ends in `.pub`, strip it to get the private key path.
fn strip_pub_extension(path: &Path) -> PathBuf {
    path.to_string_lossy()
        .strip_suffix(".pub")
        .map_or_else(|| path.to_path_buf(), PathBuf::from)
}

/// Derive the public key path from a private key path.
fn derive_public_key_path(private_key: &Path) -> Result<PathBuf> {
    let pub_path = PathBuf::from(format!("{}.pub", private_key.display()));
    if pub_path.exists() {
        return Ok(pub_path);
    }
    // Maybe the private key path itself is actually the public key
    if private_key.exists() {
        let content = std::fs::read_to_string(private_key)?;
        if content.trim().starts_with("ssh-") || content.trim().starts_with("ecdsa-") {
            return Ok(private_key.to_path_buf());
        }
    }
    bail!(
        "Cannot find public key for {}. Expected {}.pub",
        private_key.display(),
        private_key.display()
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).unwrap();
        (db, dir)
    }

    #[test]
    fn test_check_schema_pass() {
        let (db, _dir) = test_db();
        let result = check_schema(&db, false).unwrap();
        assert_eq!(result.name, "schema");
        assert!(matches!(result.status, CheckStatus::Pass));
    }

    #[test]
    fn test_check_counters_skipped_no_cache() {
        let (db, dir) = test_db();
        let crosslink_dir = dir.path();
        let result = check_counters(crosslink_dir, &db, false).unwrap();
        assert_eq!(result.name, "counters");
        assert!(matches!(result.status, CheckStatus::Skipped(_)));
    }

    #[test]
    fn test_check_counters_pass() {
        let (db, dir) = test_db();
        let crosslink_dir = dir.path();

        // Create cache dir and counters file
        let meta_dir = crosslink_dir.join(HUB_CACHE_DIR).join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        let counters = Counters {
            next_display_id: 1,
            next_comment_id: 1,
            next_milestone_id: 1,
        };
        write_counters(&meta_dir.join("counters.json"), &counters).unwrap();

        let result = check_counters(crosslink_dir, &db, false).unwrap();
        assert!(matches!(result.status, CheckStatus::Pass));
    }

    #[test]
    fn test_check_counters_fail_and_repair() {
        let (db, dir) = test_db();
        let crosslink_dir = dir.path();

        // Create an issue so max_display_id = 1
        db.create_issue("Test issue", None, "medium").unwrap();

        // Set counters too low
        let meta_dir = crosslink_dir.join(HUB_CACHE_DIR).join("meta");
        std::fs::create_dir_all(&meta_dir).unwrap();
        let counters = Counters {
            next_display_id: 1, // should be 2
            next_comment_id: 1,
            next_milestone_id: 1,
        };
        write_counters(&meta_dir.join("counters.json"), &counters).unwrap();

        // Check without repair — should fail
        let result = check_counters(crosslink_dir, &db, false).unwrap();
        assert!(matches!(result.status, CheckStatus::Fail(_)));

        // Check with repair — should fix
        let result = check_counters(crosslink_dir, &db, true).unwrap();
        assert!(matches!(result.status, CheckStatus::Repaired(_)));

        // Verify counter is now correct
        let fixed = read_counters(&meta_dir.join("counters.json")).unwrap();
        assert_eq!(fixed.next_display_id, 2);
    }

    #[test]
    fn test_check_hydration_skipped_no_cache() {
        let (db, dir) = test_db();
        let crosslink_dir = dir.path();
        let result = check_hydration(crosslink_dir, &db, false).unwrap();
        assert_eq!(result.name, "hydration");
        assert!(matches!(result.status, CheckStatus::Skipped(_)));
    }

    #[test]
    fn test_check_locks_skipped_no_sync() {
        let dir = tempdir().unwrap();
        let result = check_locks(dir.path(), false).unwrap();
        assert_eq!(result.name, "locks");
        assert!(matches!(result.status, CheckStatus::Skipped(_)));
    }

    #[test]
    fn test_print_summary_formatting() {
        let results = vec![
            CheckResult {
                name: "schema".to_string(),
                status: CheckStatus::Pass,
            },
            CheckResult {
                name: "counters".to_string(),
                status: CheckStatus::Fail("bad".to_string()),
            },
            CheckResult {
                name: "locks".to_string(),
                status: CheckStatus::Skipped("no sync".to_string()),
            },
        ];
        // Just verify it doesn't panic
        print_summary(&results);
    }
}
