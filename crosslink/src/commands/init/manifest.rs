//! Init manifest — tracks files written by `crosslink init` for safe `--update` upgrades.
//!
//! Every `crosslink init` (initial or `--force`) writes `.crosslink/init-manifest.json`
//! recording the SHA-256 of each managed file it produced. The `--update` flag uses this
//! manifest to perform a three-way comparison (manifest vs on-disk vs new template) and
//! decide whether each file can be safely auto-updated, is in conflict, or is already
//! up-to-date.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const MANIFEST_FILENAME: &str = "init-manifest.json";

/// On-disk representation of the init manifest.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub(super) struct InitManifest {
    pub crosslink_version: String,
    pub initialized_at: String,
    pub files: BTreeMap<String, ManifestEntry>,
}

/// A single file entry in the manifest.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub(super) struct ManifestEntry {
    pub sha256: String,
    pub written_by_version: String,
}

/// The result of comparing a managed file during `--update`.
#[derive(Debug, PartialEq)]
#[allow(dead_code)] // NewFile is set directly in run_update, not returned by classify_update
pub(super) enum UpdateAction {
    /// Both template and on-disk file are unchanged since last init.
    UpToDate,
    /// User never touched the file, template changed — safe to auto-update.
    AutoUpdate,
    /// Template unchanged since last init, user may have modified — nothing to do.
    TemplateUnchanged,
    /// Both user and template modified the file since last init.
    Conflict,
    /// File was deleted by the user since last init.
    Deleted,
    /// File exists in the new template set but not in the manifest (newly added file).
    NewFile,
}

// ── Hashing ─────────────────────────────────────────────────────────────────

/// Compute the SHA-256 hex digest of the given content.
pub(super) fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Compute the SHA-256 hex digest of a file on disk. Returns `None` if the file
/// doesn't exist.
pub(super) fn sha256_file(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(Some(sha256_hex(&content))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow::Error::from(e)
            .context(format!("Failed to read {} for hashing", path.display()))),
    }
}

// ── Manifest I/O ────────────────────────────────────────────────────────────

/// Read the init manifest from `.crosslink/init-manifest.json`.
///
/// Returns `None` if the file doesn't exist or contains invalid JSON
/// (the issue spec says: treat missing/corrupt manifest like "all files
/// potentially user-modified").
pub(super) fn read_manifest(crosslink_dir: &Path) -> Option<InitManifest> {
    let path = crosslink_dir.join(MANIFEST_FILENAME);
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Write the init manifest atomically (write to `.tmp`, then rename).
pub(super) fn write_manifest(crosslink_dir: &Path, manifest: &InitManifest) -> Result<()> {
    let path = crosslink_dir.join(MANIFEST_FILENAME);
    let tmp_path = crosslink_dir.join(format!("{MANIFEST_FILENAME}.tmp"));

    let mut output =
        serde_json::to_string_pretty(manifest).context("Failed to serialize init-manifest.json")?;
    output.push('\n');

    fs::write(&tmp_path, &output).context("Failed to write init-manifest.json.tmp")?;
    fs::rename(&tmp_path, &path)
        .context("Failed to rename init-manifest.json.tmp → init-manifest.json")?;
    Ok(())
}

// ── Manifest construction ───────────────────────────────────────────────────

/// Build a manifest from a list of `(relative_path, template_content)` pairs.
///
/// The SHA-256 is computed from `template_content` — for `settings.json` this
/// is the template after `__PYTHON_PREFIX__` substitution but *before* the
/// `allowedTools` merge, so user tool additions don't cause false "modified"
/// signals.
pub(super) fn build_manifest(files: &[(String, String)]) -> InitManifest {
    let version = env!("CARGO_PKG_VERSION").to_string();
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let mut entries = BTreeMap::new();
    for (path, content) in files {
        entries.insert(
            path.clone(),
            ManifestEntry {
                sha256: sha256_hex(content),
                written_by_version: version.clone(),
            },
        );
    }

    InitManifest {
        crosslink_version: version,
        initialized_at: now,
        files: entries,
    }
}

// ── Three-way classification ────────────────────────────────────────────────

/// Determine the update action for a single managed file using the three-way
/// comparison table from the design issue.
///
/// - `manifest_hash`: SHA-256 recorded in the manifest at last init
/// - `current_hash`:  SHA-256 of the on-disk file (`None` if file deleted)
/// - `new_template_hash`: SHA-256 of the current embedded template
pub(super) fn classify_update(
    manifest_hash: &str,
    current_hash: Option<&str>,
    new_template_hash: &str,
) -> UpdateAction {
    current_hash.map_or(UpdateAction::Deleted, |current| {
        let user_changed = manifest_hash != current;
        let template_changed = manifest_hash != new_template_hash;

        match (user_changed, template_changed) {
            (false, false) => UpdateAction::UpToDate,
            (false, true) => UpdateAction::AutoUpdate,
            (true, false) => UpdateAction::TemplateUnchanged,
            (true, true) => UpdateAction::Conflict,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_sha256_hex_deterministic() {
        let hash1 = sha256_hex("hello world");
        let hash2 = sha256_hex("hello world");
        assert_eq!(hash1, hash2);
        assert_eq!(hash1.len(), 64); // SHA-256 = 64 hex chars
    }

    #[test]
    fn test_sha256_hex_different_inputs() {
        let hash1 = sha256_hex("hello");
        let hash2 = sha256_hex("world");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_sha256_file_exists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello").unwrap();

        let hash = sha256_file(&path).unwrap();
        assert_eq!(hash, Some(sha256_hex("hello")));
    }

    #[test]
    fn test_sha256_file_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.txt");
        assert_eq!(sha256_file(&path).unwrap(), None);
    }

    #[test]
    fn test_manifest_roundtrip() {
        let dir = tempdir().unwrap();
        let files = vec![
            ("a.py".to_string(), "content a".to_string()),
            ("b.py".to_string(), "content b".to_string()),
        ];
        let manifest = build_manifest(&files);

        write_manifest(dir.path(), &manifest).unwrap();
        let loaded = read_manifest(dir.path()).unwrap();

        assert_eq!(loaded.crosslink_version, manifest.crosslink_version);
        assert_eq!(loaded.files.len(), 2);
        assert_eq!(loaded.files["a.py"].sha256, manifest.files["a.py"].sha256);
    }

    #[test]
    fn test_manifest_missing_returns_none() {
        let dir = tempdir().unwrap();
        assert!(read_manifest(dir.path()).is_none());
    }

    #[test]
    fn test_manifest_corrupt_returns_none() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(MANIFEST_FILENAME), "not json {{{").unwrap();
        assert!(read_manifest(dir.path()).is_none());
    }

    #[test]
    fn test_manifest_atomic_write() {
        let dir = tempdir().unwrap();
        let files = vec![("x.py".to_string(), "content".to_string())];
        let manifest = build_manifest(&files);

        write_manifest(dir.path(), &manifest).unwrap();

        // .tmp file should not linger
        assert!(!dir.path().join(format!("{MANIFEST_FILENAME}.tmp")).exists());
        // Final file should exist
        assert!(dir.path().join(MANIFEST_FILENAME).exists());
    }

    // ── classify_update tests ───────────────────────────────────────────

    #[test]
    fn test_classify_up_to_date() {
        assert_eq!(
            classify_update("abc", Some("abc"), "abc"),
            UpdateAction::UpToDate
        );
    }

    #[test]
    fn test_classify_auto_update() {
        // User never touched (manifest == current), template changed
        assert_eq!(
            classify_update("abc", Some("abc"), "def"),
            UpdateAction::AutoUpdate
        );
    }

    #[test]
    fn test_classify_template_unchanged() {
        // User changed (manifest != current), template same
        assert_eq!(
            classify_update("abc", Some("xyz"), "abc"),
            UpdateAction::TemplateUnchanged
        );
    }

    #[test]
    fn test_classify_conflict() {
        // Both changed
        assert_eq!(
            classify_update("abc", Some("xyz"), "def"),
            UpdateAction::Conflict
        );
    }

    #[test]
    fn test_classify_deleted() {
        assert_eq!(classify_update("abc", None, "def"), UpdateAction::Deleted);
    }
}
