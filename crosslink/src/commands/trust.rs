use anyhow::{bail, Result};
use std::path::Path;

use crate::signing::{AllowedSignerEntry, AllowedSigners};

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
        .join(format!("{}.pub", agent_id));
    if !pubkey_path.exists() {
        bail!(
            "No published key for agent '{}'. The agent must run `crosslink agent init` first.",
            agent_id
        );
    }
    let public_key = crate::signing::read_public_key(&pubkey_path)?;

    // Load or create allowed_signers
    let signers_path = cache.join("trust").join("allowed_signers");
    let mut signers = AllowedSigners::load(&signers_path)?;

    let principal = format!("{}@crosslink", agent_id);
    if signers.is_trusted(&principal) {
        println!("Agent '{}' is already approved.", agent_id);
        return Ok(());
    }

    signers.add_entry(AllowedSignerEntry {
        principal: principal.clone(),
        public_key,
    });
    signers.save(&signers_path)?;

    // Commit and push
    commit_trust_change(cache, &format!("trust: approve agent '{}'", agent_id))?;

    println!("Approved agent '{}' (principal: {})", agent_id, principal);
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

    let principal = format!("{}@crosslink", agent_id);
    if !signers.remove_by_principal(&principal) {
        println!("Agent '{}' is not in the trust list.", agent_id);
        return Ok(());
    }

    signers.save(&signers_path)?;

    commit_trust_change(cache, &format!("trust: revoke agent '{}'", agent_id))?;

    println!("Revoked trust for agent '{}'", agent_id);
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
        let principal = format!("{}@crosslink", agent_id);

        if !signers.is_trusted(&principal) {
            if !found {
                println!("Pending keys (not yet approved):");
                found = true;
            }
            // Read fingerprint if possible
            let fp = crate::signing::get_key_fingerprint(&path)
                .unwrap_or_else(|_| "unknown".to_string());
            println!("  {} ({})", agent_id, fp);
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

    let principal = format!("{}@crosslink", agent_id);
    let signers_path = cache.join("trust").join("allowed_signers");
    let signers = AllowedSigners::load(&signers_path)?;

    let has_published_key = cache
        .join("trust")
        .join("keys")
        .join(format!("{}.pub", agent_id))
        .exists();

    println!("Agent: {}", agent_id);
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
fn commit_trust_change(cache_dir: &Path, message: &str) -> Result<()> {
    let git = |args: &[&str]| -> Result<()> {
        let output = std::process::Command::new("git")
            .current_dir(cache_dir)
            .args(args)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("nothing to commit") {
                anyhow::bail!("git {:?} failed: {}", args, stderr);
            }
        }
        Ok(())
    };

    git(&["add", "trust/"])?;
    git(&["commit", "-m", message])?;

    // Best-effort push
    let _ = std::process::Command::new("git")
        .current_dir(cache_dir)
        .args(["push", "origin", crate::sync::HUB_BRANCH])
        .output();

    Ok(())
}

/// Publish an agent's public key to `trust/keys/<id>.pub` on the hub branch.
///
/// Called during `agent init` after key generation.
pub fn publish_agent_key(crosslink_dir: &Path, agent_id: &str, public_key: &str) -> Result<()> {
    let sync = crate::sync::SyncManager::new(crosslink_dir)?;
    if !sync.is_initialized() {
        // Hub not set up yet — skip publishing, will happen on next sync
        return Ok(());
    }
    let cache = sync.cache_path();

    let keys_dir = cache.join("trust").join("keys");
    std::fs::create_dir_all(&keys_dir)?;

    let path = keys_dir.join(format!("{}.pub", agent_id));
    std::fs::write(&path, format!("{}\n", public_key))?;

    commit_trust_change(
        cache,
        &format!("trust: publish key for agent '{}'", agent_id),
    )?;

    Ok(())
}
