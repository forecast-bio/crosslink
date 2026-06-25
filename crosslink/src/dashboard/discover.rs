//! Local filesystem discovery for crosslink-enabled repositories
//! (GH #429 followup — #705).
//!
//! Walks a set of root directories looking for git repositories with
//! a `.crosslink/` directory at the repo root. Cheap, purely local —
//! no network calls, no GitHub PAT required — so it complements the
//! online org-enumeration path in [`super::github_api`].
//!
//! Design rationale: we don't walk the entire filesystem; we walk
//! specific roots (default: `$HOME`) up to a bounded depth, skipping
//! known-noise directories (`node_modules`, `target`, `.venv`, …).
//! That keeps the scan fast (<1s for a typical dev home dir) and
//! avoids false positives from vendored third-party code.
//!
//! Detection signal: a directory is a discovered repo when it has
//! **both** a `.git` entry (file or dir — worktrees get a file) and
//! a `.crosslink/` directory. The `.crosslink/` presence is what
//! makes it "crosslink-enabled"; a plain git clone without crosslink
//! init produces no hit.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::db::DashboardDb;

/// Default walk depth when none is passed on the CLI. Deep enough to
/// catch `~/code/forecast/repo` (depth 3) and `~/work/org/repo`
/// (depth 3) without descending into every `node_modules` fossil.
pub const DEFAULT_DEPTH: usize = 4;

/// Directories skipped during the walk. Matched against the entry
/// name only (case-sensitive) — these are paths that can't
/// meaningfully contain a top-level crosslink repo and would slow
/// the scan dramatically if descended into.
const NOISE_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "out",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".venv",
    "venv",
    "env",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".cache",
    ".cargo",
    ".rustup",
    ".npm",
    ".yarn",
    ".pnpm-store",
    "Library",
    "AppData",
    ".Trash",
    ".trash",
    ".local",
    "Downloads",
    "Desktop",
    "Movies",
    "Music",
    "Pictures",
    "Videos",
];

/// One repo found during a walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRepo {
    /// Absolute canonical path to the repo root.
    pub path: PathBuf,
    /// Slug derived from `origin` remote, or `local/<basename>` if
    /// the repo has no origin remote.
    pub slug: String,
    /// True when a row for `slug` already exists in the dashboard DB.
    pub already_tracked: bool,
}

/// Options controlling the walk. A small struct because we'll likely
/// grow this (exclude lists, hidden-dir toggles, etc.).
#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    pub roots: Vec<PathBuf>,
    pub depth: usize,
}

impl DiscoverOptions {
    /// Defaults: `$HOME` (or the process CWD if `$HOME` is unset) at
    /// [`DEFAULT_DEPTH`].
    #[must_use]
    pub fn defaults() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            roots: vec![home],
            depth: DEFAULT_DEPTH,
        }
    }
}

/// Walk the given options and return every crosslink-enabled repo
/// found. Results are deduplicated + sorted by slug. Already-tracked
/// status is annotated from `db`.
///
/// # Errors
/// Returns an error only for DB open failures; filesystem errors are
/// logged and skipped (the walk must be robust to permission denials
/// on unrelated directories).
pub fn discover(db: &DashboardDb, opts: &DiscoverOptions) -> Result<Vec<DiscoveredRepo>> {
    let tracked = load_tracked_slugs(db)?;
    let mut hits: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();

    for root in &opts.roots {
        walk(root, 0, opts.depth, &mut hits, &mut visited);
    }

    let mut out: Vec<DiscoveredRepo> = hits
        .into_iter()
        .map(|(path, slug)| {
            let already_tracked = tracked.contains(&slug);
            DiscoveredRepo {
                path,
                slug,
                already_tracked,
            }
        })
        .collect();
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    Ok(out)
}

/// Returns the set of slugs currently in the `projects` table. Used
/// to flag discovered repos as already-tracked.
fn load_tracked_slugs(db: &DashboardDb) -> Result<HashSet<String>> {
    let mut stmt = db.conn.prepare("SELECT slug FROM projects")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r?);
    }
    Ok(out)
}

fn walk(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    hits: &mut BTreeMap<PathBuf, String>,
    visited: &mut HashSet<PathBuf>,
) {
    // Guard against symlink loops by recording canonical paths of
    // directories we've already descended into. If canonicalize fails
    // (dangling link, EACCES) we skip silently.
    let Ok(canon) = dir.canonicalize() else {
        return;
    };
    if !visited.insert(canon.clone()) {
        return;
    }
    if !canon.is_dir() {
        return;
    }

    // Detection: a crosslink-enabled repo has both `.git` and
    // `.crosslink/` at its root. `.git` can be a dir (normal clone) or
    // a file (worktree), so exists() — not is_dir() — is the right
    // check.
    let has_git = canon.join(".git").exists();
    let has_crosslink = canon.join(".crosslink").is_dir();
    if has_git && has_crosslink {
        if let Some(slug) = derive_slug(&canon) {
            hits.entry(canon.clone()).or_insert(slug);
        }
        // Don't descend into a crosslink repo's own subtree — its
        // contents are its own, and nested crosslink repos inside
        // would be surprising. A user that genuinely has nested ones
        // can pass --root explicitly.
        return;
    }

    if depth >= max_depth {
        return;
    }

    let Ok(entries) = std::fs::read_dir(&canon) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if should_skip(name_str.as_ref()) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // Don't follow symlinks — prevents looping and avoids
        // pulling in state that lives outside the user's expected
        // scan area.
        if file_type.is_symlink() {
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        walk(&entry.path(), depth + 1, max_depth, hits, visited);
    }
}

/// Returns `true` for directory names we never want to descend into.
fn should_skip(name: &str) -> bool {
    if NOISE_DIRS.contains(&name) {
        return true;
    }
    // Skip hidden dirs by default — nothing crosslink-managed lives
    // under a leading-dot directory (except `.crosslink` itself, but
    // we detect its sibling `.git` + `.crosslink` at the parent level,
    // never by recursing into the dot dir).
    if name.starts_with('.') {
        return true;
    }
    false
}

/// Derive `owner/repo` from `origin`, falling back to
/// `local/<basename>` when the repo has no origin remote.
fn derive_slug(repo_path: &Path) -> Option<String> {
    if let Some(slug) = origin_slug(repo_path) {
        return Some(slug);
    }
    let basename = repo_path.file_name()?.to_string_lossy().into_owned();
    Some(format!("local/{basename}"))
}

fn origin_slug(repo_path: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    super::projects::slug_from_remote_url(&url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Materialise a fake git repo at `path` with a given origin URL,
    /// and optionally a `.crosslink/` dir. Mirrors what `git init` +
    /// `crosslink init` produces minus the heavy state.
    fn mk_repo(path: &Path, origin: Option<&str>, crosslinked: bool) {
        fs::create_dir_all(path).unwrap();
        // A `.git` file is enough for detection; the worktree case
        // exercises the file path (not directory) in has_git.
        fs::write(path.join(".git"), "gitdir: /fake").unwrap();
        if crosslinked {
            fs::create_dir_all(path.join(".crosslink")).unwrap();
            fs::write(path.join(".crosslink").join("issues.db"), b"").unwrap();
        }
        if let Some(url) = origin {
            // We derive via origin_slug -> slug_from_remote_url which
            // parses the URL without spawning git. For the test, stash
            // the URL in a config-shaped file so our shell out path
            // is exercised realistically; but faster/cleaner: bypass
            // by writing a `config` that `git -C remote get-url`
            // will read. Since we don't actually init a real repo
            // here, we instead stamp the origin URL into a file our
            // test helper reads. Simpler: the tests that care about
            // slug use slug_from_remote_url directly (covered in
            // projects.rs tests) and use default local/<basename>
            // here.
            fs::write(path.join(".origin"), url).unwrap();
        }
    }

    #[test]
    fn test_discover_finds_crosslinked_repos_only() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        mk_repo(&root.join("code/forecast/alpha"), None, true); // hit
        mk_repo(&root.join("code/forecast/beta"), None, true); // hit
        mk_repo(&root.join("code/forecast/gamma"), None, false); // plain git, no crosslink
        mk_repo(&root.join("code/other/delta"), None, true); // hit
        mk_repo(&root.join("code/vendored/node_modules/package"), None, true); // skipped: noise dir ancestor
        mk_repo(&root.join(".dotdir/hidden"), None, true); // skipped: hidden dir ancestor

        let db_dir = tempdir().unwrap();
        let db = DashboardDb::open(&db_dir.path().join("d.db")).unwrap();
        let opts = DiscoverOptions {
            roots: vec![root.to_path_buf()],
            depth: 6,
        };
        let hits = discover(&db, &opts).unwrap();

        // Exactly 3 hits, slugs are local/<basename> since no origin.
        let slugs: Vec<_> = hits.iter().map(|h| h.slug.as_str()).collect();
        assert_eq!(slugs.len(), 3, "unexpected hits: {hits:?}");
        assert!(slugs.contains(&"local/alpha"));
        assert!(slugs.contains(&"local/beta"));
        assert!(slugs.contains(&"local/delta"));
        for h in &hits {
            assert!(!h.already_tracked, "empty DB — nothing tracked yet");
        }
    }

    #[test]
    fn test_discover_respects_depth() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        // depth 1: root/repo — hit at depth 1
        mk_repo(&root.join("repo1"), None, true);
        // depth 3: root/a/b/c/repo — needs --depth >= 4 to reach
        mk_repo(&root.join("a/b/c/repo2"), None, true);

        let db_dir = tempdir().unwrap();
        let db = DashboardDb::open(&db_dir.path().join("d.db")).unwrap();

        let shallow = discover(
            &db,
            &DiscoverOptions {
                roots: vec![root.to_path_buf()],
                depth: 2,
            },
        )
        .unwrap();
        let deep = discover(
            &db,
            &DiscoverOptions {
                roots: vec![root.to_path_buf()],
                depth: 5,
            },
        )
        .unwrap();

        let shallow_slugs: Vec<_> = shallow.iter().map(|h| h.slug.as_str()).collect();
        let deep_slugs: Vec<_> = deep.iter().map(|h| h.slug.as_str()).collect();
        assert!(shallow_slugs.contains(&"local/repo1"));
        assert!(!shallow_slugs.contains(&"local/repo2"));
        assert!(deep_slugs.contains(&"local/repo1"));
        assert!(deep_slugs.contains(&"local/repo2"));
    }

    #[test]
    fn test_discover_flags_already_tracked() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        mk_repo(&root.join("repo1"), None, true);
        mk_repo(&root.join("repo2"), None, true);

        let db_dir = tempdir().unwrap();
        let db = DashboardDb::open(&db_dir.path().join("d.db")).unwrap();
        db.conn
            .execute(
                "INSERT INTO projects (slug, clone_path, default_branch, status, added_at)
                 VALUES ('local/repo1', '/tmp/x', 'main', 'active', '2026-04-21T00:00:00Z')",
                [],
            )
            .unwrap();

        let hits = discover(
            &db,
            &DiscoverOptions {
                roots: vec![root.to_path_buf()],
                depth: 3,
            },
        )
        .unwrap();

        let r1 = hits.iter().find(|h| h.slug == "local/repo1").unwrap();
        let r2 = hits.iter().find(|h| h.slug == "local/repo2").unwrap();
        assert!(r1.already_tracked);
        assert!(!r2.already_tracked);
    }

    #[test]
    fn test_discover_does_not_descend_into_a_hit() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // Parent is crosslinked; nested repo beneath it would normally
        // be found, but we deliberately stop descent at a hit.
        mk_repo(&root.join("outer"), None, true);
        mk_repo(&root.join("outer/nested-repo"), None, true);

        let db_dir = tempdir().unwrap();
        let db = DashboardDb::open(&db_dir.path().join("d.db")).unwrap();
        let hits = discover(
            &db,
            &DiscoverOptions {
                roots: vec![root.to_path_buf()],
                depth: 5,
            },
        )
        .unwrap();

        let slugs: Vec<_> = hits.iter().map(|h| h.slug.as_str()).collect();
        assert_eq!(slugs, vec!["local/outer"]);
    }

    #[test]
    fn test_should_skip_catches_noise_and_hidden() {
        assert!(should_skip("node_modules"));
        assert!(should_skip("target"));
        assert!(should_skip(".venv"));
        assert!(should_skip(".local"));
        assert!(should_skip(".hidden-whatever"));
        assert!(!should_skip("code"));
        assert!(!should_skip("work"));
    }
}
