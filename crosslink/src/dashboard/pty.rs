//! PTY broker for the embedded terminal (design doc §10).
//!
//! The dashboard hosts interactive `crosslink` commands (`design`,
//! `kickoff run`, ad-hoc shells) inside an xterm.js terminal. Each
//! terminal is backed by a real PTY managed by this broker:
//!
//! 1. Frontend POSTs `/api/v1/pty { project_slug, command, args? }`
//!    → server spawns a PTY in the project's workspace and returns
//!    a `session_id`.
//! 2. Frontend opens `ws://.../ws/pty/<session_id>` and exchanges
//!    `{type: "stdin"|"resize"}` / `{type: "stdout"|"exit"}` frames.
//! 3. WS disconnects don't kill the PTY — there's a configurable
//!    grace window (default 30 min) so users can reconnect from
//!    the /terminals page.
//!
//! Sessions live in the `pty_sessions` `SQLite` table (audit trail) and
//! a process-local `SessionRegistry` (live PTY handles + buffered
//! output). Output is buffered so a reconnecting client can replay
//! recent history rather than starting blank.
//!
//! Security model: same bearer-token auth as REST; bound to
//! 127.0.0.1 by default. Same-user privileges (no sandboxing) — this
//! is intentional, the operator is running their own code.

use anyhow::{Context, Result};
use chrono::Utc;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;

/// How many bytes of recent output we keep per session for replay
/// when a client reconnects. 64 KiB is enough to redraw most TUIs.
const REPLAY_BUFFER_BYTES: usize = 64 * 1024;

/// How long a PTY survives after the last WS disconnect before being
/// torn down. Lets users close their tab and resume from another
/// device without losing in-flight work. Currently advisory — the
/// reaper that consumes it lands in a follow-up; sessions live until
/// the registry drops them or the child exits naturally.
#[allow(dead_code)]
pub const DEFAULT_GRACE_PERIOD_SECS: u64 = 30 * 60;

/// Maximum concurrent live PTY sessions across the broker. Beyond
/// this, new spawn requests are rejected with 429.
pub const DEFAULT_MAX_CONCURRENT: usize = 8;

/// Wire frame from client → server over the PTY WebSocket.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ClientFrame {
    /// Base64-encoded keystrokes / paste data.
    Stdin { data: String },
    /// Terminal resize event.
    Resize { rows: u16, cols: u16 },
}

/// Wire frame from server → client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServerFrame {
    /// Base64-encoded raw PTY bytes (terminal escape sequences intact).
    Stdout { data: String },
    /// Process exited; further `Stdout` frames will not arrive.
    Exit { code: Option<i32> },
}

/// Live state for a single PTY session held in the registry.
pub struct PtySession {
    pub id: String,
    pub project_slug: String,
    pub command: String,
    pub started_at: String,
    /// Master end of the PTY. Wrapped in `Mutex<Option<...>>` so the
    /// reader thread can take ownership of the read half while the
    /// write half stays accessible from request handlers.
    master: Arc<Mutex<Option<Box<dyn MasterPty + Send>>>>,
    /// Broadcast channel for stdout bytes — every connected WS gets a
    /// receiver. Replay buffer is filled from the same producer.
    broadcaster: broadcast::Sender<Vec<u8>>,
    /// Rolling tail of recent stdout for clients reconnecting to an
    /// already-running session.
    replay: Arc<Mutex<VecDeque<u8>>>,
    /// Set when the child process exits; carries the exit code.
    pub exit_code: Arc<Mutex<Option<i32>>>,
    /// Notified when the child process exits — lets test code (and
    /// future reaper logic) await termination cleanly. Currently used
    /// only by tests; flagged so strict CI doesn't trip.
    #[allow(dead_code)]
    pub exit_notify: Arc<Notify>,
    /// Reader thread — keep the handle so drop terminates it cleanly.
    reader_handle: Mutex<Option<JoinHandle<()>>>,
}

impl PtySession {
    /// Subscribe to the live stdout stream + grab a snapshot of the
    /// replay buffer. Callers should send the replay first, then the
    /// live frames, to give the client a coherent view.
    pub fn subscribe(&self) -> (broadcast::Receiver<Vec<u8>>, Vec<u8>) {
        let receiver = self.broadcaster.subscribe();
        let snapshot = self.replay.lock().map_or_else(
            |_| Vec::new(),
            |buf| buf.iter().copied().collect::<Vec<u8>>(),
        );
        (receiver, snapshot)
    }

    /// Forward stdin bytes to the PTY. Returns an error if the master
    /// has already been closed (process exited).
    ///
    /// # Errors
    /// Returns an error if the master end is unavailable or write fails.
    pub fn write_stdin(&self, data: &[u8]) -> Result<()> {
        let mut guard = self
            .master
            .lock()
            .map_err(|_| anyhow::anyhow!("pty master mutex poisoned"))?;
        let master = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("pty already closed"))?;
        let mut writer = master.take_writer().context("take pty writer")?;
        writer.write_all(data).context("write to pty")?;
        Ok(())
    }

    /// Resize the terminal. Best-effort — never fatal.
    ///
    /// # Errors
    /// Returns an error only if the master end is gone.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let guard = self
            .master
            .lock()
            .map_err(|_| anyhow::anyhow!("pty master mutex poisoned"))?;
        let master = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("pty already closed"))?;
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize pty")?;
        Ok(())
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        if let Ok(mut g) = self.reader_handle.lock() {
            if let Some(h) = g.take() {
                h.abort();
            }
        }
        // Dropping the master closes the PTY (kills the child).
        if let Ok(mut g) = self.master.lock() {
            *g = None;
        }
    }
}

/// In-process registry of live PTY sessions. The `Arc<RwLock<…>>` is
/// stored on `AppState` so axum handlers can look up sessions by id.
#[derive(Clone, Default)]
pub struct SessionRegistry {
    inner: Arc<tokio::sync::RwLock<std::collections::HashMap<String, Arc<PtySession>>>>,
}

impl SessionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a freshly-spawned session.
    pub async fn insert(&self, session: Arc<PtySession>) {
        self.inner.write().await.insert(session.id.clone(), session);
    }

    /// Look up a session by id.
    pub async fn get(&self, id: &str) -> Option<Arc<PtySession>> {
        self.inner.read().await.get(id).cloned()
    }

    /// Remove a session (drop will tear down the PTY). Currently
    /// only used by tests + the future reaper task.
    #[allow(dead_code)]
    pub async fn remove(&self, id: &str) -> Option<Arc<PtySession>> {
        self.inner.write().await.remove(id)
    }

    /// Snapshot of currently-live session ids.
    pub async fn list_ids(&self) -> Vec<String> {
        self.inner.read().await.keys().cloned().collect()
    }

    /// Number of live sessions — for capacity checks.
    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    /// True when no sessions are tracked. Paired with `len` to keep
    /// clippy's `len_without_is_empty` happy.
    #[allow(dead_code)]
    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }
}

/// Spawn a PTY running `command` (with `args`) in `cwd` and return a
/// session handle wired into the broadcast pipeline.
///
/// # Errors
/// Returns an error if the PTY pair can't be created or the child
/// process can't be spawned (missing binary, permission denied, etc.).
pub fn spawn_pty(
    cwd: &std::path::Path,
    command: &str,
    args: &[String],
    rows: u16,
    cols: u16,
) -> Result<Arc<PtySession>> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("open pty")?;

    let mut cmd = CommandBuilder::new(command);
    for a in args {
        cmd.arg(a);
    }
    cmd.cwd(cwd);
    // Children can detect they're running under the dashboard's PTY
    // broker (e.g. to skip TTY-detection prompts that don't apply).
    cmd.env("CROSSLINK_DASHBOARD", "1");
    cmd.env("TERM", "xterm-256color");

    let mut child = pair.slave.spawn_command(cmd).context("spawn pty child")?;
    drop(pair.slave); // Slave end stays open via the child's fds.

    let id = format!("pty-{}", uuid::Uuid::new_v4());
    let started_at = Utc::now().to_rfc3339();
    let (tx, _) = broadcast::channel::<Vec<u8>>(64);
    let replay = Arc::new(Mutex::new(VecDeque::with_capacity(REPLAY_BUFFER_BYTES)));
    let exit_code = Arc::new(Mutex::new(None::<i32>));
    let exit_notify = Arc::new(Notify::new());

    // Take a clone of the master we can read from in the background
    // thread. portable_pty exposes try_clone_reader for exactly this.
    let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;

    let tx_for_reader = tx.clone();
    let replay_for_reader = Arc::clone(&replay);
    let exit_code_for_reader = Arc::clone(&exit_code);
    let exit_notify_for_reader = Arc::clone(&exit_notify);

    // Reader runs on a blocking-friendly worker because portable_pty
    // gives us a sync Read. Copy each chunk into the replay buffer
    // and broadcast to subscribers.
    let reader_handle = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let chunk = buf[..n].to_vec();
            if let Ok(mut replay_guard) = replay_for_reader.lock() {
                for &b in &chunk {
                    if replay_guard.len() == REPLAY_BUFFER_BYTES {
                        replay_guard.pop_front();
                    }
                    replay_guard.push_back(b);
                }
            }
            // Best effort: if no subscribers, send returns Err — fine.
            let _ = tx_for_reader.send(chunk);
        }
        // Reader returned EOF — wait for the child to actually exit
        // so we record the right code.
        let code = child.wait().map_or(-1, |status| status.exit_code() as i32);
        if let Ok(mut g) = exit_code_for_reader.lock() {
            *g = Some(code);
        }
        exit_notify_for_reader.notify_waiters();
        // Send a final empty chunk so any client polling the channel
        // wakes up; ServerFrame::Exit is rendered separately.
        let _ = tx_for_reader.send(Vec::new());
    });

    // Convert the join handle to a Tokio JoinHandle of () for storage.
    let reader_handle: JoinHandle<()> = tokio::spawn(async move {
        let _ = reader_handle.await;
    });

    Ok(Arc::new(PtySession {
        id,
        project_slug: cwd.to_string_lossy().into_owned(),
        command: command.to_string(),
        started_at,
        master: Arc::new(Mutex::new(Some(pair.master))),
        broadcaster: tx,
        replay,
        exit_code,
        exit_notify,
        reader_handle: Mutex::new(Some(reader_handle)),
    }))
}

/// Snapshot of a session for the `/api/v1/pty/sessions` listing.
#[derive(Debug, Clone, Serialize)]
pub struct PtySessionView {
    pub id: String,
    pub project_slug: String,
    pub command: String,
    pub started_at: String,
    pub exit_code: Option<i32>,
}

impl From<&PtySession> for PtySessionView {
    fn from(s: &PtySession) -> Self {
        let exit = s.exit_code.lock().ok().and_then(|g| *g);
        Self {
            id: s.id.clone(),
            project_slug: s.project_slug.clone(),
            command: s.command.clone(),
            started_at: s.started_at.clone(),
            exit_code: exit,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_echo_completes_with_exit_zero() {
        let session = spawn_pty(
            &std::env::temp_dir(),
            "/bin/sh",
            &["-c".to_string(), "echo hello && exit 0".to_string()],
            24,
            80,
        )
        .expect("spawn pty");

        // Wait up to 5s for exit notification.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.exit_notify.notified(),
        )
        .await;
        let code = session.exit_code.lock().unwrap();
        assert_eq!(*code, Some(0));
    }

    #[tokio::test]
    async fn test_subscribe_returns_replay_after_output() {
        let session = spawn_pty(
            &std::env::temp_dir(),
            "/bin/sh",
            &["-c".to_string(), "printf 'foobar' && sleep 0.1".to_string()],
            24,
            80,
        )
        .expect("spawn pty");

        // Give the reader thread time to drain.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let (_rx, snapshot) = session.subscribe();
        let s = String::from_utf8_lossy(&snapshot);
        assert!(s.contains("foobar"), "got: {s:?}");
    }

    #[tokio::test]
    async fn test_session_registry_insert_get_remove() {
        let reg = SessionRegistry::new();
        // Use `sh -c :` instead of `/bin/true`: some macOS/CI runners
        // reject direct `/bin/true` with ENOENT through portable-pty.
        let s = spawn_pty(
            &std::env::temp_dir(),
            "/bin/sh",
            &["-c".to_string(), ":".to_string()],
            24,
            80,
        )
        .expect("spawn");
        let id = s.id.clone();
        reg.insert(Arc::clone(&s)).await;
        assert!(reg.get(&id).await.is_some());
        assert_eq!(reg.len().await, 1);
        let removed = reg.remove(&id).await;
        assert!(removed.is_some());
        assert!(reg.get(&id).await.is_none());
    }
}
