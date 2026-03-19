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
use crate::hydration::hydrate_to_sqlite;

const FLUSH_INTERVAL_SECS: u64 = 30;

pub fn start(crosslink_dir: &Path) -> Result<()> {
    let pid_file = crosslink_dir.join("daemon.pid");
    let log_file = crosslink_dir.join("daemon.log");

    // Check if daemon is already running
    if let Some(pid) = read_pid(&pid_file) {
        if is_process_running(pid) {
            println!("Daemon already running (PID {})", pid);
            return Ok(());
        }
        // INTENTIONAL: stale PID file removal is best-effort — we proceed either way
        let _ = fs::remove_file(&pid_file);
    }

    // Get the current executable path
    let exe = std::env::current_exe().context("Failed to get executable path")?;

    // Spawn the daemon process
    let log_handle = fs::File::create(&log_file).context("Failed to create log file")?;
    let log_handle_err = log_handle
        .try_clone()
        .context("Failed to clone log file handle")?;
    let child = Command::new(&exe)
        .arg("daemon")
        .arg("run")
        .arg("--dir")
        .arg(crosslink_dir)
        .stdin(Stdio::null())
        .stdout(log_handle)
        .stderr(log_handle_err)
        .spawn()
        .context("Failed to spawn daemon process")?;

    let pid = child.id();

    // Write PID file
    fs::write(&pid_file, pid.to_string()).context("Failed to write PID file")?;

    println!("Daemon started (PID {})", pid);
    println!("Log file: {}", log_file.display());
    Ok(())
}

pub fn stop(crosslink_dir: &Path) -> Result<()> {
    let pid_file = crosslink_dir.join("daemon.pid");

    let pid = match read_pid(&pid_file) {
        Some(p) => p,
        None => {
            println!("Daemon not running (no PID file)");
            return Ok(());
        }
    };

    if !is_process_running(pid) {
        fs::remove_file(&pid_file).ok();
        println!("Daemon not running (stale PID file removed)");
        return Ok(());
    }

    // Kill the process
    kill_process(pid)?;

    // Remove PID file
    fs::remove_file(&pid_file).ok();

    println!("Daemon stopped (PID {})", pid);
    Ok(())
}

pub fn status(crosslink_dir: &Path) -> Result<()> {
    let pid_file = crosslink_dir.join("daemon.pid");

    match read_pid(&pid_file) {
        Some(pid) => {
            if is_process_running(pid) {
                println!("Daemon running (PID {})", pid);
            } else {
                println!("Daemon not running (stale PID file)");
            }
        }
        None => {
            println!("Daemon not running");
        }
    }
    Ok(())
}

pub fn run_daemon(crosslink_dir: &Path) -> Result<()> {
    // Validate that this is a legitimate crosslink directory
    let db_path = crosslink_dir.join("issues.db");
    if !db_path.exists() {
        anyhow::bail!(
            "Invalid crosslink directory: {} does not contain issues.db",
            crosslink_dir.display()
        );
    }

    let session_file = crosslink_dir.join("session.json");

    println!("Daemon starting...");
    println!("Watching: {}", crosslink_dir.display());
    println!("Flush interval: {} seconds", FLUSH_INTERVAL_SECS);

    // Heartbeat counter: push every 5 cycles (5 * 30s = 2.5 min)
    let mut heartbeat_counter: u64 = 0;
    const HEARTBEAT_EVERY_N: u64 = 5;

    // Error tracking for consecutive failures
    let mut consecutive_db_failures: u32 = 0;
    let mut consecutive_sync_failures: u32 = 0;
    const FAILURE_WARN_THRESHOLD: u32 = 5;

    // Graceful shutdown: set should_exit on SIGTERM/SIGINT and stdin closure.
    let should_exit = Arc::new(AtomicBool::new(false));

    // Register signal handlers for graceful shutdown
    #[cfg(unix)]
    {
        let flag = Arc::clone(&should_exit);
        if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&flag))
        {
            tracing::warn!(
                "could not register SIGTERM handler: {e} — graceful shutdown unavailable"
            );
        }
        if let Err(e) = signal_hook::flag::register(signal_hook::consts::SIGINT, flag) {
            tracing::warn!(
                "could not register SIGINT handler: {e} — graceful shutdown unavailable"
            );
        }
    }

    // Zombie prevention: Monitor stdin for closure.
    // When the parent process (VS Code) dies, stdin will be closed.
    // This thread detects that and signals the main loop to exit.
    let should_exit_clone = Arc::clone(&should_exit);

    thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 1];
        // This will block until stdin is closed or data is received
        // When the parent dies, read() returns 0 (EOF) or an error
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => {
                    // EOF - parent closed stdin, time to exit
                    tracing::info!("Stdin closed, daemon shutting down (zombie prevention)");
                    should_exit_clone.store(true, Ordering::SeqCst);
                    break;
                }
                Err(_) => {
                    // Error reading stdin - parent likely crashed
                    tracing::info!("Stdin error, daemon shutting down (zombie prevention)");
                    should_exit_clone.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(_) => {
                    // Data received (unexpected, but continue)
                    continue;
                }
            }
        }
    });

    loop {
        // Check if we should exit (stdin closed)
        if should_exit.load(Ordering::SeqCst) {
            println!("Daemon exiting due to parent termination");
            break;
        }

        thread::sleep(Duration::from_secs(FLUSH_INTERVAL_SECS));

        // Check again after sleep
        if should_exit.load(Ordering::SeqCst) {
            println!("Daemon exiting due to parent termination");
            break;
        }

        // Auto-flush: read current session and write to session.json
        let mut active_issue_id: Option<i64> = None;
        match Database::open(&db_path) {
            Ok(db) => {
                consecutive_db_failures = 0;
                let agent_id = crate::identity::AgentConfig::load(crosslink_dir)
                    .ok()
                    .flatten()
                    .map(|a| a.agent_id);
                if let Ok(Some(session)) = db.get_current_session_for_agent(agent_id.as_deref()) {
                    active_issue_id = session.active_issue_id;
                    let session_data = serde_json::json!({
                        "session_id": session.id,
                        "started_at": session.started_at.to_rfc3339(),
                        "active_issue_id": session.active_issue_id,
                    });

                    if let Ok(json) = serde_json::to_string_pretty(&session_data) {
                        if let Err(e) = fs::write(&session_file, json) {
                            tracing::warn!("Failed to write session file: {}", e);
                        } else {
                            println!(
                                "Session flushed at {}",
                                chrono::Utc::now().format("%H:%M:%S")
                            );
                        }
                    }
                }
            }
            Err(e) => {
                consecutive_db_failures += 1;
                tracing::warn!(
                    "Failed to open database: {} (failure #{})",
                    e,
                    consecutive_db_failures
                );
                if consecutive_db_failures == FAILURE_WARN_THRESHOLD {
                    tracing::warn!(
                        "{} consecutive database failures. Daemon may not be functioning correctly.",
                        FAILURE_WARN_THRESHOLD
                    );
                }
            }
        }

        // Heartbeat + fetch/hydrate: every N cycles
        heartbeat_counter += 1;
        if heartbeat_counter.is_multiple_of(HEARTBEAT_EVERY_N) {
            match crate::identity::AgentConfig::load(crosslink_dir) {
                Ok(Some(agent)) => {
                    match crate::sync::SyncManager::new(crosslink_dir) {
                        Ok(sync) => {
                            consecutive_sync_failures = 0;
                            // INTENTIONAL: cache init is best-effort — heartbeat will retry next interval
                            let _ = sync.init_cache();

                            // Push heartbeat
                            match sync.push_heartbeat(&agent, active_issue_id) {
                                Ok(()) => println!(
                                    "Heartbeat pushed at {}",
                                    chrono::Utc::now().format("%H:%M:%S")
                                ),
                                Err(e) => tracing::warn!("Heartbeat push failed: {}", e),
                            }

                            // Fetch latest coordination branch and hydrate SQLite
                            match sync.fetch() {
                                Ok(()) => {
                                    if let Ok(db) = Database::open(&db_path) {
                                        match hydrate_to_sqlite(sync.cache_path(), &db) {
                                            Ok(stats) => {
                                                if stats.issues > 0 {
                                                    println!(
                                                        "Hydrated {} issue(s) at {}",
                                                        stats.issues,
                                                        chrono::Utc::now().format("%H:%M:%S")
                                                    );
                                                }
                                            }
                                            Err(e) => tracing::warn!("Hydration failed: {}", e),
                                        }
                                    }
                                }
                                Err(e) => tracing::warn!("Fetch failed: {}", e),
                            }
                        }
                        Err(e) => {
                            consecutive_sync_failures += 1;
                            tracing::warn!(
                                "Sync init failed: {} (failure #{})",
                                e,
                                consecutive_sync_failures
                            );
                            if consecutive_sync_failures == FAILURE_WARN_THRESHOLD {
                                tracing::warn!(
                                    "{} consecutive sync failures. Daemon may not be functioning correctly.",
                                    FAILURE_WARN_THRESHOLD
                                );
                            }
                        }
                    }
                }
                Ok(None) => {} // No agent configured, skip sync
                Err(e) => tracing::warn!("Failed to load agent config: {}", e),
            }
        }
    }

    Ok(())
}

fn read_pid(pid_file: &Path) -> Option<u32> {
    let mut file = fs::File::open(pid_file).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    contents.trim().parse().ok()
}

#[cfg(windows)]
fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
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
fn is_process_running(pid: u32) -> bool {
    use std::process::Command;
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn kill_process(pid: u32) -> Result<()> {
    use std::process::Command;
    Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status()
        .context("Failed to kill process")?;
    Ok(())
}

#[cfg(not(windows))]
fn kill_process(pid: u32) -> Result<()> {
    use std::process::Command;
    Command::new("kill")
        .arg(pid.to_string())
        .status()
        .context("Failed to kill process")?;
    Ok(())
}
