use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ContainerCommands;

pub fn run(command: ContainerCommands) -> Result<()> {
    match command {
        ContainerCommands::Build {
            force,
            tag,
            dockerfile,
        } => build(force, tag.as_deref(), dockerfile.as_deref()),
        ContainerCommands::Start {
            worktree,
            name,
            prompt,
            issue,
            memory,
        } => {
            let path = PathBuf::from(&worktree);
            start(
                &path,
                name.as_deref(),
                prompt.as_deref(),
                issue,
                memory.as_deref(),
            )
        }
        ContainerCommands::Ps => ps(),
        ContainerCommands::Logs { name, follow, tail } => logs(&name, follow, tail),
        ContainerCommands::Stop { name } => stop(&name),
        ContainerCommands::Rm { name } => rm(&name),
        ContainerCommands::Kill { name } => kill(&name),
        ContainerCommands::Shell { name } => shell(&name),
        ContainerCommands::Snapshot { name, tag } => snapshot(&name, tag.as_deref()),
    }
}

// GHCR-namespaced image name so this command composes with `crosslink kickoff
// run --container docker|podman` (which defaults to the same registry path).
// Built images, lookup paths, and snapshot tags all live under this name.
const IMAGE_NAME: &str = "ghcr.io/forecast-bio/crosslink-agent";
// Default tag when starting a container or checking staleness — matches
// kickoff's `--image` default so a `docker pull` of the published image
// satisfies both code paths.
const IMAGE_TAG: &str = "latest";
// Default tag emitted by `crosslink container build`. Distinct from
// IMAGE_TAG so a local rebuild doesn't shadow a pulled `:latest` —
// matches the `:local` convention used by the `just build-image` recipe.
const BUILD_DEFAULT_TAG: &str = "local";
const CONTAINER_PREFIX: &str = "crosslink-task-";
const LABEL_AGENT: &str = "crosslink-agent=true";

const DOCKERFILE: &str = include_str!("../../resources/container/Dockerfile");
const ENTRYPOINT: &str = include_str!("../../resources/container/entrypoint.sh");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check if Docker is available and the daemon is running.
pub fn docker_available() -> bool {
    Command::new("docker")
        .args(["info"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Find the crosslink binary path for copying into the build context.
fn find_crosslink_binary() -> Result<PathBuf> {
    std::env::current_exe().context("Could not determine crosslink binary path")
}

/// Compute a SHA-256 hash of a file (first 16 hex chars for brevity).
fn file_hash(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    // Read first 64KB — enough to detect changes without hashing a 50MB debug binary
    let mut buf = vec![0u8; 65536];
    let n = file.read(&mut buf)?;
    buf.truncate(n);

    // Simple FNV-1a hash (no external dep needed)
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in &buf {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    Ok(format!("{hash:016x}"))
}

/// Resolve the main repo root (handles worktrees).
fn resolve_repo_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("Failed to run git rev-parse")?;
    if !output.status.success() {
        bail!("Not in a git repository");
    }
    let path = String::from_utf8(output.stdout)?.trim().to_string();
    Ok(PathBuf::from(path))
}

/// Resolve the git common dir (shared .git for worktrees).
fn resolve_git_common_dir() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .context("Failed to run git rev-parse --git-common-dir")?;
    if !output.status.success() {
        bail!("Not in a git repository");
    }
    let path_str = String::from_utf8(output.stdout)?.trim().to_string();
    let path = PathBuf::from(&path_str);
    // git returns relative paths sometimes
    if path.is_absolute() {
        Ok(path)
    } else {
        let cwd = std::env::current_dir()?;
        Ok(cwd.join(path).canonicalize()?)
    }
}

/// Detect host memory in GB.
fn detect_host_memory_gb() -> Option<u64> {
    // Linux: /proc/meminfo
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if line.starts_with("MemTotal:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Ok(kb) = parts[1].parse::<u64>() {
                        return Some(kb / 1024 / 1024);
                    }
                }
            }
        }
    }
    // macOS: sysctl
    let output = Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    if output.status.success() {
        let bytes_str = String::from_utf8(output.stdout).ok()?.trim().to_string();
        let bytes: u64 = bytes_str.parse().ok()?;
        return Some(bytes / 1024 / 1024 / 1024);
    }
    None
}

/// Compute container memory limit: host RAM minus 2GB reserve, minimum 4GB.
fn compute_memory_limit(config_override: Option<&str>) -> String {
    if let Some(val) = config_override {
        if val != "auto" {
            return val.to_string();
        }
    }
    detect_host_memory_gb().map_or_else(
        || "8g".to_string(), // safe default
        |host_gb| {
            let container_gb = if host_gb > 6 {
                host_gb - 2
            } else {
                4.max(host_gb)
            };
            format!("{container_gb}g")
        },
    )
}

/// Get the image hash label if present.
fn get_image_hash() -> Option<String> {
    let output = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{index .Config.Labels \"crosslink-binary-hash\"}}",
            &format!("{IMAGE_NAME}:{IMAGE_TAG}"),
        ])
        .output()
        .ok()?;
    if output.status.success() {
        let hash = String::from_utf8(output.stdout).ok()?.trim().to_string();
        if !hash.is_empty() && hash != "<no value>" {
            return Some(hash);
        }
    }
    None
}

/// Check if the image is stale compared to the running binary.
fn check_staleness() {
    let Ok(binary_hash) = find_crosslink_binary().and_then(|p| file_hash(&p)) else {
        return;
    };
    if let Some(image_hash) = get_image_hash() {
        if image_hash != binary_hash {
            tracing::warn!(
                "container image {IMAGE_NAME}:{IMAGE_TAG} is stale relative to your installed crosslink binary. \
                 Pull the latest published image (`docker pull {IMAGE_NAME}:{IMAGE_TAG}`) or rebuild locally (`just build-image` or `crosslink container build`)."
            );
        }
    }
}

/// RAII guard to clean up a temp build directory on drop.
struct BuildDirCleanup(PathBuf);
impl Drop for BuildDirCleanup {
    fn drop(&mut self) {
        // INTENTIONAL: temp dir cleanup in Drop is best-effort — OS will reclaim it eventually
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Build the crosslink agent container image.
pub fn build(force: bool, tag: Option<&str>, dockerfile: Option<&str>) -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available. Install Docker and ensure the daemon is running.");
    }

    let tag = tag.unwrap_or(BUILD_DEFAULT_TAG);
    let image = format!("{IMAGE_NAME}:{tag}");

    // Create temp build context
    let build_path =
        std::env::temp_dir().join(format!("crosslink-container-build-{}", std::process::id()));
    std::fs::create_dir_all(&build_path).context("Failed to create temp build directory")?;
    // Clean up on exit (best-effort)
    let _cleanup = BuildDirCleanup(build_path.clone());

    // Write Dockerfile
    let dockerfile_content = if let Some(path) = dockerfile {
        std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read custom Dockerfile: {path}"))?
    } else {
        DOCKERFILE.to_string()
    };
    std::fs::write(build_path.join("Dockerfile"), &dockerfile_content)?;

    // Write entrypoint
    std::fs::write(build_path.join("entrypoint.sh"), ENTRYPOINT)?;

    // Copy crosslink binary
    let binary = find_crosslink_binary()?;
    std::fs::copy(&binary, build_path.join("crosslink"))
        .context("Failed to copy crosslink binary to build context")?;

    // Compute binary hash for staleness detection
    let binary_hash = file_hash(&binary).unwrap_or_else(|_| "unknown".to_string());

    println!("Building container image: {image}");

    let mut cmd = Command::new("docker");
    cmd.args(["build", "-t", &image]);
    cmd.args(["--label", LABEL_AGENT]);
    cmd.args(["--label", &format!("crosslink-binary-hash={binary_hash}")]);
    if force {
        cmd.arg("--no-cache");
    }
    cmd.arg(".");
    cmd.current_dir(build_path);

    let status = cmd.status().context("Failed to run docker build")?;
    if !status.success() {
        bail!("Docker build failed");
    }

    println!("Image built successfully: {image}");
    println!("Binary hash: {binary_hash}");
    Ok(())
}

/// Start a task container for a worktree.
pub fn start(
    worktree_path: &Path,
    name: Option<&str>,
    prompt_file: Option<&str>,
    issue_id: Option<i64>,
    memory: Option<&str>,
) -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available. Install Docker and ensure the daemon is running.");
    }

    check_staleness();

    let worktree_abs = std::fs::canonicalize(worktree_path)
        .with_context(|| format!("Worktree not found: {}", worktree_path.display()))?;

    // Derive container name from worktree directory name
    let worktree_slug = worktree_abs
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let container_name = name.map_or_else(
        || format!("{CONTAINER_PREFIX}{worktree_slug}"),
        ToString::to_string,
    );

    // Resolve paths
    let git_common_dir = resolve_git_common_dir()?;
    let repo_root = resolve_repo_root()?;
    let hub_cache = repo_root.join(".crosslink").join(".hub-cache");

    // Read the prompt file
    let prompt_path = prompt_file.map_or_else(|| worktree_abs.join("KICKOFF.md"), PathBuf::from);
    if !prompt_path.exists() {
        bail!(
            "Prompt file not found: {}. Write a KICKOFF.md in the worktree first.",
            prompt_path.display()
        );
    }
    let prompt = std::fs::read_to_string(&prompt_path).context("Failed to read prompt file")?;

    // Resolve credentials
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user".to_string());
    let credentials_path = PathBuf::from(&home)
        .join(".claude")
        .join(".credentials.json");
    if !credentials_path.exists() {
        bail!(
            "Claude credentials not found at {}. Run 'claude' to authenticate first.",
            credentials_path.display()
        );
    }

    // Compute resource limits
    let memory_limit = compute_memory_limit(memory);

    // Derive agent ID
    let agent_id = format!("container--{worktree_slug}");

    let image = format!("{IMAGE_NAME}:{IMAGE_TAG}");

    println!("Starting task container: {container_name}");
    println!("  Worktree: {}", worktree_abs.display());
    println!("  Memory:   {memory_limit}");
    println!("  Agent:    {agent_id}");

    let mut cmd = Command::new("docker");
    cmd.args(["run", "-d"]);
    cmd.args(["--name", &container_name]);
    cmd.args(["--label", LABEL_AGENT]);
    cmd.args(["--label", &format!("crosslink-task={worktree_slug}")]);
    if let Some(id) = issue_id {
        cmd.args(["--label", &format!("crosslink-issue={id}")]);
    }
    cmd.args(["--memory", &memory_limit]);

    // Mount worktree read-write
    cmd.args([
        "-v",
        &format!("{}:/workspaces/{}", worktree_abs.display(), worktree_slug),
    ]);

    // Mount .git common dir (shared git objects)
    cmd.args(["-v", &format!("{}:/repo/.git:rw", git_common_dir.display())]);

    // --- Worktree git fixup ---
    // A git worktree's `.git` file and the corresponding `gitdir` back-pointer
    // contain absolute host paths. Inside the container these paths don't exist.
    // We create temp files with container-side paths and bind-mount them *over*
    // the originals so the host files stay untouched.
    let dot_git_path = worktree_abs.join(".git");
    if dot_git_path.is_file() {
        // Store fixup files in the worktree's .crosslink/ dir so they persist
        // for the lifetime of the container (bind mounts need the source files alive).
        let fixup_dir = worktree_abs.join(".crosslink").join("container-git-fixup");
        std::fs::create_dir_all(&fixup_dir).context("Failed to create git fixup dir")?;

        let container_workspace = format!("/workspaces/{worktree_slug}");
        let container_gitdir = format!("/repo/.git/worktrees/{worktree_slug}");

        // Override the worktree's .git file → point to container-side gitdir
        let override_dot_git = fixup_dir.join("dot-git");
        std::fs::write(&override_dot_git, format!("gitdir: {container_gitdir}\n"))?;

        // Override the gitdir back-pointer → point to container-side worktree
        let override_gitdir = fixup_dir.join("gitdir");
        std::fs::write(&override_gitdir, format!("{container_workspace}/.git\n"))?;

        // Mount overrides (shadows originals inside container only)
        cmd.args([
            "-v",
            &format!(
                "{}:{}/.git:ro",
                override_dot_git.display(),
                container_workspace
            ),
        ]);
        cmd.args([
            "-v",
            &format!(
                "{}:{}/gitdir:ro",
                override_gitdir.display(),
                container_gitdir
            ),
        ]);
    }

    // Mount hub cache if it exists
    if hub_cache.exists() {
        cmd.args([
            "-v",
            &format!("{}:/repo/.crosslink/.hub-cache:rw", hub_cache.display()),
        ]);
    }

    // Mount credentials read-only
    cmd.args([
        "-v",
        &format!(
            "{}:/host-auth/.credentials.json:ro",
            credentials_path.display()
        ),
    ]);

    // Environment
    cmd.args(["-e", &format!("AGENT_ID={agent_id}")]);
    cmd.args(["-e", "CLAUDE_CONFIG_DIR=/home/agent/.claude"]);

    // Pass host UID/GID so the entrypoint can remap the agent user to match,
    // avoiding permission issues with bind-mounted files.
    if let Ok(uid_output) = Command::new("id").arg("-u").output() {
        if uid_output.status.success() {
            let uid = String::from_utf8_lossy(&uid_output.stdout)
                .trim()
                .to_string();
            cmd.args(["-e", &format!("HOST_UID={uid}")]);
        }
    }
    if let Ok(gid_output) = Command::new("id").arg("-g").output() {
        if gid_output.status.success() {
            let gid = String::from_utf8_lossy(&gid_output.stdout)
                .trim()
                .to_string();
            cmd.args(["-e", &format!("HOST_GID={gid}")]);
        }
    }

    // Image and command
    cmd.arg(&image);
    cmd.args([
        "claude",
        "--dangerously-skip-permissions",
        "--model",
        "opus",
        "--",
        &prompt,
    ]);

    let output = cmd.output().context("Failed to start container")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Failed to start container: {}", stderr.trim());
    }

    let container_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!(
        "  Container ID: {}...",
        &container_id[..12.min(container_id.len())]
    );

    // Write container ID to worktree for tracking
    let id_file = worktree_abs.join(".crosslink").join("container-id");
    if let Some(parent) = id_file.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&id_file, &container_id).ok();

    println!();
    println!("Task container started.");
    println!("  Check status: crosslink container ps");
    println!("  View logs:    crosslink container logs {container_name}");
    println!("  Shell in:     crosslink container shell {container_name}");
    println!("  Stop:         crosslink container stop {container_name}");

    Ok(())
}

/// List running crosslink task containers.
pub fn ps() -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available.");
    }

    let output = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("label={LABEL_AGENT}"),
            "--format",
            "table {{.Names}}\t{{.Status}}\t{{.Label \"crosslink-task\"}}\t{{.Label \"crosslink-issue\"}}",
        ])
        .output()
        .context("Failed to list containers")?;

    if !output.status.success() {
        bail!("Failed to list containers");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() || stdout.lines().count() <= 1 {
        println!("No crosslink task containers found.");
    } else {
        print!("{stdout}");
    }
    Ok(())
}

/// Stream logs from a container.
pub fn logs(name: &str, follow: bool, tail: Option<u32>) -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available.");
    }

    let mut cmd = Command::new("docker");
    cmd.args(["logs"]);
    if follow {
        cmd.arg("--follow");
    }
    let tail_str = tail.unwrap_or(100).to_string();
    cmd.args(["--tail", &tail_str]);
    cmd.arg(name);

    let status = cmd.status().context("Failed to read container logs")?;
    if !status.success() {
        bail!("Failed to read logs for container '{name}'. Does it exist?");
    }
    Ok(())
}

/// Stop a running container.
pub fn stop(name: &str) -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available.");
    }

    println!("Stopping container: {name}");
    let status = Command::new("docker")
        .args(["stop", name])
        .status()
        .context("Failed to stop container")?;

    if !status.success() {
        bail!("Failed to stop container '{name}'");
    }
    println!("Container stopped.");
    Ok(())
}

/// Remove a stopped container.
pub fn rm(name: &str) -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available.");
    }

    println!("Removing container: {name}");
    let status = Command::new("docker")
        .args(["rm", name])
        .status()
        .context("Failed to remove container")?;

    if !status.success() {
        bail!("Failed to remove container '{name}'");
    }
    println!("Container removed.");
    Ok(())
}

/// Stop and remove a container.
pub fn kill(name: &str) -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available.");
    }

    println!("Stopping and removing container: {name}");
    // INTENTIONAL: stop may fail if container is already stopped — rm -f below handles that
    let _ = Command::new("docker").args(["stop", name]).status();
    let status = Command::new("docker")
        .args(["rm", "-f", name])
        .status()
        .context("Failed to remove container")?;

    if !status.success() {
        bail!("Failed to remove container '{name}'");
    }
    println!("Container removed.");
    Ok(())
}

/// Drop into a shell inside a running container.
pub fn shell(name: &str) -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available.");
    }

    let status = Command::new("docker")
        .args(["exec", "-it", name, "/bin/bash"])
        .status()
        .context("Failed to exec into container")?;

    if !status.success() {
        bail!("Shell exited with error");
    }
    Ok(())
}

/// Snapshot a running container as a cached image.
pub fn snapshot(name: &str, tag: Option<&str>) -> Result<()> {
    if !docker_available() {
        bail!("Docker is not available.");
    }

    let tag = tag.unwrap_or("cached");
    let image = format!("{IMAGE_NAME}:{tag}");

    println!("Snapshotting container '{name}' as '{image}'...");
    let status = Command::new("docker")
        .args(["commit", name, &image])
        .status()
        .context("Failed to snapshot container")?;

    if !status.success() {
        bail!("Failed to snapshot container '{name}'");
    }
    println!("Snapshot saved: {image}");
    println!("Use with: crosslink container start --image {image}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The container subcommands MUST address the same registry-qualified
    /// image name as `crosslink kickoff run --container docker|podman`.
    /// Regressing `IMAGE_NAME` back to the bare `crosslink-agent` form
    /// silently un-composes the two code paths and re-opens GH#576.
    #[test]
    fn image_name_is_ghcr_namespaced() {
        assert_eq!(IMAGE_NAME, "ghcr.io/forecast-bio/crosslink-agent");
        assert_eq!(
            IMAGE_NAME,
            crate::commands::kickoff::DEFAULT_AGENT_IMAGE
                .rsplit_once(':')
                .map_or(IMAGE_NAME, |(name, _)| name),
            "container.rs IMAGE_NAME diverged from kickoff DEFAULT_AGENT_IMAGE — \
             re-opens the GH#576 compose-failure between `crosslink container build` \
             and `crosslink kickoff run --container …`"
        );
    }

    /// `build()` must default to a tag distinct from the lookup tag so a
    /// local rebuild doesn't shadow a `docker pull`ed `:latest`.
    #[test]
    fn build_default_tag_is_distinct_from_lookup_tag() {
        assert_eq!(BUILD_DEFAULT_TAG, "local");
        assert_ne!(
            BUILD_DEFAULT_TAG, IMAGE_TAG,
            "BUILD_DEFAULT_TAG and IMAGE_TAG must differ — otherwise `crosslink container build` \
             clobbers the published `:latest` users pulled from GHCR"
        );
    }
}
