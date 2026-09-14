//! `serve` subcommand end-to-end tests (bead claudepr-7f088327).
//!
//! These tests invoke the *compiled* `claude-print` binary as a subprocess,
//! pinning `--claude-binary` to `mock-claude` (same hermetic strategy as
//! `tests/binary_e2e.rs`). They pin the ADR-005 serve contract:
//!
//!   * **enter the server path** — `serve` binds the selected Unix socket and
//!     starts maintaining the pool; it never falls through to ordinary prompt
//!     validation (a fall-through would exit 4 with "no prompt provided",
//!     since no positional prompt accompanies the subcommand). The shared
//!     missing-binary check runs before the dispatch: serve with a bogus
//!     `--claude-binary` exits 2 before binding anything.
//!   * **invalid pool size** — `--pool-size 0`, `--pool-size` past
//!     `MAX_POOL_SIZE`, and non-numeric values all exit 2 with actionable
//!     stderr before any worker is spawned.
//!   * **socket setup failures** — an unbindable socket path (parent a file,
//!     or parent missing entirely) exits 2 with actionable stderr naming the
//!     exact path and what to check.
//!   * **safe local permissions** — the socket node is user-only (0600),
//!     whatever umask the invoking shell carried (dedicated pin under a
//!     deliberately cleared umask: `serve_socket_is_owner_only_regardless_of_umask`).
//!   * **population** — the daemon warms exactly the requested number of
//!     workers (mock-claude completes the full warmup: trust dialog →
//!     dismissal → idle-settle, then blocks awaiting a prompt that warmup
//!     never sends).
//!   * **clean bounded shutdown** — SIGINT *and* SIGTERM stop the daemon
//!     within a bounded time, remove the socket file, and exit 0 (a
//!     supervisor stopping the service is not a failure); SIGINT *then*
//!     SIGTERM back-to-back still tears down completely — the second signal
//!     is inert once the shutdown flag is set — and so is a repeated signal
//!     landing inside active teardown (claudepr-41b6a99b).
//!   * **child reaping** — after shutdown no worker process survives in
//!     /proc: not alive (leaked), not unreaped (zombie).
//!   * **malformed clients** — clients that close mid-frame or send an
//!     absurd length prefix are dropped cleanly; the daemon keeps serving a
//!     well-formed acquire afterwards (claudepr-b78932de audit repair).
//!   * **default path unchanged** — an ordinary prompt invocation without the
//!     subcommand still runs the plain session path, with no daemon behavior.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

/// A captured subprocess outcome: exit code (or `None` if killed on timeout),
/// and decoded stdout/stderr.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
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
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Budget for the negative (fast-exit) cases. Generous ceiling that still
/// fails fast on a wedge.
const BUDGET: Duration = Duration::from_secs(30);

/// How long a shutdown signal may take to produce an exited daemon. The
/// accept loop notices the signal within ACCEPT_POLL_TIMEOUT_MS (250 ms),
/// `shutdown_all` gives each worker a 2 s SIGTERM grace before SIGKILL, so
/// even a pathological multi-worker teardown lands far inside this bound.
const SHUTDOWN_BOUND: Duration = Duration::from_secs(15);

// ── Child-process accounting (/proc scan) ────────────────────────────────────

/// Parse the parent PID out of the contents of `/proc/<pid>/stat`.
///
/// The comm field may contain spaces and parentheses of its own, so scanning
/// starts after its closing `)`; the fields then resume with state (field 3)
/// followed by ppid (field 4).
fn stat_ppid(stat: &str) -> Option<u32> {
    let rest = stat.rsplit_once(')')?.1.trim_start();
    let mut fields = rest.split_whitespace();
    fields.next()?; // state
    fields.next()?.parse().ok()
}

/// Every live process whose parent is `ppid`, read from /proc.
///
/// Pool workers are forked directly by the daemon (`PtySpawner::spawn` — no
/// intermediate shell) and nothing else in serve mode forks, so while the
/// daemon is alive this is exactly its set of mock-claude workers.
fn children_of(ppid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
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

/// The reaping half of the shutdown contract: once the daemon has exited,
/// every pooled worker pid is gone from /proc — neither alive (leaked) nor
/// dead-but-unreaped (zombie).
///
/// The daemon removes the socket and exits only *after* `shutdown_all` has
/// reaped every worker (each worker teardown ends in a blocking `waitpid`),
/// so a worker pid visible here is teardown that never ran. `grace` absorbs
/// only the reaping instant the daemon no longer controls — the moment
/// between the test observing the exit and init finishing the reap of an
/// orphaned zombie — and is poll-width short: an orphaned mock-claude whose
/// PTY master died with the daemon self-exits on its next stdin read, so pid
/// evidence disappears quickly in every scenario. This is a tripwire for
/// teardown that never ran, not a leniency window for a stuck worker.
fn assert_workers_gone(workers: &[u32], grace: Duration) {
    let deadline = Instant::now() + grace;
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
            "worker processes were still present in /proc after daemon exit \
             (leaked or unreaped; the letter is the state from \
             /proc/<pid>/stat): {survivors:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

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
    let socket = blocker.join("pool.sock");

    let out = run(
        claude_print()
            .args(["serve", "--pool-size", "1", "--socket"])
            .arg(&socket),
        BUDGET,
    );

    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("failed to set up pool socket"),
        "stderr must name the socket failure: {}",
        out.stderr
    );
    // Actionable means the operator can act on it: the failing path named in
    // full, plus what to check about it.
    assert!(
        out.stderr
            .contains(socket.to_str().expect("utf-8 tempdir path")),
        "stderr must name the exact socket path that failed: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("check that the parent directory"),
        "stderr must say what to check: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("Spawning worker"),
        "a socket failure must abort before any worker spawn: {}",
        out.stderr
    );
}

// The other unbindable shape: the socket path's parent directory does not
// exist. Same contract — exit 2, the failing path named, nothing spawned.
#[test]
fn serve_fails_fast_on_socket_path_with_missing_parent_directory() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("no-such-dir").join("pool.sock");

    let out = run(
        claude_print()
            .args(["serve", "--pool-size", "1", "--socket"])
            .arg(&socket),
        BUDGET,
    );

    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("failed to set up pool socket"),
        "stderr must name the socket failure: {}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains(socket.to_str().expect("utf-8 tempdir path")),
        "stderr must name the exact socket path that failed: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("Spawning worker"),
        "a socket failure must abort before any worker spawn: {}",
        out.stderr
    );
}

// The shared missing-binary check runs BEFORE the serve dispatch (main.rs:
// the pool spawns workers with this binary, so entering serve without it
// would just churn failing warmups). This pins that ordering: serve with a
// bogus `--claude-binary` must exit 2 with actionable stderr before binding
// any socket. If dispatch ever moved above the check, this test stops exiting
// fast and dies at the budget with a bound daemon instead.
#[test]
fn serve_rejects_missing_claude_binary_before_binding() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut cmd = Command::new(workspace_bin("claude-print"));
    let out = run(
        cmd.arg("--claude-binary")
            .arg(dir.path().join("no-such-claude"))
            .args(["serve", "--pool-size", "1", "--socket"])
            .arg(&socket),
        BUDGET,
    );

    assert_eq!(out.code, Some(2), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("not found"),
        "stderr must say the binary was not found: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("Listening on"),
        "the binary check must precede the bind: {}",
        out.stderr
    );
    assert!(
        !socket.exists(),
        "the binary check must precede socket setup"
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
        Self::start_with_env(mock, socket, size, &[])
    }

    /// [`Self::start`] plus extra environment for the daemon — which
    /// `build_child_env` forwards to the mock-claude workers (only the
    /// CLAUDE_CODE session markers are scrubbed), so `MOCK_*` knobs set here
    /// reach every worker the pool spawns.
    fn start_with_env(
        mock: &Path,
        socket: &Path,
        size: Option<&str>,
        env: &[(&str, &str)],
    ) -> Daemon {
        let bin = workspace_bin("claude-print");
        let mut cmd = Command::new(&bin);
        cmd.arg("--claude-binary").arg(mock).arg("serve");
        if let Some(size) = size {
            cmd.args(["--pool-size", size]);
        }
        cmd.arg("--socket").arg(socket).arg("--verbose");
        for (key, value) in env {
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

    /// SIGTERM the daemon and hold it to the full shutdown contract. The
    /// 20 s bound doubles as the SIGKILL-fallback window the older contract
    /// tests were written against.
    fn terminate(self, socket: &Path, expected_workers: usize) -> Outcome {
        self.shutdown_daemon(
            &[Signal::SIGTERM],
            Duration::from_secs(20),
            socket,
            Some(expected_workers),
            true,
        )
    }

    /// Signal the daemon with each of `sigs` in order and pin the entire
    /// shutdown contract: the process exits 0 within `bound` of the signals,
    /// the socket file is removed (unless `expect_socket_removed` is false —
    /// the foreign-file ownership test replaces the path and inverts that
    /// one assertion), and — the reaping check — every pooled worker is gone
    /// from /proc afterwards.
    ///
    /// The worker set is snapshotted from /proc *before* signaling: pool
    /// workers are forked directly by the daemon, so while it lives they are
    /// exactly its children. `expected_workers` — `Some(n)` — additionally
    /// proves the snapshot captured the whole pool (a respawn-churning daemon
    /// would fail here before the reaping assertion could go vacuous); `None`
    /// skips that count (used by the dispatch-entry test, which signals while
    /// the first spawn may still be mid-fork and pins no exact population).
    /// After the daemon exits it can no longer hold zombies — orphans are
    /// re-parented immediately — so the post-exit check polls a short grace
    /// window for every pid to vanish: a leaked worker survives indefinitely,
    /// and the grace absorbs only the instant init needs to finish reaping an
    /// orphaned zombie.
    fn shutdown_daemon(
        self,
        sigs: &[Signal],
        bound: Duration,
        socket: &Path,
        expected_workers: Option<usize>,
        expect_socket_removed: bool,
    ) -> Outcome {
        self.shutdown_daemon_inner(
            sigs,
            None,
            bound,
            socket,
            expected_workers,
            expect_socket_removed,
        )
    }

    /// [`Self::shutdown_daemon`], with the difference that matters when the
    /// signal arrives: the first signal is delivered immediately, and every
    /// signal in `trailing` is held back until stderr shows
    /// `mid_teardown_needle` — so they land *inside* the work that line
    /// proves is under way, instead of racing the first signal into the
    /// accept loop's 250 ms poll tick.
    #[allow(clippy::too_many_arguments)] // same shape as shutdown_daemon plus the hold
    fn shutdown_daemon_during_teardown(
        self,
        first: Signal,
        mid_teardown_needle: &str,
        trailing: &[Signal],
        bound: Duration,
        socket: &Path,
        expected_workers: Option<usize>,
        expect_socket_removed: bool,
    ) -> Outcome {
        self.shutdown_daemon_inner(
            &[first],
            Some((mid_teardown_needle, trailing)),
            bound,
            socket,
            expected_workers,
            expect_socket_removed,
        )
    }

    fn shutdown_daemon_inner(
        mut self,
        sigs: &[Signal],
        hold: Option<(&str, &[Signal])>,
        bound: Duration,
        socket: &Path,
        expected_workers: Option<usize>,
        expect_socket_removed: bool,
    ) -> Outcome {
        let workers = children_of(self.child.id());
        if let Some(expected) = expected_workers {
            assert_eq!(
                workers.len(),
                expected,
                "daemon must hold exactly {expected} worker processes at \
                 shutdown time; found {workers:?}"
            );
        }

        let start = Instant::now();
        let pid = Pid::from_raw(self.child.id() as i32);
        // The first signal must always find a live daemon — nothing else
        // kills it, so delivery is load-bearing for the contract.
        kill(pid, sigs[0]).expect("failed to signal daemon");

        // Remaining legs: fired immediately (`hold` is None — a trailing
        // signal races the teardown the earlier ones started, and if the
        // daemon already exited the pid is gone and delivery is moot), or
        // held back until stderr proves the named work is under way.
        let holding_for_teardown = hold.is_some();
        let trailing: &[Signal] = match hold {
            None => &sigs[1..],
            Some((needle, held)) => {
                self.wait_for(needle, 1, Duration::from_secs(10));
                held
            }
        };
        for sig in trailing {
            if holding_for_teardown {
                // A held-back signal exists to land inside teardown; if the
                // daemon is already gone the window this test pins has
                // collapsed (e.g. workers died without burning their grace)
                // and the contract below would pass vacuously — fail loudly
                // instead. Teardown holds the window open for seconds, so
                // this cannot flake on a healthy daemon.
                assert!(
                    matches!(self.child.try_wait(), Ok(None)),
                    "daemon exited before the held-back {sig:?} could be \
                     delivered — the mid-teardown window collapsed"
                );
            }
            let _ = kill(pid, *sig);
        }

        let code = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    assert!(
                        start.elapsed() < bound,
                        "daemon did not exit within {bound:?} of {sigs:?} — shutdown must be \
                         bounded; workers: {workers:?}"
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => panic!("failed to reap daemon after {sigs:?}"),
            }
        };

        // The daemon writes nothing to stdout; the pipe is at EOF by exit.
        // `Child::wait_with_output` would move `self.child` out from under
        // the Drop impl, so drain the taken handle instead — the process is
        // already reaped by the try_wait loop above.
        let mut stdout_bytes = Vec::new();
        if let Some(mut pipe) = self.child.stdout.take() {
            use std::io::Read;
            let _ = pipe.read_to_end(&mut stdout_bytes);
        }
        let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();

        if let Some(reader) = self.reader.take() {
            reader.join().expect("stderr reader thread");
        }

        let stderr = self.stderr().join("\n");
        assert_eq!(
            code,
            Some(0),
            "a shutdown signal is a clean stop, not a failure; stderr: {stderr}"
        );
        if expect_socket_removed {
            assert!(
                !socket.exists(),
                "clean shutdown must remove the socket file"
            );
        }
        assert_workers_gone(&workers, Duration::from_secs(2));

        Outcome {
            code,
            stdout,
            stderr,
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Assertion-failure escape hatch: never leak the daemon or its
        // mock-claude workers into the rest of the suite. Skip the kill when
        // the child has already been reaped (a completed shutdown_daemon) —
        // its pid is recyclable, and a blind SIGKILL there could hit an
        // unrelated process.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

// The dispatch pin in its purest form, decoupled from the warmup
// choreography: `serve` must enter the server path — bind the selected
// socket, run the accept/maintain loop — and never consult prompt validation
// or stdin. A fall-through regression exits 4 with "no prompt provided"
// (no positional prompt accompanies the subcommand, and stdin is null here);
// the full-lifecycle tests below would only surface that as a settle-wait
// timeout, so this one names the contract and fails in seconds. No exact
// worker count is pinned (the first spawn may still be mid-fork when the
// signal lands — population is owned by the tests below); whatever children
// existed at signal time must still be gone, which shutdown_daemon's `None`
// mode asserts.
#[test]
fn serve_dispatch_enters_the_server_path_and_never_validates_a_prompt() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut daemon = Daemon::start(&mock, &socket, Some("1"));
    daemon.wait_for("Listening on", 1, Duration::from_secs(30));
    assert!(
        socket.exists(),
        "entering the server path must bind the selected socket"
    );
    // The maintain loop is running: the pool below the target is being topped
    // up, not waiting on a prompt.
    daemon.wait_for("Spawning worker", 1, Duration::from_secs(30));
    assert!(
        !daemon
            .stderr()
            .iter()
            .any(|l| l.contains("no prompt provided")),
        "serve must not fall through to prompt validation: {:?}",
        daemon.stderr()
    );

    let out = daemon.shutdown_daemon(
        &[Signal::SIGTERM],
        SHUTDOWN_BOUND,
        &socket,
        None, // no exact population: the spawn raced above is not pinned here
        true,
    );
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("Shutdown complete"),
        "the entered server path must still stop cleanly: {}",
        out.stderr
    );
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

    let out = daemon.terminate(&socket, 2);

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

    let out = daemon.terminate(&socket, 1);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
}

// Named pin for the safe-permissions bullet (claudepr-2075eea1 coverage
// audit): the population test above asserts the same mode on its happy path,
// but only under whatever umask the machine running the suite happened to
// start with — a chmod regression there is caught only because CI's ambient
// umask is narrow. This makes the bullet its own named failure and removes
// the ambient-umask dependency: the daemon is spawned under a deliberately
// cleared umask 000, so if bind_socket ever drops its explicit
// set_permissions (src/pool.rs) or its narrowed-umask bind window, the node
// comes out 0777 — group/world-connectable, i.e. anyone who can reach the
// path can acquire a warmed worker — and the exact-equality assert fails
// loudly instead of leaning on the environment.
//
// The flip is process-wide but held only across the spawn: the child
// inherits the mask at fork, and bind_socket then narrows and chmods itself,
// so nothing the daemon does afterwards observes the test process's mask.
// Concurrent tests in this binary create no umask-sensitive state in that
// window (tempfile pins its dirs/files to 0700/0600; every socket goes
// through bind_socket), so the few milliseconds cannot perturb them.
#[test]
fn serve_socket_is_owner_only_regardless_of_umask() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let previous_mask = unsafe { libc::umask(0o000) };
    let mut daemon = Daemon::start(&mock, &socket, Some("1"));
    unsafe {
        libc::umask(previous_mask);
    }

    // bind_socket runs before "Listening on" is logged, so the mode is final
    // the moment the daemon announces the bind.
    daemon.wait_for("Listening on", 1, Duration::from_secs(30));

    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&socket)
        .expect("socket metadata")
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "socket must be owner-only even under umask 000, got {mode:o}"
    );

    // The daemon under that hostile umask still satisfies the full clean-stop
    // contract. No exact worker count is pinned (the first spawn may still be
    // mid-fork — population is owned by the tests above); whatever children
    // existed at signal time must be gone, which the `None` mode asserts.
    let out = daemon.shutdown_daemon(&[Signal::SIGTERM], SHUTDOWN_BOUND, &socket, None, true);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
}

// SIGINT (Ctrl-C on a foreground daemon) must take the same clean path as
// SIGTERM — the serve signal handler treats both alike — and the shutdown
// must be *bounded*: the signal may not merely eventually work, it must land
// inside SHUTDOWN_BOUND. Pool size 2 makes the reaping check cover a
// multi-worker teardown, and the teardown log pins that shutdown_all saw the
// whole pool rather than a subset.
#[test]
fn serve_sigint_stops_bounded_and_reaps_every_worker() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut daemon = Daemon::start(&mock, &socket, Some("2"));
    daemon.wait_for("settled and ready", 2, Duration::from_secs(90));

    let out = daemon.shutdown_daemon(&[Signal::SIGINT], SHUTDOWN_BOUND, &socket, Some(2), true);

    assert!(
        out.stderr.contains("cleaning up 2 workers"),
        "teardown must see the entire pool, not a subset: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Shutdown complete"),
        "shutdown must run to completion and report it: {}",
        out.stderr
    );

    // Teardown order (claudepr-ffaf4def): stop accepting, reap the children,
    // and only then remove the socket file. The stderr sequence pins the
    // observable half — the last worker-destroy line precedes the socket
    // removal, which precedes the completion line. destroy_worker is serial
    // inside shutdown_all, so the last destroy line also means every destroy
    // had started; the /proc check above pins that they finished.
    let last_destroy = out
        .stderr
        .rfind("Destroying worker")
        .expect("worker destroy lines present in verbose stderr");
    let socket_removed = out
        .stderr
        .find("Removed pool socket")
        .expect("socket removal line present in verbose stderr");
    let complete = out
        .stderr
        .find("Shutdown complete")
        .expect("completion line present");
    assert!(
        last_destroy < socket_removed && socket_removed < complete,
        "teardown must reap children before removing the socket, and report \
         completion last; stderr: {}",
        out.stderr
    );
}

// Acceptance shape for the shutdown-signaling bead: SIGINT *then* SIGTERM,
// back to back. The handlers stay installed through teardown, so the second
// signal re-sets the already-latched flag instead of killing the daemon by
// default disposition mid-cleanup — which would strand the workers teardown
// had not reaped yet. And once the flag is set the pool must never respawn:
// pinned by the "Spawning worker" count staying at its pre-signal level in
// the final stderr.
#[test]
fn serve_sigint_then_sigterm_stops_cleanly_without_respawn() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut daemon = Daemon::start(&mock, &socket, Some("2"));
    daemon.wait_for("settled and ready", 2, Duration::from_secs(90));

    let spawns_at_rest = daemon
        .stderr()
        .iter()
        .filter(|l| l.contains("Spawning worker"))
        .count();
    assert_eq!(
        spawns_at_rest,
        2,
        "expected a stable pool of 2 before signaling; stderr: {:?}",
        daemon.stderr()
    );

    let out = daemon.shutdown_daemon(
        &[Signal::SIGINT, Signal::SIGTERM],
        SHUTDOWN_BOUND,
        &socket,
        Some(2),
        true,
    );

    assert!(
        out.stderr.contains("cleaning up 2 workers"),
        "teardown must see the entire pool, not a subset: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Shutdown complete"),
        "the second signal must not abort teardown before it completes: {}",
        out.stderr
    );
    let spawns_total = out
        .stderr
        .lines()
        .filter(|l| l.contains("Spawning worker"))
        .count();
    assert_eq!(
        spawns_total, spawns_at_rest,
        "no worker may be spawned after the shutdown flag is set; stderr: {}",
        out.stderr
    );
}

// The back-to-back test above delivers its second signal microseconds after
// the first — before the accept loop's next poll tick, let alone teardown —
// so a teardown that *disarmed* the handlers on entry (reset default
// dispositions, a plausible "fix" for a spurious-double-shutdown bug) would
// pass every existing test and still die on a real repeated Ctrl-C:
// workers stranded mid-reap, socket left behind, exit killed-by-signal.
// This pin holds the door shut on that shape: the repeated SIGINT and the
// second SIGTERM land only after "cleaning up N workers" proves teardown is
// actively running, and must be inert — the full clean-stop contract holds
// anyway.
//
// The window is deterministic, not raced: the worker runs with
// MOCK_IGNORE_TERM_HUP, so it ignores destroy_worker's group SIGTERM and
// the SIGHUP of the master close and must be ended by the SIGKILL
// escalation after the full 2 s grace — teardown stays open for seconds
// while the held-back signals are delivered within tens of milliseconds of
// the needle appearing. shutdown_daemon_during_teardown asserts the daemon
// was still alive at each delivery, so a regression that collapses the
// window fails loudly instead of passing vacuously.
#[test]
fn serve_repeated_and_second_signals_during_teardown_do_not_wedge_or_respawn() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut daemon =
        Daemon::start_with_env(&mock, &socket, Some("1"), &[("MOCK_IGNORE_TERM_HUP", "1")]);
    daemon.wait_for("settled and ready", 1, Duration::from_secs(90));

    let spawns_at_rest = daemon
        .stderr()
        .iter()
        .filter(|l| l.contains("Spawning worker"))
        .count();
    assert_eq!(
        spawns_at_rest,
        1,
        "expected a stable pool of 1 before signaling; stderr: {:?}",
        daemon.stderr()
    );

    // SIGINT starts the shutdown; the repeat and the second, different
    // signal land inside the teardown it started.
    let out = daemon.shutdown_daemon_during_teardown(
        Signal::SIGINT,
        "cleaning up 1 workers",
        &[Signal::SIGINT, Signal::SIGTERM],
        SHUTDOWN_BOUND,
        &socket,
        Some(1),
        true,
    );

    assert!(
        out.stderr.contains("cleaning up 1 workers"),
        "teardown must have begun before the trailing signals landed: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Shutdown complete"),
        "signals landing mid-teardown must not abort it before completion: {}",
        out.stderr
    );
    let spawns_total = out
        .stderr
        .lines()
        .filter(|l| l.contains("Spawning worker"))
        .count();
    assert_eq!(
        spawns_total, spawns_at_rest,
        "no worker may be spawned by or after the mid-teardown signals; \
         stderr: {}",
        out.stderr
    );
    // exit 0 within SHUTDOWN_BOUND, socket removed, the worker gone from
    // /proc — pinned by shutdown_daemon_during_teardown above.
}

// The socket path can stop naming this daemon's socket while it runs —
// another daemon taking the path, an administrator, a test. The daemon holds
// its bound socket by inode and never watches the path, so shutdown must
// remove only what it created and leave the replacement exactly as found
// (claudepr-ffaf4def). Pre-repair, cleanup unlinked whatever sat at the path.
#[test]
fn serve_shutdown_leaves_a_foreign_file_at_the_socket_path_untouched() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut daemon = Daemon::start(&mock, &socket, Some("1"));
    daemon.wait_for("settled and ready", 1, Duration::from_secs(90));

    // Replace the daemon's socket with a foreign regular file. Inode numbers
    // are recycled after unlink, so this replacement may well reuse the
    // daemon's socket inode number — the ownership guard must not be fooled
    // (it also requires the occupant to be a socket).
    std::fs::remove_file(&socket).expect("remove the daemon's socket");
    std::fs::write(&socket, b"not the daemon's file").expect("write replacement file");

    let out = daemon.shutdown_daemon(
        &[Signal::SIGTERM],
        SHUTDOWN_BOUND,
        &socket,
        Some(1),
        false, // the socket assertion is inverted: the replacement must survive
    );

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("Shutdown complete"),
        "shutdown must run to completion: {}",
        out.stderr
    );
    assert_eq!(
        std::fs::read(&socket).expect("replacement file must survive shutdown"),
        b"not the daemon's file",
        "shutdown deleted a file at the socket path that this daemon never created"
    );
}

// A foreign file at the path *before* the daemon starts takes a different
// path through the ownership story: startup replaces it (the restart story —
// bind_socket unlinks whatever sits at the path), the identity is recorded
// from the daemon's own node, and shutdown then removes that node normally.
// This pins that the pre-start occupant leaves no stale claim behind — the
// guard must not preserve a file that merely *shared the path* before the
// bind, or a stopped daemon would strand its own socket file.
#[test]
fn serve_shutdown_removes_its_socket_even_when_a_foreign_file_preceded_it() {
    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    std::fs::write(&socket, b"pre-existing, replaced at bind").expect("write pre-start file");

    let mut daemon = Daemon::start(&mock, &socket, Some("1"));
    daemon.wait_for("settled and ready", 1, Duration::from_secs(90));

    let out = daemon.shutdown_daemon(&[Signal::SIGTERM], SHUTDOWN_BOUND, &socket, Some(1), true);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert!(
        !socket.exists(),
        "the daemon's own socket must be removed even though the path held a \
         foreign file before startup"
    );
}

// The serve dispatch and its signal handling now sit above the ordinary path
// in main(); this pins that they did not perturb it. A plain prompt run — no
// subcommand — still exits 0 with the text contract, shows no daemon behavior
// at all, and terminates promptly (run()'s kill-on-overrun is the bound: a
// regression into the accept loop would hang until the budget fires).
#[test]
fn default_non_serve_invocation_is_unchanged() {
    let config = tempfile::tempdir().unwrap();
    let socket = config.path().join("never-created.sock");

    let mut base = claude_print();
    let cmd = base.arg("plain prompt");
    cmd.env("XDG_CONFIG_HOME", config.path());
    let out = run(cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "plain prompt run must still succeed; stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stdout.trim().is_empty(),
        "text-mode stdout must stay non-empty; stderr: {}",
        out.stderr
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(out.stdout.trim()).is_err(),
        "text-mode stdout must not have turned into JSON: {}",
        out.stdout
    );
    for marker in ["[claude-print pool]", "Listening on", "Shutdown complete"] {
        assert!(
            !out.stdout.contains(marker) && !out.stderr.contains(marker),
            "plain run must show no pool-daemon behavior ({marker}); stdout: {}\nstderr: {}",
            out.stdout,
            out.stderr
        );
    }
    assert!(
        !socket.exists(),
        "serve machinery must not bind a socket for a plain run"
    );
}

// ── Malformed clients (claudepr-b78932de audit repair) ──────────────────────

/// Send one length-prefixed frame (the pool wire format: 4-byte big-endian
/// length, then the payload).
fn send_frame(stream: &mut std::os::unix::net::UnixStream, payload: &[u8]) {
    use std::io::Write;
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .expect("write frame length");
    stream.write_all(payload).expect("write frame body");
}

/// Read one length-prefixed response frame; `None` = server closed.
fn read_response(stream: &mut std::os::unix::net::UnixStream) -> std::io::Result<Option<String>> {
    use std::io::Read;
    let mut len_buf = [0u8; 4];
    if stream.read_exact(&mut len_buf).is_err() {
        return Ok(None);
    }
    let msg_len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; msg_len];
    stream.read_exact(&mut buf)?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

// Abusive connections must not degrade the daemon: a client that closes
// mid-frame ends its connection cleanly (the pre-repair reader spun its
// connection thread on sticky EOF forever — see read_frame's unit tests in
// src/pool.rs for the mechanism-level pins), and an absurd length prefix is
// refused instead of allocated. After the abuse the daemon must still be
// serving: a well-formed acquire gets a worker_assigned response, and the
// full shutdown contract holds on the (replenished) worker set.
#[test]
fn serve_survives_malformed_clients_and_keeps_serving() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let mock = workspace_bin("mock-claude");
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("pool.sock");

    let mut daemon = Daemon::start(&mock, &socket, Some("2"));
    daemon.wait_for("settled and ready", 2, Duration::from_secs(90));

    // (a) connects and closes without sending anything.
    drop(UnixStream::connect(&socket).expect("connect (a)"));

    // (b) sends half a length prefix, then closes.
    let mut truncator = UnixStream::connect(&socket).expect("connect (b)");
    truncator.write_all(&[0, 0]).expect("write partial prefix");
    drop(truncator);

    // (c) claims a frame larger than the daemon's cap.
    let mut glutton = UnixStream::connect(&socket).expect("connect (c)");
    glutton
        .write_all(&u32::MAX.to_be_bytes())
        .expect("write huge prefix");
    drop(glutton);

    // The daemon must still serve a well-formed acquire after all that.
    let mut client = UnixStream::connect(&socket).expect("connect (acquire)");
    send_frame(&mut client, br#"{"type": "acquire", "timeout_secs": 5}"#);
    let response = read_response(&mut client)
        .expect("read acquire response")
        .expect("server closed instead of answering a valid acquire");
    assert!(
        response.contains("worker_assigned"),
        "acquire after abusive clients must still be served: {response}"
    );

    // Acquiring one worker drops the pool below target, so the maintain tick
    // spawns a replenishment replacement (target counts Warming+Ready, not
    // the handed-out worker). Waiting for that third spawn both proves the
    // accept loop is still ticking and makes the /proc snapshot in terminate
    // deterministic.
    daemon.wait_for("Spawning worker", 3, Duration::from_secs(30));

    // A shutdown signal must still take the daemon through the full
    // clean-stop contract with the whole (replenished) worker set.
    let out = daemon.terminate(&socket, 3);
    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
}
