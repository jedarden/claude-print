//! Adversarial pool failure-path end-to-end (bead claudepr-c470b8aa, a
//! split-child of the ADR-005 umbrella claudepr-a03e32d7; built on the harness
//! from its predecessor claudepr-29abb756 / `tests/pool_socket_e2e.rs`).
//!
//! The companion binary pins the client matrix and the malformed-acquire
//! shapes against a scripted fake daemon. The sibling
//! `tests/pool_adversarial_e2e.rs` carries the concurrency shapes (distinct
//! workers under simultaneous acquisition, zero sibling contamination under a
//! shared cwd). This one is the failure-path complement: every scenario here
//! breaks a REAL `claude-print serve` daemon or a REAL client — SIGKILL,
//! SIGSTOP, orphaned workers, competing daemons —
//! and pins what the surviving side owes the ADR-005 contract:
//!
//!   * **daemon crash during handoff** —
//!     `daemon_crash_during_handoff_fails_safely_within_the_caller_timeout`:
//!     a daemon frozen mid-acquire and then SIGKILLed is a protocol failure
//!     (exit 2), never a fallback, well inside the caller budget; the orphaned
//!     warm worker dies with the daemon's PTY master and no socket cleanup
//!     runs, leaving exactly a stale node.
//!   * **manager stops responding** —
//!     `daemon_silence_fails_within_the_caller_timeout_and_recovers_after`:
//!     a SIGSTOPed daemon (listening, never answering) burns the client's
//!     whole acquire budget and surfaces as a protocol failure; after the
//!     daemon resumes it has leaked nothing — the abandoned connection is
//!     closed, its fd count returns to at-rest, the wedged assignment is
//!     never retried, and the next client is served pooled on a fresh
//!     replacement while the orphaned InUse worker is reclaimed only at
//!     shutdown.
//!   * **daemon crash mid-drive** —
//!     `daemon_crash_mid_drive_never_blocks_the_client_past_its_deadline`:
//!     a client whose daemon dies while it drives finishes its session anyway
//!     (the worker is daemon-independent once assigned), exits 0 inside its
//!     `--timeout`, its best-effort release against the dead daemon is bounded,
//!     the driven worker is not leaked (its PTY hangup ends it), and the next
//!     caller on the stale path falls back statelessly.
//!   * **client cancellation** —
//!     `killed_client_orphans_its_worker_which_is_never_reassigned`: a
//!     SIGKILLed client leaves its worker InUse; the worker is never handed to
//!     a second prompt and never destroyed early — the daemon warms a
//!     replacement for new clients while holding the orphan, and reclaims the
//!     orphan only at shutdown (INV-9, INV-13).
//!   * **manager restart/recovery** —
//!     `manager_restart_recovers_pooled_serving_on_the_same_socket_path`
//!     (SIGKILL → stale node → stateless fallback → a new daemon takes the
//!     path → pooled serving resumes) and
//!     `a_replacing_daemons_bind_survives_the_replaced_daemons_shutdown`
//!     (the daemon that lost its socket node at bind time must not unlink the
//!     winner's socket when it exits — ownership-checked cleanup).
//!   * **malformed daemon responses** —
//!     `daemon_garbage_json_response_fails_safely_within_the_caller_timeout`
//!     extends the companion's three acquire shapes with an unparseable
//!     response body (exit 2, no fallback, inside the budget).
//!   * **stateless fallback compatibility across every socket state and
//!     output format** — `absent_socket_falls_back_compatible_across_all_
//!     output_formats`, `stale_socket_falls_back_compatible_across_all_
//!     output_formats`,
//!     `unavailable_pool_falls_back_compatible_across_all_output_formats`:
//!     for each of absent / stale (bound, nothing listening) / unavailable
//!     (a listening daemon that answers `pool_full`), text, json, and
//!     stream-json fallback runs are compared against a no-flag baseline run
//!     of the same format in the same fixture — byte-identical stdout, so the
//!     fallback is the ordinary stateless session, not merely a similar one.

use std::io::BufRead;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

/// The one-word prompt every invocation in this file sends.
const PROMPT: &str = "Reply with exactly one word: pong";

/// Budget for a pooled or mid-drive invocation.
const POOLED_BUDGET: Duration = Duration::from_secs(120);

/// Budget for a plain fallback invocation.
const FAST_BUDGET: Duration = Duration::from_secs(30);

/// Ceiling for a daemon warmup to reach `settled and ready` (mock-claude
/// completes trust dialog → dismissal → idle-settle in well under a second;
/// the ceiling only absorbs a loaded CI box).
const WARMUP: Duration = Duration::from_secs(90);

/// Ceiling for the daemon's ledger to show a release or connection error.
const LEDGER: Duration = Duration::from_secs(15);

/// Ceiling for the daemon to spawn a replacement worker and settle it.
const REPLACE: Duration = Duration::from_secs(90);

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
/// strategy as `tests/pool_socket_e2e.rs`).
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
    reap(spawn(cmd), budget)
}

/// Wire stdio pipes and spawn. The mid-flight variant used where a test must
/// kill the child while it runs (client cancellation).
fn spawn(cmd: &mut Command) -> Child {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.spawn()
        .unwrap_or_else(|e| panic!("failed to spawn claude-print: {e}"))
}

/// Wait for a spawned child, killing it past `budget`, and collect its output.
fn reap(mut child: Child, budget: Duration) -> Outcome {
    let start = Instant::now();
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= start + budget {
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
    /// child-env build to every mock-claude worker it spawns).
    fn daemon(&self, extra_env: &[(&str, &str)]) -> Daemon {
        self.daemon_sized(1, extra_env)
    }

    /// The same, with an explicit pool size.
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

/// A running `claude-print serve` daemon with its stderr collected line-by-line
/// on a reader thread. Dropping the guard kills the daemon so a failing
/// assertion cannot leak worker processes into the rest of the suite; the
/// deliberate exit paths are [`Daemon::terminate`], [`Daemon::kill_hard`], and
/// [`Daemon::sigterm_expect_clean_exit`].
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

    fn pid(&self) -> u32 {
        self.child.id()
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

    /// SIGSTOP the daemon: the process freezes mid-anything, its listening
    /// socket stays open in the kernel, and every later client connect is
    /// queued by the backlog and never answered. This is "the manager stops
    /// responding" with a real daemon — no fake server can freeze an accept
    /// loop that honestly already ran.
    fn freeze(&self) {
        kill(Pid::from_raw(self.child.id() as i32), Signal::SIGSTOP).expect("SIGSTOP the daemon");
    }

    /// SIGCONT the daemon again; it resumes exactly where the accept loop
    /// stopped and drains everything queued while frozen.
    fn thaw(&self) {
        kill(Pid::from_raw(self.child.id() as i32), Signal::SIGCONT).expect("SIGCONT the daemon");
    }

    /// Snapshot the daemon's worker pids, SIGKILL the daemon, and reap it.
    /// A SIGKILLed daemon runs no cleanup: the returned workers are orphaned
    /// from this instant, and the socket file is left on disk stale.
    fn kill_hard(mut self) -> Vec<u32> {
        let workers = children_of(self.child.id());
        kill(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL).expect("SIGKILL the daemon");
        let status = self.child.wait().expect("reap the SIGKILLed daemon");
        assert!(
            status.code().is_none(),
            "the daemon must have died by signal, not exited: {status:?}"
        );
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        workers
    }

    /// SIGTERM the daemon and hold it to the full shutdown contract: exit 0
    /// within [`SHUTDOWN_BOUND`], the socket file removed, and — the reaping
    /// check — exactly `expected_workers` children snapshotted before the
    /// signal, all gone from /proc afterwards (neither leaked nor zombie).
    fn terminate(mut self, expected_workers: usize) {
        let workers = self.sigterm_expect_clean_exit();
        assert_eq!(
            workers.len(),
            expected_workers,
            "daemon must hold exactly {expected_workers} worker(s) at shutdown \
             (deterministic population, no churn); children: {workers:?}"
        );
        self.assert_socket_gone();
        reap_all(workers);
    }

    /// SIGTERM the daemon and assert only the exit-half of the shutdown
    /// contract (exit 0 within [`SHUTDOWN_BOUND`]); returns the worker pids
    /// snapshotted at signal time so the caller can assert its own teardown
    /// properties — used where the socket-removal half must be INVERTED (a
    /// daemon that lost its socket node to a replacement must leave the
    /// winner's socket alone).
    fn sigterm_expect_clean_exit(&mut self) -> Vec<u32> {
        let workers = children_of(self.child.id());
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
        workers
    }

    fn assert_socket_gone(&self) {
        assert!(
            !self.socket.exists(),
            "the daemon must remove its own socket file on shutdown"
        );
    }

    fn finish(mut self) {
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Assertion-failure escape hatch: never leak the daemon or its
        // mock-claude workers into the rest of the suite. Skip the kill when
        // the child has already been reaped (a completed terminate/kill_hard)
        // — its pid is recyclable, and a blind SIGKILL there could hit an
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

/// Poll until every pid in `workers` has vanished from /proc. The daemon reaps
/// a released worker before it answers the release, so a normal teardown
/// converges fast; the window also absorbs init reaping an orphaned worker
/// that a PTY hangup (daemon death, client exit) already terminated.
fn reap_all(workers: Vec<u32>) {
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

/// Count EVERY descriptor the daemon holds open. The generic leak probe: a
/// leaked pipe (a warmup that never ended), an accepted pool-socket connection
/// nobody closed, or an unreaped worker's descriptors all grow this number,
/// while a clean daemon returns to its at-rest count after every cycle.
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

/// Poll until `pid` has vanished from /proc.
fn assert_pid_gone(pid: u32, grace: Duration) {
    let start = Instant::now();
    while proc_state(pid).is_some() {
        assert!(
            start.elapsed() < grace,
            "worker pid {pid} must be gone; it survived {grace:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Poll until `pid` is still visible in /proc after `grace` — the negative of
/// [`assert_pid_gone`], for "the orphan was held, not killed".
fn assert_pid_alive(pid: u32, grace: Duration) {
    std::thread::sleep(grace);
    assert!(
        proc_state(pid).is_some(),
        "worker pid {pid} must still be alive"
    );
}

/// The worker ids the daemon's ledger shows as assigned, in order ("Assigned
/// worker <uuid>" is the verbose post-handoff line — it exists only for
/// handoffs that fully succeeded, fd transfer included).
fn assigned_worker_ids(stderr: &[String]) -> Vec<String> {
    stderr
        .iter()
        .filter(|l| l.contains("Assigned worker"))
        .filter_map(|l| l.split_whitespace().last().map(str::to_owned))
        .collect()
}

/// The verbose `pool:` diagnostic lines a fallback client prints.
fn pool_diagnostics(stderr: &str) -> Vec<&str> {
    stderr.lines().filter(|l| l.contains("pool:")).collect()
}

// ── Bullet 1: daemon crash during handoff ────────────────────────────────────

/// A daemon that dies MID-HANDOFF — after the client connected, before it
/// answered — is a hard protocol failure, not a fallback: the pool is
/// reachable-but-broken, and silently spawning a full-price stateless session
/// would mask the breakage. Construction: SIGSTOP freezes the daemon before
/// the client even starts, so the client's connect is queued by the kernel and
/// no answer can ever be produced; the SIGKILL then makes the loss permanent.
/// The failure must land inside the caller's `--timeout` budget, and the
/// daemon's death must leave no live worker (its PTY master died with it) and
/// no cleanup — exactly a stale socket node.
#[test]
fn daemon_crash_during_handoff_fails_safely_within_the_caller_timeout() {
    let fx = Fixture::start();
    let mut daemon = fx.daemon(&[]);
    daemon.wait_for("settled and ready", 1, WARMUP);

    // Freeze FIRST, so the crash lands mid-handoff by construction rather
    // than by racing the exchange.
    daemon.freeze();

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .args(["--timeout", "20"])
        .arg(PROMPT);
    let client = spawn(&mut cmd);

    // Give the client its connect + acquire write: against a frozen daemon
    // both complete kernel-side (backlog) while no userspace code runs.
    std::thread::sleep(Duration::from_millis(700));

    let workers = daemon.kill_hard();
    assert_eq!(
        workers.len(),
        1,
        "precondition: the daemon held exactly its one warm worker at crash time"
    );

    let started = Instant::now();
    let out = reap(client, FAST_BUDGET);
    let elapsed = started.elapsed();

    assert_eq!(
        out.code,
        Some(2),
        "a daemon that dies mid-handoff must exit non-zero, not fall back\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("error: pool protocol failure"),
        "the error must name the protocol failure\nstderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("falling back"),
        "a broken daemon must never be masked by a fallback\nstderr:\n{}",
        out.stderr
    );
    assert!(
        out.stdout.trim().is_empty(),
        "no stateless session may have run\nstdout:\n{}",
        out.stdout
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "the failure must land well inside the caller's 20 s deadline, took {elapsed:?}"
    );

    // The crash cleans up nothing: the socket node is left stale (no listener
    // behind it), and the orphaned warm worker — whose PTY master was a
    // daemon fd that died with the daemon — is hung up by the kernel and
    // exits on its own. Neither leaks.
    assert!(
        fx.socket.exists(),
        "a SIGKILLed daemon leaves its socket node stale (no cleanup ran)"
    );
    reap_all(workers);
}

// ── Bullet 2: the manager stops responding ───────────────────────────────────

/// The other wedged shape: the daemon is alive but NEVER ANSWERS — SIGSTOPed
/// after warmup, so its listener still accepts at the kernel level while its
/// accept loop is frozen. The client must burn its whole acquire budget (the
/// `--timeout` it was given, 8 s here) and then fail as a protocol timeout —
/// not fall back, not hang past its budget. And the daemon must be none the
/// worse for it: after the thaw, the abandoned connection is closed (fd count
/// back to at-rest), the next client is served pooled on a fresh replacement
/// (the frozen exchange's assignment is never retried against the dead
/// caller), and shutdown reaps both the replacement and the InUse worker the
/// frozen exchange left behind.
#[test]
fn daemon_silence_fails_within_the_caller_timeout_and_recovers_after() {
    let fx = Fixture::start();
    let mut daemon = fx.daemon(&[]);
    daemon.wait_for("settled and ready", 1, WARMUP);
    let baseline_fds = at_rest_fd_count(daemon.pid());

    daemon.freeze();

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .args(["--timeout", "8"])
        .arg(PROMPT);
    let started = Instant::now();
    let out = run(&mut cmd, FAST_BUDGET);
    let elapsed = started.elapsed();

    assert_eq!(
        out.code,
        Some(2),
        "silence past the acquire deadline is a protocol failure\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("error: pool protocol failure"),
        "the error must name the protocol failure\nstderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("falling back"),
        "a wedged daemon must never be masked by a fallback\nstderr:\n{}",
        out.stderr
    );
    assert!(
        out.stdout.trim().is_empty(),
        "no stateless session may have run\nstdout:\n{}",
        out.stdout
    );
    // The budget is min(60 s, --timeout) = 8 s: the client must wait it out
    // (nothing can answer a frozen daemon sooner) and must not hang past it.
    assert!(
        elapsed >= Duration::from_secs(7),
        "the deadline must actually be waited out, took only {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "the client must return inside its 8 s budget (plus scheduler slack), \
         took {elapsed:?}"
    );

    // Thaw: the daemon resumes and finds the abandoned exchange. Its reply
    // fails (the caller is gone), the assignment is voided as a connection
    // error, and the connection fd is closed — the daemon leaks nothing from
    // the wedged client.
    daemon.thaw();
    daemon.wait_for("Connection error", 1, LEDGER);
    wait_fds_back_to_baseline(daemon.pid(), baseline_fds, Duration::from_secs(15));

    // The daemon is still serving: maintain() warms a replacement for the
    // voided slot, and the next client — once it is READY to hand out —
    // acquires it and completes pooled. Proof the wedged exchange wedged
    // nothing: same socket, same daemon, fresh worker.
    daemon.wait_for("Spawning worker", 2, REPLACE);
    daemon.wait_for("settled and ready", 2, WARMUP);
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "a daemon that recovered from a wedged client must serve the next \
         caller\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("driving prewarmed worker"),
        "the recovered daemon must serve pooled, not fall back\nstderr:\n{}",
        out.stderr
    );

    // Replacement lifecycle completes, and the voided slot's orphan is still
    // held — the population at shutdown is the orphan plus the settled
    // third-cycle worker, exactly.
    daemon.wait_for("Released worker", 1, LEDGER);
    daemon.wait_for("Spawning worker", 3, REPLACE);
    daemon.wait_for("settled and ready", 3, WARMUP);
    let held = children_of(daemon.pid());
    assert_eq!(
        held.len(),
        2,
        "the orphan from the wedged exchange is still held, plus its \
         replacement; children: {held:?}"
    );
    daemon.terminate(2);
}

// ── Bullet 2: daemon crash mid-drive ─────────────────────────────────────────

/// A daemon that dies while a client DRIVES its worker changes nothing for the
/// client: the worker is daemon-independent once assigned (the client owns a
/// dup of the PTY master), so the session completes, the answer lands inside
/// the caller's `--timeout`, and the release against the dead daemon fails
/// best-effort within its own bound instead of blocking. The crash must not
/// leak the worker either: with both the daemon and the client gone, the
/// worker's PTY master is fully closed, the kernel hangs its session up, and
/// the worker ends on its own. The stale node the crash leaves must route the
/// next caller through the ordinary stateless fallback.
#[test]
fn daemon_crash_mid_drive_never_blocks_the_client_past_its_deadline() {
    let fx = Fixture::start();
    // MOCK_DELAY_STOP widens the mid-drive window: after the prompt is
    // injected, the worker holds its Stop payload for 8 s, so the daemon kill
    // below lands strictly between assignment and completion.
    let mut daemon = fx.daemon(&[("MOCK_DELAY_STOP", "8000")]);
    daemon.wait_for("settled and ready", 1, WARMUP);

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .args(["--timeout", "60"])
        .arg(PROMPT);
    let client = spawn(&mut cmd);
    daemon.wait_for("Assigned worker", 1, LEDGER);

    std::thread::sleep(Duration::from_millis(1500));
    let workers = daemon.kill_hard();

    let out = reap(client, POOLED_BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "a mid-drive daemon crash must not fail the invocation\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Hello from mock_claude"),
        "the client must still get its answer from the worker transcript\n\
         stdout:\n{}",
        out.stdout
    );
    assert!(
        !out.stderr.contains("falling back"),
        "a completed pooled session must not have fallen back\nstderr:\n{}",
        out.stderr
    );

    // No leaked worker: the client was the last holder of the PTY master; its
    // exit hung the session up and the worker ended on its own.
    reap_all(workers);

    // The crash left the socket node stale, and the very next caller on the
    // path must get the ordinary stateless fallback — one diagnostic, exit 0.
    assert!(
        fx.socket.exists(),
        "a SIGKILLed daemon leaves its socket node stale"
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
        "the stale path must fall back, not fail\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    let diags = pool_diagnostics(&out.stderr);
    assert_eq!(
        diags.len(),
        1,
        "exactly one fallback diagnostic expected\nstderr:\n{}",
        out.stderr
    );
    assert!(
        diags[0].contains("no pool reachable at") && diags[0].contains("falling back"),
        "the diagnostic must name the unreachable pool and the fallback: {}",
        diags[0]
    );
}

// ── Bullet 3: client cancellation ────────────────────────────────────────────

/// A SIGKILLed client orphans its InUse worker. The daemon must neither reuse
/// it for a second prompt (workers are single-prompt) nor destroy it early —
/// it warms a replacement for NEW clients while holding the orphan, and
/// reclaims the orphan only at shutdown (INV-9, INV-13). The ledger must show
/// the orphan's id assigned exactly once, ever.
#[test]
fn killed_client_orphans_its_worker_which_is_never_reassigned() {
    let fx = Fixture::start();
    // MOCK_DELAY_STOP keeps the first client mid-drive long enough to be
    // killed while holding its worker; it also slows the follow-up drive (the
    // replacement inherits the daemon env), which POOLED_BUDGET absorbs.
    let mut daemon = fx.daemon(&[("MOCK_DELAY_STOP", "15000")]);
    daemon.wait_for("settled and ready", 1, WARMUP);

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .args(["--timeout", "120"])
        .arg(PROMPT);
    let client = spawn(&mut cmd);
    daemon.wait_for("Assigned worker", 1, LEDGER);

    // The orphan's identity comes from the daemon ledger (the only assignment
    // so far) and its pid from the daemon's children (its only worker).
    let orphan_id = assigned_worker_ids(&daemon.stderr())[0].clone();
    let orphan_pid = children_of(daemon.pid())[0];

    // Cancel the client mid-drive. SIGKILL runs no release: the daemon never
    // learns its client died, which is exactly the state under test.
    let _ = kill(Pid::from_raw(client.id() as i32), Signal::SIGKILL);
    let out = reap(client, FAST_BUDGET);
    assert_eq!(
        out.code, None,
        "the client must have died by signal (not exited) for this scenario \
         to exist\nstdout:\n{}\nstderr:\n{}",
        out.stdout, out.stderr
    );

    // The worker is daemon-owned: the client's death closes only the client's
    // master dup, the daemon still holds its own, so the worker stays alive —
    // held InUse, never destroyed, never reassigned.
    assert_pid_alive(orphan_pid, Duration::from_secs(1));
    assert!(
        children_of(daemon.pid()).contains(&orphan_pid),
        "the orphaned worker must still be the daemon's child"
    );

    // The daemon warms a replacement for new clients and serves the next
    // caller on it — the orphan is never handed out again.
    daemon.wait_for("Spawning worker", 2, REPLACE);
    daemon.wait_for("settled and ready", 2, WARMUP);

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "the follow-up caller must be served on the replacement\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    let (follow_up_id, _pid) = driven_worker(&out.stderr);
    assert_ne!(
        follow_up_id, orphan_id,
        "the orphaned worker must never be reassigned to a second prompt"
    );
    assert!(
        out.stdout.contains("Hello from mock_claude"),
        "the follow-up caller must get its answer\nstdout:\n{}",
        out.stdout
    );

    // Ledger: exactly two assignments ever; the orphan's id appears exactly
    // once (its first and only prompt).
    daemon.wait_for("Released worker", 1, LEDGER);
    let ids = assigned_worker_ids(&daemon.stderr());
    assert_eq!(
        ids.len(),
        2,
        "exactly two handoffs may ever happen; ledger: {:?}",
        daemon.stderr()
    );
    assert_eq!(
        ids.iter().filter(|id| **id == orphan_id).count(),
        1,
        "the orphan id must be assigned exactly once; ledger: {:?}",
        daemon.stderr()
    );
    assert!(
        children_of(daemon.pid()).contains(&orphan_pid),
        "the orphan must still be held right up to shutdown"
    );

    // Shutdown reclaims the orphan: wait for the third cycle's worker to
    // settle so the population at signal time is deterministic (orphan +
    // replacement), then hold shutdown to its full contract.
    daemon.wait_for("Spawning worker", 3, REPLACE);
    daemon.wait_for("settled and ready", 3, WARMUP);
    daemon.terminate(2);
    assert_pid_gone(orphan_pid, Duration::from_secs(10));
}

// ── Bullet 4: manager restart/recovery ───────────────────────────────────────

/// Kill a serving daemon (stale node left behind), prove a caller falls back
/// statelessly against the corpse, then start a NEW daemon on the SAME path
/// and prove pooled serving resumes end to end. Recovery is the whole point
/// of the fallback contract: the outage costs one stateless session, nothing
/// more.
#[test]
fn manager_restart_recovers_pooled_serving_on_the_same_socket_path() {
    let fx = Fixture::start();
    let mut daemon_a = fx.daemon(&[]);
    daemon_a.wait_for("settled and ready", 1, WARMUP);

    // Pooled serving works before the crash.
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "pooled serving must work before the crash\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(out.stderr.contains("driving prewarmed worker"));

    // The replacement cycle the first caller triggered must complete so the
    // population checks below stay deterministic.
    daemon_a.wait_for("Released worker", 1, LEDGER);
    daemon_a.wait_for("settled and ready", 2, WARMUP);

    let workers = daemon_a.kill_hard();
    reap_all(workers);
    assert!(fx.socket.exists(), "the crash leaves the socket node stale");

    // A caller against the corpse falls back statelessly, once, verbosely.
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out = run(&mut cmd, FAST_BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "the stale path must fall back, not fail\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(out.stdout.contains("Hello from mock_claude"));
    let diags = pool_diagnostics(&out.stderr);
    assert_eq!(
        diags.len(),
        1,
        "exactly one fallback diagnostic expected\nstderr:\n{}",
        out.stderr
    );
    assert!(diags[0].contains("no pool reachable at") && diags[0].contains("falling back"));

    // Recovery: a new daemon takes the same path (bind removes the stale
    // node), warms, and pooled serving resumes.
    let mut daemon_b = fx.daemon(&[]);
    daemon_b.wait_for("settled and ready", 1, WARMUP);
    assert!(
        fx.socket.exists(),
        "the new daemon's bind must (re)create the socket node"
    );

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "pooled serving must resume after the restart\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("driving prewarmed worker"),
        "the resumed caller must drive a prewarmed worker, not fall back\n\
         stderr:\n{}",
        out.stderr
    );
    assert!(out.stdout.contains("Hello from mock_claude"));

    daemon_b.wait_for("Released worker", 1, LEDGER);
    daemon_b.wait_for("Spawning worker", 2, REPLACE);
    daemon_b.wait_for("settled and ready", 2, WARMUP);
    daemon_b.terminate(1);
}

/// The restart-ordering hazard: daemon B binds the path while daemon A still
/// lives (B's bind unlinks A's node and binds a fresh one). When A later
/// shuts down, its cleanup must NOT remove B's socket — removal is
/// ownership-checked against the (dev, ino) A bound, not the path. If A
/// removed B's node, the very next caller would silently lose the pool; this
/// test fails loudly instead.
#[test]
fn a_replacing_daemons_bind_survives_the_replaced_daemons_shutdown() {
    let fx = Fixture::start();
    let mut daemon_a = fx.daemon(&[]);
    daemon_a.wait_for("settled and ready", 1, WARMUP);

    // B replaces A on the same path while A lives.
    let mut daemon_b = fx.daemon(&[]);
    daemon_b.wait_for("settled and ready", 1, WARMUP);

    // A's shutdown must be clean for A — and must leave the path serving.
    let workers = daemon_a.sigterm_expect_clean_exit();
    reap_all(workers);
    daemon_a.finish();
    assert!(
        fx.socket.exists(),
        "the replaced daemon's shutdown must not remove the replacement's socket"
    );

    // The path still serves pooled — through B, never A: A's listener lost
    // its path at B's bind, so a path connect can only reach B.
    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .arg("--verbose")
        .arg(PROMPT);
    let out = run(&mut cmd, POOLED_BUDGET);
    assert_eq!(
        out.code,
        Some(0),
        "the replacement's socket must serve after the replaced daemon exits\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("driving prewarmed worker"),
        "the caller must be served pooled\nstderr:\n{}",
        out.stderr
    );
    let b_stderr = daemon_b.stderr();
    assert!(
        assigned_worker_ids(&b_stderr).len() == 1,
        "the assignment must be B's; B's ledger: {b_stderr:?}"
    );

    daemon_b.wait_for("Released worker", 1, LEDGER);
    daemon_b.wait_for("Spawning worker", 2, REPLACE);
    daemon_b.wait_for("settled and ready", 2, WARMUP);
    daemon_b.terminate(1);
}

// ── Bullet 5: malformed daemon responses (garbage JSON shape) ────────────────

/// The companion suite (`tests/pool_socket_e2e.rs`) pins close-mid-exchange,
/// wrong-shape JSON, and assignment-without-fd. This is the fourth acquire
/// shape: a well-formed frame whose body is NOT JSON at all. Same contract —
/// exit 2, protocol-failure diagnostic, never a fallback, well inside the
/// caller's `--timeout`.
#[test]
fn daemon_garbage_json_response_fails_safely_within_the_caller_timeout() {
    let fx = Fixture::start();

    let listener = UnixListener::bind(&fx.socket).expect("bind the broken daemon");
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};

        let (mut stream, _) = listener.accept().expect("accept the pooled client");
        // Drain the client's length-prefixed acquire frame, so what the client
        // reports is the garbage reply, never a lost-send race.
        let mut prefix = [0u8; 4];
        stream
            .read_exact(&mut prefix)
            .expect("read the client's length prefix");
        let len = u32::from_be_bytes(prefix) as usize;
        let mut payload = vec![0u8; len];
        stream
            .read_exact(&mut payload)
            .expect("read the client's acquire frame");

        let body: &[u8] = b"<this is not json>";
        stream
            .write_all(&(body.len() as u32).to_be_bytes())
            .expect("write the garbage prefix");
        stream.write_all(body).expect("write the garbage frame");
        // Dropping the stream closes the connection.
    });

    let mut cmd = fx.client();
    cmd.arg("--pool-socket")
        .arg(&fx.socket)
        .args(["--timeout", "20"])
        .arg(PROMPT);
    let started = Instant::now();
    let out = run(&mut cmd, FAST_BUDGET);
    let elapsed = started.elapsed();
    server.join().expect("the abusive server thread");

    assert_eq!(
        out.code,
        Some(2),
        "garbage JSON must exit non-zero, not fall back\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("error: pool protocol failure"),
        "the error must name the protocol failure\nstderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("malformed response"),
        "a non-JSON body must be reported as a malformed response\nstderr:\n{}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("falling back"),
        "a broken daemon must never be masked by a fallback\nstderr:\n{}",
        out.stderr
    );
    assert!(
        out.stdout.trim().is_empty(),
        "no stateless session may have run\nstdout:\n{}",
        out.stdout
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "the failure must land inside the caller deadline, took {elapsed:?}"
    );
}

// ── Bullet 6: fallback compatibility across socket states and formats ────────

/// The three output formats, for `state_fallback` tests to iterate.
const FORMATS: [&str; 3] = ["text", "json", "stream-json"];

/// Run one invocation per output format with NO `--pool-socket` — the
/// ordinary stateless sessions a fallback must be behaviorally identical to.
/// Each invocation gets a FRESH HOME: the mock's shared default session id
/// routes every run in one HOME to the same transcript path, and the
/// stream-json discovery cannot bind a transcript that already existed from a
/// previous run — a same-HOME artifact of the deterministic default, not a
/// property of either path under test here.
fn stateless_baselines(fx: &Fixture) -> Vec<Outcome> {
    FORMATS
        .iter()
        .map(|format| {
            let home = tempfile::tempdir().expect("per-run home");
            let mut cmd = fx.client();
            cmd.env("HOME", home.path());
            cmd.args(["--output-format", format]).arg(PROMPT);
            run(&mut cmd, FAST_BUDGET)
        })
        .collect()
}

/// Run one invocation per output format against the fixture's (broken) socket.
fn fallback_runs(fx: &Fixture) -> Vec<Outcome> {
    FORMATS
        .iter()
        .map(|format| {
            let home = tempfile::tempdir().expect("per-run home");
            let mut cmd = fx.client();
            cmd.env("HOME", home.path());
            cmd.arg("--pool-socket")
                .arg(&fx.socket)
                .arg("--verbose")
                .args(["--output-format", format])
                .arg(PROMPT);
            run(&mut cmd, FAST_BUDGET)
        })
        .collect()
}

/// The parsed json result object with its wall-clock `duration_ms` removed —
/// the one field that measures the machine rather than the behavior, so it is
/// asserted present (a number) and excluded from the equality comparison.
fn json_result_normalized(stdout: &str) -> serde_json::Value {
    let mut value: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not valid JSON: {e}\nraw:\n{stdout}"));
    let duration = value
        .as_object_mut()
        .expect("the json result must be an object")
        .remove("duration_ms");
    assert!(
        duration.and_then(|d| d.as_u64()).is_some(),
        "duration_ms must be present and numeric"
    );
    value
}

/// Shared assertion body: every fallback run must succeed, carry exactly one
/// diagnostic naming the unreachable pool and the fallback, and produce
/// stdout behaviorally identical to the same format's no-flag baseline —
/// text byte-for-byte, json as an equal parsed object, stream-json as the
/// same forwarded line sequence.
fn assert_fallbacks_match_baselines(fx: &Fixture, state: &str) {
    let baselines = stateless_baselines(fx);
    let fallbacks = fallback_runs(fx);

    for ((format, base), fb) in FORMATS.iter().zip(&baselines).zip(&fallbacks) {
        assert_eq!(
            fb.code,
            Some(0),
            "{state}/{format}: the fallback invocation must succeed\n\
             stdout:\n{}\nstderr:\n{}",
            fb.stdout,
            fb.stderr
        );
        assert!(
            !fb.stderr.contains("driving prewarmed worker"),
            "{state}/{format}: the fallback must not claim a pooled worker\n\
             stderr:\n{}",
            fb.stderr
        );
        let diags = pool_diagnostics(&fb.stderr);
        assert_eq!(
            diags.len(),
            1,
            "{state}/{format}: exactly one fallback diagnostic expected\nstderr:\n{}",
            fb.stderr
        );
        assert!(
            diags[0].contains("falling back"),
            "{state}/{format}: the diagnostic must name the fallback: {}",
            diags[0]
        );

        match *format {
            "text" => {
                assert_eq!(
                    base.stdout.trim(),
                    fb.stdout.trim(),
                    "{state}/text: fallback stdout must be identical to the \
                     stateless baseline\nbaseline:\n{}\nfallback:\n{}",
                    base.stdout,
                    fb.stdout
                );
                assert!(
                    fb.stdout.contains("Hello from mock_claude"),
                    "{state}/text: the session must have run to its answer\nstdout:\n{}",
                    fb.stdout
                );
            }
            "json" => {
                let base_json = json_result_normalized(&base.stdout);
                let fb_json = json_result_normalized(&fb.stdout);
                assert_eq!(
                    base_json, fb_json,
                    "{state}/json: the fallback result object must equal the \
                     stateless baseline (modulo duration_ms)"
                );
                assert_eq!(fb_json["type"], "result");
                assert_eq!(fb_json["is_error"], false);
            }
            "stream-json" => {
                let base_lines: Vec<&str> = base
                    .stdout
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                let fb_lines: Vec<&str> =
                    fb.stdout.lines().filter(|l| !l.trim().is_empty()).collect();
                assert_eq!(
                    base_lines, fb_lines,
                    "{state}/stream-json: the forwarded event stream must equal \
                     the stateless baseline"
                );
                assert!(
                    !fb_lines.is_empty(),
                    "{state}/stream-json: the stream must not be empty"
                );
            }
            other => panic!("unknown format {other}"),
        }
    }
}

/// Absent socket (the path was never bound): all three formats fall back
/// quietly-compatible.
#[test]
fn absent_socket_falls_back_compatible_across_all_output_formats() {
    let fx = Fixture::start();
    assert!(
        !fx.socket.exists(),
        "precondition: the path was never bound"
    );
    assert_fallbacks_match_baselines(&fx, "absent");
    assert!(
        !fx.socket.exists(),
        "a fallback run must not create the absent socket"
    );
}

/// Stale socket (a bound node with nothing behind it): all three formats fall
/// back compatible, and the client leaves the node alone — it is not the
/// client's to unlink.
#[test]
fn stale_socket_falls_back_compatible_across_all_output_formats() {
    let fx = Fixture::start();
    {
        let listener = UnixListener::bind(&fx.socket).expect("bind the doomed listener");
        drop(listener);
    }
    assert!(fx.socket.exists(), "precondition: the stale node exists");
    assert_fallbacks_match_baselines(&fx, "stale");
    assert!(
        fx.socket.exists(),
        "the client must not unlink a socket node it does not own"
    );
}

/// Unavailable pool (a listening daemon that answers every acquire with a
/// well-formed `pool_full` error): all three formats fall back compatible.
#[test]
fn unavailable_pool_falls_back_compatible_across_all_output_formats() {
    let fx = Fixture::start();

    let listener = UnixListener::bind(&fx.socket).expect("bind the unavailable pool");
    // Serve exactly one well-formed `pool_full` answer per format run. The
    // pool is up and speaking protocol; it just has nothing to hand out.
    let server = std::thread::spawn(move || {
        use std::io::{Read, Write};

        for _ in 0..FORMATS.len() {
            let (mut stream, _) = listener.accept().expect("accept the pooled client");
            let mut prefix = [0u8; 4];
            stream
                .read_exact(&mut prefix)
                .expect("read the client's length prefix");
            let len = u32::from_be_bytes(prefix) as usize;
            let mut payload = vec![0u8; len];
            stream
                .read_exact(&mut payload)
                .expect("read the client's acquire frame");

            let body: &[u8] = br#"{"type":"error","error":"no ready workers","code":"pool_full"}"#;
            stream
                .write_all(&(body.len() as u32).to_be_bytes())
                .expect("write the pool_full prefix");
            stream.write_all(body).expect("write the pool_full frame");
        }
    });

    let baselines = stateless_baselines(&fx);
    let fallbacks = fallback_runs(&fx);
    server.join().expect("the unavailable-pool server thread");

    for ((format, base), fb) in FORMATS.iter().zip(&baselines).zip(&fallbacks) {
        assert_eq!(
            fb.code,
            Some(0),
            "unavailable/{format}: the fallback invocation must succeed\n\
             stdout:\n{}\nstderr:\n{}",
            fb.stdout,
            fb.stderr
        );
        let diags = pool_diagnostics(&fb.stderr);
        assert_eq!(
            diags.len(),
            1,
            "unavailable/{format}: exactly one fallback diagnostic expected\nstderr:\n{}",
            fb.stderr
        );
        assert!(
            diags[0].contains("pool cannot serve a worker")
                && diags[0].contains("pool_full")
                && diags[0].contains("falling back"),
            "unavailable/{format}: the diagnostic must name the pool_full \
             failure and the fallback: {}",
            diags[0]
        );
        assert!(
            !fb.stderr.contains("driving prewarmed worker"),
            "unavailable/{format}: the fallback must not claim a pooled worker"
        );

        match *format {
            "text" => {
                assert_eq!(
                    base.stdout.trim(),
                    fb.stdout.trim(),
                    "unavailable/text: fallback stdout must equal the baseline"
                );
            }
            "json" => {
                let base_json = json_result_normalized(&base.stdout);
                let fb_json = json_result_normalized(&fb.stdout);
                assert_eq!(
                    base_json, fb_json,
                    "unavailable/json: the fallback result object must equal the \
                     stateless baseline (modulo duration_ms)"
                );
            }
            "stream-json" => {
                let base_lines: Vec<&str> = base
                    .stdout
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                let fb_lines: Vec<&str> =
                    fb.stdout.lines().filter(|l| !l.trim().is_empty()).collect();
                assert_eq!(
                    base_lines, fb_lines,
                    "unavailable/stream-json: the forwarded stream must equal the \
                     stateless baseline"
                );
            }
            other => panic!("unknown format {other}"),
        }
    }
}
