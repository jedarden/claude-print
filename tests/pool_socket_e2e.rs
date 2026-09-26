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
//!   * **per-invocation launch flags are inert on the pool** (claudepr-af36fc41,
//!     the `--mcp-config` leg) —
//!     `pooled_invocation_keeps_mcp_config_off_the_worker_argv_and_reports_it_unapplied`:
//!     the worker's `claude` is launched by the daemon with a fixed argv, so the
//!     client's `--mcp-config` never reaches any child argv (proved by a
//!     daemon-side MOCK_RECORD_ARGS dump of the worker's own argv) — the run
//!     still succeeds on the daemon's launch and the flag is named in the
//!     client's `not applied:` verbose diagnostic.
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
//!   * **concurrent acquisition and pool exhaustion** (claudepr-29abb756) —
//!     `concurrent_clients_exhaust_the_pool_and_the_surplus_caller_falls_back_statelessly`:
//!     two simultaneous callers take a `--pool-size 2` daemon's only two
//!     workers; a third caller arriving mid-exhaustion is answered `pool_full`
//!     and falls back statelessly behind exactly one verbose diagnostic; both
//!     pooled callers drive distinct workers and neither sees the other's
//!     prompt (`MOCK_ECHO_PROMPT` makes the injected prompt the answer).
//!   * **a released worker is never reused; sequential requests never cross** —
//!     `a_released_worker_is_never_reused_and_sequential_prompts_stay_isolated`:
//!     three callers with distinct prompts; each released worker's id is
//!     assigned exactly once, its process is gone before the next caller runs,
//!     a raw re-release of the id is refused `invalid_worker_id` at the wire,
//!     every caller's output carries only its own prompt/session/transcript,
//!     and the daemon's descriptors return to their at-rest count after every
//!     completed cycle (children, PTY masters, and the socket file are gone at
//!     clean shutdown).
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
        self.daemon_sized(1, extra_env)
    }

    /// The same, with an explicit pool size — the concurrent-acquisition
    /// coverage needs a pool that more callers can exhaust.
    fn daemon_sized(&self, pool_size: usize, extra_env: &[(&str, &str)]) -> Daemon {
        Daemon::start(
            &self.socket,
            self.home.path().to_str().expect("utf-8 home"),
            pool_size,
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
    fn start(socket: &Path, home: &str, pool_size: usize, extra_env: &[(&str, &str)]) -> Daemon {
        let bin = workspace_bin("claude-print");
        let mock = workspace_bin("mock-claude");
        let mut cmd = Command::new(&bin);
        cmd.arg("--claude-binary")
            .arg(&mock)
            .arg("serve")
            .args(["--pool-size", &pool_size.to_string()])
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

/// Count EVERY descriptor the daemon holds open. The generic counterpart to
/// [`daemon_pty_fd_count`]: a leaked pipe (a warmup that never ended), an
/// accepted pool-socket connection nobody closed, or an unreaped worker's
/// descriptors all grow this number across warm/replace cycles, while a clean
/// daemon returns to its at-rest count after every one of them.
fn daemon_fd_count(daemon_pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{daemon_pid}/fd"))
        .unwrap_or_else(|e| panic!("read /proc/{daemon_pid}/fd: {e}"))
        .filter_map(|e| e.ok())
        .count()
}

/// Sample the daemon's fd count a few times and take the minimum — an at-rest
/// reading. A sample taken the instant "settled and ready" prints can still
/// carry the finishing warmup thread's self-pipe; the minimum over three
/// samples 200 ms apart is the count a clean daemon comes back to.
fn at_rest_fd_count(daemon_pid: u32) -> usize {
    let mut floor = usize::MAX;
    for _ in 0..3 {
        floor = floor.min(daemon_fd_count(daemon_pid));
        std::thread::sleep(Duration::from_millis(200));
    }
    floor
}

/// Poll until the daemon's total fd count is back at or under `baseline`.
/// `baseline` is an at-rest count taken before any client ran, so a count
/// that will not come back down is a descriptor from a completed cycle that
/// the daemon leaked and will leak again on every future one.
fn wait_fds_back_to_baseline(daemon_pid: u32, baseline: usize, grace: Duration) {
    let start = Instant::now();
    loop {
        let count = daemon_fd_count(daemon_pid);
        if count <= baseline {
            return;
        }
        assert!(
            start.elapsed() < grace,
            "daemon fd count must return to its at-rest {baseline} after a \
             completed cycle; stuck at {count} — a descriptor leaked"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
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

/// Speak one length-prefixed pool-protocol request directly to the daemon and
/// return its decoded reply. A test-side raw client: the happy-path tests
/// drive the compiled CLI, but "a released worker id is out of the pool" is a
/// daemon-ledger property the CLI never surfaces — it discards release
/// replies — so it is pinned at the wire itself.
fn raw_exchange(socket: &Path, request: serde_json::Value) -> serde_json::Value {
    use std::io::{Read, Write};

    let mut stream =
        std::os::unix::net::UnixStream::connect(socket).expect("connect to the pool socket");
    let body = serde_json::to_vec(&request).expect("serialize request");
    stream
        .write_all(&(body.len() as u32).to_be_bytes())
        .expect("write the request length prefix");
    stream.write_all(&body).expect("write the request body");

    let mut prefix = [0u8; 4];
    stream
        .read_exact(&mut prefix)
        .expect("read the reply length prefix");
    let len = u32::from_be_bytes(prefix) as usize;
    let mut reply = vec![0u8; len];
    stream.read_exact(&mut reply).expect("read the reply body");
    serde_json::from_slice(&reply).expect("the reply must be valid JSON")
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

/// `--mcp-config` on a pooled invocation (claudepr-af36fc41): the flag cannot
/// reach the child, because the worker's `claude` was launched by the daemon
/// with a fixed argv before the client existed — pooled invocations do not
/// build child argv at all. Three proofs in one run:
///
///  * the daemon-side `MOCK_RECORD_ARGS` dump (inherited by the worker it
///    spawns) shows the worker's own argv carries `--settings=` and
///    `--setting-sources=` and NO mcp flags, even with the client passing
///    `--mcp-config`;
///  * the invocation is inert, never fatal — it still acquires, drives, and
///    releases a prewarmed worker and answers from its transcript;
///  * the client's `--verbose` diagnostic lists `--mcp-config` in its
///    `not applied:` line rather than silently dropping the flag.
#[test]
fn pooled_invocation_keeps_mcp_config_off_the_worker_argv_and_reports_it_unapplied() {
    let fx = Fixture::start();
    // The recording path lives under the fixture's HOME so the worker (which
    // inherits the daemon's env via build_child_env) can write it; the client
    // never sees MOCK_RECORD_ARGS, so the only recorder is the worker itself.
    let record = fx.home.path().join("worker_argv");
    let record_str = record.to_string_lossy().into_owned();
    let mut daemon = fx.daemon(&[("MOCK_RECORD_ARGS", record_str.as_str())]);
    daemon.wait_for("settled and ready", 1, WARMUP);

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg("--mcp-config")
        .arg("/client-side-mcp.json")
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "the pooled invocation with --mcp-config must still succeed\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Hello from mock_claude"),
        "the answer must come from the driven worker's transcript\nstdout:\n{}",
        out.stdout
    );
    assert!(
        out.stderr.contains("driving prewarmed worker"),
        "the client must drive a prewarmed worker, not fall back\nstderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("falling back"),
        "a healthy pool must never fall back\nstderr:\n{}",
        out.stderr
    );
    let not_applied = out
        .stderr
        .lines()
        .find(|l| l.contains("not applied:"))
        .unwrap_or_else(|| {
            panic!(
                "the client must report the inert flag in a 'not applied:' \
                 diagnostic\nstderr:\n{}",
                out.stderr
            )
        });
    assert!(
        not_applied.contains("--mcp-config"),
        "the 'not applied:' diagnostic must name --mcp-config: {not_applied}"
    );

    // The worker's own argv, recorded at daemon spawn before the client ran:
    // the daemon's launch and nothing else — the client's flag never touched
    // a child argv on the pool path.
    let bytes = std::fs::read(&record)
        .unwrap_or_else(|e| panic!("worker argv recording missing at {}: {e}", record.display()));
    let worker_args: Vec<String> = bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect();
    assert!(
        worker_args.iter().any(|a| a.starts_with("--settings=")),
        "the worker argv must carry the relay --settings= (recording sanity), \
         got: {worker_args:?}"
    );
    assert!(
        worker_args.iter().any(|a| a == "--setting-sources="),
        "pool workers launch isolated (--setting-sources=), got: {worker_args:?}"
    );
    assert!(
        !worker_args.iter().any(|a| a == "--strict-mcp-config"),
        "--strict-mcp-config must never reach a pooled worker's argv, got: {worker_args:?}"
    );
    assert!(
        !worker_args.iter().any(|a| a == "--mcp-config"),
        "--mcp-config must never reach a pooled worker's argv, got: {worker_args:?}"
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

// ── Bullet 4: concurrent acquisition, exhaustion, and released-worker reuse ──

/// Two clients acquire a `--pool-size 2` daemon's only two workers
/// CONCURRENTLY; a third caller arriving while the pool is exhausted is
/// answered `pool_full` and falls back statelessly behind exactly one verbose
/// diagnostic; the two pooled callers drive distinct workers and neither sees
/// the other's prompt.
///
/// Determinism: `MOCK_DELAY_STOP` holds each driven worker's Stop payload for
/// 8 s, so once the daemon ledger shows both assignments the exhaustion window
/// is ~8 s wide while the surplus caller's whole stateless run takes ~1 s —
/// its `pool_full` answer is not a race outcome. Both assignments are awaited
/// on the ledger BEFORE the surplus caller connects, so its acquire cannot
/// precede an assignment, and its fallback cannot itself be the reason the
/// pool had nothing ready.
///
/// `MOCK_ECHO_PROMPT` makes each worker answer with the prompt it was actually
/// injected (the stateless surplus caller, whose own env carries no MOCK_*
/// knobs, keeps the default response) — so prompt isolation between concurrent
/// callers is asserted, not assumed, and any leak of the daemon's worker
/// environment into the fallback path is visible as a wrong answer.
#[test]
fn concurrent_clients_exhaust_the_pool_and_the_surplus_caller_falls_back_statelessly() {
    let fx = Fixture::start();
    let mut daemon = fx.daemon_sized(
        2,
        &[
            ("MOCK_UNIQUE_SESSION_ID", "1"),
            ("MOCK_ECHO_PROMPT", "1"),
            ("MOCK_DELAY_STOP", "8000"),
        ],
    );
    daemon.wait_for("settled and ready", 2, WARMUP);
    let daemon_pid = daemon.child.id();

    // The at-rest descriptor count BEFORE any client ran: the floor every
    // completed cycle must return to.
    let baseline_fds = at_rest_fd_count(daemon_pid);

    let alpha = "pool alpha prompt";
    let beta = "pool beta prompt";
    let mut driven: Option<((String, u32), (String, u32))> = None;

    std::thread::scope(|scope| {
        let alpha_run = scope.spawn(|| {
            let mut cmd = fx.client();
            cmd.arg("--pool-socket")
                .arg(&fx.socket)
                .arg("--verbose")
                .args(["--timeout", "90"])
                .arg(alpha);
            run(&mut cmd, POOLED_BUDGET)
        });
        let beta_run = scope.spawn(|| {
            let mut cmd = fx.client();
            cmd.arg("--pool-socket")
                .arg(&fx.socket)
                .arg("--verbose")
                .args(["--timeout", "90", "--output-format", "json"])
                .arg(beta);
            run(&mut cmd, POOLED_BUDGET)
        });

        // Both workers handed out: the pool is exhausted by construction.
        daemon.wait_for("Assigned worker", 2, REPLACE);

        // ── the surplus caller: pool_full → one diagnostic → stateless run ──
        let mut cmd = fx.client();
        cmd.arg("--pool-socket")
            .arg(&fx.socket)
            .arg("--verbose")
            .args(["--timeout", "60"])
            .arg("surplus pool prompt");
        let surplus = run(&mut cmd, FAST_BUDGET);

        assert_eq!(
            surplus.code,
            Some(0),
            "the surplus caller must still succeed, statelessly\nstdout:\n{}\nstderr:\n{}",
            surplus.stdout,
            surplus.stderr
        );
        assert!(
            surplus.stdout.contains("Hello from mock_claude"),
            "the surplus caller must answer from its own stateless mock — the \
             pool daemon's MOCK_* environment must not leak into the fallback \
             (the echoed surplus prompt would prove it did)\nstdout:\n{}",
            surplus.stdout
        );
        let pool_lines: Vec<&str> = surplus
            .stderr
            .lines()
            .filter(|l| l.contains("pool:"))
            .collect();
        assert_eq!(
            pool_lines.len(),
            1,
            "exactly one verbose diagnostic for the exhausted pool\nstderr:\n{}",
            surplus.stderr
        );
        assert!(
            pool_lines[0].contains("pool cannot serve")
                && pool_lines[0].contains("pool_full")
                && pool_lines[0].contains("falling back"),
            "the diagnostic must name the exhaustion and the fallback: {}",
            pool_lines[0]
        );
        assert!(
            !surplus.stderr.contains("driving prewarmed worker"),
            "the surplus caller must not claim a pooled worker\nstderr:\n{}",
            surplus.stderr
        );
        // The exhaustion answer is the whole documented behavior: no third
        // assignment may exist for the surplus caller, however briefly.
        assert_eq!(
            daemon
                .stderr()
                .iter()
                .filter(|l| l.contains("Assigned worker"))
                .count(),
            2,
            "an exhausted pool must not grow a third assignment; stderr: {:?}",
            daemon.stderr()
        );

        // ── the two pooled callers: distinct workers, isolated answers ──────
        let out_alpha = alpha_run.join().expect("alpha runner thread");
        let out_beta = beta_run.join().expect("beta runner thread");

        assert_eq!(
            out_alpha.code,
            Some(0),
            "the alpha caller must succeed\nstdout:\n{}\nstderr:\n{}",
            out_alpha.stdout,
            out_alpha.stderr
        );
        assert!(
            out_alpha.stderr.contains("driving prewarmed worker"),
            "the alpha caller must drive a prewarmed worker\nstderr:\n{}",
            out_alpha.stderr
        );
        assert!(
            out_alpha.stdout.contains(alpha),
            "the alpha caller's answer must echo its own prompt\nstdout:\n{}",
            out_alpha.stdout
        );
        assert!(
            !out_alpha.stdout.contains(beta),
            "the alpha caller must not see the beta caller's prompt\nstdout:\n{}",
            out_alpha.stdout
        );
        let a = driven_worker(&out_alpha.stderr);

        assert_eq!(
            out_beta.code,
            Some(0),
            "the beta caller must succeed\nstdout:\n{}\nstderr:\n{}",
            out_beta.stdout,
            out_beta.stderr
        );
        assert!(
            out_beta.stderr.contains("driving prewarmed worker"),
            "the beta caller must drive a prewarmed worker\nstderr:\n{}",
            out_beta.stderr
        );
        let v: serde_json::Value =
            serde_json::from_str(out_beta.stdout.trim()).unwrap_or_else(|e| {
                panic!(
                    "beta stdout is not valid JSON: {e}\nraw:\n{}",
                    out_beta.stdout
                )
            });
        let text = v["result"]
            .as_str()
            .unwrap_or_else(|| panic!("the beta result must be a string: {v}"));
        assert!(
            text.contains(beta),
            "the beta caller's answer must echo its own prompt, got: {text:?}"
        );
        assert!(
            !text.contains(alpha),
            "the beta caller must not see the alpha caller's prompt: {text:?}"
        );
        let sess_beta = v["session_id"]
            .as_str()
            .unwrap_or_else(|| panic!("the beta result must carry a session id: {v}"));
        assert!(
            sess_beta.starts_with("mock-session-pid-"),
            "the beta caller must answer from its own worker's unique session, \
             got {sess_beta:?}"
        );
        let b = driven_worker(&out_beta.stderr);

        assert_ne!(
            a.0, b.0,
            "concurrent callers must drive distinct worker ids"
        );
        assert_ne!(
            a.1, b.1,
            "concurrent callers must drive distinct worker processes"
        );
        driven = Some((a, b));
    });

    let (a, b) = driven.expect("both pooled callers must have been driven");

    // ── replacement after the completed handoffs, and cleanup at rest ───────
    daemon.wait_for("Released worker", 2, LEDGER);
    assert_pid_gone(a.1, Duration::from_secs(10));
    assert_pid_gone(b.1, Duration::from_secs(10));
    daemon.wait_for("Spawning worker", 4, REPLACE);
    daemon.wait_for("settled and ready", 4, WARMUP);

    // Ledger: exactly two assignments — each id exactly once, never re-issued
    // — and exactly two teardowns. Teardowns are counted via "Destroying
    // worker", which only a real destroy prints: the daemon's "Released
    // worker" line also fires for a REFUSED release (it logs after the
    // attempt, success or not), so it is not a ledger of actual releases.
    let ledger = daemon.stderr();
    assert_eq!(
        ledger
            .iter()
            .filter(|l| l.contains("Assigned worker"))
            .count(),
        2,
        "exactly two workers may be handed out in total; stderr: {ledger:?}"
    );
    for id in [&a.0, &b.0] {
        assert_eq!(
            ledger
                .iter()
                .filter(|l| l.contains(&format!("Assigned worker {id}")))
                .count(),
            1,
            "worker {id} must be assigned exactly once — never re-issued; stderr: {ledger:?}"
        );
    }
    assert_eq!(
        ledger
            .iter()
            .filter(|l| l.contains("Destroying worker"))
            .count(),
        2,
        "each released worker must be torn down exactly once; stderr: {ledger:?}"
    );
    for id in [&a.0, &b.0] {
        assert_eq!(
            ledger
                .iter()
                .filter(|l| l.contains(&format!("Destroying worker {id} (pid ")))
                .count(),
            1,
            "worker {id} must be destroyed exactly once; stderr: {ledger:?}"
        );
    }

    // At rest the daemon holds exactly one PTY master per pool slot...
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        daemon_pty_fd_count(daemon_pid),
        2,
        "the daemon must hold exactly one PTY master per pool worker at rest"
    );
    // ...and nothing else: the total descriptor count is back at the
    // pre-client at-rest floor.
    wait_fds_back_to_baseline(daemon_pid, baseline_fds, Duration::from_secs(10));

    // Clean shutdown with a full pool: both workers reaped, exit 0, socket
    // file removed.
    daemon.terminate(2);
}

/// Sequential callers against a pool-size-1 daemon: a released worker is never
/// reused — never re-issued per the ledger, never again a live process, and
/// refused `invalid_worker_id` at the wire after its release — and no caller's
/// prompt, session, transcript, or answer reaches any later caller.
///
/// `MOCK_ECHO_PROMPT` is what turns this into assertions rather than hopes:
/// each worker answers with the exact prompt it was injected, so cross-caller
/// contamination of prompts AND results is visible as the wrong words in the
/// output. `MOCK_UNIQUE_SESSION_ID` makes every worker mint a pid-derived
/// session id and transcript file, so transcript-path isolation is visible as
/// distinct sessions and, for the stream-json caller, as a stream carrying
/// exactly its own result event.
#[test]
fn a_released_worker_is_never_reused_and_sequential_prompts_stay_isolated() {
    let fx = Fixture::start();
    let mut daemon = fx.daemon(&[("MOCK_UNIQUE_SESSION_ID", "1"), ("MOCK_ECHO_PROMPT", "1")]);
    daemon.wait_for("settled and ready", 1, WARMUP);
    let daemon_pid = daemon.child.id();
    let baseline_fds = at_rest_fd_count(daemon_pid);

    let p1 = "sequential prompt one alpha";
    let p2 = "sequential prompt two beta";
    let p3 = "sequential prompt three gamma";

    // ── caller 1: text ──────────────────────────────────────────────────────
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(p1);
    let out1 = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out1.code,
        Some(0),
        "caller 1 must succeed\nstdout:\n{}\nstderr:\n{}",
        out1.stdout,
        out1.stderr
    );
    assert!(
        out1.stdout.contains(p1),
        "caller 1's answer must echo its own prompt\nstdout:\n{}",
        out1.stdout
    );
    let (id1, pid1) = driven_worker(&out1.stderr);

    // Released: the process is reaped before the reply is even answered, so
    // once the pid is gone the worker can never receive a second prompt.
    daemon.wait_for("Released worker", 1, LEDGER);
    assert_pid_gone(pid1, Duration::from_secs(10));

    // And the id is out of the pool at the wire: a raw re-release is refused
    // as unknown — the released worker can neither be handed out again nor
    // released again. (The CLI discards release replies, so this ledger
    // property is only observable with a raw protocol client.)
    let reply = raw_exchange(
        &fx.socket,
        serde_json::json!({"type": "release", "worker_id": id1}),
    );
    assert_eq!(
        reply["type"], "error",
        "a released id must be refused, not re-released: {reply}"
    );
    assert_eq!(
        reply["code"], "invalid_worker_id",
        "a released id must be refused as unknown, not reusable: {reply}"
    );

    // Replacement warm; the daemon's descriptors back at their at-rest floor.
    daemon.wait_for("settled and ready", 2, WARMUP);
    wait_fds_back_to_baseline(daemon_pid, baseline_fds, Duration::from_secs(10));

    // ── caller 2: json ──────────────────────────────────────────────────────
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .args(["--output-format", "json"])
        .arg(p2);
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
    let text2 = v2["result"]
        .as_str()
        .unwrap_or_else(|| panic!("caller 2's result must be a string: {v2}"));
    assert!(
        text2.contains(p2) && !text2.contains(p1) && !text2.contains(p3),
        "caller 2's result must carry its own prompt and no earlier caller's, \
         got: {text2:?}"
    );
    let sess2 = v2["session_id"]
        .as_str()
        .expect("caller 2's result must carry a session id")
        .to_string();
    assert!(
        sess2.starts_with("mock-session-pid-"),
        "caller 2 must answer from its own worker's unique transcript, got {sess2:?}"
    );
    let (id2, pid2) = driven_worker(&out2.stderr);

    daemon.wait_for("Released worker", 2, LEDGER);
    daemon.wait_for("Spawning worker", 3, REPLACE);
    daemon.wait_for("settled and ready", 3, WARMUP);
    assert_pid_gone(pid2, Duration::from_secs(10));
    wait_fds_back_to_baseline(daemon_pid, baseline_fds, Duration::from_secs(10));

    // ── caller 3: stream-json, third caller deep ────────────────────────────
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .args(["--output-format", "stream-json"])
        .arg(p3);
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
    assert_eq!(
        result_event_count(&out3.stdout),
        1,
        "caller 3's stream must carry exactly its own result event\nstdout:\n{}",
        out3.stdout
    );
    assert!(
        out3.stdout.contains(p3),
        "caller 3's stream must carry its own prompt's answer\nstdout:\n{}",
        out3.stdout
    );
    assert!(
        !out3.stdout.contains(p1) && !out3.stdout.contains(p2),
        "caller 3's stream must carry no earlier caller's prompt or transcript \
         events\nstdout:\n{}",
        out3.stdout
    );
    let (id3, pid3) = driven_worker(&out3.stderr);

    daemon.wait_for("Released worker", 3, LEDGER);
    daemon.wait_for("Spawning worker", 4, REPLACE);
    daemon.wait_for("settled and ready", 4, WARMUP);
    assert_pid_gone(pid3, Duration::from_secs(10));
    wait_fds_back_to_baseline(daemon_pid, baseline_fds, Duration::from_secs(10));

    // ── the never-reused ledger, in full ────────────────────────────────────
    let ledger = daemon.stderr();
    assert_eq!(
        ledger
            .iter()
            .filter(|l| l.contains("Assigned worker"))
            .count(),
        3,
        "exactly three workers may be handed out; stderr: {ledger:?}"
    );
    for id in [&id1, &id2, &id3] {
        assert_eq!(
            ledger
                .iter()
                .filter(|l| l.contains(&format!("Assigned worker {id}")))
                .count(),
            1,
            "worker {id} must be assigned exactly once — never re-issued for a \
             second prompt; stderr: {ledger:?}"
        );
    }
    assert_eq!(
        ledger
            .iter()
            .filter(|l| l.contains("Destroying worker"))
            .count(),
        3,
        "each released worker must be torn down exactly once (counted via \
         'Destroying worker' — the daemon's 'Released worker' line also fires \
         for a refused release); stderr: {ledger:?}"
    );
    for id in [&id1, &id2, &id3] {
        assert_eq!(
            ledger
                .iter()
                .filter(|l| l.contains(&format!("Destroying worker {id} (pid ")))
                .count(),
            1,
            "worker {id} must be destroyed exactly once; stderr: {ledger:?}"
        );
    }
    assert_ne!(id1, id2, "worker id reused across callers 1 and 2");
    assert_ne!(id2, id3, "worker id reused across callers 2 and 3");
    assert_ne!(id1, id3, "worker id reused across callers 1 and 3");
    assert_ne!(pid1, pid2, "worker pid reused across callers 1 and 2");
    assert_ne!(pid2, pid3, "worker pid reused across callers 2 and 3");
    assert_ne!(pid1, pid3, "worker pid reused across callers 1 and 3");

    // At rest: one PTY master (the replacement), then a clean shutdown — the
    // held worker reaped, exit 0, socket file removed.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        daemon_pty_fd_count(daemon_pid),
        1,
        "the daemon must hold exactly one PTY master at rest"
    );
    daemon.terminate(1);
}
