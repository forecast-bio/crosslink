//! Point-in-time `SQLite` snapshots for safety nets around destructive
//! operations (e.g. `integrity hydration --repair`, see #602).
//!
//! Uses `SQLite`'s native `VACUUM INTO` (`SQLite` 3.27+) so the snapshot is
//! self-contained — it bundles the main database, any WAL pages, and
//! settles all in-flight checkpoints into a single file the user can
//! drop in place to recover state.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;

use super::Database;

/// Directory (under `.crosslink/`) where snapshots are written.
pub const SNAPSHOT_DIR: &str = "integrity";

/// Filename prefix for hydration-repair backups.
pub const HYDRATION_BACKUP_PREFIX: &str = "hydration-backup-";

/// Write a point-in-time snapshot of `db` to
/// `<crosslink_dir>/integrity/<prefix><utc-ts>.sqlite`, returning the
/// absolute path of the snapshot file.
///
/// The destination directory is created if it does not already exist.
/// Uses `VACUUM INTO` so the resulting file is self-contained regardless
/// of journal mode (WAL/DELETE/etc.).
///
/// # Errors
///
/// Returns an error if the destination directory cannot be created or if
/// the `VACUUM INTO` statement fails (e.g. destination already exists,
/// disk full, locked source database).
pub fn snapshot_to_integrity_dir(
    db: &Database,
    crosslink_dir: &Path,
    prefix: &str,
) -> Result<PathBuf> {
    let snapshot_dir = crosslink_dir.join(SNAPSHOT_DIR);
    std::fs::create_dir_all(&snapshot_dir).with_context(|| {
        format!(
            "Failed to create snapshot directory {}",
            snapshot_dir.display()
        )
    })?;

    // UTC ISO 8601 with colons replaced for filename safety on Windows.
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let filename = format!("{prefix}{ts}.sqlite");
    let dest_path = snapshot_dir.join(filename);

    // `VACUUM INTO` requires a string literal in its grammar — there is
    // no parameter binding for VACUUM. The destination path is generated
    // by this function (UTC timestamp under crosslink_dir), so it cannot
    // contain hostile input, but SQL-escape single quotes anyway as a
    // belt-and-suspenders safety measure.
    let escaped = dest_path.to_string_lossy().replace('\'', "''");
    db.conn
        .execute(&format!("VACUUM INTO '{escaped}'"), [])
        .with_context(|| format!("Failed to write SQLite snapshot to {}", dest_path.display()))?;

    Ok(dest_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_snapshot_creates_file() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("source.db")).unwrap();
        db.create_issue("test", None, "medium").unwrap();

        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let snap_path =
            snapshot_to_integrity_dir(&db, &crosslink_dir, HYDRATION_BACKUP_PREFIX).unwrap();

        assert!(snap_path.exists(), "snapshot file must be created");
        assert!(snap_path.starts_with(crosslink_dir.join(SNAPSHOT_DIR)));
        assert!(snap_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(HYDRATION_BACKUP_PREFIX));

        // Snapshot should be a valid SQLite db with the same data.
        let restored = Database::open(&snap_path).unwrap();
        let issues = restored.list_issues(None, None, None).unwrap();
        assert_eq!(issues.len(), 1, "snapshot must contain the source row");
    }

    #[test]
    fn test_snapshot_creates_integrity_dir_if_missing() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("source.db")).unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        // Deliberately do NOT create crosslink_dir/integrity.

        let snap_path =
            snapshot_to_integrity_dir(&db, &crosslink_dir, HYDRATION_BACKUP_PREFIX).unwrap();

        assert!(snap_path.parent().unwrap().exists());
    }

    #[test]
    fn test_snapshot_filename_has_utc_timestamp() {
        let dir = tempdir().unwrap();
        let db = Database::open(&dir.path().join("source.db")).unwrap();
        let crosslink_dir = dir.path().join(".crosslink");

        let snap_path =
            snapshot_to_integrity_dir(&db, &crosslink_dir, HYDRATION_BACKUP_PREFIX).unwrap();
        let name = snap_path.file_name().unwrap().to_string_lossy();
        // Format: hydration-backup-YYYYMMDDTHHMMSSZ.sqlite (16 chars between
        // prefix and .sqlite extension).
        let stripped = name
            .strip_prefix(HYDRATION_BACKUP_PREFIX)
            .and_then(|s| s.strip_suffix(".sqlite"))
            .expect("filename must follow <prefix><ts>.sqlite shape");
        assert_eq!(
            stripped.len(),
            16,
            "timestamp section must be exactly 16 chars (YYYYMMDDTHHMMSSZ), got: {stripped}"
        );
        assert!(stripped.ends_with('Z'), "timestamp must be UTC (ends in Z)");
    }
}
