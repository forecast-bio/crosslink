// Mission control: tmux dashboard showing all active agents
use anyhow::{bail, Context, Result};
use std::io::IsTerminal;
use std::path::Path;
use std::process::Command;

use super::kickoff::{command_available, tmux_session_name};

const MC_SESSION: &str = "mission-control";

/// An active agent discovered from worktrees and runtime inspection.
struct ActiveAgent {
    /// Human-readable name (worktree slug)
    slug: String,
    /// How to attach: tmux session name or container log command
    source: AgentSource,
}

enum AgentSource {
    /// Agent running in a tmux session
    Tmux(String),
    /// Agent running in a Docker/Podman container
    Container { runtime: String, name: String },
}

/// Discover all active agents by scanning worktrees and checking runtimes.
fn discover_agents(crosslink_dir: &Path) -> Result<Vec<ActiveAgent>> {
    let root = crosslink_dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine repo root"))?;

    let worktrees_dir = root.join(".worktrees");
    let mut agents = Vec::new();

    if !worktrees_dir.is_dir() {
        return Ok(agents);
    }

    let mut entries: Vec<_> = std::fs::read_dir(&worktrees_dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let slug = entry.file_name().to_string_lossy().to_string();

        // Check tmux session
        let session_name = tmux_session_name(&slug);
        if tmux_session_exists(&session_name) {
            agents.push(ActiveAgent {
                slug: slug.clone(),
                source: AgentSource::Tmux(session_name),
            });
            continue;
        }

        // Check container runtimes
        let container_name = format!("crosslink-agent-driver--{slug}");
        for runtime in &["docker", "podman"] {
            if !command_available(runtime) {
                continue;
            }
            if container_running(runtime, &container_name) {
                agents.push(ActiveAgent {
                    slug: slug.clone(),
                    source: AgentSource::Container {
                        runtime: runtime.to_string(),
                        name: container_name.clone(),
                    },
                });
                break;
            }
        }
    }

    Ok(agents)
}

fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn container_running(runtime: &str, name: &str) -> bool {
    Command::new(runtime)
        .args(["inspect", "--format", "{{.State.Running}}", name])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// Build the command string to view an agent's output in a pane.
fn pane_command(agent: &ActiveAgent) -> String {
    match &agent.source {
        AgentSource::Tmux(session) => {
            // Refresh the pane every 2 seconds with the agent's latest output.
            // `capture-pane -p` dumps the visible content; the loop keeps it live.
            format!(
                "while tmux has-session -t {session} 2>/dev/null; do clear; tmux capture-pane -t {session} -p -S -50; sleep 2; done; echo 'Session ended.'"
            )
        }
        AgentSource::Container { runtime, name } => {
            format!("{runtime} logs -f --tail 200 {name}")
        }
    }
}

/// Main entry point: `crosslink mc`
pub fn run(crosslink_dir: &Path, layout: &str) -> Result<()> {
    // Validate layout
    let tmux_layout = match layout {
        "tiled" => "tiled",
        "even-horizontal" | "horizontal" => "even-horizontal",
        "even-vertical" | "vertical" => "even-vertical",
        _ => bail!("Unknown layout '{layout}'. Use: tiled, even-horizontal, even-vertical"),
    };

    if !command_available("tmux") {
        bail!("tmux is required for mission control but was not found on PATH");
    }

    let agents = discover_agents(crosslink_dir)?;

    if agents.is_empty() {
        println!("No active agents found.");
        println!("Launch agents with: crosslink kickoff run \"<description>\"");
        return Ok(());
    }

    println!(
        "Found {} active agent{}:",
        agents.len(),
        if agents.len() == 1 { "" } else { "s" }
    );
    for a in &agents {
        let runtime = match &a.source {
            AgentSource::Tmux(s) => format!("tmux:{s}"),
            AgentSource::Container { runtime, name } => format!("{runtime}:{name}"),
        };
        println!("  {} ({})", a.slug, runtime);
    }
    println!();

    // Kill existing mission-control session to avoid duplicates
    if tmux_session_exists(MC_SESSION) {
        // INTENTIONAL: kill-session failure is non-fatal — new-session below will fail if session still exists
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", MC_SESSION])
            .output();
    }

    // Create the mission control session with the first agent
    let first_cmd = pane_command(&agents[0]);
    let output = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            MC_SESSION,
            "-n",
            "agents",
            "bash",
            "-c",
            &first_cmd,
        ])
        .output()
        .context("Failed to create mission-control tmux session")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to create mission-control session: {}",
            stderr.trim()
        );
    }

    // INTENTIONAL: pane title is cosmetic — failure doesn't affect functionality
    let _ = Command::new("tmux")
        .args([
            "select-pane",
            "-t",
            &format!("{MC_SESSION}:0.0"),
            "-T",
            &agents[0].slug,
        ])
        .output();

    // Add remaining agents as split panes
    for agent in agents.iter().skip(1) {
        let cmd = pane_command(agent);
        let output = Command::new("tmux")
            .args([
                "split-window",
                "-t",
                &format!("{MC_SESSION}:0"),
                "bash",
                "-c",
                &cmd,
            ])
            .output()
            .context("Failed to add agent pane")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "Warning: failed to add pane for {}: {}",
                agent.slug,
                stderr.trim()
            );
            continue;
        }

        // INTENTIONAL: pane title is cosmetic — failure doesn't affect functionality
        let _ = Command::new("tmux")
            .args([
                "select-pane",
                "-t",
                &format!("{MC_SESSION}:0"),
                "-T",
                &agent.slug,
            ])
            .output();
    }

    // INTENTIONAL: layout and pane border config are cosmetic — failure doesn't affect functionality
    let _ = Command::new("tmux")
        .args([
            "select-layout",
            "-t",
            &format!("{MC_SESSION}:0"),
            tmux_layout,
        ])
        .output();

    let _ = Command::new("tmux")
        .args(["set-option", "-t", MC_SESSION, "pane-border-status", "top"])
        .output();

    let _ = Command::new("tmux")
        .args([
            "set-option",
            "-t",
            MC_SESSION,
            "pane-border-format",
            " #{pane_title} ",
        ])
        .output();

    println!("Mission control ready.");
    println!("  tmux attach -t {MC_SESSION}");

    // If we're not inside tmux already and have a terminal, attach automatically
    if std::env::var("TMUX").is_err() && std::io::stdout().is_terminal() {
        // INTENTIONAL: attach failure is non-fatal — user can manually attach via printed command
        let _ = Command::new("tmux")
            .args(["attach", "-t", MC_SESSION])
            .status();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pane_command_tmux() {
        let agent = ActiveAgent {
            slug: "test-agent".to_string(),
            source: AgentSource::Tmux("feat-test-agent".to_string()),
        };
        let cmd = pane_command(&agent);
        assert!(cmd.contains("feat-test-agent"));
        assert!(cmd.contains("capture-pane"));
        assert!(cmd.contains("has-session"));
    }

    #[test]
    fn test_pane_command_container() {
        let agent = ActiveAgent {
            slug: "test-agent".to_string(),
            source: AgentSource::Container {
                runtime: "docker".to_string(),
                name: "crosslink-agent-test".to_string(),
            },
        };
        let cmd = pane_command(&agent);
        assert_eq!(cmd, "docker logs -f --tail 200 crosslink-agent-test");
    }

    #[test]
    fn test_discover_agents_no_worktrees() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        let agents = discover_agents(&crosslink_dir).unwrap();
        assert!(agents.is_empty());
    }

    #[test]
    fn test_discover_agents_worktrees_no_active() {
        let dir = tempfile::tempdir().unwrap();
        let crosslink_dir = dir.path().join(".crosslink");
        std::fs::create_dir_all(&crosslink_dir).unwrap();

        // Create a worktree directory but no tmux session or container
        let wt_dir = dir.path().join(".worktrees").join("some-feature");
        std::fs::create_dir_all(&wt_dir).unwrap();

        let agents = discover_agents(&crosslink_dir).unwrap();
        assert!(agents.is_empty());
    }
}
