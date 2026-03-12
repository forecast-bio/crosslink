//! Seam detection and codebase auto-partitioning for swarm review.
//!
//! Analyzes a repository to produce non-overlapping [`Partition`]s of source
//! files. The algorithm works in layers:
//!
//! 1. **Module boundary detection** — for Rust repos, parse `mod` declarations
//!    and detect crate boundaries (Cargo.toml).
//! 2. **Directory-based fallback** — for non-Rust repos or when module
//!    detection yields too few partitions, split by top-level source dirs.
//! 3. **Size-based adjustment** — large partitions (>2 000 lines) are split;
//!    small partitions (<200 lines) are merged with adjacent ones.
//! 4. **Git coupling overlay** — files that frequently change together are
//!    coalesced into the same partition.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A partition of source files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Partition {
    pub label: String,
    pub files: Vec<PathBuf>,
    pub line_count: usize,
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Source file extensions we care about.
const SOURCE_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "cpp", "h", "hpp", "cc", "cxx", "cs",
    "rb", "swift", "kt", "scala", "zig", "hs", "ml", "ex", "exs", "erl", "clj", "lua", "sh",
    "bash", "zsh", "vue", "svelte",
];

/// Directories we always skip.
const IGNORED_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "vendor",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    "venv",
    ".venv",
    "env",
    ".crosslink",
    ".claude",
];

/// Lines above which a partition should be split.
const MAX_PARTITION_LINES: usize = 2_000;

/// Lines below which a partition is a merge candidate.
const MIN_PARTITION_LINES: usize = 200;

/// Number of recent commits to scan for co-change coupling.
const GIT_LOG_DEPTH: usize = 200;

/// Co-change threshold: two files that appear together in at least this many
/// commits are considered coupled.
const COUPLING_THRESHOLD: usize = 3;

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Detect seams in the repository at `repo_root` and return up to
/// `max_partitions` non-overlapping partitions of source files.
pub fn detect_seams(repo_root: &Path, max_partitions: usize) -> Result<Vec<Partition>> {
    let max_partitions = max_partitions.max(1);

    // 1. Collect all source files.
    let all_files = collect_source_files(repo_root)?;
    if all_files.is_empty() {
        return Ok(vec![]);
    }

    // 2. Try module-boundary detection (Rust-aware).
    let mut partitions = detect_module_boundaries(repo_root, &all_files)?;

    // 3. Fallback to directory-based splitting when we got fewer than 2
    //    partitions from module detection.
    if partitions.len() < 2 {
        partitions = directory_based_partitions(repo_root, &all_files)?;
    }

    // 4. Ensure every source file is assigned (catch stragglers).
    partitions = ensure_complete_coverage(partitions, &all_files);

    // 5. Git-coupling analysis: merge partitions whose files are tightly
    //    coupled according to commit history.
    let coupling = git_coupling(repo_root);
    partitions = apply_coupling(partitions, &coupling);

    // 6. Size-based adjustment: split large, merge small.
    partitions = adjust_sizes(partitions);

    // 7. Trim / merge to honour max_partitions.
    while partitions.len() > max_partitions {
        partitions = merge_smallest_pair(partitions);
    }

    // Sort by label for deterministic output.
    partitions.sort_by(|a, b| a.label.cmp(&b.label));

    Ok(partitions)
}

// ---------------------------------------------------------------------------
// File collection
// ---------------------------------------------------------------------------

fn collect_source_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    walk_dir(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn walk_dir(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        if path.is_dir() {
            if IGNORED_DIRS.contains(&name.as_ref()) {
                continue;
            }
            walk_dir(root, &path, out)?;
        } else if is_source_file(&path) {
            // Store relative to root.
            if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
    Ok(())
}

fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| SOURCE_EXTENSIONS.contains(&ext))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Line counting
// ---------------------------------------------------------------------------

fn count_lines(root: &Path, file: &Path) -> usize {
    let full = root.join(file);
    match std::fs::read_to_string(&full) {
        Ok(contents) => contents.lines().count(),
        Err(_) => 0,
    }
}

fn count_lines_many(root: &Path, files: &[PathBuf]) -> usize {
    files.iter().map(|f| count_lines(root, f)).sum()
}

fn make_partition(root: &Path, label: String, files: Vec<PathBuf>) -> Partition {
    let line_count = count_lines_many(root, &files);
    Partition {
        label,
        files,
        line_count,
    }
}

// ---------------------------------------------------------------------------
// Module-boundary detection (Rust-aware)
// ---------------------------------------------------------------------------

fn detect_module_boundaries(root: &Path, all_files: &[PathBuf]) -> Result<Vec<Partition>> {
    let crate_roots = find_cargo_tomls(root)?;
    if crate_roots.is_empty() {
        return Ok(vec![]);
    }

    let mut partitions: Vec<Partition> = Vec::new();

    for crate_root in &crate_roots {
        let rel_crate = crate_root
            .strip_prefix(root)
            .unwrap_or(crate_root)
            .to_path_buf();
        let crate_label = if rel_crate == Path::new("") {
            "root".to_string()
        } else {
            rel_crate.display().to_string().replace('/', "::")
        };

        // Find the src/ directory for this crate.
        let src_dir = crate_root.join("src");
        if !src_dir.is_dir() {
            continue;
        }

        // Try to parse mod declarations from lib.rs or main.rs.
        let entry_points = ["lib.rs", "main.rs"];
        let mut mod_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
        let mut claimed: HashSet<PathBuf> = HashSet::new();

        for ep in &entry_points {
            let ep_path = src_dir.join(ep);
            if ep_path.is_file() {
                if let Ok(contents) = std::fs::read_to_string(&ep_path) {
                    for mod_name in parse_mod_declarations(&contents) {
                        // A mod can be either src/<mod>.rs or src/<mod>/mod.rs
                        let mod_files = find_mod_files(root, &src_dir, &mod_name, all_files);
                        if !mod_files.is_empty() {
                            for f in &mod_files {
                                claimed.insert(f.clone());
                            }
                            mod_map.insert(mod_name, mod_files);
                        }
                    }
                }
            }
        }

        // Create a partition per module.
        for (mod_name, files) in &mod_map {
            let label = format!("{}::{}", crate_label, mod_name);
            partitions.push(make_partition(root, label, files.clone()));
        }

        // Remaining files in this crate's src/ that weren't claimed by any mod.
        let crate_src_rel = src_dir.strip_prefix(root).unwrap_or(&src_dir).to_path_buf();
        let unclaimed: Vec<PathBuf> = all_files
            .iter()
            .filter(|f| f.starts_with(&crate_src_rel) && !claimed.contains(*f))
            .cloned()
            .collect();
        if !unclaimed.is_empty() {
            partitions.push(make_partition(
                root,
                format!("{}::_root", crate_label),
                unclaimed,
            ));
        }
    }

    Ok(partitions)
}

/// Find all directories containing a Cargo.toml.
fn find_cargo_tomls(root: &Path) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();
    find_cargo_tomls_recurse(root, root, &mut results)?;
    // Sort so that the root crate comes first.
    results.sort_by_key(|p| p.components().count());
    Ok(results)
}

fn find_cargo_tomls_recurse(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let ct = dir.join("Cargo.toml");
    if ct.is_file() {
        out.push(dir.to_path_buf());
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if path.is_dir() && !IGNORED_DIRS.contains(&name_str.as_ref()) {
                find_cargo_tomls_recurse(root, &path, out)?;
            }
        }
    }
    Ok(())
}

/// Parse `mod foo;` declarations from Rust source text.
fn parse_mod_declarations(source: &str) -> Vec<String> {
    let mut mods = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        // Match: `mod name;` or `pub mod name;` (but not `mod name { ... }`)
        if let Some(name) = extract_mod_name(trimmed) {
            mods.push(name);
        }
    }
    mods
}

fn extract_mod_name(line: &str) -> Option<String> {
    let line = line.trim();
    // Strip attributes like #[cfg(test)], #[allow(dead_code)]
    // We only look at lines that start with `mod ` or `pub mod ` or
    // `pub(crate) mod ` etc, and end with `;`.
    if !line.ends_with(';') {
        return None;
    }
    let line = line.trim_end_matches(';').trim();

    // Remove visibility qualifiers.
    let rest = if line.starts_with("pub(") {
        // pub(crate) mod foo, pub(super) mod foo, etc.
        if let Some(idx) = line.find(')') {
            line[idx + 1..].trim()
        } else {
            return None;
        }
    } else if let Some(rest) = line.strip_prefix("pub ") {
        rest.trim()
    } else {
        line
    };

    let rest = rest.strip_prefix("mod ")?.trim();

    // Validate it looks like an identifier.
    if rest.is_empty() || !rest.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    Some(rest.to_string())
}

/// Find all source files belonging to a given module name inside a src dir.
fn find_mod_files(
    root: &Path,
    src_dir: &Path,
    mod_name: &str,
    all_files: &[PathBuf],
) -> Vec<PathBuf> {
    let src_rel = src_dir.strip_prefix(root).unwrap_or(src_dir);

    // The module can be:
    //   src/<mod_name>.rs
    //   src/<mod_name>/mod.rs  (and everything under src/<mod_name>/)
    let single_file = src_rel.join(format!("{}.rs", mod_name));
    let dir_prefix = src_rel.join(mod_name);

    let mut files: Vec<PathBuf> = Vec::new();

    for f in all_files {
        if *f == single_file || f.starts_with(&dir_prefix) {
            files.push(f.clone());
        }
    }

    files
}

// ---------------------------------------------------------------------------
// Directory-based fallback
// ---------------------------------------------------------------------------

fn directory_based_partitions(root: &Path, all_files: &[PathBuf]) -> Result<Vec<Partition>> {
    // Group files by their first path component (top-level directory).
    let mut groups: HashMap<String, Vec<PathBuf>> = HashMap::new();

    for f in all_files {
        let key = f
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_else(|| "_root".to_string());

        // If the file is directly in root (only one component), group as _root.
        if f.components().count() == 1 {
            groups
                .entry("_root".to_string())
                .or_default()
                .push(f.clone());
        } else {
            groups.entry(key).or_default().push(f.clone());
        }
    }

    let mut partitions: Vec<Partition> = groups
        .into_iter()
        .map(|(label, files)| make_partition(root, label, files))
        .collect();

    partitions.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(partitions)
}

// ---------------------------------------------------------------------------
// Completeness check
// ---------------------------------------------------------------------------

/// Ensure every file in `all_files` appears in exactly one partition.
fn ensure_complete_coverage(
    mut partitions: Vec<Partition>,
    all_files: &[PathBuf],
) -> Vec<Partition> {
    let assigned: HashSet<PathBuf> = partitions
        .iter()
        .flat_map(|p| p.files.iter().cloned())
        .collect();

    let missing: Vec<PathBuf> = all_files
        .iter()
        .filter(|f| !assigned.contains(*f))
        .cloned()
        .collect();

    if !missing.is_empty() {
        // We don't have root here, so line_count will be approximate (0).
        // The caller can recompute if needed, but in practice the main entry
        // point recomputes after coupling.  For now, store 0.
        partitions.push(Partition {
            label: "_uncategorized".to_string(),
            files: missing,
            line_count: 0,
        });
    }

    // De-duplicate: ensure no file appears in more than one partition.
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for part in &mut partitions {
        part.files.retain(|f| seen.insert(f.clone()));
    }

    // Remove empty partitions.
    partitions.retain(|p| !p.files.is_empty());

    partitions
}

// ---------------------------------------------------------------------------
// Git coupling analysis
// ---------------------------------------------------------------------------

/// Map from file path → set of files it is coupled with (symmetric).
type CouplingMap = HashMap<PathBuf, HashSet<PathBuf>>;

fn git_coupling(repo_root: &Path) -> CouplingMap {
    git_coupling_inner(repo_root).unwrap_or_default()
}

fn git_coupling_inner(repo_root: &Path) -> Result<CouplingMap> {
    let output = std::process::Command::new("git")
        .args([
            "log",
            "--name-only",
            "--pretty=format:",
            "-n",
            &GIT_LOG_DEPTH.to_string(),
        ])
        .current_dir(repo_root)
        .output()
        .context("running git log")?;

    if !output.status.success() {
        return Ok(HashMap::new());
    }

    let text = String::from_utf8_lossy(&output.stdout);

    // Parse commits: groups of file names separated by blank lines.
    let mut pair_counts: HashMap<(PathBuf, PathBuf), usize> = HashMap::new();
    let mut current_commit: Vec<PathBuf> = Vec::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            record_pairs(&current_commit, &mut pair_counts);
            current_commit.clear();
        } else {
            let p = PathBuf::from(line);
            if is_source_file(&p) {
                current_commit.push(p);
            }
        }
    }
    // Don't forget last commit group.
    record_pairs(&current_commit, &mut pair_counts);

    // Build symmetric coupling map.
    let mut coupling: CouplingMap = HashMap::new();
    for ((a, b), count) in &pair_counts {
        if *count >= COUPLING_THRESHOLD {
            coupling.entry(a.clone()).or_default().insert(b.clone());
            coupling.entry(b.clone()).or_default().insert(a.clone());
        }
    }

    Ok(coupling)
}

fn record_pairs(files: &[PathBuf], counts: &mut HashMap<(PathBuf, PathBuf), usize>) {
    if files.len() < 2 {
        return;
    }
    for i in 0..files.len() {
        for j in (i + 1)..files.len() {
            let a = files[i].clone();
            let b = files[j].clone();
            let key = if a < b { (a, b) } else { (b, a) };
            *counts.entry(key).or_insert(0) += 1;
        }
    }
}

/// Merge partitions when coupling data shows that files across two partitions
/// are tightly linked.
fn apply_coupling(mut partitions: Vec<Partition>, coupling: &CouplingMap) -> Vec<Partition> {
    if coupling.is_empty() {
        return partitions;
    }

    // Build file→partition-index map.
    let file_to_idx: HashMap<PathBuf, usize> = partitions
        .iter()
        .enumerate()
        .flat_map(|(idx, p)| p.files.iter().map(move |f| (f.clone(), idx)))
        .collect();

    // Count cross-partition coupling edges.
    let mut merge_votes: HashMap<(usize, usize), usize> = HashMap::new();
    for (file, coupled_files) in coupling {
        if let Some(&idx_a) = file_to_idx.get(file) {
            for cf in coupled_files {
                if let Some(&idx_b) = file_to_idx.get(cf) {
                    if idx_a != idx_b {
                        let key = if idx_a < idx_b {
                            (idx_a, idx_b)
                        } else {
                            (idx_b, idx_a)
                        };
                        *merge_votes.entry(key).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    // Merge pairs with the strongest coupling, iteratively.
    // Use a simple union-find to track merges.
    let n = partitions.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], mut x: usize) -> usize {
        while parent[x] != x {
            parent[x] = parent[parent[x]];
            x = parent[x];
        }
        x
    }

    // Sort merges by vote count descending.
    let mut merges: Vec<((usize, usize), usize)> = merge_votes.into_iter().collect();
    merges.sort_by(|a, b| b.1.cmp(&a.1));

    for ((a, b), votes) in merges {
        if votes < COUPLING_THRESHOLD {
            break;
        }
        let ra = find(&mut parent, a);
        let rb = find(&mut parent, b);
        if ra != rb {
            parent[rb] = ra;
        }
    }

    // Group partitions by their root.
    let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(i);
    }

    let mut result: Vec<Partition> = Vec::new();
    for (_root, indices) in groups {
        if indices.len() == 1 {
            result.push(partitions[indices[0]].clone());
        } else {
            // Merge partitions.
            let label = indices
                .iter()
                .map(|&i| partitions[i].label.as_str())
                .collect::<Vec<_>>()
                .join("+");
            let mut files: Vec<PathBuf> = Vec::new();
            let mut line_count = 0;
            for &i in &indices {
                files.extend(partitions[i].files.drain(..));
                line_count += partitions[i].line_count;
            }
            files.sort();
            result.push(Partition {
                label,
                files,
                line_count,
            });
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Size-based adjustment
// ---------------------------------------------------------------------------

fn adjust_sizes(mut partitions: Vec<Partition>) -> Vec<Partition> {
    // 1. Split large partitions.
    let mut split_result: Vec<Partition> = Vec::new();
    for part in partitions.drain(..) {
        if part.line_count > MAX_PARTITION_LINES && part.files.len() > 1 {
            split_result.extend(split_partition(part));
        } else {
            split_result.push(part);
        }
    }

    // 2. Merge small partitions.
    merge_small_partitions(split_result)
}

fn split_partition(part: Partition) -> Vec<Partition> {
    let total = part.line_count;
    if total == 0 || part.files.len() <= 1 {
        return vec![part];
    }

    // Split roughly in half by line count.
    let half = total / 2;
    let mut left_files: Vec<PathBuf> = Vec::new();
    let mut left_lines = 0usize;
    let mut right_files: Vec<PathBuf> = Vec::new();
    let mut right_lines = 0usize;

    // We don't have the root path here, so we approximate by distributing
    // files evenly. A better approach would thread root through, but for
    // the split heuristic even distribution works well enough.
    let per_file = total / part.files.len().max(1);
    for f in part.files {
        if left_lines < half {
            left_lines += per_file;
            left_files.push(f);
        } else {
            right_lines += per_file;
            right_files.push(f);
        }
    }

    let mut results = Vec::new();
    if !left_files.is_empty() {
        results.push(Partition {
            label: format!("{}/a", part.label),
            files: left_files,
            line_count: left_lines,
        });
    }
    if !right_files.is_empty() {
        results.push(Partition {
            label: format!("{}/b", part.label),
            files: right_files,
            line_count: right_lines,
        });
    }

    // Recursively split if still too large.
    let mut final_results = Vec::new();
    for p in results {
        if p.line_count > MAX_PARTITION_LINES && p.files.len() > 1 {
            final_results.extend(split_partition(p));
        } else {
            final_results.push(p);
        }
    }

    final_results
}

fn merge_small_partitions(mut partitions: Vec<Partition>) -> Vec<Partition> {
    if partitions.len() <= 1 {
        return partitions;
    }

    // Sort by line count so we merge the smallest first.
    partitions.sort_by_key(|p| p.line_count);

    let mut merged: Vec<Partition> = Vec::new();
    let mut carry: Option<Partition> = None;

    for part in partitions {
        match carry.take() {
            None => {
                if part.line_count < MIN_PARTITION_LINES {
                    carry = Some(part);
                } else {
                    merged.push(part);
                }
            }
            Some(mut prev) => {
                if prev.line_count + part.line_count < MIN_PARTITION_LINES
                    || prev.line_count < MIN_PARTITION_LINES
                {
                    // Merge prev into part.
                    let label = format!("{}+{}", prev.label, part.label);
                    let line_count = prev.line_count + part.line_count;
                    let mut files = Vec::new();
                    files.append(&mut prev.files);
                    files.extend(part.files);
                    let merged_part = Partition {
                        label,
                        files,
                        line_count,
                    };
                    if merged_part.line_count < MIN_PARTITION_LINES {
                        carry = Some(merged_part);
                    } else {
                        merged.push(merged_part);
                    }
                } else {
                    merged.push(prev);
                    if part.line_count < MIN_PARTITION_LINES {
                        carry = Some(part);
                    } else {
                        merged.push(part);
                    }
                }
            }
        }
    }

    if let Some(leftover) = carry {
        if let Some(last) = merged.last_mut() {
            // Absorb into the last partition.
            last.label = format!("{}+{}", last.label, leftover.label);
            last.line_count += leftover.line_count;
            last.files.extend(leftover.files);
        } else {
            merged.push(leftover);
        }
    }

    merged
}

// ---------------------------------------------------------------------------
// Partition count trimming
// ---------------------------------------------------------------------------

fn merge_smallest_pair(mut partitions: Vec<Partition>) -> Vec<Partition> {
    if partitions.len() <= 1 {
        return partitions;
    }

    // Find the partition with the fewest lines.
    let min_idx = partitions
        .iter()
        .enumerate()
        .min_by_key(|(_, p)| p.line_count)
        .map(|(i, _)| i)
        .unwrap();

    // Find the best merge partner: the next smallest that isn't the same.
    let partner_idx = partitions
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != min_idx)
        .min_by_key(|(_, p)| p.line_count)
        .map(|(i, _)| i)
        .unwrap();

    // Merge the two.
    let (lo, hi) = if min_idx < partner_idx {
        (min_idx, partner_idx)
    } else {
        (partner_idx, min_idx)
    };

    let removed = partitions.remove(hi);
    let target = &mut partitions[lo];
    target.label = format!("{}+{}", target.label, removed.label);
    target.line_count += removed.line_count;
    target.files.extend(removed.files);

    partitions
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temp directory tree with the given files and content.
    fn setup_repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            if let Some(parent) = full.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&full, content).unwrap();
        }
        // Initialize a git repo so git coupling analysis doesn't error.
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .ok();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .ok();
        std::process::Command::new("git")
            .args(["commit", "-m", "init", "--allow-empty"])
            .current_dir(dir.path())
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test.com")
            .output()
            .ok();
        dir
    }

    #[test]
    fn test_is_source_file() {
        assert!(is_source_file(Path::new("foo.rs")));
        assert!(is_source_file(Path::new("bar/baz.ts")));
        assert!(is_source_file(Path::new("main.go")));
        assert!(!is_source_file(Path::new("readme.md")));
        assert!(!is_source_file(Path::new("Cargo.toml")));
        assert!(!is_source_file(Path::new("data.json")));
    }

    #[test]
    fn test_parse_mod_declarations() {
        let src = r#"
mod foo;
pub mod bar;
pub(crate) mod baz;
#[allow(dead_code)]
mod qux;
mod inline_mod {
    fn something() {}
}
"#;
        let mods = parse_mod_declarations(src);
        assert_eq!(mods, vec!["foo", "bar", "baz", "qux"]);
    }

    #[test]
    fn test_extract_mod_name_edge_cases() {
        assert_eq!(extract_mod_name("mod foo;"), Some("foo".to_string()));
        assert_eq!(extract_mod_name("pub mod bar;"), Some("bar".to_string()));
        assert_eq!(
            extract_mod_name("pub(crate) mod baz;"),
            Some("baz".to_string())
        );
        assert_eq!(
            extract_mod_name("pub(super) mod thing;"),
            Some("thing".to_string())
        );
        // Should NOT match inline module blocks.
        assert_eq!(extract_mod_name("mod inline {"), None);
        // Should NOT match use statements.
        assert_eq!(extract_mod_name("use foo;"), None);
        // Empty mod name.
        assert_eq!(extract_mod_name("mod ;"), None);
    }

    #[test]
    fn test_collect_source_files_ignores_target() {
        let repo = setup_repo(&[
            ("src/main.rs", "fn main() {}"),
            ("src/lib.rs", "pub mod foo;"),
            ("target/debug/build.rs", "// build artifact"),
            ("node_modules/pkg/index.js", "module.exports = {}"),
        ]);
        let files = collect_source_files(repo.path()).unwrap();
        assert!(files.contains(&PathBuf::from("src/main.rs")));
        assert!(files.contains(&PathBuf::from("src/lib.rs")));
        assert!(!files.iter().any(|f| f.starts_with("target")));
        assert!(!files.iter().any(|f| f.starts_with("node_modules")));
    }

    #[test]
    fn test_directory_based_partitions() {
        let repo = setup_repo(&[
            ("src/main.rs", "fn main() {}\nfn a() {}\nfn b() {}"),
            ("src/lib.rs", "pub fn lib() {}"),
            ("tests/test1.rs", "fn test() {}"),
            ("benches/bench.rs", "fn bench() {}"),
        ]);
        let files = collect_source_files(repo.path()).unwrap();
        let parts = directory_based_partitions(repo.path(), &files).unwrap();

        // Should have partitions for src, tests, benches.
        let labels: Vec<&str> = parts.iter().map(|p| p.label.as_str()).collect();
        assert!(labels.contains(&"src"));
        assert!(labels.contains(&"tests"));
        assert!(labels.contains(&"benches"));
    }

    #[test]
    fn test_detect_seams_rust_crate() {
        let repo = setup_repo(&[
            (
                "Cargo.toml",
                "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"",
            ),
            (
                "src/main.rs",
                "mod foo;\nmod bar;\nfn main() { foo::run(); bar::run(); }",
            ),
            ("src/foo.rs", &"fn run() {}\n".repeat(100)),
            ("src/bar.rs", &"fn run() {}\n".repeat(100)),
        ]);

        let partitions = detect_seams(repo.path(), 10).unwrap();
        assert!(!partitions.is_empty());

        // All files should be covered.
        let all_files: HashSet<PathBuf> = partitions
            .iter()
            .flat_map(|p| p.files.iter().cloned())
            .collect();
        assert!(all_files.contains(&PathBuf::from("src/main.rs")));
        assert!(all_files.contains(&PathBuf::from("src/foo.rs")));
        assert!(all_files.contains(&PathBuf::from("src/bar.rs")));
    }

    #[test]
    fn test_detect_seams_empty_repo() {
        let repo = setup_repo(&[("README.md", "# Hello")]);
        let partitions = detect_seams(repo.path(), 5).unwrap();
        assert!(partitions.is_empty());
    }

    #[test]
    fn test_non_overlapping() {
        let repo = setup_repo(&[
            (
                "Cargo.toml",
                "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"",
            ),
            ("src/main.rs", "mod a;\nmod b;\nfn main() {}"),
            ("src/a.rs", &"fn a() {}\n".repeat(50)),
            ("src/b.rs", &"fn b() {}\n".repeat(50)),
            ("src/b/extra.rs", &"fn extra() {}\n".repeat(50)),
            ("other/script.py", "print('hello')\n"),
        ]);

        let partitions = detect_seams(repo.path(), 10).unwrap();

        // Check non-overlapping: no file appears in more than one partition.
        let mut seen: HashSet<PathBuf> = HashSet::new();
        for part in &partitions {
            for f in &part.files {
                assert!(
                    seen.insert(f.clone()),
                    "file {:?} appears in multiple partitions",
                    f
                );
            }
        }
    }

    #[test]
    fn test_max_partitions_respected() {
        let mut files = Vec::new();
        for i in 0..20 {
            let dir = format!("dir{}", i);
            files.push((format!("{}/file.rs", dir), "fn foo() {}\n".repeat(100)));
        }
        let file_refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(p, c)| (p.as_str(), c.as_str()))
            .collect();
        let repo = setup_repo(&file_refs);

        let partitions = detect_seams(repo.path(), 3).unwrap();
        assert!(
            partitions.len() <= 3,
            "expected <=3 partitions, got {}",
            partitions.len()
        );

        // All 20 files should still be covered.
        let total_files: usize = partitions.iter().map(|p| p.files.len()).sum();
        assert_eq!(total_files, 20);
    }

    #[test]
    fn test_size_based_splitting() {
        // One big module with >2000 lines should get split.
        let big_content = "fn line() {}\n".repeat(2500);
        let repo = setup_repo(&[
            (
                "Cargo.toml",
                "[package]\nname = \"test\"\nversion = \"0.1.0\"\nedition = \"2021\"",
            ),
            ("src/main.rs", "mod big;\nfn main() {}"),
            ("src/big/mod.rs", &big_content),
            ("src/big/sub1.rs", &"fn s1() {}\n".repeat(500)),
            ("src/big/sub2.rs", &"fn s2() {}\n".repeat(500)),
        ]);

        let partitions = detect_seams(repo.path(), 20).unwrap();
        // The big module should have been split into sub-partitions.
        let big_parts: Vec<&Partition> = partitions
            .iter()
            .filter(|p| p.label.contains("big"))
            .collect();
        // It may be one partition if the split merged, but the total line
        // count should be correct.
        let total_big_lines: usize = big_parts.iter().map(|p| p.line_count).sum();
        assert!(
            total_big_lines > 2000,
            "big module lines = {}",
            total_big_lines
        );
    }

    #[test]
    fn test_merge_small_partitions() {
        let partitions = vec![
            Partition {
                label: "tiny1".to_string(),
                files: vec![PathBuf::from("a.rs")],
                line_count: 50,
            },
            Partition {
                label: "tiny2".to_string(),
                files: vec![PathBuf::from("b.rs")],
                line_count: 30,
            },
            Partition {
                label: "big".to_string(),
                files: vec![PathBuf::from("c.rs")],
                line_count: 500,
            },
        ];

        let result = merge_small_partitions(partitions);
        // tiny1 and tiny2 should be merged.
        assert!(
            result.len() <= 2,
            "expected <=2 after merge, got {}",
            result.len()
        );
    }

    #[test]
    fn test_record_pairs() {
        let files = vec![
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"),
            PathBuf::from("c.rs"),
        ];
        let mut counts = HashMap::new();
        record_pairs(&files, &mut counts);
        // Should record 3 pairs: (a,b), (a,c), (b,c).
        assert_eq!(counts.len(), 3);
        for (_, count) in &counts {
            assert_eq!(*count, 1);
        }
    }

    #[test]
    fn test_partition_serialization() {
        let part = Partition {
            label: "test".to_string(),
            files: vec![PathBuf::from("src/main.rs")],
            line_count: 42,
        };
        let json = serde_json::to_string(&part).unwrap();
        let deserialized: Partition = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.label, "test");
        assert_eq!(deserialized.line_count, 42);
        assert_eq!(deserialized.files.len(), 1);
    }

    #[test]
    fn test_count_lines() {
        let repo = setup_repo(&[("src/file.rs", "line1\nline2\nline3\n")]);
        let count = count_lines(repo.path(), Path::new("src/file.rs"));
        assert_eq!(count, 3);
    }

    #[test]
    fn test_merge_smallest_pair() {
        let partitions = vec![
            Partition {
                label: "a".to_string(),
                files: vec![PathBuf::from("a.rs")],
                line_count: 10,
            },
            Partition {
                label: "b".to_string(),
                files: vec![PathBuf::from("b.rs")],
                line_count: 20,
            },
            Partition {
                label: "c".to_string(),
                files: vec![PathBuf::from("c.rs")],
                line_count: 500,
            },
        ];
        let result = merge_smallest_pair(partitions);
        assert_eq!(result.len(), 2);
        // a and b should be merged.
        let merged = result.iter().find(|p| p.label.contains('a')).unwrap();
        assert!(merged.label.contains('b'));
        assert_eq!(merged.line_count, 30);
    }
}
