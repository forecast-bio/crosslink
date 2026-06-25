//! Python toolchain detection and cpitd installation.

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
        .is_ok_and(|o| o.status.success())
}

/// Check whether a program is resolvable on PATH (used to gate pipx).
fn program_on_path(program: &str) -> bool {
    // `--version` is universally supported by pip/pipx; a successful exit (or
    // even a clean run) proves the program resolves. We only care about
    // resolvability, not the exact version.
    std::process::Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

const CPITD_REPO_URL: &str = "https://github.com/scythia-marrow/cpitd.git";

/// Classification of why an install command failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InstallFailureKind {
    /// pip refused because the environment is PEP 668 externally-managed.
    ExternallyManaged,
    /// Any other failure (network, missing program, build error, …).
    Other,
}

/// Classify an install command's stderr to detect the PEP 668
/// "externally-managed-environment" condition.
///
/// On modern Debian/Ubuntu/Homebrew Python builds, `pip install` (and
/// `pip install --user`) refuse to run against the system interpreter and
/// print an `error: externally-managed-environment` block. We detect that
/// marker so the final guidance can be specific (pipx / venv) rather than
/// the useless "pip install cpitd" suggestion from before.
pub(super) fn classify_install_failure(stderr: &str) -> InstallFailureKind {
    let lowered = stderr.to_ascii_lowercase();
    if lowered.contains("externally-managed-environment")
        || lowered.contains("externally managed environment")
    {
        InstallFailureKind::ExternallyManaged
    } else {
        InstallFailureKind::Other
    }
}

/// A single `PyPI` install candidate: the program plus its argument vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct InstallCandidate {
    pub program: String,
    pub args: Vec<String>,
}

impl InstallCandidate {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_string(),
            args: args.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

/// Derive the pip executable path from a `.venv` python prefix.
fn venv_pip(python_prefix: &str) -> String {
    python_prefix
        .replace("python3", "pip")
        .replace("python.exe", "pip.exe")
        .replace("python", "pip")
}

/// Build the ordered list of `PyPI` install candidates for `cpitd`, given the
/// detected python prefix and whether `pipx` is resolvable on PATH.
///
/// Ordering rationale (system-python case):
///   1. `pipx install cpitd` — the canonical PEP 668 answer; installs into an
///      isolated venv and exposes the entry point on PATH. Only offered when
///      pipx is actually present (PEP 668 distros don't ship it by default).
///   2. `pip install --user cpitd` — works on non-PEP-668 systems; on PEP 668
///      systems it ALSO fails with externally-managed, but trying it is cheap
///      and its failure is what produces the actionable marker.
///   3. `python3 -m pip install cpitd` — plain system install; last pip resort.
///
/// Managed toolchains (uv/poetry/pipenv/.venv) install into their own
/// environment and are never PEP 668 affected, so they get a single direct
/// candidate.
pub(super) fn build_install_candidates(
    python_prefix: &str,
    pipx_available: bool,
) -> Vec<InstallCandidate> {
    if python_prefix.starts_with("uv ") {
        return vec![InstallCandidate::new("uv", &["pip", "install", "cpitd"])];
    }
    if python_prefix.starts_with("poetry ") {
        return vec![InstallCandidate::new(
            "poetry",
            &["add", "--group", "dev", "cpitd"],
        )];
    }
    if python_prefix.starts_with(".venv/") || python_prefix.starts_with(".venv\\") {
        let pip = venv_pip(python_prefix);
        return vec![InstallCandidate::new(&pip, &["install", "cpitd"])];
    }
    if python_prefix.starts_with("pipenv ") {
        return vec![InstallCandidate::new(
            "pipenv",
            &["install", "--dev", "cpitd"],
        )];
    }

    // System python: pipx first (when present), then pip --user, then plain pip.
    let mut candidates = Vec::new();
    if pipx_available {
        candidates.push(InstallCandidate::new("pipx", &["install", "cpitd"]));
    }
    candidates.push(InstallCandidate::new(
        "python3",
        &["-m", "pip", "install", "--user", "cpitd"],
    ));
    candidates.push(InstallCandidate::new(
        "python3",
        &["-m", "pip", "install", "cpitd"],
    ));
    candidates
}

/// Result of cpitd installation attempt.
pub(super) enum CpitdResult {
    AlreadyInstalled,
    InstalledFromPypi,
    InstalledFromSource,
}

/// Detailed failure carried out of `install_cpitd` so the caller can render
/// actionable guidance when (and only when) PEP 668 blocked every attempt.
pub(super) struct CpitdInstallError {
    /// The last underlying error message (from the final failed candidate).
    pub message: String,
    /// Whether any attempt failed specifically due to PEP 668.
    pub externally_managed: bool,
}

impl std::fmt::Display for CpitdInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// Install cpitd using the detected Python toolchain.
///
/// Chain (system python): `pipx install cpitd` (when pipx is on PATH) ->
/// `pip install --user cpitd` -> `pip install cpitd` -> git-source fallback.
/// Managed toolchains (uv/poetry/pipenv/.venv) install directly into their
/// own environment. Records whether any attempt hit PEP 668
/// externally-managed so the caller can show specific guidance.
pub(super) fn install_cpitd(
    python_prefix: &str,
) -> std::result::Result<CpitdResult, CpitdInstallError> {
    if cpitd_is_installed() {
        return Ok(CpitdResult::AlreadyInstalled);
    }

    let pipx_available = program_on_path("pipx");
    let candidates = build_install_candidates(python_prefix, pipx_available);

    let mut saw_externally_managed = false;
    // Track the most actionable pip failure message; the PEP 668 block is far
    // more useful to surface than a later git-clone error.
    let mut pep668_message: Option<String> = None;

    for candidate in &candidates {
        let arg_refs: Vec<&str> = candidate.args.iter().map(String::as_str).collect();
        match run_install_command(&candidate.program, &arg_refs) {
            Ok(true) => return Ok(CpitdResult::InstalledFromPypi),
            Ok(false) => {}
            Err(failure) => {
                if failure.kind == InstallFailureKind::ExternallyManaged {
                    saw_externally_managed = true;
                    pep668_message.get_or_insert(failure.message);
                }
            }
        }
    }

    // Final attempt: clone repo and install from source (still subject to the
    // same PEP 668 constraint when installing into system python, but free for
    // managed toolchains and offline-with-cache cases).
    match install_cpitd_from_source(python_prefix) {
        Ok(true) => Ok(CpitdResult::InstalledFromSource),
        Ok(false) => Ok(CpitdResult::AlreadyInstalled),
        Err(failure) => {
            if failure.kind == InstallFailureKind::ExternallyManaged {
                saw_externally_managed = true;
                if pep668_message.is_none() {
                    pep668_message = Some(failure.message.clone());
                }
            }
            // Prefer the PEP 668 message when we saw one — it is what the
            // actionable guidance keys off — otherwise the final failure.
            let message = pep668_message.unwrap_or(failure.message);
            Err(CpitdInstallError {
                message,
                externally_managed: saw_externally_managed,
            })
        }
    }
}

/// A classified install-command failure (program failed to run, or it ran
/// and exited non-zero with stderr we classify for PEP 668).
struct CommandFailure {
    message: String,
    kind: InstallFailureKind,
}

/// Clone the cpitd repo to a temp directory and install from source.
fn install_cpitd_from_source(python_prefix: &str) -> std::result::Result<bool, CommandFailure> {
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
        .map_err(|e| CommandFailure {
            message: format!("Failed to run git clone for cpitd: {e}"),
            kind: InstallFailureKind::Other,
        })?;

    if !clone_output.status.success() {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);
        // INTENTIONAL: temp dir cleanup on failure is best-effort — OS will reclaim it eventually
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(CommandFailure {
            message: format!("git clone failed: {}", stderr.trim()),
            kind: InstallFailureKind::Other,
        });
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
        let pip = venv_pip(python_prefix);
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

fn run_install_command(program: &str, args: &[&str]) -> std::result::Result<bool, CommandFailure> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| CommandFailure {
            message: format!("Failed to run {} {}: {e}", program, args.join(" ")),
            kind: InstallFailureKind::Other,
        })?;

    if output.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let kind = classify_install_failure(&stderr);
        Err(CommandFailure {
            message: format!("cpitd install failed: {}", stderr.trim()),
            kind,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const PEP668_STDERR: &str = "error: externally-managed-environment\n\
        \n\
        × This environment is externally managed\n\
        ╰─> To install Python packages system-wide, try apt install ...";

    #[test]
    fn classify_detects_pep668() {
        assert_eq!(
            classify_install_failure(PEP668_STDERR),
            InstallFailureKind::ExternallyManaged
        );
    }

    #[test]
    fn classify_detects_pep668_case_insensitive() {
        assert_eq!(
            classify_install_failure("ERROR: Externally-Managed-Environment blah"),
            InstallFailureKind::ExternallyManaged
        );
        // Spaced variant some tooling prints.
        assert_eq!(
            classify_install_failure("this environment is externally managed environment"),
            InstallFailureKind::ExternallyManaged
        );
    }

    #[test]
    fn classify_other_failures_are_other() {
        assert_eq!(
            classify_install_failure("ERROR: Could not find a version that satisfies cpitd"),
            InstallFailureKind::Other
        );
        assert_eq!(
            classify_install_failure("Network is unreachable"),
            InstallFailureKind::Other
        );
        assert_eq!(classify_install_failure(""), InstallFailureKind::Other);
    }

    #[test]
    fn system_chain_pipx_first_then_pip_user_then_pip() {
        let candidates = build_install_candidates("python3", true);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].program, "pipx");
        assert_eq!(candidates[0].args, vec!["install", "cpitd"]);
        // pip --user before plain pip.
        assert_eq!(candidates[1].program, "python3");
        assert_eq!(
            candidates[1].args,
            vec!["-m", "pip", "install", "--user", "cpitd"]
        );
        assert_eq!(candidates[2].program, "python3");
        assert_eq!(candidates[2].args, vec!["-m", "pip", "install", "cpitd"]);
    }

    #[test]
    fn system_chain_skips_pipx_when_absent() {
        let candidates = build_install_candidates("python3", false);
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|c| c.program != "pipx"));
        assert_eq!(
            candidates[0].args,
            vec!["-m", "pip", "install", "--user", "cpitd"]
        );
        assert_eq!(candidates[1].args, vec!["-m", "pip", "install", "cpitd"]);
    }

    #[test]
    fn uv_toolchain_single_candidate() {
        let candidates = build_install_candidates("uv run python3", true);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].program, "uv");
        assert_eq!(candidates[0].args, vec!["pip", "install", "cpitd"]);
    }

    #[test]
    fn poetry_toolchain_single_candidate() {
        let candidates = build_install_candidates("poetry run python3", false);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].program, "poetry");
        assert_eq!(candidates[0].args, vec!["add", "--group", "dev", "cpitd"]);
    }

    #[test]
    fn pipenv_toolchain_single_candidate() {
        let candidates = build_install_candidates("pipenv run python3", true);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].program, "pipenv");
        assert_eq!(candidates[0].args, vec!["install", "--dev", "cpitd"]);
    }

    #[test]
    fn venv_toolchain_uses_venv_pip() {
        let candidates = build_install_candidates(".venv/bin/python3", true);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].program, ".venv/bin/pip");
        assert_eq!(candidates[0].args, vec!["install", "cpitd"]);
    }

    #[test]
    fn managed_toolchains_never_offer_pipx() {
        // pipx is only relevant for system python; managed envs are PEP 668 safe.
        for prefix in ["uv run python3", "poetry run python3", "pipenv run python3"] {
            let candidates = build_install_candidates(prefix, true);
            assert!(candidates.iter().all(|c| c.program != "pipx"), "{prefix}");
        }
    }
}
