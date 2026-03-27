//! SSH driver signing key setup for `crosslink init`.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

use super::InitUI;

/// Detect or configure the driver's SSH signing key.
///
/// If `signing_key` is provided, uses that path. Otherwise checks for an
/// existing git signing key, then falls back to common SSH key locations.
/// Stores the driver's public key at `.crosslink/driver-key.pub`.
pub(super) fn setup_driver_signing(
    project_root: &Path,
    signing_key: Option<&str>,
    ui: &InitUI,
) -> Result<()> {
    use crate::signing;

    let crosslink_dir = project_root.join(".crosslink");
    let driver_pub_path = crosslink_dir.join("driver-key.pub");

    // If driver key already configured and not forcing, skip
    if driver_pub_path.exists() {
        ui.step_start("Configuring signing");
        ui.step_ok(Some("already configured"));
        return Ok(());
    }

    // Find the key to use
    let pubkey_path = if let Some(key_path) = signing_key {
        let p = std::path::PathBuf::from(key_path);
        if !p.exists() {
            ui.warn(&format!("Signing key not found at {}", key_path));
            return Ok(());
        }
        Some(p)
    } else {
        signing::find_git_signing_key().or_else(signing::find_default_ssh_key)
    };

    let pubkey_path = match pubkey_path {
        Some(p) => p,
        None => {
            ui.step_skip("Signing: no SSH key found");
            ui.detail("Generate one with: ssh-keygen -t ed25519");
            ui.detail("Then re-run: crosslink init --force");
            return Ok(());
        }
    };

    // Ensure it's a public key (not private)
    let pubkey_path = if !pubkey_path.to_string_lossy().ends_with(".pub") {
        let pub_variant = std::path::PathBuf::from(format!("{}.pub", pubkey_path.display()));
        if pub_variant.exists() {
            pub_variant
        } else {
            pubkey_path
        }
    } else {
        pubkey_path
    };

    ui.step_start("Configuring signing");
    match signing::read_public_key(&pubkey_path) {
        Ok(public_key) => {
            fs::write(&driver_pub_path, &public_key).context("Failed to write driver-key.pub")?;

            match signing::get_key_fingerprint(&pubkey_path) {
                Ok(fp) => ui.step_ok(Some(&fp)),
                Err(_) => ui.step_ok(Some(&pubkey_path.display().to_string())),
            }

            // NOTE: We intentionally do NOT call configure_git_ssh_signing()
            // on the project worktree here. Crosslink should not override the
            // user's git signing configuration. The hub cache worktree (used for
            // lock claims, issue entries, etc.) has its own signing config set
            // up separately in sync.rs.
        }
        Err(_) => {
            // Finish the step_start line, then show warning below
            println!();
            ui.warn(&format!(
                "{} does not appear to be an SSH public key",
                pubkey_path.display()
            ));
        }
    }

    Ok(())
}
