//! `serve` subcommand end-to-end tests (bead claudepr-7f088327).
//!
//! These tests invoke the *compiled* `claude-print` binary as a subprocess,
//! pinning `--claude-binary` to `mock-claude` (same hermetic strategy as
//! `tests/binary_e2e.rs`). They pin the ADR-005 serve contract:
//!
//!   * **enter the server path** — `serve` binds the selected Unix socket and
//!     starts maintaining the pool; it never falls through to ordinary prompt
//!     validation (a fall-through would exit 4 with "no prompt provided",
//!     since no positional prompt accompanies the subcommand).
//!   * **invalid pool size** — `--pool-size 0`, `--pool-size` past
//!     `MAX_POOL_SIZE`, and non-numeric values all exit 2 with actionable
//!     stderr before any worker is spawned.
//!   * **socket setup failures** — an unbindable socket path exits 2 with
//!     actionable stderr naming the socket.
//!   * **safe local permissions** — the socket node is user-only (0600),
//!     whatever umask the invoking shell carried.
//!   * **population** — the daemon warms exactly the requested number of
//!     workers (mock-claude completes the full warmup: trust dialog →
//!     dismissal → idle-settle, then blocks awaiting a prompt that warmup
//!     never sends).
//!   * **clean shutdown** — SIGTERM destroys every worker, removes the socket
//!     file, and exits 0.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

/// A captured subprocess outcome: exit code (or `None` if killed on timeout),
/// and decoded stderr.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    stderr: String,
}

/// Locate a workspace bin built alongside this test binary (same resolution
/// strategy as `tests/binary_e2e.rs`).
fn workspace_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

/// Build a `claude-print` Command pre-wired to use mock-claude as the backend.
fn claude_print() -> Command {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    assert!(
        bin.exists(),
        "claude-print binary missing at {}; run `cargo build`",
        bin.display()
    );
    assert!(
        mock.exists(),
        "mock-claude binary missing at {}; run `cargo build`",
        mock.display()
    );
    let mut cmd = Command::new(&bin);
    cmd.arg("--claude-binary").arg(&mock);
    cmd
}

/// Run `cmd` to completion, decoding stdout/stderr as UTF-8. Kills the child
/// if it outlives `budget` so a wedged daemon cannot hang the suite.
fn run(cmd: &mut Command, budget: Duration) -> Outcome {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn claude-print: {e}"));

    let deadline = start + budget;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("claude-print did not exit within {:?}", budget);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break None,
        }
    };

    let output = child.wait_with_output().expect("wait_with_output");
    Outcome {
        code: code.or(output.status.code()),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Budget for the negative (fast-exit) cases. Generous ceiling that still
/// fails fast on a wedge.
const BUDGET: Duration = Duration::from_secs(30);

// ── Invalid pool size ────────────────────────────────────────────────────────

#[test]
fn serve_rejects_zero_pool_size() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let out = run(
        claude_print()
            .args(["serve", "--pool-size", "0", "--socket"])
            .arg(&socket),
        BUDGET,
    );

    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("--pool-size") && out.stderr.contains("at least 1"),
        "stderr must name the flag and the minimum: {}",
        out.stderr
    );
    // Never falls through to ordinary prompt validation.
    assert!(
        !out.stderr.contains("no prompt provided"),
        "serve must not reach prompt validation: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("Listening on"),
        "rejected pool size must not enter the server loop: {}",
        out.stderr
    );
    assert!(
        !socket.exists(),
        "no socket may be created for a rejected size"
    );
}

#[test]
fn serve_rejects_pool_size_above_max() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let out = run(
        claude_print()
            .args(["serve", "--pool-size", "257", "--socket"])
            .arg(&socket),
        BUDGET,
    );

    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("--pool-size") && out.stderr.contains("256"),
        "stderr must name the flag and the 256 cap: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("no prompt provided"),
        "serve must not reach prompt validation: {}",
        out.stderr
    );
    assert!(
        !socket.exists(),
        "no socket may be created for a rejected size"
    );
}

// Non-numeric values are rejected by clap's own argv parsing, which also
// exits 2 — the contract is the exit code, whichever layer rejects it.
#[test]
fn serve_rejects_non_numeric_pool_size() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let out = run(
        claude_print()
            .args(["serve", "--pool-size", "abc", "--socket"])
            .arg(&socket),
        BUDGET,
    );

    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("pool-size"),
        "stderr must name the offending flag: {}",
        out.stderr
    );
    assert!(
        !socket.exists(),
        "no socket may be created for a rejected size"
    );
}

// ── Socket setup failures ────────────────────────────────────────────────────

#[test]
fn serve_fails_fast_on_unbindable_socket() {
    let dir = tempfile::tempdir().unwrap();
    // Parent of the socket path is a regular file → bind cannot succeed.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();

    let out = run(
        claude_print()
            .args(["serve", "--pool-size", "1", "--socket"])
            .arg(blocker.join("pool.sock")),
        BUDGET,
    );

    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("failed to set up pool socket"),
        "stderr must name the socket failure: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("Spawning worker"),
        "a socket failure must abort before any worker spawn: {}",
        out.stderr
    );
}

// ── Happy path: server path, permissions, population, shutdown ──────────────

/// A running `serve` daemon with its stderr collected line-by-line on a
/// reader thread. Dropping the guard terminates the daemon (SIGTERM, then
/// SIGKILL after a grace period) so a failing assertion cannot leak worker
/// processes into the rest of the suite.
struct Daemon {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    /// Start `claude-print serve --socket <socket> --verbose`, with
    /// `--pool-size <size>` when `size` is `Some` (omit it to exercise the
    /// compiled-in default).
    fn start(mock: &Path, socket: &Path, size: Option<&str>) -> Daemon {
        let bin = workspace_bin("claude-print");
        let mut cmd = Command::new(&bin);
        cmd.arg("--claude-binary").arg(mock).arg("serve");
        if let Some(size) = size {
            cmd.args(["--pool-size", size]);
        }
        cmd.arg("--socket").arg(socket).arg("--verbose");
        let mut child = cmd
            // The daemon reads no prompt: a null stdin proves the serve path
            // never consults prompt resolution (a fall-through would exit 4
            // immediately and the settle wait below would time out).
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn claude-print serve");

        let stderr = child.stderr.take().expect("stderr piped");
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&lines);
        let reader = std::thread::spawn(move || {
            let reader = std::io::BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(line) => sink.lock().unwrap().push(line),
                    Err(_) => break,
                }
            }
        });

        Daemon {
            child,
            lines,
            reader: Some(reader),
        }
    }

    fn stderr(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }

    /// Block until stderr contains `needle` at least `count` times.
    fn wait_for(&mut self, needle: &str, count: usize, budget: Duration) {
        let start = Instant::now();
        loop {
            let hits = self.stderr().iter().filter(|l| l.contains(needle)).count();
            if hits >= count {
                return;
            }
            assert!(
                start.elapsed() < budget,
                "timed out waiting for {count:?} occurrences of {needle:?}; stderr so far: {:?}",
                self.stderr()
            );
            if let Ok(Some(_)) = self.child.try_wait() {
                panic!(
                    "daemon exited while waiting for {needle:?}; stderr: {:?}",
                    self.stderr()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// SIGTERM the daemon and wait (budget-bounded, SIGKILL fallback) for a
    /// clean exit. Returns the captured outcome.
    fn terminate(mut self, socket: &Path) -> Outcome {
        kill(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM)
            .expect("failed to signal daemon");

        let start = Instant::now();
        let code = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if start.elapsed() > Duration::from_secs(20) {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        panic!("daemon did not exit within 20s of SIGTERM");
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => panic!("failed to reap daemon after SIGTERM"),
            }
        };

        if let Some(reader) = self.reader.take() {
            reader.join().expect("stderr reader thread");
        }

        assert!(
            !socket.exists(),
            "clean shutdown must remove the socket file"
        );

        Outcome {
            code,
            stderr: self.stderr().join("\n"),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Assertion-failure escape hatch: never leak the daemon or its
        // mock-claude workers into the rest of the suite.
        let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Full serve lifecycle with mock-claude, which performs the real warmup
/// choreography (trust dialog → dismissal → idle-settle) and then blocks
/// awaiting the prompt warmup never injects.
#[test]
fn serve_warms_exactly_requested_workers_then_shuts_down_cleanly() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut daemon = Daemon::start(&mock, &socket, Some("2"));

    // Server path entered: the selected socket exists and is bound.
    daemon.wait_for("Listening on", 1, Duration::from_secs(30));
    assert!(socket.exists(), "daemon must create the selected socket");

    // User-only permissions on the socket node, regardless of the invoking
    // shell's umask.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&socket)
        .expect("socket metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "socket must be user-only, got {mode:o}"
    );

    // Exactly the requested pool size: both workers warm to Ready, and no
    // respawn churn happens on the way (a failed warmup would add a second
    // "Spawning worker" line for its replacement).
    daemon.wait_for("settled and ready", 2, Duration::from_secs(90));
    let spawns = daemon
        .stderr()
        .iter()
        .filter(|l| l.contains("Spawning worker"))
        .count();
    assert_eq!(
        spawns,
        2,
        "exactly 2 workers must be spawned; stderr: {:?}",
        daemon.stderr()
    );

    let out = daemon.terminate(&socket);

    assert_eq!(
        out.code,
        Some(0),
        "SIGTERM is a clean stop; stderr: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Shutdown complete"),
        "shutdown must tear down workers and report completion: {}",
        out.stderr
    );
}

// The default (`--pool-size` omitted) resolves to 1 — the same server path
// with a single worker.
#[test]
fn serve_default_pool_size_warms_one_worker() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut daemon = Daemon::start(&mock, &socket, None);
    daemon.wait_for("settled and ready", 1, Duration::from_secs(90));
    let spawns = daemon
        .stderr()
        .iter()
        .filter(|l| l.contains("Spawning worker"))
        .count();
    assert_eq!(
        spawns,
        1,
        "exactly 1 worker must be spawned; stderr: {:?}",
        daemon.stderr()
    );

    let out = daemon.terminate(&socket);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
}
