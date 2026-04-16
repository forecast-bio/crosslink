use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::StyleCommands;

pub fn run(command: StyleCommands, crosslink_dir: &Path) -> Result<()> {
    match command {
        StyleCommands::Set { url, ref_name } => set(crosslink_dir, &url, ref_name.as_deref()),
        StyleCommands::Sync { dry_run } => sync(crosslink_dir, dry_run),
        StyleCommands::Diff => diff(crosslink_dir),
        StyleCommands::Show => show(crosslink_dir),
        StyleCommands::Unset => unset(crosslink_dir),
    }
}

/// The marker comment that acknowledges intentional customization.
const CUSTOM_MARKER: &str = "# crosslink:custom";

/// House style configuration stored in the `house_style` field of `hook-config.json`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HouseStyleConfig {
    pub url: String,
    #[serde(rename = "ref", default = "default_ref")]
    pub ref_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_synced: Option<String>,
    #[serde(default = "default_components")]
    pub components: Vec<String>,
}

fn default_ref() -> String {
    "main".to_string()
}

fn default_components() -> Vec<String> {
    vec![
        "rules".into(),
        "hooks".into(),
        "commands".into(),
        "config".into(),
    ]
}

/// Component directory mappings: (component name, source subdir in cache, target relative to project root)
const COMPONENT_DIRS: &[(&str, &str, &str)] = &[
    ("rules", "rules", ".crosslink/rules"),
    ("hooks", "hooks", ".claude/hooks"),
    ("commands", "commands", ".claude/commands"),
];

/// Read the current hook-config.json as a `serde_json::Value`.
fn read_hook_config(crosslink_dir: &Path) -> Result<serde_json::Value> {
    let config_path = crosslink_dir.join("hook-config.json");
    let raw = fs::read_to_string(&config_path).context("Failed to read hook-config.json")?;
    serde_json::from_str(&raw).context("hook-config.json is not valid JSON")
}

/// Write a `serde_json::Value` back to hook-config.json.
fn write_hook_config(crosslink_dir: &Path, value: &serde_json::Value) -> Result<()> {
    let config_path = crosslink_dir.join("hook-config.json");
    let mut output =
        serde_json::to_string_pretty(value).context("Failed to serialize hook-config.json")?;
    output.push('\n');
    fs::write(&config_path, output).context("Failed to write hook-config.json")
}

/// Extract the `HouseStyleConfig` from hook-config.json, if present.
fn get_house_style(crosslink_dir: &Path) -> Result<Option<HouseStyleConfig>> {
    let config = read_hook_config(crosslink_dir)?;
    match config.get("house_style") {
        Some(v) => {
            let hs: HouseStyleConfig =
                serde_json::from_value(v.clone()).context("Invalid house_style config")?;
            Ok(Some(hs))
        }
        None => Ok(None),
    }
}

/// Save the `HouseStyleConfig` into hook-config.json.
fn set_house_style(crosslink_dir: &Path, hs: &HouseStyleConfig) -> Result<()> {
    let mut config = read_hook_config(crosslink_dir)?;
    let obj = config
        .as_object_mut()
        .context("hook-config.json is not a JSON object")?;
    obj.insert(
        "house_style".to_string(),
        serde_json::to_value(hs).context("Failed to serialize house_style")?,
    );
    write_hook_config(crosslink_dir, &config)
}

/// Remove the `house_style` field from hook-config.json.
fn remove_house_style(crosslink_dir: &Path) -> Result<()> {
    let mut config = read_hook_config(crosslink_dir)?;
    let obj = config
        .as_object_mut()
        .context("hook-config.json is not a JSON object")?;
    obj.remove("house_style");
    write_hook_config(crosslink_dir, &config)
}

/// Path to the style cache directory.
fn cache_dir(crosslink_dir: &Path) -> std::path::PathBuf {
    crosslink_dir.join(".style-cache")
}

/// Clone or fetch the house style repo into the cache directory.
fn fetch_style_repo(crosslink_dir: &Path, url: &str, ref_name: &str) -> Result<()> {
    let cache = cache_dir(crosslink_dir);

    if cache.join(".git").exists() {
        // Already cloned — fetch and reset
        let fetch = std::process::Command::new("git")
            .args(["-C", &cache.to_string_lossy(), "fetch", "origin", ref_name])
            .output()
            .context("Failed to run git fetch")?;

        if !fetch.status.success() {
            let stderr = String::from_utf8_lossy(&fetch.stderr);
            bail!("git fetch failed: {}", stderr.trim());
        }

        let reset = std::process::Command::new("git")
            .args([
                "-C",
                &cache.to_string_lossy(),
                "reset",
                "--hard",
                &format!("origin/{ref_name}"),
            ])
            .output()
            .context("Failed to run git reset")?;

        if !reset.status.success() {
            let stderr = String::from_utf8_lossy(&reset.stderr);
            bail!("git reset failed: {}", stderr.trim());
        }
    } else {
        // Fresh clone
        if cache.exists() {
            fs::remove_dir_all(&cache).context("Failed to clean existing cache directory")?;
        }

        let clone = std::process::Command::new("git")
            .args(["clone", "--depth", "1", "--branch", ref_name, url])
            .arg(&cache)
            .output()
            .context("Failed to run git clone")?;

        if !clone.status.success() {
            let stderr = String::from_utf8_lossy(&clone.stderr);
            bail!("git clone failed: {}", stderr.trim());
        }
    }

    Ok(())
}

/// Check whether a file contains the `# crosslink:custom` marker.
fn has_custom_marker(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|content| content.contains(CUSTOM_MARKER))
}

/// Result of comparing a source file against a deployed file.
enum FileAction {
    /// Files are identical — no action needed.
    Unchanged,
    /// Deployed file has `# crosslink:custom` marker — skip.
    CustomMarker,
    /// File differs and should be updated. Contains a description.
    Update(String),
    /// File is new (doesn't exist locally).
    New,
}

/// Compare a source file from the cache against a deployed file.
fn compare_files(source: &Path, deployed: &Path) -> FileAction {
    let Ok(source_content) = fs::read_to_string(source) else {
        return FileAction::Unchanged; // source doesn't exist, nothing to do
    };

    fs::read_to_string(deployed).map_or(FileAction::New, |deployed_content| {
        if deployed_content == source_content {
            FileAction::Unchanged
        } else if has_custom_marker(deployed) {
            FileAction::CustomMarker
        } else {
            let diff_lines = deployed_content
                .lines()
                .zip(source_content.lines())
                .filter(|(a, b)| a != b)
                .count();
            let len_diff = deployed_content
                .lines()
                .count()
                .abs_diff(source_content.lines().count());
            let total = diff_lines + len_diff;
            FileAction::Update(format!("{total} lines differ"))
        }
    })
}

/// Ensure .style-cache/ is in .crosslink/.gitignore.
fn ensure_gitignore(crosslink_dir: &Path) -> Result<()> {
    let gitignore_path = crosslink_dir.join(".gitignore");
    let entry = ".style-cache/";

    let content = fs::read_to_string(&gitignore_path).unwrap_or_default();
    if !content.lines().any(|line| line.trim() == entry) {
        let mut new_content = content;
        if !new_content.ends_with('\n') && !new_content.is_empty() {
            new_content.push('\n');
        }
        new_content.push_str("\n# House style cache\n.style-cache/\n");
        fs::write(&gitignore_path, new_content)
            .context("Failed to update .crosslink/.gitignore")?;
    }

    Ok(())
}

/// `crosslink style set <url> [--ref <branch-or-tag>]`
pub fn set(crosslink_dir: &Path, url: &str, ref_name: Option<&str>) -> Result<()> {
    let ref_name = ref_name.unwrap_or("main");

    if url.is_empty() {
        bail!("URL cannot be empty");
    }

    println!("Setting house style source: {url}");
    println!("  ref: {ref_name}");

    // Ensure .style-cache/ is gitignored
    ensure_gitignore(crosslink_dir)?;

    // Fetch the repo
    println!("  Fetching...");
    fetch_style_repo(crosslink_dir, url, ref_name)?;

    // Validate it looks like a house style repo
    let cache = cache_dir(crosslink_dir);
    let has_content = cache.join("style.json").exists()
        || cache.join("rules").is_dir()
        || cache.join("hooks").is_dir()
        || cache.join("commands").is_dir()
        || cache.join("hook-config.json").exists();

    if !has_content {
        println!("  Warning: repo does not contain expected house style structure");
        println!("  Expected: rules/, hooks/, commands/, hook-config.json, or style.json");
    }

    // Show style.json metadata if available
    if let Ok(raw) = fs::read_to_string(cache.join("style.json")) {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(name) = meta.get("name").and_then(|v| v.as_str()) {
                println!("  Style: {name}");
            }
            if let Some(version) = meta.get("version").and_then(|v| v.as_str()) {
                println!("  Version: {version}");
            }
            if let Some(desc) = meta.get("description").and_then(|v| v.as_str()) {
                println!("  Description: {desc}");
            }
        }
    }

    // Save config
    let hs = HouseStyleConfig {
        url: url.to_string(),
        ref_name: ref_name.to_string(),
        last_synced: None,
        components: default_components(),
    };
    set_house_style(crosslink_dir, &hs)?;

    println!("House style configured. Run 'crosslink style sync' to apply.");
    Ok(())
}

/// `crosslink style sync [--dry-run]`
pub fn sync(crosslink_dir: &Path, dry_run: bool) -> Result<()> {
    let hs = get_house_style(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!("No house style configured. Run 'crosslink style set <url>' first.")
    })?;

    let project_root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine project root"))?;

    if dry_run {
        println!("Dry run — showing what would change:");
    } else {
        println!("Syncing house style from {}", hs.url);
    }

    // Fetch latest
    if !dry_run {
        println!("  Fetching latest...");
    }
    fetch_style_repo(crosslink_dir, &hs.url, &hs.ref_name)?;

    let cache = cache_dir(crosslink_dir);
    let mut changed = 0u32;
    let mut skipped = 0u32;

    // Sync directory-based components (rules, hooks, commands)
    for (component, src_subdir, target_rel) in COMPONENT_DIRS {
        if !hs.components.contains(&component.to_string()) {
            continue;
        }

        let src_dir = cache.join(src_subdir);
        if !src_dir.is_dir() {
            continue;
        }

        let target_dir = project_root.join(target_rel);

        let entries =
            fs::read_dir(&src_dir).with_context(|| format!("Failed to read cache/{src_subdir}"))?;

        for entry in entries {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() {
                continue;
            }

            let filename = entry.file_name();
            let src_path = entry.path();
            let target_path = target_dir.join(&filename);

            let action = compare_files(&src_path, &target_path);

            match action {
                FileAction::Unchanged => {}
                FileAction::CustomMarker => {
                    if dry_run {
                        println!(
                            "  SKIP {}/{} (has {} marker)",
                            target_rel,
                            filename.to_string_lossy(),
                            CUSTOM_MARKER
                        );
                    }
                    skipped += 1;
                }
                FileAction::Update(desc) => {
                    if dry_run {
                        println!(
                            "  UPDATE {}/{} ({})",
                            target_rel,
                            filename.to_string_lossy(),
                            desc
                        );
                    } else {
                        fs::create_dir_all(&target_dir).ok();
                        let content = fs::read_to_string(&src_path)?;
                        fs::write(&target_path, content)?;
                        println!("  Updated {}/{}", target_rel, filename.to_string_lossy());
                    }
                    changed += 1;
                }
                FileAction::New => {
                    if dry_run {
                        println!("  ADD    {}/{}", target_rel, filename.to_string_lossy());
                    } else {
                        fs::create_dir_all(&target_dir).ok();
                        let content = fs::read_to_string(&src_path)?;
                        fs::write(&target_path, content)?;
                        println!("  Added  {}/{}", target_rel, filename.to_string_lossy());
                    }
                    changed += 1;
                }
            }
        }
    }

    // Sync config component (merge hook-config.json)
    if hs.components.contains(&"config".to_string()) {
        let remote_config_path = cache.join("hook-config.json");
        if remote_config_path.exists() {
            let merge_result = merge_hook_config(crosslink_dir, &remote_config_path, dry_run)?;
            changed += merge_result.fields_updated;
            if dry_run && merge_result.fields_updated > 0 {
                println!(
                    "  MERGE  hook-config.json ({} fields updated)",
                    merge_result.fields_updated
                );
            } else if !dry_run && merge_result.fields_updated > 0 {
                println!(
                    "  Merged hook-config.json ({} fields updated)",
                    merge_result.fields_updated
                );
            }
        }
    }

    // Update last_synced timestamp
    if !dry_run {
        let mut updated_hs = hs;
        updated_hs.last_synced = Some(chrono::Utc::now().to_rfc3339());
        set_house_style(crosslink_dir, &updated_hs)?;
    }

    println!();
    if dry_run {
        println!("Would change {changed} file(s), {skipped} skipped (custom marker).");
    } else {
        println!("Sync complete. {changed} file(s) updated, {skipped} skipped (custom marker).");
    }

    Ok(())
}

/// Result of merging hook-config.json.
struct MergeResult {
    fields_updated: u32,
}

/// Merge remote hook-config.json fields into local, preserving `house_style` and local-only fields.
fn merge_hook_config(
    crosslink_dir: &Path,
    remote_config_path: &Path,
    dry_run: bool,
) -> Result<MergeResult> {
    let local = read_hook_config(crosslink_dir)?;
    let remote_raw =
        fs::read_to_string(remote_config_path).context("Failed to read remote hook-config.json")?;
    let remote: serde_json::Value =
        serde_json::from_str(&remote_raw).context("Remote hook-config.json is not valid JSON")?;

    let local_obj = local.as_object().context("Local config is not an object")?;
    let remote_obj = remote
        .as_object()
        .context("Remote config is not an object")?;

    let mut merged = local_obj.clone();
    let mut fields_updated = 0u32;

    for (key, remote_value) in remote_obj {
        // Never overwrite the house_style section from remote
        if key == "house_style" {
            continue;
        }

        let should_update = local_obj.get(key) != Some(remote_value);

        if should_update {
            if dry_run {
                println!("  MERGE  hook-config.json: update field \"{key}\"");
            }
            merged.insert(key.clone(), remote_value.clone());
            fields_updated += 1;
        }
    }

    if !dry_run && fields_updated > 0 {
        write_hook_config(crosslink_dir, &serde_json::Value::Object(merged))?;
    }

    Ok(MergeResult { fields_updated })
}

/// `crosslink style diff`
pub fn diff(crosslink_dir: &Path) -> Result<()> {
    let hs = get_house_style(crosslink_dir)?.ok_or_else(|| {
        anyhow::anyhow!("No house style configured. Run 'crosslink style set <url>' first.")
    })?;

    let project_root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine project root"))?;

    let cache = cache_dir(crosslink_dir);
    if !cache.join(".git").exists() {
        bail!("Style cache not found. Run 'crosslink style sync' to fetch the house style first.");
    }

    // Fetch latest for accurate comparison
    fetch_style_repo(crosslink_dir, &hs.url, &hs.ref_name)?;

    let mut drift_count = 0u32;

    // Check directory-based components
    for (component, src_subdir, target_rel) in COMPONENT_DIRS {
        if !hs.components.contains(&component.to_string()) {
            continue;
        }

        let src_dir = cache.join(src_subdir);
        if !src_dir.is_dir() {
            continue;
        }

        let target_dir = project_root.join(target_rel);

        let entries =
            fs::read_dir(&src_dir).with_context(|| format!("Failed to read cache/{src_subdir}"))?;

        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }

            let filename = entry.file_name();
            let src_path = entry.path();
            let target_path = target_dir.join(&filename);
            let display_path = format!("{}/{}", target_rel, filename.to_string_lossy());

            let action = compare_files(&src_path, &target_path);

            match action {
                FileAction::Unchanged => {}
                FileAction::CustomMarker => {
                    println!("  ~ {display_path} (custom marker — skipped)");
                }
                FileAction::Update(desc) => {
                    println!("  ! {display_path} ({desc})");
                    drift_count += 1;
                }
                FileAction::New => {
                    println!("  + {display_path} (not deployed)");
                    drift_count += 1;
                }
            }
        }
    }

    // Check config component
    if hs.components.contains(&"config".to_string()) {
        let remote_config_path = cache.join("hook-config.json");
        if remote_config_path.exists() {
            let merge_result = merge_hook_config(crosslink_dir, &remote_config_path, true)?;
            if merge_result.fields_updated > 0 {
                println!(
                    "  ! hook-config.json ({} fields differ)",
                    merge_result.fields_updated
                );
                drift_count += merge_result.fields_updated;
            }
        }
    }

    if drift_count == 0 {
        println!("No drift detected. Local files match the house style.");
    } else {
        println!();
        println!(
            "Drift detected: {drift_count} difference(s). Run 'crosslink style sync' to update."
        );
        std::process::exit(1);
    }

    Ok(())
}

/// `crosslink style show`
pub fn show(crosslink_dir: &Path) -> Result<()> {
    let Some(hs) = get_house_style(crosslink_dir)? else {
        println!("No house style configured.");
        println!("Run 'crosslink style set <url>' to configure one.");
        return Ok(());
    };

    println!("House style configuration:");
    println!("  URL:        {}", hs.url);
    println!("  Ref:        {}", hs.ref_name);
    println!(
        "  Last sync:  {}",
        hs.last_synced.as_deref().unwrap_or("never")
    );
    println!("  Components: {}", hs.components.join(", "));

    // Show style.json metadata if cache exists
    let cache = cache_dir(crosslink_dir);
    if let Ok(raw) = fs::read_to_string(cache.join("style.json")) {
        if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&raw) {
            println!();
            if let Some(name) = meta.get("name").and_then(|v| v.as_str()) {
                println!("  Style name:    {name}");
            }
            if let Some(version) = meta.get("version").and_then(|v| v.as_str()) {
                println!("  Style version: {version}");
            }
            if let Some(desc) = meta.get("description").and_then(|v| v.as_str()) {
                println!("  Description:   {desc}");
            }
        }
    }

    Ok(())
}

/// `crosslink style unset`
pub fn unset(crosslink_dir: &Path) -> Result<()> {
    let hs = get_house_style(crosslink_dir)?;
    if hs.is_none() {
        println!("No house style configured. Nothing to do.");
        return Ok(());
    }

    // Remove cache directory
    let cache = cache_dir(crosslink_dir);
    if cache.exists() {
        fs::remove_dir_all(&cache).context("Failed to remove style cache")?;
        println!("Removed style cache.");
    }

    // Remove house_style from config
    remove_house_style(crosslink_dir)?;
    println!("House style configuration removed.");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_crosslink_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        fs::create_dir_all(&crosslink_dir).unwrap();
        fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{
  "tracking_mode": "strict",
  "intervention_tracking": true
}
"#,
        )
        .unwrap();
        fs::write(crosslink_dir.join(".gitignore"), "agent.json\n").unwrap();
        (dir, crosslink_dir)
    }

    #[test]
    fn test_house_style_config_roundtrip() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();

        assert!(get_house_style(&crosslink_dir).unwrap().is_none());

        let hs = HouseStyleConfig {
            url: "https://github.com/org/style.git".to_string(),
            ref_name: "main".to_string(),
            last_synced: None,
            components: default_components(),
        };
        set_house_style(&crosslink_dir, &hs).unwrap();

        let loaded = get_house_style(&crosslink_dir).unwrap().unwrap();
        assert_eq!(loaded.url, "https://github.com/org/style.git");
        assert_eq!(loaded.ref_name, "main");
        assert!(loaded.last_synced.is_none());
        assert_eq!(loaded.components, default_components());

        let config = read_hook_config(&crosslink_dir).unwrap();
        assert_eq!(
            config.get("tracking_mode").and_then(|v| v.as_str()),
            Some("strict")
        );
    }

    #[test]
    fn test_remove_house_style() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();

        let hs = HouseStyleConfig {
            url: "https://github.com/org/style.git".to_string(),
            ref_name: "main".to_string(),
            last_synced: None,
            components: default_components(),
        };
        set_house_style(&crosslink_dir, &hs).unwrap();
        assert!(get_house_style(&crosslink_dir).unwrap().is_some());

        remove_house_style(&crosslink_dir).unwrap();
        assert!(get_house_style(&crosslink_dir).unwrap().is_none());

        let config = read_hook_config(&crosslink_dir).unwrap();
        assert_eq!(
            config.get("tracking_mode").and_then(|v| v.as_str()),
            Some("strict")
        );
    }

    #[test]
    fn test_has_custom_marker_true() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "# crosslink:custom\nsome content").unwrap();
        assert!(has_custom_marker(&path));
    }

    #[test]
    fn test_has_custom_marker_false() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.md");
        fs::write(&path, "no marker here").unwrap();
        assert!(!has_custom_marker(&path));
    }

    #[test]
    fn test_has_custom_marker_missing_file() {
        let dir = tempdir().unwrap();
        assert!(!has_custom_marker(&dir.path().join("nope.md")));
    }

    #[test]
    fn test_compare_files_unchanged() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.md");
        let dst = dir.path().join("dst.md");
        fs::write(&src, "same content").unwrap();
        fs::write(&dst, "same content").unwrap();
        assert!(matches!(compare_files(&src, &dst), FileAction::Unchanged));
    }

    #[test]
    fn test_compare_files_update() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.md");
        let dst = dir.path().join("dst.md");
        fs::write(&src, "new content\nline 2").unwrap();
        fs::write(&dst, "old content\nline 2").unwrap();
        assert!(matches!(compare_files(&src, &dst), FileAction::Update(_)));
    }

    #[test]
    fn test_compare_files_new() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.md");
        let dst = dir.path().join("nonexistent.md");
        fs::write(&src, "content").unwrap();
        assert!(matches!(compare_files(&src, &dst), FileAction::New));
    }

    #[test]
    fn test_compare_files_custom_marker() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.md");
        let dst = dir.path().join("dst.md");
        fs::write(&src, "new content").unwrap();
        fs::write(&dst, "# crosslink:custom\nold content").unwrap();
        assert!(matches!(
            compare_files(&src, &dst),
            FileAction::CustomMarker
        ));
    }

    #[test]
    fn test_merge_hook_config_updates_fields() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        fs::create_dir_all(&crosslink_dir).unwrap();

        fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{
  "tracking_mode": "strict",
  "intervention_tracking": true,
  "house_style": {"url": "test", "ref": "main"}
}
"#,
        )
        .unwrap();

        let remote = dir.path().join("remote-config.json");
        fs::write(
            &remote,
            r#"{
  "tracking_mode": "normal",
  "new_field": true,
  "house_style": {"url": "should-not-overwrite", "ref": "other"}
}
"#,
        )
        .unwrap();

        let result = merge_hook_config(&crosslink_dir, &remote, false).unwrap();
        assert_eq!(result.fields_updated, 2);

        let config = read_hook_config(&crosslink_dir).unwrap();
        assert_eq!(
            config.get("tracking_mode").and_then(|v| v.as_str()),
            Some("normal")
        );
        assert_eq!(
            config.get("new_field").and_then(serde_json::Value::as_bool),
            Some(true)
        );
        assert_eq!(
            config
                .get("intervention_tracking")
                .and_then(serde_json::Value::as_bool),
            Some(true)
        );
        let hs = config.get("house_style").unwrap();
        assert_eq!(hs.get("url").and_then(|v| v.as_str()), Some("test"));
    }

    #[test]
    fn test_merge_hook_config_dry_run_no_changes() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        fs::create_dir_all(&crosslink_dir).unwrap();

        fs::write(
            crosslink_dir.join("hook-config.json"),
            r#"{"tracking_mode": "strict"}"#,
        )
        .unwrap();

        let remote = dir.path().join("remote.json");
        fs::write(&remote, r#"{"tracking_mode": "normal"}"#).unwrap();

        let result = merge_hook_config(&crosslink_dir, &remote, true).unwrap();
        assert_eq!(result.fields_updated, 1);

        let config = read_hook_config(&crosslink_dir).unwrap();
        assert_eq!(
            config.get("tracking_mode").and_then(|v| v.as_str()),
            Some("strict")
        );
    }

    #[test]
    fn test_ensure_gitignore_adds_entry() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        fs::create_dir_all(&crosslink_dir).unwrap();
        fs::write(crosslink_dir.join(".gitignore"), "agent.json\n").unwrap();

        ensure_gitignore(&crosslink_dir).unwrap();

        let content = fs::read_to_string(crosslink_dir.join(".gitignore")).unwrap();
        assert!(content.contains(".style-cache/"));
    }

    #[test]
    fn test_ensure_gitignore_idempotent() {
        let dir = tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        fs::create_dir_all(&crosslink_dir).unwrap();
        fs::write(
            crosslink_dir.join(".gitignore"),
            "agent.json\n.style-cache/\n",
        )
        .unwrap();

        ensure_gitignore(&crosslink_dir).unwrap();

        let content = fs::read_to_string(crosslink_dir.join(".gitignore")).unwrap();
        assert_eq!(content.matches(".style-cache/").count(), 1);
    }

    #[test]
    fn test_show_no_config() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();
        show(&crosslink_dir).unwrap();
    }

    #[test]
    fn test_show_with_config() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();
        let hs = HouseStyleConfig {
            url: "https://github.com/org/style.git".to_string(),
            ref_name: "v1.0".to_string(),
            last_synced: Some("2026-02-28T00:00:00Z".to_string()),
            components: vec!["rules".into(), "config".into()],
        };
        set_house_style(&crosslink_dir, &hs).unwrap();
        show(&crosslink_dir).unwrap();
    }

    #[test]
    fn test_unset_no_config() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();
        unset(&crosslink_dir).unwrap();
    }

    #[test]
    fn test_unset_with_config() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();
        let hs = HouseStyleConfig {
            url: "https://github.com/org/style.git".to_string(),
            ref_name: "main".to_string(),
            last_synced: None,
            components: default_components(),
        };
        set_house_style(&crosslink_dir, &hs).unwrap();

        fs::create_dir_all(cache_dir(&crosslink_dir)).unwrap();

        unset(&crosslink_dir).unwrap();

        assert!(get_house_style(&crosslink_dir).unwrap().is_none());
        assert!(!cache_dir(&crosslink_dir).exists());
    }

    #[test]
    fn test_set_empty_url_fails() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();
        assert!(set(&crosslink_dir, "", None).is_err());
    }

    #[test]
    fn test_default_components() {
        let components = default_components();
        assert!(components.contains(&"rules".to_string()));
        assert!(components.contains(&"hooks".to_string()));
        assert!(components.contains(&"commands".to_string()));
        assert!(components.contains(&"config".to_string()));
    }

    #[test]
    fn test_serde_ref_field_rename() {
        let json = r#"{"url":"https://example.com/style.git","ref":"v2","components":["rules"]}"#;
        let hs: HouseStyleConfig = serde_json::from_str(json).unwrap();
        assert_eq!(hs.ref_name, "v2");
        assert_eq!(hs.components, vec!["rules"]);

        let serialized = serde_json::to_string(&hs).unwrap();
        assert!(serialized.contains(r#""ref":"v2""#));
        assert!(!serialized.contains("ref_name"));
    }

    #[test]
    fn test_serde_defaults() {
        let json = r#"{"url":"https://example.com/style.git"}"#;
        let hs: HouseStyleConfig = serde_json::from_str(json).unwrap();
        assert_eq!(hs.ref_name, "main");
        assert_eq!(hs.components, default_components());
        assert!(hs.last_synced.is_none());
    }

    #[test]
    fn test_sync_no_config_fails() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();
        let result = sync(&crosslink_dir, false);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No house style configured"));
    }

    #[test]
    fn test_diff_no_config_fails() {
        let (_dir, crosslink_dir) = setup_crosslink_dir();
        let result = diff(&crosslink_dir);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No house style configured"));
    }
}
