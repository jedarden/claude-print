//! Pool-socket client matrix end-to-end (bead claudepr-c7824b71 — child 4/4 of
//! claudepr-309d826d, the ADR-005 warm-pool umbrella).
//!
//! One binary covering the parent's required matrix through the *compiled*
//! `claude-print` CLI, with `mock-claude` as the backend — the same hermetic
//! strategy as `tests/binary_e2e.rs` and `tests/serve.rs`: temp socket paths
//! and the mock binary only, no real `claude` install, no network, no fixed
//! ports. The mechanism-level pins for these contracts live in `src/pool.rs`
//! unit tests and `tests/serve.rs`; this binary is the one place the whole
//! matrix is exercised as real invocations, one named test per matrix row:
//!
//!   * **text / json / stream-json over an acquired worker** —
//!     `pooled_text_invocation_answers_through_an_acquired_worker`,
//!     `pooled_json_invocation_emits_the_transcript_sourced_result_object`
//!     (also pins that billing/usage capture survives the pool path),
//!     `pooled_stream_json_invocation_forwards_the_worker_transcript`.
//!   * **stateless fallback, socket absent** —
//!     `absent_pool_socket_runs_the_stateless_session_quietly`.
//!   * **stateless fallback, socket present but nothing listening** —
//!     `stale_pool_socket_falls_back_with_one_verbose_diagnostic`.
//!   * **repeated sequential clients against one pool** —
//!     `sequential_clients_get_fresh_replaced_workers_with_no_cross_caller_leakage`:
//!     three callers (one per output format) against one `--pool-size 1`
//!     daemon; every driven worker is destroyed and replaced before the next
//!     caller runs, and nothing of an earlier caller (worker id, pid, PTY
//!     master, session, transcript) reaches a later one.
//!   * **malformed daemon acquire responses fail safely within the caller
//!     timeout** — `daemon_close_mid_exchange_fails_safely_within_the_caller_timeout`,
//!     `daemon_wrong_shape_response_fails_safely_within_the_caller_timeout`,
//!     `assignment_without_fd_transfer_fails_safely_within_the_caller_timeout`:
//!     reachable-but-broken daemons exit 2 with the protocol-failure diagnostic
//!     and never fall back, well inside the `--timeout` budget.
//!
//! Every pooled run proves it really drove a prewarmed worker (not a quiet
//! stateless fallback) via the `driving prewarmed worker` verbose trace, which
//! only the pooled dispatch prints.

use std::io::BufRead;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

/// The one-word prompt every pooled invocation sends.
const PROMPT: &str = "Reply with exactly one word: pong";

/// Budget for a pooled invocation (warm path: acquire + drive + release).
const POOLED_BUDGET: Duration = Duration::from_secs(120);

/// Budget for a non-pooled invocation (fallback and malformed-daemon runs).
const FAST_BUDGET: Duration = Duration::from_secs(30);

/// Ceiling for a daemon warmup to reach `settled and ready` (mock-claude
/// completes trust dialog → dismissal → idle-settle in well under a second;
/// the ceiling only absorbs a loaded CI box).
const WARMUP: Duration = Duration::from_secs(90);

/// Ceiling for the daemon's ledger to show a release.
const LEDGER: Duration = Duration::from_secs(15);

/// Ceiling for the daemon to spawn a replacement worker.
const REPLACE: Duration = Duration::from_secs(30);

/// How long a shutdown signal may take to produce an exited daemon (SIGTERM
/// grace per worker is 2 s before SIGKILL).
const SHUTDOWN_BOUND: Duration = Duration::from_secs(20);

/// A captured subprocess outcome: exit code (or `None` if killed on timeout),
/// and decoded stdout/stderr.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Locate a workspace bin built alongside this test binary (same resolution
/// strategy as `tests/binary_e2e.rs` and `tests/serve.rs`).
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
/// if it outlives `budget` so a wedged invocation cannot hang the suite.
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
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

// ── Hermetic per-test fixture ────────────────────────────────────────────────

/// Everything one test needs isolated: a private config root, a private
/// `HOME` shared by the daemon (forwarded to its workers) and the client, and
/// a temp socket path. Nothing a test touches outlives the struct.
struct Fixture {
    /// `$XDG_CONFIG_HOME` for the client: holds an empty
    /// `claude-print/config.toml`, so no real config on this host can steer a
    /// run (timeouts, model defaults, hook inheritance).
    config: tempfile::TempDir,
    /// Shared `HOME` for BOTH sides: the daemon forwards it to workers, which
    /// write their transcripts there; the client reads the transcript path the
    /// worker's Stop payload advertises and derives the stream-json discovery
    /// dir from it. Every transcript stays out of the real `$HOME`.
    home: tempfile::TempDir,
    /// The pool socket path — temp, never fixed.
    socket: PathBuf,
}

impl Fixture {
    fn start() -> Fixture {
        let config = tempfile::tempdir().expect("config tempdir");
        std::fs::create_dir_all(config.path().join("claude-print")).expect("config dir");
        std::fs::write(config.path().join("claude-print/config.toml"), "").expect("empty config");
        let socket = config.path().join("pool.sock");
        Fixture {
            config,
            home: tempfile::tempdir().expect("home tempdir"),
            socket,
        }
    }

    /// A `claude-print` Command wired to the mock backend and this fixture's
    /// config root and HOME.
    fn client(&self) -> Command {
        let mut cmd = claude_print();
        cmd.env("XDG_CONFIG_HOME", self.config.path())
            .env("HOME", self.home.path());
        cmd
    }

    /// Start a `--pool-size 1` serve daemon over this fixture's socket with
    /// the fixture's HOME, plus extra environment (forwarded by the daemon's
    /// `build_child_env` to every mock-claude worker it spawns).
    fn daemon(&self, extra_env: &[(&str, &str)]) -> Daemon {
        Daemon::start(
            &self.socket,
            self.home.path().to_str().expect("utf-8 home"),
            extra_env,
        )
    }
}

// ── Daemon harness ───────────────────────────────────────────────────────────

/// A running `claude-print serve --pool-size 1` daemon with its stderr
/// collected line-by-line on a reader thread. Dropping the guard kills the
/// daemon so a failing assertion cannot leak worker processes into the rest
/// of the suite; the clean exit path is [`Daemon::terminate`].
struct Daemon {
    child: Child,
    socket: PathBuf,
    lines: Arc<Mutex<Vec<String>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    fn start(socket: &Path, home: &str, extra_env: &[(&str, &str)]) -> Daemon {
        let bin = workspace_bin("claude-print");
        let mock = workspace_bin("mock-claude");
        let mut cmd = Command::new(&bin);
        cmd.arg("--claude-binary")
            .arg(&mock)
            .arg("serve")
            .args(["--pool-size", "1"])
            .arg("--socket")
            .arg(socket)
            .arg("--verbose")
            .env("HOME", home);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
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
            socket: socket.to_path_buf(),
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

    /// SIGTERM the daemon and hold it to the full shutdown contract: exit 0
    /// within [`SHUTDOWN_BOUND`], the socket file removed, and — the reaping
    /// check — exactly `expected_workers` children snapshotted before the
    /// signal, all gone from /proc afterwards (neither leaked nor zombie).
    fn terminate(mut self, expected_workers: usize) {
        let workers = children_of(self.child.id());
        assert_eq!(
            workers.len(),
            expected_workers,
            "daemon must hold exactly {expected_workers} worker(s) at shutdown \
             (deterministic population, no churn); children: {workers:?}"
        );
        kill(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM).expect("SIGTERM the daemon");

        let start = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    assert_eq!(
                        status.code(),
                        Some(0),
                        "a supervisor's SIGTERM is a clean stop, not a failure"
                    );
                    break;
                }
                Ok(None) => {
                    assert!(
                        start.elapsed() < SHUTDOWN_BOUND,
                        "daemon ignored SIGTERM; stderr: {:?}",
                        self.stderr()
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => panic!("failed to reap daemon after SIGTERM: {e}"),
            }
        }

        assert!(
            !self.socket.exists(),
            "the daemon must remove its own socket file on shutdown"
        );

        // Every worker reaped before the daemon exited (destroy_worker ends
        // in a blocking waitpid), so anything still visible is teardown that
        // never ran; the grace absorbs only init finishing the reap of an
        // orphaned zombie.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let survivors: Vec<(u32, String)> = workers
                .iter()
                .copied()
                .filter_map(|pid| proc_state(pid).map(|state| (pid, state)))
                .collect();
            if survivors.is_empty() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "worker processes survived the daemon's exit (leaked or unreaped): {survivors:?}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Assertion-failure escape hatch: never leak the daemon or its
        // mock-claude workers into the rest of the suite. Skip the kill when
        // the child has already been reaped (a completed terminate) — its pid
        // is recyclable, and a blind SIGKILL there could hit an unrelated
        // process.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Parse the parent PID out of the contents of `/proc/<pid>/stat` (the comm
/// field may contain spaces, so scanning starts after its closing `)`).
fn stat_ppid(stat: &str) -> Option<u32> {
    let rest = stat.rsplit_once(')')?.1.trim_start();
    let mut fields = rest.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// Every live process whose parent is `ppid`, read from /proc. Pool workers
/// are forked directly by the daemon and nothing else in serve mode forks, so
/// while the daemon lives this is exactly its worker set.
fn children_of(ppid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            if stat_ppid(&stat) == Some(ppid) {
                found.push(pid);
            }
        }
    }
    found
}

/// The process state letter from `/proc/<pid>/stat` (`Z` = zombie), or `None`
/// once the pid no longer has a /proc entry.
fn proc_state(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1.trim_start();
    rest.split_whitespace().next().map(str::to_owned)
}

/// Count the daemon's open PTY masters (`/dev/ptmx` readlinks). At rest with
/// one ready worker this is exactly 1 — a leaked assignment would read 2.
fn daemon_pty_fd_count(daemon_pid: u32) -> usize {
    let fd_dir = format!("/proc/{daemon_pid}/fd");
    std::fs::read_dir(&fd_dir)
        .unwrap_or_else(|e| panic!("read {fd_dir}: {e}"))
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_link(e.path()).ok())
        .filter(|t| t.to_string_lossy() == "/dev/ptmx")
        .count()
}

/// Extract the (worker id, worker pid) a pooled drive traced. The trace only
/// exists on the pooled path, so finding it doubles as the proof the
/// invocation drove a prewarmed worker rather than falling back statelessly.
fn driven_worker(stderr: &str) -> (String, u32) {
    let line = stderr
        .lines()
        .find(|l| l.contains("driving prewarmed worker"))
        .expect("the pooled drive must trace the worker it drives");
    let rest = line
        .split("driving prewarmed worker ")
        .nth(1)
        .expect("worker id segment");
    let (id, pid) = rest.split_once(" (pid ").expect("pid segment");
    let pid = pid
        .trim_end_matches(')')
        .trim()
        .parse::<u32>()
        .expect("pid must be numeric");
    (id.trim().to_string(), pid)
}

/// Poll until `pid` has vanished from /proc. The daemon reaps a released
/// worker before it answers the release, so this converges fast; the window
/// only absorbs scheduler lag.
fn assert_pid_gone(pid: u32, grace: Duration) {
    let start = Instant::now();
    while proc_state(pid).is_some() {
        assert!(
            start.elapsed() < grace,
            "worker pid {pid} must be gone after its release was processed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The `session_id` of a stream-json run's result event — the only carrier of
/// the session the run actually answered (the CLI adds nothing to forwarded
/// transcript lines).
fn result_session_id(stream: &str) -> Option<String> {
    stream.lines().find_map(|line| {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("type").and_then(|t| t.as_str()) == Some("result") {
            value
                .get("session_id")
                .and_then(|s| s.as_str())
                .map(String::from)
        } else {
            None
        }
    })
}

/// How many transcript result events a stream-json run forwarded.
fn result_event_count(stream: &str) -> usize {
    stream
        .lines()
        .filter(|line| {
            matches!(
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .as_ref()
                    .and_then(|v| v.get("type")),
                Some(serde_json::Value::String(t)) if t == "result"
            )
        })
        .count()
}

// ── Bullet 1: text / json / stream-json over an acquired worker ─────────────

/// Text format over the pool: the invocation acquires a prewarmed worker,
/// drives the prompt through it via the ordinary Session event loop, prints
/// the worker transcript's answer, and releases the worker exactly once —
/// after which the daemon tears it down and warms a replacement.
#[test]
fn pooled_text_invocation_answers_through_an_acquired_worker() {
    let fx = Fixture::start();
    let mut daemon = fx.daemon(&[]);
    daemon.wait_for("settled and ready", 1, WARMUP);

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "the pooled invocation must succeed\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Hello from mock_claude"),
        "the answer must come from the driven worker's transcript\nstdout:\n{}",
        out.stdout
    );
    // Pooled-path proof: this trace only the pooled dispatch prints — a quiet
    // stateless fallback would succeed without it.
    assert!(
        out.stderr.contains("driving prewarmed worker"),
        "the client must drive a prewarmed worker, not fall back\nstderr:\n{}",
        out.stderr
    );
    // The shared Stop tail ran on the worker's own FIFO, and the worker was
    // handed back explicitly after the drive.
    assert!(
        out.stderr.contains("stop received session_id="),
        "the pooled session must consume the worker's Stop payload\nstderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("released"),
        "the worker must be released after the drive\nstderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("falling back"),
        "a healthy pool must never fall back\nstderr:\n{}",
        out.stderr
    );

    // Daemon side: one assignment, one release, replacement warmed.
    daemon.wait_for("Released worker", 1, LEDGER);
    daemon.wait_for("Spawning worker", 2, REPLACE);
    daemon.wait_for("settled and ready", 2, WARMUP);
    daemon.terminate(1);
}

/// Json format over the pool: the emitted result object is the shared
/// stateless shape, and its usage numbers come from the WORKER's transcript —
/// the mock's assistant event carries {input:10, output:25, cache_creation:5,
/// cache_read:15}, which a `last_assistant_message` fallback would report as
/// zeros. Non-zero usage is the billing invariant: capture survives the pool
/// path.
#[test]
fn pooled_json_invocation_emits_the_transcript_sourced_result_object() {
    let fx = Fixture::start();
    let mut daemon = fx.daemon(&[]);
    daemon.wait_for("settled and ready", 1, WARMUP);

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .args(["--output-format", "json"])
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "the pooled json invocation must succeed\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("driving prewarmed worker"),
        "the client must drive a prewarmed worker, not fall back\nstderr:\n{}",
        out.stderr
    );

    let trimmed = out.stdout.trim();
    assert!(
        !trimmed.contains('\n'),
        "the json result must be a single line, got:\n{}",
        out.stdout
    );
    let v: serde_json::Value = serde_json::from_str(trimmed)
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\nraw:\n{}", out.stdout));
    assert_eq!(v["type"], "result");
    assert_eq!(v["subtype"], "success");
    assert_eq!(v["is_error"], false);
    let text = v["result"]
        .as_str()
        .unwrap_or_else(|| panic!("result must be a string, got: {:?}", v["result"]));
    assert!(
        text.contains("Hello from mock_claude"),
        "the result text must be the worker transcript's answer, got: {text:?}"
    );
    assert_eq!(
        v["session_id"], "mock-session-abc123",
        "the session id must come from the worker's transcript result event"
    );
    assert_eq!(v["usage"]["input_tokens"], 10, "usage: {}", v["usage"]);
    assert_eq!(v["usage"]["output_tokens"], 25, "usage: {}", v["usage"]);
    assert_eq!(
        v["usage"]["cache_creation_input_tokens"], 5,
        "usage: {}",
        v["usage"]
    );
    assert_eq!(
        v["usage"]["cache_read_input_tokens"], 15,
        "usage: {}",
        v["usage"]
    );
    assert!(
        v.get("claude_version").and_then(|c| c.as_str()).is_some(),
        "claude_version must be present and a string: {v}"
    );

    daemon.wait_for("Released worker", 1, LEDGER);
    daemon.wait_for("Spawning worker", 2, REPLACE);
    daemon.wait_for("settled and ready", 2, WARMUP);
    daemon.terminate(1);
}

/// Stream-json over the pool: the live transcript reader discovers the
/// session's JSONL in the WORKER's projects dir (both sides share the
/// fixture's hermetic HOME, so the discovery dir the client derives is
/// exactly where the worker-side claude writes) and forwards its events
/// verbatim.
#[test]
fn pooled_stream_json_invocation_forwards_the_worker_transcript() {
    let fx = Fixture::start();
    let mut daemon = fx.daemon(&[]);
    daemon.wait_for("settled and ready", 1, WARMUP);

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .args(["--output-format", "stream-json"])
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "the pooled stream-json invocation must succeed\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("driving prewarmed worker"),
        "the client must drive a prewarmed worker, not fall back\nstderr:\n{}",
        out.stderr
    );
    let non_empty = out.stdout.lines().filter(|l| !l.trim().is_empty());
    assert!(
        non_empty.clone().count() > 0,
        "stream-json must forward the worker transcript's events\nstdout:\n{}",
        out.stdout
    );
    for line in non_empty {
        assert!(
            serde_json::from_str::<serde_json::Value>(line).is_ok(),
            "every stream-json line must be valid JSON, got: {line:?}"
        );
    }
    let session = result_session_id(&out.stdout)
        .unwrap_or_else(|| panic!("stream-json must forward a result event\nstdout: {out:?}"));
    assert_eq!(
        session, "mock-session-abc123",
        "the forwarded result event must be the worker's own session"
    );
    assert!(
        out.stdout.contains("assistant"),
        "stream-json must forward the assistant turn\nstdout:\n{}",
        out.stdout
    );
    assert!(
        !out.stderr.contains("could not derive"),
        "deriving the worker's projects dir must not fail\nstderr:\n{}",
        out.stderr
    );

    daemon.wait_for("Released worker", 1, LEDGER);
    daemon.wait_for("Spawning worker", 2, REPLACE);
    daemon.wait_for("settled and ready", 2, WARMUP);
    daemon.terminate(1);
}

// ── Bullet 3a: stateless fallback — socket absent, socket unreachable ───────

/// A `--pool-socket` path with NO socket file at all (no daemon ever ran
/// there): the invocation must succeed statelessly and quietly — the ADR-005
/// fallback is additive, so without `--verbose` nothing about the pool may
/// surface on stderr.
#[test]
fn absent_pool_socket_runs_the_stateless_session_quietly() {
    let fx = Fixture::start();
    assert!(
        !fx.socket.exists(),
        "precondition: no daemon has ever bound this path"
    );

    let mut cmd = fx.client();
    cmd.arg("--pool-socket").arg(&fx.socket).arg(PROMPT);
    let out = run(&mut cmd, FAST_BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "an absent pool must fall back to a successful stateless session\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Hello from mock_claude"),
        "the stateless session must have run to its answer\nstdout:\n{}",
        out.stdout
    );
    assert!(
        !out.stderr.contains("pool:"),
        "quiet mode must stay quiet about the fallback\nstderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("driving prewarmed worker"),
        "the stateless session must not claim to drive a pooled worker\nstderr:\n{}",
        out.stderr
    );
}

/// The other fallback shape: a STALE socket file — the path exists (a daemon
/// bound it once, then died without cleanup) but nothing is listening, so the
/// connect is refused. Still `Unreachable` per ADR-005: one verbose
/// diagnostic naming the failure and the fallback, then a successful
/// stateless session.
#[test]
fn stale_pool_socket_falls_back_with_one_verbose_diagnostic() {
    let fx = Fixture::start();

    // Bind and immediately drop a listener: std's UnixListener does not unlink
    // on drop, so the socket node remains with nobody behind it.
    {
        let listener = UnixListener::bind(&fx.socket).expect("bind the doomed listener");
        drop(listener);
    }
    assert!(
        fx.socket.exists(),
        "precondition: the stale socket node must exist with no listener behind it"
    );

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out = run(&mut cmd, FAST_BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "a stale socket must fall back to a successful stateless session\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Hello from mock_claude"),
        "the stateless session must have run to its answer\nstdout:\n{}",
        out.stdout
    );
    let pool_lines: Vec<&str> = out.stderr.lines().filter(|l| l.contains("pool:")).collect();
    assert_eq!(
        pool_lines.len(),
        1,
        "exactly one verbose diagnostic line expected\nstderr:\n{}",
        out.stderr
    );
    assert!(
        pool_lines[0].contains("no pool reachable at") && pool_lines[0].contains("falling back"),
        "the diagnostic must name the failure and the fallback: {}",
        pool_lines[0]
    );
    assert!(
        !out.stderr.contains("driving prewarmed worker"),
        "the stateless session must not claim to drive a pooled worker\nstderr:\n{}",
        out.stderr
    );
}

// ── Bullet 2: repeated sequential clients, teardown/replace, no leakage ─────

/// Three sequential clients — one per output format — against one
/// `--pool-size 1` daemon. Each caller is driven through a DISTINCT worker
/// (id and pid); each earlier worker is destroyed before the next caller
/// runs (its pid gone from /proc, the daemon's PTY-master count back to one
/// at rest); each caller answers from its own worker's transcript with its
/// own session; and the daemon's ledger shows exactly three assignments and
/// three releases. Nothing of an earlier caller reaches a later one.
///
/// `MOCK_UNIQUE_SESSION_ID` makes every worker mint its own pid-derived
/// session id and transcript file, so cross-caller leakage in the json and
/// stream-json outputs is observable rather than blurred by a shared
/// default session.
#[test]
fn sequential_clients_get_fresh_replaced_workers_with_no_cross_caller_leakage() {
    let fx = Fixture::start();
    let mut daemon = fx.daemon(&[("MOCK_UNIQUE_SESSION_ID", "1")]);
    daemon.wait_for("settled and ready", 1, WARMUP);
    let daemon_pid = daemon.child.id();

    // ── caller 1: text ──────────────────────────────────────────────────────
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out1 = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out1.code,
        Some(0),
        "caller 1 must succeed\nstdout:\n{}\nstderr:\n{}",
        out1.stdout,
        out1.stderr
    );
    assert!(
        out1.stdout.contains("Hello from mock_claude"),
        "caller 1 must get the worker's answer\nstdout:\n{}",
        out1.stdout
    );
    let (id1, pid1) = driven_worker(&out1.stderr);

    // Caller 1's worker torn down and its replacement warm before caller 2.
    daemon.wait_for("Released worker", 1, LEDGER);
    daemon.wait_for("Spawning worker", 2, REPLACE);
    daemon.wait_for("settled and ready", 2, WARMUP);
    assert_pid_gone(pid1, Duration::from_secs(10));

    // ── caller 2: json ──────────────────────────────────────────────────────
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .args(["--output-format", "json"])
        .arg(PROMPT);
    let out2 = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out2.code,
        Some(0),
        "caller 2 must succeed\nstdout:\n{}\nstderr:\n{}",
        out2.stdout,
        out2.stderr
    );
    let v2: serde_json::Value = serde_json::from_str(out2.stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "caller 2 stdout is not valid JSON: {e}\nraw:\n{}",
            out2.stdout
        )
    });
    let sess2 = v2["session_id"]
        .as_str()
        .expect("caller 2's result object must carry a session id")
        .to_string();
    assert!(
        sess2.starts_with("mock-session-pid-"),
        "caller 2 must answer from its own worker's transcript (unique session), \
         got {sess2:?}"
    );
    let (id2, pid2) = driven_worker(&out2.stderr);

    daemon.wait_for("Released worker", 2, LEDGER);
    daemon.wait_for("Spawning worker", 3, REPLACE);
    daemon.wait_for("settled and ready", 3, WARMUP);
    assert_pid_gone(pid2, Duration::from_secs(10));

    // ── caller 3: stream-json ───────────────────────────────────────────────
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .args(["--output-format", "stream-json"])
        .arg(PROMPT);
    let out3 = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out3.code,
        Some(0),
        "caller 3 must succeed\nstdout:\n{}\nstderr:\n{}",
        out3.stdout,
        out3.stderr
    );
    let sess3 = result_session_id(&out3.stdout)
        .unwrap_or_else(|| panic!("caller 3 must forward a result event\nstdout: {out3:?}"));
    assert_ne!(
        sess2, sess3,
        "caller 3 must answer from its own worker's transcript, not caller 2's"
    );
    // Transcript-offset isolation, third caller deep: by now two complete
    // transcripts from earlier callers sit in the same projects dir; caller
    // 3's tail must start from its own worker's state — exactly its own
    // result event, never a re-emission of an earlier caller's events.
    assert_eq!(
        result_event_count(&out3.stdout),
        1,
        "caller 3's stream must carry exactly its own result event\nstdout:\n{}",
        out3.stdout
    );
    assert!(
        !out3.stdout.contains(&sess2),
        "caller 3's stream must not carry caller 2's transcript events \
         (session {sess2})\nstdout:\n{}",
        out3.stdout
    );
    let (id3, pid3) = driven_worker(&out3.stderr);

    daemon.wait_for("Released worker", 3, LEDGER);
    daemon.wait_for("Spawning worker", 4, REPLACE);
    daemon.wait_for("settled and ready", 4, WARMUP);
    assert_pid_gone(pid3, Duration::from_secs(10));

    // Identity: a released worker is never handed a second caller.
    assert_ne!(id1, id2, "worker id reused across callers 1 and 2");
    assert_ne!(id2, id3, "worker id reused across callers 2 and 3");
    assert_ne!(pid1, pid2, "worker pid reused across callers 1 and 2");
    assert_ne!(pid2, pid3, "worker pid reused across callers 2 and 3");

    // At rest the daemon holds exactly ONE PTY master: every driven worker's
    // master fd was closed at destroy, and the pool is exactly the last
    // replacement.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        daemon_pty_fd_count(daemon_pid),
        1,
        "the daemon must hold exactly one PTY master at rest (each driven \
         worker's fd closed with its release, one replacement warm)"
    );

    // The daemon's ledger: three assignments with the three distinct ids,
    // three releases — zero leaks, no double-send on any caller.
    let daemon_stderr = daemon.stderr();
    let assigned: Vec<&String> = daemon_stderr
        .iter()
        .filter(|l| l.contains("Assigned worker"))
        .collect();
    let released = daemon_stderr
        .iter()
        .filter(|l| l.contains("Released worker"))
        .count();
    assert_eq!(
        assigned.len(),
        3,
        "exactly three workers may be handed out; stderr: {daemon_stderr:?}"
    );
    for id in [&id1, &id2, &id3] {
        assert!(
            assigned.iter().any(|l| l.contains(id)),
            "the assignments must include the worker caller drove ({id}); stderr: {daemon_stderr:?}"
        );
    }
    assert_eq!(
        released, 3,
        "each caller must release exactly once; stderr: {daemon_stderr:?}"
    );

    daemon.terminate(1);
}

// ── Bullet 3b: malformed daemon acquire responses ────────────────────────────

/// What the fake broken daemon does after draining the client's acquire
/// frame. Each shape is a distinct malformation the parent acceptance names:
/// no answer at all, an unparseable answer, and an answer that promises a
/// worker but transfers no fd.
enum Abuse {
    /// Close the connection without answering — the client must see the
    /// lost exchange as a protocol failure, not hang.
    CloseMidExchange,
    /// Answer with a well-formed frame whose payload is valid JSON of no
    /// protocol type.
    WrongShape,
    /// Answer with a well-formed `worker_assigned` frame but never send the
    /// SCM_RIGHTS fd the assignment is useless without.
    AssignmentWithoutFd,
}

/// Bind `socket` and serve exactly one connection, draining the client's
/// acquire frame (so what the client reports is the malformation, never a
/// lost send race) and then reacting per `abuse`. Draining first also makes
/// the close deterministic for [`Abuse::CloseMidExchange`]: the client has
/// finished sending, so its failure surfaces on the read side.
fn serve_one_abusive_exchange(socket: &Path, abuse: Abuse) -> std::thread::JoinHandle<()> {
    let listener = UnixListener::bind(socket).expect("bind the broken daemon");
    std::thread::spawn(move || {
        use std::io::{Read, Write};

        let (mut stream, _) = listener.accept().expect("accept the pooled client");

        // Drain the length-prefixed acquire frame.
        let mut prefix = [0u8; 4];
        stream
            .read_exact(&mut prefix)
            .expect("read the client's length prefix");
        let len = u32::from_be_bytes(prefix) as usize;
        let mut payload = vec![0u8; len];
        stream
            .read_exact(&mut payload)
            .expect("read the client's acquire frame");

        match abuse {
            Abuse::CloseMidExchange => {} // dropping the stream closes it
            Abuse::WrongShape => {
                let body: &[u8] = br#"{"type":"mystery_shape"}"#;
                stream
                    .write_all(&(body.len() as u32).to_be_bytes())
                    .expect("write the wrong-shape prefix");
                stream.write_all(body).expect("write the wrong-shape frame");
                std::thread::sleep(Duration::from_millis(300));
            }
            Abuse::AssignmentWithoutFd => {
                // One line on purpose: a raw string does NOT fold `\`-newline,
                // so the folded form shipped literal backslashes and failed the
                // JSON parse instead of exercising the missing fd transfer —
                // the malformation this arm exists to pin.
                let body: &[u8] = br#"{"type":"worker_assigned","worker_id":"fake-worker","message":"warm","stop_fifo":"/tmp/no-such-stop.fifo","pid":424242,"cwd":"/tmp"}"#;
                stream
                    .write_all(&(body.len() as u32).to_be_bytes())
                    .expect("write the assignment prefix");
                stream.write_all(body).expect("write the assignment frame");
                std::thread::sleep(Duration::from_millis(300));
            }
        }
        // Dropping the stream closes the connection.
    })
}

/// Shared body of the three malformed-daemon tests: the invocation must exit
/// 2 with the protocol-failure diagnostic, never fall back, run nothing
/// statelessly, and land well inside the `--timeout` budget it was given.
fn assert_malformed_daemon_fails_safely(fx: &Fixture, abuse: Abuse, abuse_name: &str) {
    let _server = serve_one_abusive_exchange(&fx.socket, abuse);

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .args(["--timeout", "20"])
        .arg(PROMPT);
    let started = Instant::now();
    let out = run(&mut cmd, FAST_BUDGET);
    let elapsed = started.elapsed();

    assert_eq!(
        out.code,
        Some(2),
        "{abuse_name}: a broken daemon must exit non-zero, not fall back\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("error: pool protocol failure"),
        "{abuse_name}: the error must name the protocol failure\nstderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("falling back"),
        "{abuse_name}: a broken daemon must never be masked by a fallback\nstderr:\n{}",
        out.stderr
    );
    assert!(
        out.stdout.trim().is_empty(),
        "{abuse_name}: the stateless session must not have run\nstdout:\n{}",
        out.stdout
    );
    // The acquire budget is the caller's --timeout (20 s) — a malformation
    // must surface long inside it, never by hanging out the whole budget.
    assert!(
        elapsed < Duration::from_secs(15),
        "{abuse_name}: the failure must land inside the caller deadline, took {elapsed:?}"
    );
}

/// The daemon accepts, drains the acquire, and closes without answering.
#[test]
fn daemon_close_mid_exchange_fails_safely_within_the_caller_timeout() {
    let fx = Fixture::start();
    assert_malformed_daemon_fails_safely(&fx, Abuse::CloseMidExchange, "close mid-exchange");
}

/// The daemon answers with a well-formed frame carrying valid JSON of no
/// protocol type.
#[test]
fn daemon_wrong_shape_response_fails_safely_within_the_caller_timeout() {
    let fx = Fixture::start();
    assert_malformed_daemon_fails_safely(&fx, Abuse::WrongShape, "wrong-shape response");
}

/// The daemon answers with a well-formed `worker_assigned` frame but the fd
/// transfer never comes — an assignment is only usable with its PTY master.
#[test]
fn assignment_without_fd_transfer_fails_safely_within_the_caller_timeout() {
    let fx = Fixture::start();
    assert_malformed_daemon_fails_safely(&fx, Abuse::AssignmentWithoutFd, "assignment without fd");
}
