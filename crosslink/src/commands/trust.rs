use anyhow::{bail, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::identity::resolve_driver_fingerprint;
use crate::signing::{AllowedSignerEntry, AllowedSigners};
use crate::TrustCommands;

pub fn run(command: TrustCommands, crosslink_dir: &Path) -> Result<()> {
    match command {
        TrustCommands::Approve { agent_id } => approve(crosslink_dir, &agent_id),
        TrustCommands::Revoke { agent_id } => revoke(crosslink_dir, &agent_id),
        TrustCommands::List => list(crosslink_dir),
        TrustCommands::Pending => pending(crosslink_dir),
        TrustCommands::Check { agent_id } => check(crosslink_dir, &agent_id),
    }
}

/// Metadata for a trust approval decision, stored in `trust/approvals/<agent-id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustApproval {
    pub agent_id: String,
    pub principal: String,
    pub approved_by: Option<String>,
    pub approved_at: String,
}

/// Metadata for a trust revocation, stored in `trust/approvals/<agent-id>.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustRevocation {
    pub agent_id: String,
    pub principal: String,
    pub revoked_by: Option<String>,
    pub revoked_at: String,
}

/// `crosslink trust approve <agent-id>`
///
/// Reads the agent's public key from `trust/keys/<id>.pub` on the hub branch,
/// adds it to `trust/allowed_signers`, commits, and pushes.
pub fn approve(crosslink_dir: &Path, agent_id: &str) -> Result<()> {
    let sync = crate::sync::SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Sync cache not initialized. Run `crosslink sync` first.");
    }
    let cache = sync.cache_path();

    // Read the agent's published public key
    let pubkey_path = cache
        .join("trust")
        .join("keys")
        .join(format!("{agent_id}.pub"));
    if !pubkey_path.exists() {
        bail!(
            "No published key for agent '{agent_id}'. The agent must run `crosslink agent init` first."
        );
    }
    let public_key = crate::signing::read_public_key(&pubkey_path)?;

    // Load or create allowed_signers
    let signers_path = cache.join("trust").join("allowed_signers");
    let mut signers = AllowedSigners::load(&signers_path)?;

    let principal = format!("{agent_id}@crosslink");
    if signers.is_trusted(&principal) {
        println!("Agent '{agent_id}' is already approved.");
        return Ok(());
    }

    signers.add_entry(AllowedSignerEntry {
        principal: principal.clone(),
        public_key,
        metadata_comment: Some(format!(
            "approved by {} at {}",
            std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .unwrap_or_else(|_| "unknown".to_string()),
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC")
        )),
    });
    signers.save(&signers_path)?;

    // Record approval metadata with driver identity
    let driver_fp = resolve_driver_fingerprint(crosslink_dir);
    let approval = TrustApproval {
        agent_id: agent_id.to_string(),
        principal: principal.clone(),
        approved_by: driver_fp.clone(),
        approved_at: Utc::now().to_rfc3339(),
    };
    let approvals_dir = cache.join("trust").join("approvals");
    std::fs::create_dir_all(&approvals_dir)?;
    let approval_path = approvals_dir.join(format!("{agent_id}.json"));
    std::fs::write(&approval_path, serde_json::to_string_pretty(&approval)?)?;

    // Complete bootstrap if pending — the first approval establishes the
    // trust chain, enabling signing enforcement (#644).
    let bootstrap_completed =
        if let Some(state) = crate::sync::bootstrap::read_bootstrap_state(cache) {
            if state.status == "pending" {
                crate::sync::bootstrap::complete_bootstrap(cache)?;
                true
            } else {
                false
            }
        } else {
            false
        };

    // Commit and push
    commit_trust_change(
        cache,
        crosslink_dir,
        &format!("trust: approve agent '{agent_id}'"),
    )?;

    if let Some(fp) = driver_fp {
        println!("Approved agent '{agent_id}' (principal: {principal}, approved by: {fp})");
    } else {
        println!("Approved agent '{agent_id}' (principal: {principal})");
    }
    if bootstrap_completed {
        println!("Bootstrap complete — signing enforcement is now active.");
    }
    Ok(())
}

/// `crosslink trust revoke <agent-id>`
pub fn revoke(crosslink_dir: &Path, agent_id: &str) -> Result<()> {
    let sync = crate::sync::SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Sync cache not initialized. Run `crosslink sync` first.");
    }
    let cache = sync.cache_path();

    let signers_path = cache.join("trust").join("allowed_signers");
    let mut signers = AllowedSigners::load(&signers_path)?;

    let principal = format!("{agent_id}@crosslink");
    if !signers.remove_by_principal(&principal) {
        println!("Agent '{agent_id}' is not in the trust list.");
        return Ok(());
    }

    signers.save(&signers_path)?;

    // Record revocation metadata with driver identity
    let driver_fp = resolve_driver_fingerprint(crosslink_dir);
    let revocation = TrustRevocation {
        agent_id: agent_id.to_string(),
        principal: principal.clone(),
        revoked_by: driver_fp,
        revoked_at: Utc::now().to_rfc3339(),
    };
    let approvals_dir = cache.join("trust").join("approvals");
    std::fs::create_dir_all(&approvals_dir)?;
    let approval_path = approvals_dir.join(format!("{agent_id}.json"));
    std::fs::write(&approval_path, serde_json::to_string_pretty(&revocation)?)?;

    commit_trust_change(
        cache,
        crosslink_dir,
        &format!("trust: revoke agent '{agent_id}'"),
    )?;

    println!("Revoked trust for agent '{agent_id}'");
    Ok(())
}

/// `crosslink trust list`
pub fn list(crosslink_dir: &Path) -> Result<()> {
    let sync = crate::sync::SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Sync cache not initialized. Run `crosslink sync` first.");
    }
    let cache = sync.cache_path();

    let signers_path = cache.join("trust").join("allowed_signers");
    let signers = AllowedSigners::load(&signers_path)?;

    if signers.entries.is_empty() {
        println!("No trusted signers configured.");
        return Ok(());
    }

    println!("Trusted signers:");
    for entry in &signers.entries {
        // Extract key type from the public key line (e.g. "ssh-ed25519")
        let key_type = entry
            .public_key
            .split_whitespace()
            .next()
            .unwrap_or("unknown");
        println!("  {} ({})", entry.principal, key_type);
    }

    Ok(())
}

/// `crosslink trust pending`
///
/// Shows agent keys published to `trust/keys/` that aren't yet in `allowed_signers`.
pub fn pending(crosslink_dir: &Path) -> Result<()> {
    let sync = crate::sync::SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Sync cache not initialized. Run `crosslink sync` first.");
    }
    let cache = sync.cache_path();

    let keys_dir = cache.join("trust").join("keys");
    let signers_path = cache.join("trust").join("allowed_signers");
    let signers = AllowedSigners::load(&signers_path)?;

    if !keys_dir.exists() {
        println!("No pending keys.");
        return Ok(());
    }

    let mut found = false;
    for entry in std::fs::read_dir(&keys_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("pub") {
            continue;
        }
        let agent_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown");
        let principal = format!("{agent_id}@crosslink");

        if !signers.is_trusted(&principal) {
            if !found {
                println!("Pending keys (not yet approved):");
                found = true;
            }
            // Read fingerprint if possible
            let fp = crate::signing::get_key_fingerprint(&path)
                .unwrap_or_else(|_| "unknown".to_string());
            println!("  {agent_id} ({fp})");
        }
    }

    if !found {
        println!("No pending keys. All published keys are approved.");
    }

    Ok(())
}

/// `crosslink trust check <agent-id>`
pub fn check(crosslink_dir: &Path, agent_id: &str) -> Result<()> {
    let sync = crate::sync::SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        bail!("Sync cache not initialized. Run `crosslink sync` first.");
    }
    let cache = sync.cache_path();

    let principal = format!("{agent_id}@crosslink");
    let signers_path = cache.join("trust").join("allowed_signers");
    let signers = AllowedSigners::load(&signers_path)?;

    let has_published_key = cache
        .join("trust")
        .join("keys")
        .join(format!("{agent_id}.pub"))
        .exists();

    println!("Agent: {agent_id}");
    println!(
        "  Key published: {}",
        if has_published_key { "yes" } else { "no" }
    );
    println!(
        "  Approved: {}",
        if signers.is_trusted(&principal) {
            "yes"
        } else {
            "no"
        }
    );

    Ok(())
}

/// Stage trust files, commit, and push (best-effort).
fn commit_trust_change(cache_dir: &Path, crosslink_dir: &Path, message: &str) -> Result<()> {
    commit_trust_change_impl(cache_dir, crosslink_dir, message, false)
}

/// Stage trust files, commit without signing, and push (best-effort).
///
/// Used for key publishing during agent init bootstrap, where signing
/// is not yet configured and would cause a chicken-and-egg failure.
fn commit_trust_change_unsigned(
    cache_dir: &Path,
    crosslink_dir: &Path,
    message: &str,
) -> Result<()> {
    commit_trust_change_impl(cache_dir, crosslink_dir, message, true)
}

/// Shared implementation for trust change commits.
fn commit_trust_change_impl(
    cache_dir: &Path,
    crosslink_dir: &Path,
    message: &str,
    unsigned: bool,
) -> Result<()> {
    let git = |args: &[&str]| -> Result<()> {
        let output = std::process::Command::new("git")
            .current_dir(cache_dir)
            .args(args)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("nothing to commit") {
                anyhow::bail!("git {args:?} failed: {stderr}");
            }
        }
        Ok(())
    };

    git(&["add", "trust/"])?;
    // Stage bootstrap state if updated by this trust change (#644)
    if cache_dir.join("meta").join("bootstrap.json").exists() {
        let _ = git(&["add", "meta/bootstrap.json"]);
    }

    if unsigned {
        git(&["-c", "commit.gpgsign=false", "commit", "-m", message])?;
    } else {
        git(&["commit", "-m", message])?;
    }

    // INTENTIONAL: push is best-effort — trust changes will be pushed on next sync
    let remote = crate::sync::read_tracker_remote(crosslink_dir);
    let _ = std::process::Command::new("git")
        .current_dir(cache_dir)
        .args(["push", &remote, crate::sync::HUB_BRANCH])
        .output();

    Ok(())
}

/// Publish an agent's public key to `trust/keys/<id>.pub` on the hub branch.
///
/// Called during `agent init` after key generation. Uses an unsigned commit
/// to avoid the chicken-and-egg problem where signing must be configured
/// before the key can be published.
pub fn publish_agent_key(crosslink_dir: &Path, agent_id: &str, public_key: &str) -> Result<()> {
    let sync = crate::sync::SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        // Hub not set up yet — will be published on next `crosslink sync`
        // via ensure_agent_key_published()
        return Ok(());
    }
    let cache = sync.cache_path();

    let keys_dir = cache.join("trust").join("keys");
    std::fs::create_dir_all(&keys_dir)?;

    let path = keys_dir.join(format!("{agent_id}.pub"));
    std::fs::write(&path, format!("{public_key}\n"))?;

    // Use unsigned commit for key publishing — signing may not be
    // configured yet during agent init bootstrap.
    commit_trust_change_unsigned(
        cache,
        crosslink_dir,
        &format!("trust: publish key for agent '{agent_id}'"),
    )?;

    Ok(())
}
