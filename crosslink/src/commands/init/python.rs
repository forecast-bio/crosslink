//! Python toolchain detection and cpitd installation.

use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

/// Detect the Python invocation prefix for hook commands based on project toolchain markers.
///
/// Checks (in priority order):
/// 1. `uv.lock` or `pyproject.toml` with `[tool.uv]` → `"uv run python3"`
/// 2. `poetry.lock` or `pyproject.toml` with `[tool.poetry]` → `"poetry run python3"`
/// 3. `.venv/` directory → `".venv/bin/python3"`
/// 4. `Pipfile` or `Pipfile.lock` → `"pipenv run python3"`
/// 5. Fallback → `"python3"`
pub fn detect_python_prefix(project_root: &Path) -> String {
    // 1. uv: check uv.lock or [tool.uv] in pyproject.toml
    if project_root.join("uv.lock").exists() {
        return "uv run python3".to_string();
    }
    if let Some(ref pyproject) = read_pyproject(project_root) {
        if pyproject.contains("[tool.uv]") {
            return "uv run python3".to_string();
        }
    }

    // 2. poetry: check poetry.lock or [tool.poetry] in pyproject.toml
    if project_root.join("poetry.lock").exists() {
        return "poetry run python3".to_string();
    }
    if let Some(ref pyproject) = read_pyproject(project_root) {
        if pyproject.contains("[tool.poetry]") {
            return "poetry run python3".to_string();
        }
    }

    // 3. local venv
    if project_root.join(".venv").is_dir() {
        if cfg!(target_os = "windows") {
            return ".venv\\Scripts\\python.exe".to_string();
        }
        return ".venv/bin/python3".to_string();
    }

    // 4. pipenv
    if project_root.join("Pipfile").exists() || project_root.join("Pipfile.lock").exists() {
        return "pipenv run python3".to_string();
    }

    // 5. system default
    "python3".to_string()
}

/// Read pyproject.toml contents, returning None if it doesn't exist or can't be read.
fn read_pyproject(project_root: &Path) -> Option<String> {
    fs::read_to_string(project_root.join("pyproject.toml")).ok()
}

/// Check if cpitd is already available on PATH.
fn cpitd_is_installed() -> bool {
    std::process::Command::new("cpitd")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const CPITD_REPO_URL: &str = "https://github.com/scythia-marrow/cpitd.git";

/// Install cpitd using the detected Python toolchain.
/// Returns Ok(true) if installed, Ok(false) if already present, Err on failure.
///
/// Tries `pip install cpitd` first (PyPI). If that fails, falls back to
/// cloning the git repo into a temp directory and installing from source.
/// Result of cpitd installation attempt.
pub(super) enum CpitdResult {
    AlreadyInstalled,
    InstalledFromPypi,
    InstalledFromSource,
}

pub(super) fn install_cpitd(python_prefix: &str) -> Result<CpitdResult> {
    if cpitd_is_installed() {
        return Ok(CpitdResult::AlreadyInstalled);
    }

    // First attempt: install from PyPI
    let pypi_result = install_cpitd_from_pypi(python_prefix);
    if let Ok(true) = pypi_result {
        return Ok(CpitdResult::InstalledFromPypi);
    }

    // Second attempt: clone repo and install from source
    match install_cpitd_from_source(python_prefix) {
        Ok(true) => Ok(CpitdResult::InstalledFromSource),
        Ok(false) => Ok(CpitdResult::AlreadyInstalled),
        Err(e) => Err(e),
    }
}

/// Try installing cpitd from PyPI via pip/uv/poetry.
fn install_cpitd_from_pypi(python_prefix: &str) -> Result<bool> {
    if python_prefix.starts_with("uv ") {
        return run_install_command("uv", &["pip", "install", "cpitd"]);
    }
    if python_prefix.starts_with("poetry ") {
        return run_install_command("poetry", &["add", "--group", "dev", "cpitd"]);
    }
    if python_prefix.starts_with(".venv/") || python_prefix.starts_with(".venv\\") {
        let pip = python_prefix
            .replace("python3", "pip")
            .replace("python.exe", "pip.exe")
            .replace("python", "pip");
        return run_install_command(&pip, &["install", "cpitd"]);
    }
    if python_prefix.starts_with("pipenv ") {
        return run_install_command("pipenv", &["install", "--dev", "cpitd"]);
    }

    // Fallback: system python
    run_install_command("python3", &["-m", "pip", "install", "cpitd"])
}

/// Clone the cpitd repo to a temp directory and install from source.
fn install_cpitd_from_source(python_prefix: &str) -> Result<bool> {
    let tmp_dir = std::env::temp_dir().join("crosslink-cpitd-install");

    // Clean up any previous failed attempt
    if tmp_dir.exists() {
        // INTENTIONAL: cleanup of previous failed attempt is best-effort — clone below will fail if stale dir remains
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    // Clone the repo
    let clone_output = std::process::Command::new("git")
        .args(["clone", "--depth", "1", CPITD_REPO_URL])
        .arg(&tmp_dir)
        .output()
        .context("Failed to run git clone for cpitd")?;

    if !clone_output.status.success() {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);
        // INTENTIONAL: temp dir cleanup on failure is best-effort — OS will reclaim it eventually
        let _ = fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("git clone failed: {}", stderr.trim());
    }

    let tmp_dir_str = tmp_dir.to_string_lossy();

    // Install from the cloned directory
    let result = if python_prefix.starts_with("uv ") {
        run_install_command("uv", &["pip", "install", &tmp_dir_str])
    } else if python_prefix.starts_with("poetry ") {
        // Poetry can't install from arbitrary paths into dev deps easily,
        // fall back to pip inside the poetry env
        run_install_command("poetry", &["run", "pip", "install", &tmp_dir_str])
    } else if python_prefix.starts_with(".venv/") || python_prefix.starts_with(".venv\\") {
        let pip = python_prefix
            .replace("python3", "pip")
            .replace("python.exe", "pip.exe")
            .replace("python", "pip");
        run_install_command(&pip, &["install", &tmp_dir_str])
    } else if python_prefix.starts_with("pipenv ") {
        run_install_command("pipenv", &["run", "pip", "install", &tmp_dir_str])
    } else {
        run_install_command("python3", &["-m", "pip", "install", &tmp_dir_str])
    };

    // INTENTIONAL: temp dir cleanup is best-effort — OS will reclaim it eventually
    let _ = fs::remove_dir_all(&tmp_dir);

    result
}

fn run_install_command(program: &str, args: &[&str]) -> Result<bool> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run {} {}", program, args.join(" ")))?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("cpitd install failed: {}", stderr.trim());
    }
}
