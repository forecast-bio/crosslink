//! SSH signing and verification for crosslink.
//!
//! Provides Ed25519 key generation, commit signing configuration,
//! detached entry signing, and the `AllowedSigners` trust store.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Metadata for a generated SSH key pair.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SshKeyPair {
    /// Path to the private key file.
    pub private_key_path: PathBuf,
    /// Path to the public key file (.pub).
    pub public_key_path: PathBuf,
    /// SSH key fingerprint (e.g. "SHA256:...").
    pub fingerprint: String,
    /// Full public key line (e.g. "ssh-ed25519 AAAA... comment").
    pub public_key: String,
}

/// Result of signature verification on a commit.
///
/// Replaces the old `GpgVerification` enum with SSH-aware variants.
#[derive(Debug)]
pub enum SignatureVerification {
    /// Signature is valid and (optionally) the signer is identified.
    Valid {
        commit: String,
        fingerprint: Option<String>,
        principal: Option<String>,
    },
    /// Commit exists but is not signed.
    Unsigned { commit: String },
    /// Signature verification failed.
    Invalid { commit: String, reason: String },
    /// No commits exist on the branch yet.
    NoCommits,
}

// ── Key generation ──────────────────────────────────────────────────

/// Generate a new Ed25519 SSH key pair for an agent.
///
/// Keys are stored at `{keys_dir}/{agent_id}_ed25519` (.pub for public).
pub fn generate_agent_key(keys_dir: &Path, agent_id: &str, machine_id: &str) -> Result<SshKeyPair> {
    std::fs::create_dir_all(keys_dir)?;

    let private_path = keys_dir.join(format!("{}_ed25519", agent_id));
    let public_path = keys_dir.join(format!("{}_ed25519.pub", agent_id));

    if private_path.exists() {
        bail!(
            "SSH key already exists at {}. Use `crosslink agent rotate-key` to regenerate.",
            private_path.display()
        );
    }

    let comment = format!("crosslink-agent:{}@{}", agent_id, machine_id);
    let output = Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            &private_path.to_string_lossy(),
            "-N",
            "", // no passphrase
            "-C",
            &comment,
        ])
        .output()
        .context("Failed to run ssh-keygen. Is OpenSSH installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ssh-keygen failed: {}", stderr.trim());
    }

    // Enforce restrictive permissions on keys directory and private key
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(keys_dir, std::fs::Permissions::from_mode(0o700))
            .context("Failed to set permissions on keys directory")?;
        std::fs::set_permissions(&private_path, std::fs::Permissions::from_mode(0o600))
            .context("Failed to set permissions on private key")?;
    }

    let public_key = std::fs::read_to_string(&public_path)
        .context("Failed to read generated public key")?
        .trim()
        .to_string();

    let fingerprint = get_key_fingerprint(&public_path)?;

    Ok(SshKeyPair {
        private_key_path: private_path,
        public_key_path: public_path,
        fingerprint,
        public_key,
    })
}

/// Get the fingerprint of an SSH public key file (e.g. "SHA256:xxxx").
pub fn get_key_fingerprint(public_key_path: &Path) -> Result<String> {
    let output = Command::new("ssh-keygen")
        .args(["-l", "-f", &public_key_path.to_string_lossy()])
        .output()
        .context("Failed to run ssh-keygen -l")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ssh-keygen -l failed: {}", stderr.trim());
    }

    // Output format: "256 SHA256:xxxx comment (ED25519)"
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parts: Vec<&str> = stdout.split_whitespace().collect();
    if parts.len() >= 2 {
        Ok(parts[1].to_string())
    } else {
        bail!("Unexpected ssh-keygen -l output: {}", stdout.trim());
    }
}

// ── Key discovery ───────────────────────────────────────────────────

/// Well-known SSH key filenames to check, in priority order.
const SSH_KEY_NAMES: &[&str] = &["id_ed25519.pub", "id_ecdsa.pub", "id_rsa.pub"];

/// Find the user's default SSH public key, if one exists.
///
/// Checks `~/.ssh/` for common key names.
pub fn find_default_ssh_key() -> Option<PathBuf> {
    let home = dirs_next().or_else(home_dir_fallback)?;
    let ssh_dir = home.join(".ssh");

    for name in SSH_KEY_NAMES {
        let path = ssh_dir.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

/// Find git's configured signing key for the current user.
pub fn find_git_signing_key() -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["config", "--global", "user.signingkey"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let key_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if key_path.is_empty() {
        return None;
    }

    let path = PathBuf::from(&key_path);
    // If the path exists as-is, use it; otherwise check for .pub variant
    if path.exists() {
        return Some(path);
    }
    let pub_path = PathBuf::from(format!("{}.pub", key_path));
    if pub_path.exists() {
        return Some(pub_path);
    }
    None
}

/// Read a public key file and return the full key line.
pub fn read_public_key(path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read public key at {}", path.display()))?;
    let line = content.trim().to_string();
    if !line.starts_with("ssh-") && !line.starts_with("ecdsa-") {
        bail!(
            "File does not look like an SSH public key: {}",
            path.display()
        );
    }
    Ok(line)
}

// ── Git signing configuration ───────────────────────────────────────

/// Check whether `repo_dir` is a linked git worktree (not the main repo).
///
/// Compares `git rev-parse --git-dir` vs `--git-common-dir`. When they
/// differ, `--local` config writes leak into the shared `.git/config`.
pub fn is_linked_worktree(repo_dir: &Path) -> bool {
    let git_dir = Command::new("git")
        .current_dir(repo_dir)
        .args(["rev-parse", "--git-dir"])
        .output();
    let common_dir = Command::new("git")
        .current_dir(repo_dir)
        .args(["rev-parse", "--git-common-dir"])
        .output();

    let (Ok(gd), Ok(cd)) = (git_dir, common_dir) else {
        return false;
    };
    if !gd.status.success() || !cd.status.success() {
        return false;
    }

    let gd_raw = String::from_utf8_lossy(&gd.stdout).trim().to_string();
    let cd_raw = String::from_utf8_lossy(&cd.stdout).trim().to_string();

    let gd_path = if Path::new(&gd_raw).is_absolute() {
        PathBuf::from(&gd_raw)
    } else {
        repo_dir.join(&gd_raw)
    };
    let cd_path = if Path::new(&cd_raw).is_absolute() {
        PathBuf::from(&cd_raw)
    } else {
        repo_dir.join(&cd_raw)
    };

    let gd_canonical = gd_path.canonicalize().unwrap_or(gd_path);
    let cd_canonical = cd_path.canonicalize().unwrap_or(cd_path);

    gd_canonical != cd_canonical
}

/// Enable `extensions.worktreeConfig` in the shared git config.
///
/// Required before `git config --worktree` will work. Idempotent.
pub fn enable_worktree_config(repo_dir: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["config", "extensions.worktreeConfig", "true"])
        .output()
        .context("Failed to enable extensions.worktreeConfig")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to enable extensions.worktreeConfig: {}",
            stderr.trim()
        );
    }
    Ok(())
}

/// Remove agent signing keys that leaked into the shared `.git/config`.
///
/// Only unsets values whose path contains `.crosslink/keys/` (agent keys).
/// User-set keys (e.g. `~/.ssh/id_ecdsa_signing`) are left untouched.
fn cleanup_leaked_signing_config(repo_dir: &Path) -> Result<()> {
    // Read the current user.signingkey from --local (shared config)
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["config", "--local", "user.signingkey"])
        .output();

    let Ok(output) = output else {
        return Ok(());
    };
    if !output.status.success() {
        // No signing key in shared config — nothing to clean
        return Ok(());
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !value.contains(".crosslink/keys/") && !value.contains(".crosslink\\keys\\") {
        // Not an agent key — leave it alone
        return Ok(());
    }

    // Unset the leaked agent signing config from shared config
    for key in &[
        "user.signingkey",
        "gpg.format",
        "commit.gpgsign",
        "gpg.ssh.allowedSignersFile",
    ] {
        let _ = Command::new("git")
            .current_dir(repo_dir)
            .args(["config", "--local", "--unset", key])
            .output();
    }

    Ok(())
}

/// Configure git to use SSH signing in a repository.
///
/// Sets `gpg.format=ssh`, `user.signingkey`, and `commit.gpgsign=true`.
///
/// Automatically detects linked worktrees and uses `--worktree` scope
/// to avoid leaking agent signing config into the shared `.git/config`.
pub fn configure_git_ssh_signing(
    repo_dir: &Path,
    private_key_path: &Path,
    allowed_signers_path: Option<&Path>,
) -> Result<()> {
    let use_worktree = is_linked_worktree(repo_dir);

    if use_worktree {
        enable_worktree_config(repo_dir)?;
        cleanup_leaked_signing_config(repo_dir)?;
    }

    run_git_config(repo_dir, "gpg.format", "ssh", use_worktree)?;
    run_git_config(
        repo_dir,
        "user.signingkey",
        &private_key_path.to_string_lossy(),
        use_worktree,
    )?;
    run_git_config(repo_dir, "commit.gpgsign", "true", use_worktree)?;

    if let Some(signers) = allowed_signers_path {
        run_git_config(
            repo_dir,
            "gpg.ssh.allowedSignersFile",
            &signers.to_string_lossy(),
            use_worktree,
        )?;
    }

    Ok(())
}

fn run_git_config(repo_dir: &Path, key: &str, value: &str, worktree_scope: bool) -> Result<()> {
    let scope_flag = if worktree_scope {
        "--worktree"
    } else {
        "--local"
    };
    let output = Command::new("git")
        .current_dir(repo_dir)
        .args(["config", scope_flag, key, value])
        .output()
        .with_context(|| format!("Failed to set git config {}", key))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git config {} failed: {}", key, stderr.trim());
    }
    Ok(())
}

// ── Allowed signers ─────────────────────────────────────────────────

/// An entry in the `trust/allowed_signers` file (git's native format).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AllowedSignerEntry {
    /// Principal identifier (e.g. "agent-id@crosslink" or "driver@example.com").
    pub principal: String,
    /// Full public key line ("ssh-ed25519 AAAA... comment").
    pub public_key: String,
    /// Optional metadata comment rendered above the entry (e.g. "approved by max at 2026-02-28").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_comment: Option<String>,
}

/// Manages the `trust/allowed_signers` file.
#[derive(Debug, Clone, Default)]
pub struct AllowedSigners {
    pub entries: Vec<AllowedSignerEntry>,
}

impl AllowedSigners {
    /// Load from a file. Returns empty if the file doesn't exist.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        Ok(Self::parse(&content))
    }

    /// Known SSH public key type prefixes.
    const KNOWN_KEY_TYPES: &'static [&'static str] = &[
        "ssh-ed25519",
        "ssh-rsa",
        "ssh-dss",
        "ecdsa-sha2-",
        "sk-ssh-ed25519",
        "sk-ecdsa-sha2-",
    ];

    /// Parse the allowed_signers content.
    fn parse(content: &str) -> Self {
        let mut entries = Vec::new();
        // Track metadata comments (lines starting with "# approved" or "# revoked")
        // that immediately precede an entry
        let mut pending_metadata: Option<String> = None;

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                pending_metadata = None;
                continue;
            }
            if trimmed.starts_with('#') {
                // Check if this is a metadata comment (not the file header)
                let comment_text = trimmed.trim_start_matches('#').trim();
                if comment_text.starts_with("approved ") || comment_text.starts_with("revoked ") {
                    pending_metadata = Some(comment_text.to_string());
                }
                continue;
            }

            // Format: <principal> <key-type> <base64> [comment]
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            if parts.len() < 2 {
                eprintln!(
                    "warning: skipping malformed allowed_signers line (no space): {}",
                    line
                );
                pending_metadata = None;
                continue;
            }

            let principal = parts[0];
            let public_key = parts[1];

            // Validate principal: non-empty, no control characters
            if principal.is_empty() || principal.chars().any(|c| c.is_control()) {
                eprintln!(
                    "warning: skipping allowed_signers entry with invalid principal: {}",
                    principal
                );
                pending_metadata = None;
                continue;
            }

            // Validate public key starts with a known SSH key type
            if !Self::KNOWN_KEY_TYPES
                .iter()
                .any(|prefix| public_key.starts_with(prefix))
            {
                eprintln!(
                    "warning: skipping allowed_signers entry with unrecognized key type for principal '{}': {}",
                    principal,
                    public_key.split_whitespace().next().unwrap_or("<empty>")
                );
                pending_metadata = None;
                continue;
            }

            entries.push(AllowedSignerEntry {
                principal: principal.to_string(),
                public_key: public_key.to_string(),
                metadata_comment: pending_metadata.take(),
            });
        }
        Self { entries }
    }

    /// Save to a file in git's allowed_signers format.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = self.render();
        std::fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
    }

    /// Render as the file content string.
    fn render(&self) -> String {
        let mut lines = vec!["# Crosslink trusted signers".to_string()];
        lines.push("# Format: <principal> <key-type> <base64-key> [comment]".to_string());
        for entry in &self.entries {
            if let Some(ref comment) = entry.metadata_comment {
                lines.push(format!("# {}", comment));
            }
            lines.push(format!("{} {}", entry.principal, entry.public_key));
        }
        lines.push(String::new()); // trailing newline
        lines.join("\n")
    }

    /// Add an entry. Returns false if the principal already exists.
    pub fn add_entry(&mut self, entry: AllowedSignerEntry) -> bool {
        if self.entries.iter().any(|e| e.principal == entry.principal) {
            return false;
        }
        self.entries.push(entry);
        true
    }

    /// Remove an entry by principal. Returns true if removed.
    pub fn remove_by_principal(&mut self, principal: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.principal != principal);
        self.entries.len() < before
    }

    /// Check if a principal is trusted.
    pub fn is_trusted(&self, principal: &str) -> bool {
        self.entries.iter().any(|e| e.principal == principal)
    }
}

// ── SSH verify-commit output parsing ────────────────────────────────

/// Parse SSH signature info from `git verify-commit` stderr output.
///
/// When `gpg.format=ssh`, git outputs lines like:
/// `Good "git" signature for principal with ED25519 key SHA256:xxxx`
///
/// Returns `(principal, fingerprint)` if found.
pub fn parse_ssh_verify_output(output: &str) -> Option<(String, String)> {
    for line in output.lines() {
        if line.contains("Good") && line.contains("signature for") {
            if let Some(for_idx) = line.find("signature for ") {
                let after_for = &line[for_idx + "signature for ".len()..];
                if let Some(with_idx) = after_for.find(" with ") {
                    let principal = after_for[..with_idx].to_string();
                    if let Some(key_idx) = after_for.find("key ") {
                        let fingerprint = after_for[key_idx + "key ".len()..].trim().to_string();
                        return Some((principal, fingerprint));
                    }
                }
            }
        }
    }
    None
}

/// Parse GPG fingerprint from `git verify-commit --raw` output (legacy).
///
/// Looks for lines like: `[GNUPG:] VALIDSIG <fingerprint> ...`
pub fn parse_gpg_fingerprint(gpg_output: &str) -> Option<String> {
    for line in gpg_output.lines() {
        if line.contains("VALIDSIG") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                return Some(parts[2].to_string());
            }
        }
    }
    None
}

/// Try to parse verify-commit output, handling both SSH and GPG formats.
pub fn parse_verify_output(stderr: &str) -> Option<(Option<String>, String)> {
    // Try SSH format first
    if let Some((principal, fingerprint)) = parse_ssh_verify_output(stderr) {
        return Some((Some(principal), fingerprint));
    }
    // Fall back to GPG format
    if let Some(fp) = parse_gpg_fingerprint(stderr) {
        return Some((None, fp));
    }
    None
}

// ── Per-entry signing ────────────────────────────────────────────────

/// Canonicalize fields into a deterministic byte string for signing.
///
/// Fields are sorted by key, joined as `key=value\n`.
pub fn canonicalize_for_signing(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut sorted: Vec<(&str, &str)> = fields.to_vec();
    sorted.sort_by_key(|(k, _)| *k);
    let mut out = Vec::new();
    for (k, v) in sorted {
        out.extend_from_slice(k.as_bytes());
        out.push(b'=');
        out.extend_from_slice(v.as_bytes());
        out.push(b'\n');
    }
    out
}

/// Sign content using SSH private key (`ssh-keygen -Y sign`).
///
/// Returns the base64-encoded signature (the content between the PEM markers).
pub fn sign_content(private_key_path: &Path, content: &[u8], namespace: &str) -> Result<String> {
    let tmp = make_temp_dir("crosslink-sign")?;
    let content_path = tmp.join("content");
    let sig_path = tmp.join("content.sig");

    std::fs::write(&content_path, content)?;

    let output = Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            &private_key_path.to_string_lossy(),
            "-n",
            namespace,
        ])
        .arg(&content_path)
        .output()
        .context("Failed to run ssh-keygen -Y sign")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ssh-keygen sign failed: {}", stderr.trim());
    }

    // Read the signature file
    let sig_content =
        std::fs::read_to_string(&sig_path).context("Failed to read signature file")?;

    // Extract just the base64 content between the PEM markers
    let sig = sig_content
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");

    Ok(sig)
}

/// Verify content against an SSH signature using `ssh-keygen -Y verify`.
///
/// Returns `true` if the signature is valid and the principal is trusted.
pub fn verify_content(
    allowed_signers_path: &Path,
    principal: &str,
    namespace: &str,
    content: &[u8],
    signature_b64: &str,
) -> Result<bool> {
    let tmp = make_temp_dir("crosslink-verify")?;
    let content_path = tmp.join("content");
    let sig_path = tmp.join("content.sig");

    std::fs::write(&content_path, content)?;

    // Reconstruct PEM-wrapped signature
    let pem_sig = format!(
        "-----BEGIN SSH SIGNATURE-----\n{}\n-----END SSH SIGNATURE-----\n",
        signature_b64
    );
    std::fs::write(&sig_path, pem_sig)?;

    // ssh-keygen -Y verify reads the data to verify from stdin
    let mut child = Command::new("ssh-keygen")
        .args([
            "-Y",
            "verify",
            "-f",
            &allowed_signers_path.to_string_lossy(),
            "-I",
            principal,
            "-n",
            namespace,
            "-s",
        ])
        .arg(&sig_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to run ssh-keygen -Y verify")?;

    if let Some(ref mut stdin) = child.stdin {
        use std::io::Write;
        let _ = stdin.write_all(content);
    }
    // Drop stdin to close it so ssh-keygen can proceed
    drop(child.stdin.take());

    // Wait with timeout to prevent hanging on malformed input
    {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        let timeout = Duration::from_secs(30);
        loop {
            match child.try_wait()? {
                Some(_) => break,
                None => {
                    if start.elapsed() > timeout {
                        let _ = child.kill();
                        let _ = std::fs::remove_dir_all(&tmp);
                        bail!("ssh-keygen verification timed out after 30 seconds");
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    let output = child.wait_with_output()?;
    let _ = std::fs::remove_dir_all(&tmp);

    if !output.status.success() {
        return Ok(false);
    }

    // Parse stderr to confirm "Good signature" message from ssh-keygen
    // On success, ssh-keygen outputs: Good "namespace" signature for principal ...
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.contains("Good") {
        return Ok(false);
    }

    Ok(true)
}

// ── Platform helpers ────────────────────────────────────────────────

/// Create a temporary directory with a descriptive prefix.
fn make_temp_dir(prefix: &str) -> Result<PathBuf> {
    let id = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{}-{}-{}", prefix, id, ts));
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create temp dir {}", dir.display()))?;
    Ok(dir)
}

/// Get the user's home directory (cross-platform).
fn dirs_next() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

fn home_dir_fallback() -> Option<PathBuf> {
    // Last resort
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_ssh_verify_output_valid() {
        let output =
            r#"Good "git" signature for m1@crosslink with ED25519 key SHA256:AbCdEf123456"#;
        let result = parse_ssh_verify_output(output);
        assert_eq!(
            result,
            Some((
                "m1@crosslink".to_string(),
                "SHA256:AbCdEf123456".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_ssh_verify_output_multiline() {
        let output = "some preamble\nGood \"git\" signature for driver@example.com with ECDSA key SHA256:XyZ789\nmore stuff";
        let result = parse_ssh_verify_output(output);
        assert_eq!(
            result,
            Some((
                "driver@example.com".to_string(),
                "SHA256:XyZ789".to_string()
            ))
        );
    }

    #[test]
    fn test_parse_ssh_verify_output_no_match() {
        assert!(parse_ssh_verify_output("").is_none());
        assert!(parse_ssh_verify_output("Bad signature").is_none());
        assert!(parse_ssh_verify_output("Good but no signature for").is_none());
    }

    #[test]
    fn test_parse_gpg_fingerprint_valid() {
        let output = "[GNUPG:] VALIDSIG ABCDEF1234567890 2024-01-01 12345678\n[GNUPG:] GOODSIG";
        let fp = parse_gpg_fingerprint(output);
        assert_eq!(fp, Some("ABCDEF1234567890".to_string()));
    }

    #[test]
    fn test_parse_gpg_fingerprint_no_match() {
        assert!(parse_gpg_fingerprint("").is_none());
        assert!(parse_gpg_fingerprint("[GNUPG:] GOODSIG ABC123").is_none());
    }

    #[test]
    fn test_parse_verify_output_ssh_preferred() {
        let output = r#"Good "git" signature for agent@host with ED25519 key SHA256:Test123"#;
        let result = parse_verify_output(output);
        assert_eq!(
            result,
            Some((Some("agent@host".to_string()), "SHA256:Test123".to_string()))
        );
    }

    #[test]
    fn test_parse_verify_output_gpg_fallback() {
        let output = "[GNUPG:] VALIDSIG DEADBEEF 2024-01-01";
        let result = parse_verify_output(output);
        assert_eq!(result, Some((None, "DEADBEEF".to_string())));
    }

    #[test]
    fn test_allowed_signers_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("allowed_signers");

        let mut signers = AllowedSigners::default();
        signers.add_entry(AllowedSignerEntry {
            principal: "driver@example.com".to_string(),
            public_key: "ssh-ed25519 AAAA1234 driver-key".to_string(),
            metadata_comment: None,
        });
        signers.add_entry(AllowedSignerEntry {
            principal: "m1@crosslink".to_string(),
            public_key: "ssh-ed25519 BBBB5678 agent-m1".to_string(),
            metadata_comment: None,
        });

        signers.save(&path).unwrap();
        let loaded = AllowedSigners::load(&path).unwrap();

        assert_eq!(loaded.entries.len(), 2);
        assert_eq!(loaded.entries[0].principal, "driver@example.com");
        assert_eq!(
            loaded.entries[0].public_key,
            "ssh-ed25519 AAAA1234 driver-key"
        );
        assert_eq!(loaded.entries[1].principal, "m1@crosslink");
    }

    #[test]
    fn test_allowed_signers_add_duplicate() {
        let mut signers = AllowedSigners::default();
        assert!(signers.add_entry(AllowedSignerEntry {
            principal: "m1@crosslink".to_string(),
            public_key: "ssh-ed25519 AAAA".to_string(),
            metadata_comment: None,
        }));
        assert!(!signers.add_entry(AllowedSignerEntry {
            principal: "m1@crosslink".to_string(),
            public_key: "ssh-ed25519 BBBB".to_string(),
            metadata_comment: None,
        }));
        assert_eq!(signers.entries.len(), 1);
    }

    #[test]
    fn test_allowed_signers_remove() {
        let mut signers = AllowedSigners::default();
        signers.add_entry(AllowedSignerEntry {
            principal: "m1@crosslink".to_string(),
            public_key: "ssh-ed25519 AAAA".to_string(),
            metadata_comment: None,
        });
        assert!(signers.remove_by_principal("m1@crosslink"));
        assert!(!signers.remove_by_principal("m1@crosslink"));
        assert!(signers.entries.is_empty());
    }

    #[test]
    fn test_allowed_signers_is_trusted() {
        let mut signers = AllowedSigners::default();
        signers.add_entry(AllowedSignerEntry {
            principal: "m1@crosslink".to_string(),
            public_key: "ssh-ed25519 AAAA".to_string(),
            metadata_comment: None,
        });
        assert!(signers.is_trusted("m1@crosslink"));
        assert!(!signers.is_trusted("unknown@crosslink"));
    }

    #[test]
    fn test_allowed_signers_parse_comments_and_blanks() {
        let content = "# comment line\n\ndriver@example.com ssh-ed25519 AAAA key\n# another comment\nm1@crosslink ssh-ed25519 BBBB key2\n";
        let signers = AllowedSigners::parse(content);
        assert_eq!(signers.entries.len(), 2);
    }

    #[test]
    fn test_allowed_signers_metadata_comment_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("allowed_signers");

        let mut signers = AllowedSigners::default();
        signers.add_entry(AllowedSignerEntry {
            principal: "m1@crosslink".to_string(),
            public_key: "ssh-ed25519 AAAA".to_string(),
            metadata_comment: Some("approved by max at 2026-02-28 12:00:00 UTC".to_string()),
        });
        signers.save(&path).unwrap();

        let loaded = AllowedSigners::load(&path).unwrap();
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(
            loaded.entries[0].metadata_comment.as_deref(),
            Some("approved by max at 2026-02-28 12:00:00 UTC")
        );
    }

    #[test]
    fn test_allowed_signers_rejects_invalid_key_type() {
        let content = "agent@crosslink not-an-ssh-key AAAA\n";
        let signers = AllowedSigners::parse(content);
        assert!(signers.entries.is_empty());
    }

    #[test]
    fn test_allowed_signers_rejects_control_chars_in_principal() {
        let content = "agent\x00bad@crosslink ssh-ed25519 AAAA\n";
        let signers = AllowedSigners::parse(content);
        assert!(signers.entries.is_empty());
    }

    #[test]
    fn test_allowed_signers_accepts_valid_key_types() {
        let content = "a@crosslink ssh-ed25519 AAAA\nb@crosslink ssh-rsa BBBB\nc@crosslink ecdsa-sha2-nistp256 CCCC\n";
        let signers = AllowedSigners::parse(content);
        assert_eq!(signers.entries.len(), 3);
    }

    #[test]
    fn test_allowed_signers_load_missing_file() {
        let dir = tempdir().unwrap();
        let signers = AllowedSigners::load(&dir.path().join("nonexistent")).unwrap();
        assert!(signers.entries.is_empty());
    }

    #[test]
    fn test_read_public_key_rejects_non_ssh() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.pub");
        std::fs::write(&path, "not an ssh key").unwrap();
        assert!(read_public_key(&path).is_err());
    }

    // Integration tests requiring ssh-keygen on PATH
    #[test]
    fn test_generate_agent_key() {
        if Command::new("ssh-keygen").arg("--help").output().is_err() {
            eprintln!("Skipping: ssh-keygen not available");
            return;
        }

        let dir = tempdir().unwrap();
        let keys_dir = dir.path().join("keys");

        let keypair = generate_agent_key(&keys_dir, "test-agent", "test-host").unwrap();

        assert!(keypair.private_key_path.exists());
        assert!(keypair.public_key_path.exists());
        assert!(keypair.fingerprint.starts_with("SHA256:"));
        assert!(keypair.public_key.starts_with("ssh-ed25519"));
        assert!(keypair
            .public_key
            .contains("crosslink-agent:test-agent@test-host"));
    }

    #[test]
    fn test_generate_agent_key_rejects_existing() {
        if Command::new("ssh-keygen").arg("--help").output().is_err() {
            return;
        }

        let dir = tempdir().unwrap();
        let keys_dir = dir.path().join("keys");

        generate_agent_key(&keys_dir, "test-agent", "host").unwrap();
        let result = generate_agent_key(&keys_dir, "test-agent", "host");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn test_get_key_fingerprint() {
        if Command::new("ssh-keygen").arg("--help").output().is_err() {
            return;
        }

        let dir = tempdir().unwrap();
        let keys_dir = dir.path().join("keys");
        let keypair = generate_agent_key(&keys_dir, "fp-test", "host").unwrap();

        let fp = get_key_fingerprint(&keypair.public_key_path).unwrap();
        assert!(fp.starts_with("SHA256:"));
        assert_eq!(fp, keypair.fingerprint);
    }

    #[test]
    fn test_configure_git_ssh_signing() {
        let dir = tempdir().unwrap();
        let repo = dir.path();

        // Init a git repo
        Command::new("git")
            .current_dir(repo)
            .args(["init", "-q"])
            .output()
            .unwrap();

        let key_path = repo.join("fake-key");
        std::fs::write(&key_path, "fake").unwrap();

        configure_git_ssh_signing(repo, &key_path, None).unwrap();

        // Verify the config was set
        let output = Command::new("git")
            .current_dir(repo)
            .args(["config", "--local", "gpg.format"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ssh");

        let output = Command::new("git")
            .current_dir(repo)
            .args(["config", "--local", "commit.gpgsign"])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "true");
    }

    /// Helper to init a git repo with a dummy commit (needed for worktree creation).
    fn init_git_repo_with_commit(path: &Path) {
        Command::new("git")
            .current_dir(path)
            .args(["init", "-q"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(path)
            .args(["config", "user.email", "test@test.com"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(path)
            .args(["config", "user.name", "Test"])
            .output()
            .unwrap();
        Command::new("git")
            .current_dir(path)
            .args(["commit", "--allow-empty", "-m", "init"])
            .output()
            .unwrap();
    }

    #[test]
    fn test_configure_git_ssh_signing_in_linked_worktree() {
        let dir = tempdir().unwrap();
        let main_root = dir.path().join("main");
        std::fs::create_dir_all(&main_root).unwrap();
        init_git_repo_with_commit(&main_root);

        // Set a user signing key in the main repo's shared config
        Command::new("git")
            .current_dir(&main_root)
            .args([
                "config",
                "--local",
                "user.signingkey",
                "~/.ssh/id_ecdsa_signing",
            ])
            .output()
            .unwrap();

        // Create a branch and linked worktree
        Command::new("git")
            .current_dir(&main_root)
            .args(["branch", "wt-test"])
            .output()
            .unwrap();
        let wt_path = dir.path().join("worktree");
        Command::new("git")
            .current_dir(&main_root)
            .args(["worktree", "add", &wt_path.to_string_lossy(), "wt-test"])
            .output()
            .unwrap();

        // Configure signing in the linked worktree with a fake agent key path
        let agent_key = wt_path.join(".crosslink/keys/agent_ed25519");
        std::fs::create_dir_all(agent_key.parent().unwrap()).unwrap();
        std::fs::write(&agent_key, "fake-agent-key").unwrap();

        configure_git_ssh_signing(&wt_path, &agent_key, None).unwrap();

        // Verify: agent key is in the worktree-scoped config
        let output = Command::new("git")
            .current_dir(&wt_path)
            .args(["config", "--worktree", "user.signingkey"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .contains(".crosslink/keys/"),
            "agent key should be in worktree config"
        );

        // Verify: user's signing key is preserved in shared config
        let output = Command::new("git")
            .current_dir(&main_root)
            .args(["config", "--local", "user.signingkey"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "~/.ssh/id_ecdsa_signing",
            "user's signing key must not be overwritten in shared config"
        );

        // Verify: extensions.worktreeConfig was enabled
        let output = Command::new("git")
            .current_dir(&main_root)
            .args(["config", "extensions.worktreeConfig"])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "true",
            "extensions.worktreeConfig should be enabled"
        );
    }

    #[test]
    fn test_configure_git_ssh_signing_standalone_still_uses_local() {
        let dir = tempdir().unwrap();
        let repo = dir.path();

        Command::new("git")
            .current_dir(repo)
            .args(["init", "-q"])
            .output()
            .unwrap();

        let key_path = repo.join("fake-key");
        std::fs::write(&key_path, "fake").unwrap();

        configure_git_ssh_signing(repo, &key_path, None).unwrap();

        // Verify config is in --local scope
        let output = Command::new("git")
            .current_dir(repo)
            .args(["config", "--local", "user.signingkey"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "signing key should be in local config for standalone repos"
        );

        // Verify extensions.worktreeConfig is NOT set
        let output = Command::new("git")
            .current_dir(repo)
            .args(["config", "extensions.worktreeConfig"])
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "extensions.worktreeConfig should not be set for standalone repos"
        );
    }
}
