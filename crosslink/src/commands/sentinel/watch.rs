use anyhow::{Context, Result};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::db::Database;

use super::config::SentinelConfig;
use super::engine;

/// Start the sentinel daemon as a background process.
pub fn start(crosslink_dir: &Path, interval: u64) -> Result<()> {
    let pid_file = crosslink_dir.join("sentinel.pid");
    let log_file = crosslink_dir.join("sentinel.log");

    // Check if already running
    if let Some(pid) = read_pid(&pid_file) {
        if is_process_running(pid) {
            println!("Sentinel already running (PID {pid})");
            return Ok(());
        }
        fs::remove_file(&pid_file).with_context(|| {
            format!(
                "Cannot remove stale sentinel PID file at {}",
                pid_file.display()
            )
        })?;
    }

    let exe = std::env::current_exe().context("Failed to get executable path")?;

    let log_handle = fs::File::create(&log_file).context("Failed to create sentinel log file")?;
    let log_handle_err = log_handle
        .try_clone()
        .context("Failed to clone log file handle")?;
    let child = Command::new(&exe)
        .arg("sentinel")
        .arg("run-daemon")
        .arg("--dir")
        .arg(crosslink_dir)
        .arg("--interval")
        .arg(interval.to_string())
        .stdin(Stdio::null())
        .stdout(log_handle)
        .stderr(log_handle_err)
        .spawn()
        .context("Failed to spawn sentinel daemon")?;

    let pid = child.id();
    fs::write(&pid_file, pid.to_string()).context("Failed to write sentinel PID file")?;

    println!("Sentinel started (PID {pid})");
    println!("  Interval: {interval} minutes");
    println!("  Log file: {}", log_file.display());
    Ok(())
}

/// Stop the sentinel daemon.
pub fn stop(crosslink_dir: &Path) -> Result<()> {
    let pid_file = crosslink_dir.join("sentinel.pid");

    let Some(pid) = read_pid(&pid_file) else {
        println!("Sentinel not running (no PID file)");
        return Ok(());
    };

    if !is_process_running(pid) {
        fs::remove_file(&pid_file).ok();
        println!("Sentinel not running (stale PID file removed)");
        return Ok(());
    }

    kill_process(pid)?;
    fs::remove_file(&pid_file).ok();
    println!("Sentinel stopped (PID {pid})");
    Ok(())
}

/// Show sentinel daemon status.
pub fn status(crosslink_dir: &Path, db: &Database) -> Result<()> {
    let pid_file = crosslink_dir.join("sentinel.pid");

    let running = if let Some(pid) = read_pid(&pid_file) {
        if is_process_running(pid) {
            println!("Sentinel running (PID {pid})");
            true
        } else {
            println!("Sentinel not running (stale PID file)");
            false
        }
    } else {
        println!("Sentinel not running");
        false
    };

    // Show in-flight agents regardless of daemon state
    let pending_dispatches = db.get_pending_dispatches()?;
    let config = SentinelConfig::load(crosslink_dir)?;
    println!(
        "  In-flight: {} / {} agents",
        pending_dispatches.len(),
        config.max_concurrent_agents
    );

    // List each in-flight agent
    for d in &pending_dispatches {
        let elapsed = super::collect::format_elapsed(&d.created_at);
        let agent = d.agent_id.as_deref().unwrap_or("unknown");
        let model = d.model_used.as_deref().unwrap_or("?");
        println!(
            "    {} — {} (attempt {}, {}, {})",
            d.signal_ref, agent, d.attempt_number, model, elapsed
        );
    }

    // Show last run
    let runs = db.list_sentinel_runs(1)?;
    if let Some(last) = runs.first() {
        let started = last
            .started_at
            .get(..19)
            .unwrap_or(&last.started_at)
            .replace('T', " ");
        println!(
            "  Last run:  {} ({} signals, {} dispatched)",
            started, last.signals_found, last.dispatched
        );
    }

    if !running && !pending_dispatches.is_empty() {
        println!(
            "  Warning: {} agent(s) in-flight but daemon not running — results won't be collected",
            pending_dispatches.len()
        );
    }

    Ok(())
}

/// Run the sentinel watch loop (called by the spawned daemon process).
pub fn run_watch_loop(crosslink_dir: &Path, interval_minutes: u64) -> Result<()> {
    let db_path = crosslink_dir.join("issues.db");
    if !db_path.exists() {
        anyhow::bail!(
            "Invalid crosslink directory: {} does not contain issues.db",
            crosslink_dir.display()
        );
    }

    let config = SentinelConfig::load(crosslink_dir)?;
    if !config.enabled {
        println!("Sentinel is disabled in hook-config.json");
        return Ok(());
    }

    let interval = Duration::from_secs(interval_minutes * 60);
    let mut backoff_multiplier: u32 = 1;

    println!("Sentinel daemon starting...");
    println!("  Watching: {}", crosslink_dir.display());
    println!("  Interval: {interval_minutes} minutes");

    // Graceful shutdown via SIGTERM/SIGINT
    let should_exit = Arc::new(AtomicBool::new(false));

    #[cfg(unix)]
    {
        let flag = Arc::clone(&should_exit);
        if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&flag))
        {
            tracing::warn!("could not register SIGTERM handler: {e}");
        }
        if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGINT, flag) {
            tracing::warn!("could not register SIGINT handler: {e}");
        }
    }

    // Zombie prevention: exit when stdin closes (parent died)
    let should_exit_stdin = Arc::clone(&should_exit);
    thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => {
                    tracing::info!("Stdin closed, sentinel shutting down");
                    should_exit_stdin.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(_) => {}
            }
        }
    });

    loop {
        if should_exit.load(Ordering::SeqCst) {
            println!("Sentinel exiting");
            break;
        }

        // Run one cycle
        match run_cycle(crosslink_dir) {
            Ok(()) => {
                backoff_multiplier = 1;
            }
            Err(e) => {
                tracing::error!("sentinel cycle failed: {e}");
                backoff_multiplier = (backoff_multiplier * 2).min(8);
            }
        }

        // Sleep with early exit check
        let sleep_duration = interval * backoff_multiplier;
        let sleep_step = Duration::from_secs(5);
        let mut slept = Duration::ZERO;
        while slept < sleep_duration {
            if should_exit.load(Ordering::SeqCst) {
                println!("Sentinel exiting");
                return Ok(());
            }
            thread::sleep(sleep_step.min(sleep_duration - slept));
            slept += sleep_step;
        }
    }

    Ok(())
}

/// Execute a single sentinel cycle within the watch loop.
fn run_cycle(crosslink_dir: &Path) -> Result<()> {
    let db = Database::open(&crosslink_dir.join("issues.db"))?;
    let config = SentinelConfig::load(crosslink_dir)?;

    // Construct SharedWriter if hub is available
    let writer = crate::shared_writer::SharedWriter::new(crosslink_dir)
        .ok()
        .flatten();

    let stats = engine::run_oneshot(
        crosslink_dir,
        &db,
        writer.as_ref(),
        &config,
        false, // not dry run
        None,  // no label filter
        true,  // quiet in daemon mode (output goes to log)
    )?;

    if stats.signals_found > 0 || stats.collected > 0 {
        println!(
            "Cycle complete at {}: {} signals, {} dispatched, {} collected",
            chrono::Utc::now().format("%H:%M:%S"),
            stats.signals_found,
            stats.dispatched,
            stats.collected,
        );
    }

    Ok(())
}

// --- Process management helpers (mirrored from daemon.rs) ---

fn read_pid(pid_file: &Path) -> Option<u32> {
    let mut file = fs::File::open(pid_file).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    contents.trim().parse().ok()
}

#[cfg(not(windows))]
fn is_process_running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout.contains(&pid.to_string())
        })
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn kill_process(pid: u32) -> Result<()> {
    Command::new("kill")
        .arg(pid.to_string())
        .status()
        .context("Failed to kill sentinel process")?;
    Ok(())
}

#[cfg(windows)]
fn kill_process(pid: u32) -> Result<()> {
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status()
        .context("Failed to kill sentinel process")?;
    Ok(())
}
