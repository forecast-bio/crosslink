// Items below are public API for other test modules — suppress dead_code
// warnings until those modules are populated.
#![allow(dead_code)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Result of running a crosslink CLI command.
#[derive(Debug)]
pub struct CmdResult {
    pub success: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdResult {
    /// Check whether stdout contains the given substring.
    pub fn stdout_contains(&self, expected: &str) -> bool {
        self.stdout.contains(expected)
    }

    /// Check whether stderr contains the given substring.
    pub fn stderr_contains(&self, expected: &str) -> bool {
        self.stderr.contains(expected)
    }
}

/// Isolated test environment for smoke-testing the crosslink CLI.
///
/// Each harness gets its own temp directory, an optional bare git remote for
/// hub coordination tests, and optional server lifecycle management. Everything
/// is cleaned up automatically on drop.
pub struct SmokeHarness {
    pub temp_dir: TempDir,
    pub crosslink_bin: PathBuf,
    server_handle: Option<Child>,
    pub server_port: Option<u16>,
    /// Bearer token for API authentication, captured from server stdout.
    pub server_auth_token: Option<String>,
    pub agent_id: String,
    /// Path to a bare git repo used as the shared remote.  `None` for bare
    /// harnesses and harnesses that don't need coordination.
    bare_remote: Option<PathBuf>,
    /// When created via `fork_agent`, the remote TempDir is owned by the
    /// original harness.  We keep a reference here only so we know where it
    /// lives, but the TempDir itself is *not* owned by forks — the original
    /// harness (and its `_remote_dir`) keeps it alive.
    _remote_dir: Option<TempDir>,
}

impl SmokeHarness {
    // ── Constructors ──────────────────────────────────────────────────

    /// Create a fully initialised test environment.
    ///
    /// 1. Creates a temp directory.
    /// 2. Runs `git init` inside it (crosslink init writes `.gitignore` etc.).
    /// 3. Configures `user.name` and `user.email` so git operations work.
    /// 4. Creates a bare git repo and adds it as `origin` (for hub tests).
    /// 5. Makes an initial commit and pushes to the bare remote.
    /// 6. Runs `crosslink init --defaults --skip-cpitd --skip-signing`.
    pub fn new() -> Self {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let remote_dir = TempDir::new().expect("failed to create remote temp dir");
        let bin = PathBuf::from(env!("CARGO_BIN_EXE_crosslink"));

        // Initialise bare remote
        let out = Command::new("git")
            .current_dir(remote_dir.path())
            .args(["init", "--bare", "-b", "main"])
            .output()
            .expect("git init --bare failed to execute");
        assert!(out.status.success(), "git init --bare failed");

        // Initialise work repo
        let out = Command::new("git")
            .current_dir(temp_dir.path())
            .args(["init", "-b", "main"])
            .output()
            .expect("git init failed to execute");
        assert!(out.status.success(), "git init failed");

        // Configure git identity and remote
        for args in [
            vec!["config", "user.email", "smoke@test.local"],
            vec!["config", "user.name", "Smoke Test"],
            vec![
                "remote",
                "add",
                "origin",
                remote_dir
                    .path()
                    .to_str()
                    .expect("remote path not valid UTF-8"),
            ],
        ] {
            let out = Command::new("git")
                .current_dir(temp_dir.path())
                .args(&args)
                .output()
                .expect("git config/remote failed to execute");
            assert!(out.status.success(), "git {args:?} failed");
        }

        // Initial commit + push so the remote has a main branch
        std::fs::write(temp_dir.path().join("README.md"), "# smoke\n")
            .expect("failed to write README.md");
        let _ = Command::new("git")
            .current_dir(temp_dir.path())
            .args(["add", "README.md"])
            .output();
        let out = Command::new("git")
            .current_dir(temp_dir.path())
            .args(["commit", "-m", "initial", "--no-gpg-sign"])
            .output()
            .expect("git commit failed to execute");
        assert!(out.status.success(), "initial git commit failed");
        let out = Command::new("git")
            .current_dir(temp_dir.path())
            .args(["push", "-u", "origin", "main"])
            .output()
            .expect("git push failed to execute");
        assert!(out.status.success(), "initial git push failed");

        // Run crosslink init
        let out = Command::new(&bin)
            .current_dir(temp_dir.path())
            .args(["init", "--defaults", "--skip-cpitd", "--skip-signing"])
            .output()
            .expect("crosslink init failed to execute");
        assert!(
            out.status.success(),
            "crosslink init failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let bare_remote = Some(remote_dir.path().to_path_buf());

        SmokeHarness {
            temp_dir,
            crosslink_bin: bin,
            server_handle: None,
            server_port: None,
            server_auth_token: None,
            agent_id: "smoke-primary".to_string(),
            bare_remote,
            _remote_dir: Some(remote_dir),
        }
    }

    /// Create a harness *without* running `crosslink init`.
    ///
    /// Useful for testing the init command itself or verifying behaviour in an
    /// uninitialised directory.  No git repo is created either.
    pub fn new_bare() -> Self {
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let bin = PathBuf::from(env!("CARGO_BIN_EXE_crosslink"));
        SmokeHarness {
            temp_dir,
            crosslink_bin: bin,
            server_handle: None,
            server_port: None,
            server_auth_token: None,
            agent_id: "smoke-bare".to_string(),
            bare_remote: None,
            _remote_dir: None,
        }
    }

    // ── Command execution ─────────────────────────────────────────────

    /// Run a crosslink CLI command and return the full result.
    pub fn run(&self, args: &[&str]) -> CmdResult {
        let output = Command::new(&self.crosslink_bin)
            .current_dir(self.temp_dir.path())
            .args(args)
            .output()
            .expect("failed to execute crosslink");

        CmdResult {
            success: output.status.success(),
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }

    /// Run a crosslink CLI command and assert it succeeds (exit code 0).
    ///
    /// Panics with stdout/stderr on failure.
    pub fn run_ok(&self, args: &[&str]) -> CmdResult {
        let result = self.run(args);
        assert!(
            result.success,
            "expected crosslink {:?} to succeed but got exit code {}.\nstdout: {}\nstderr: {}",
            args, result.exit_code, result.stdout, result.stderr,
        );
        result
    }

    /// Run a crosslink CLI command and assert it fails (non-zero exit code).
    ///
    /// Panics if the command succeeds.
    pub fn run_err(&self, args: &[&str]) -> CmdResult {
        let result = self.run(args);
        assert!(
            !result.success,
            "expected crosslink {:?} to fail but it succeeded.\nstdout: {}\nstderr: {}",
            args, result.stdout, result.stderr,
        );
        result
    }

    // ── Path helpers ──────────────────────────────────────────────────

    /// Path to the `.crosslink/` directory inside the temp dir.
    pub fn crosslink_dir(&self) -> PathBuf {
        self.temp_dir.path().join(".crosslink")
    }

    /// Path to the SQLite database.
    pub fn db_path(&self) -> PathBuf {
        self.crosslink_dir().join("issues.db")
    }

    // ── Server lifecycle ──────────────────────────────────────────────

    /// Start `crosslink serve` on a random free port.
    ///
    /// Returns the port number.  The server process is stored internally and
    /// will be killed on `stop_server()` or when the harness is dropped.
    pub fn start_server(&mut self) -> u16 {
        // Find a free port by binding to port 0
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind to a free port");
            listener
                .local_addr()
                .expect("failed to get local addr")
                .port()
        };
        // The listener is dropped, freeing the port for the server.

        let mut child = Command::new(&self.crosslink_bin)
            .current_dir(self.temp_dir.path())
            .args(["serve", "--port", &port.to_string()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn crosslink serve");

        // Capture the auth token from server stdout before storing the child.
        // The server prints "  Auth:      Bearer <token>" during startup.
        if let Some(stdout) = child.stdout.take() {
            use std::io::BufRead;
            let reader = std::io::BufReader::new(stdout);
            let deadline = Instant::now() + Duration::from_secs(10);
            for line in reader.lines() {
                if Instant::now() > deadline {
                    break;
                }
                let Ok(line) = line else { break };
                if let Some(token) = line.trim().strip_prefix("Auth:") {
                    let token = token.trim();
                    if let Some(token) = token.strip_prefix("Bearer ") {
                        self.server_auth_token = Some(token.to_string());
                    }
                    break;
                }
            }
        }

        self.server_handle = Some(child);
        self.server_port = Some(port);

        // Wait for the server to be ready by attempting a TCP connection.
        // Previous approach used TcpListener::bind which is unreliable on macOS
        // when the server binds 0.0.0.0 but the check binds 127.0.0.1.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if Instant::now() > deadline {
                // Dump whatever output we have for debugging
                self.stop_server();
                panic!("crosslink serve did not become ready within 10 seconds on port {port}");
            }
            if std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{port}").parse().unwrap(),
                Duration::from_millis(100),
            )
            .is_ok()
            {
                // Connection succeeded — server is accepting connections
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        port
    }

    /// Stop the running server process, if any.
    pub fn stop_server(&mut self) {
        if let Some(mut child) = self.server_handle.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.server_port = None;
    }

    // ── Multi-agent support ───────────────────────────────────────────

    /// Create a second harness that shares the same bare git remote.
    ///
    /// The new harness gets its own temp directory, clones from the same
    /// remote, and runs `crosslink init`.  This is useful for testing
    /// multi-agent coordination (concurrent pushes, lock contention, etc.).
    ///
    /// # Panics
    ///
    /// Panics if this harness has no bare remote (i.e., was created with
    /// `new_bare()`).
    pub fn fork_agent(&self, agent_id: &str) -> SmokeHarness {
        let remote_path = self
            .bare_remote
            .as_ref()
            .expect("cannot fork_agent from a bare harness (no remote)");

        let temp_dir = TempDir::new().expect("failed to create temp dir for fork");

        // Initialise a new git repo and add the shared remote
        let out = Command::new("git")
            .current_dir(temp_dir.path())
            .args(["init", "-b", "main"])
            .output()
            .expect("git init failed");
        assert!(out.status.success(), "git init for fork failed");

        for args in [
            vec!["config", "user.email", &format!("{agent_id}@test.local")],
            vec!["config", "user.name", agent_id],
            vec![
                "remote",
                "add",
                "origin",
                remote_path.to_str().expect("remote path not valid UTF-8"),
            ],
        ] {
            let out = Command::new("git")
                .current_dir(temp_dir.path())
                .args(&args)
                .output()
                .expect("git config/remote failed");
            assert!(out.status.success(), "git {args:?} failed for fork");
        }

        // Fetch and checkout main from the shared remote
        let out = Command::new("git")
            .current_dir(temp_dir.path())
            .args(["fetch", "origin"])
            .output()
            .expect("git fetch failed");
        assert!(out.status.success(), "git fetch for fork failed");

        let out = Command::new("git")
            .current_dir(temp_dir.path())
            .args(["reset", "--hard", "origin/main"])
            .output()
            .expect("git reset failed");
        assert!(out.status.success(), "git reset for fork failed");

        // Set up tracking
        let out = Command::new("git")
            .current_dir(temp_dir.path())
            .args(["branch", "--set-upstream-to=origin/main", "main"])
            .output()
            .expect("git branch --set-upstream-to failed");
        assert!(out.status.success(), "set upstream for fork failed");

        // Run crosslink init
        let bin = PathBuf::from(env!("CARGO_BIN_EXE_crosslink"));
        let out = Command::new(&bin)
            .current_dir(temp_dir.path())
            .args(["init", "--defaults", "--skip-cpitd", "--skip-signing"])
            .output()
            .expect("crosslink init failed for fork");
        assert!(
            out.status.success(),
            "crosslink init failed for fork: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        SmokeHarness {
            temp_dir,
            crosslink_bin: bin,
            server_handle: None,
            server_port: None,
            server_auth_token: None,
            agent_id: agent_id.to_string(),
            bare_remote: Some(remote_path.clone()),
            _remote_dir: None, // The remote TempDir is owned by the original harness
        }
    }
}

impl Drop for SmokeHarness {
    fn drop(&mut self) {
        self.stop_server();
    }
}

// ── Assertion helpers ─────────────────────────────────────────────────

/// Assert that `result.stdout` contains `expected`, with a diagnostic message
/// on failure.
pub fn assert_stdout_contains(result: &CmdResult, expected: &str) {
    assert!(
        result.stdout_contains(expected),
        "expected stdout to contain {:?} but got:\n{}",
        expected,
        result.stdout,
    );
}

/// Assert that `result.stderr` contains `expected`, with a diagnostic message
/// on failure.
pub fn assert_stderr_contains(result: &CmdResult, expected: &str) {
    assert!(
        result.stderr_contains(expected),
        "expected stderr to contain {:?} but got:\n{}",
        expected,
        result.stderr,
    );
}

/// Assert that `crosslink issue list -s <status>` reports exactly `expected`
/// issues.
///
/// Uses `--json` output to count entries reliably.
pub fn assert_issue_count(harness: &SmokeHarness, status: &str, expected: usize) {
    let result = harness.run_ok(&["issue", "list", "-s", status, "--json"]);
    // The JSON output is an array of issue objects.  Count top-level entries.
    let parsed: serde_json::Value = serde_json::from_str(&result.stdout).unwrap_or_else(|e| {
        panic!(
            "failed to parse issue list JSON: {}\nstdout was:\n{}",
            e, result.stdout
        )
    });
    let count = parsed
        .as_array()
        .map(|a| a.len())
        .unwrap_or_else(|| panic!("expected JSON array, got: {}", result.stdout));
    assert_eq!(
        count, expected,
        "expected {} issues with status {:?}, got {}.\nJSON:\n{}",
        expected, status, count, result.stdout,
    );
}

// ── Self-tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_harness_new() {
        let h = SmokeHarness::new();
        assert!(h.crosslink_dir().exists());
        assert!(h.db_path().exists());
    }

    #[test]
    fn test_harness_run_ok() {
        let h = SmokeHarness::new();
        let result = h.run_ok(&["issue", "list"]);
        assert!(result.success);
    }

    #[test]
    fn test_harness_run_err() {
        let h = SmokeHarness::new();
        let result = h.run_err(&["issue", "show", "99999"]);
        assert!(!result.success);
    }

    #[test]
    fn test_harness_bare_no_crosslink_dir() {
        let h = SmokeHarness::new_bare();
        assert!(!h.crosslink_dir().exists());
    }

    #[test]
    fn test_harness_create_and_list() {
        let h = SmokeHarness::new();
        h.run_ok(&["issue", "create", "Test issue from harness"]);
        let result = h.run_ok(&["issue", "list"]);
        assert!(result.stdout_contains("Test issue from harness"));
    }

    #[test]
    fn test_cmd_result_helpers() {
        let result = CmdResult {
            success: true,
            exit_code: 0,
            stdout: "hello world".to_string(),
            stderr: "warning: something".to_string(),
        };
        assert!(result.stdout_contains("hello"));
        assert!(!result.stdout_contains("goodbye"));
        assert!(result.stderr_contains("warning"));
        assert!(!result.stderr_contains("error"));
    }

    #[test]
    fn test_assert_stdout_contains() {
        let result = CmdResult {
            success: true,
            exit_code: 0,
            stdout: "Created issue #1".to_string(),
            stderr: String::new(),
        };
        assert_stdout_contains(&result, "Created issue");
    }

    #[test]
    fn test_fork_agent() {
        let h = SmokeHarness::new();
        let h2 = h.fork_agent("agent-b");
        assert!(h2.crosslink_dir().exists());
        assert!(h2.db_path().exists());
        assert_eq!(h2.agent_id, "agent-b");
        // The two harnesses have different temp dirs
        assert_ne!(h.temp_dir.path(), h2.temp_dir.path(),);
    }
}
