//! Adversarial pool-coverage end-to-end: the identity proofs.
//!
//! This binary carries the two adversarial shapes where several clients are
//! in flight at once and every forwarded byte must still belong to the
//! client that drove it:
//!
//!   * **concurrent acquisition** —
//!     `concurrent_clients_each_drive_a_distinct_worker` (INV-9, INV-11):
//!     three clients acquire simultaneously from a `--pool-size 3` daemon;
//!     every one drives its OWN worker (distinct id and pid), answers from
//!     its OWN session (the mock's `MOCK_UNIQUE_SESSION_ID` mints the session
//!     id from the worker's pid, so session↔worker binding is proven, not
//!     assumed), and the daemon's ledger shows three assignments / three
//!     releases with exactly three PTY masters held at rest.
//!   * **zero sibling contamination under same-cwd concurrency** —
//!     `concurrent_stream_json_clients_share_a_cwd_without_forwarding_siblings`
//!     (claudepr-a927ec0c): the end-to-end proof the original defect is dead.
//!     That defect: a stream-json client drove worker pid A while its stdout
//!     result event carried mock-session-pid-B from a sibling two pids away —
//!     the incremental reader guessed its transcript by newest mtime, and
//!     same-cwd pool concurrency made the guess wrong exactly when it
//!     mattered. Here three stream-json clients acquire simultaneously; all
//!     three workers share the daemon's HOME and cwd, so three transcripts
//!     grow as same-cwd candidates in ONE projects dir while three readers
//!     bind. Every forwarded stream — events and final result event alike —
//!     must carry ONLY its own session: the reader now binds through the
//!     per-drive identity file (the UserPromptSubmit relay, claudepr-0c002513)
//!     and forwards nothing until it has bound positively (the binding
//!     ladder, claudepr-e71d4c3f).
//!
//! The remaining adversarial shapes — exhaustion under load, daemon crash
//! mid-drive, client cancellation without destructors — land with the
//! ADR-005 proof umbrella (bead claudepr-a03e32d7); the client matrix
//! (`tests/pool_socket_e2e.rs`) and the serve/teardown pins (`tests/serve.rs`)
//! already cover sequential clients, malformed daemons, deadline firing,
//! SIGINT, and stale-socket fallback one shape at a time.
//!
//! Hermetic strategy identical to `tests/pool_socket_e2e.rs`: compiled
//! `claude-print` + `mock-claude`, temp sockets and HOMEs, no real `claude`,
//! no network, no fixed ports. Real-session (credential-backed) evidence for
//! the billing invariant lives in the pooled canary leg of
//! `scripts/billing-canary.sh`, not here.

use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

// Every test in this binary spawns a daemon plus real worker/client
// processes, and each test's deadlines assume those processes get the
// machine to themselves. Run at cargo's default parallelism, the concurrent
// process storms contend for the cgroup's CPUs and a cold-start client can
// overshoot a ledger budget that is trivial at single-test load (observed
// once as a 15 s "Assigned worker" timeout). The tests share no global state
// — the lock exists purely to serialize them, so each runs at the load its
// budgets were calibrated against. Same pattern as tests/watchdog.rs's
// ENV_LOCK.
static PROCESS_LOCK: Mutex<()> = Mutex::new(());

fn process_lock() -> MutexGuard<'static, ()> {
    PROCESS_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The one-word prompt every pooled invocation sends.
const PROMPT: &str = "Reply with exactly one word: pong";

/// Budget for a pooled invocation (warm path: acquire + drive + release).
const POOLED_BUDGET: Duration = Duration::from_secs(120);

/// Ceiling for daemon warmup to reach `settled and ready` per worker.
const WARMUP: Duration = Duration::from_secs(90);

/// Ceiling for the daemon's ledger to show a release.
const LEDGER: Duration = Duration::from_secs(15);

/// Ceiling for the daemon to spawn a replacement worker.
const REPLACE: Duration = Duration::from_secs(30);

/// How long a shutdown signal may take to produce an exited daemon.
const SHUTDOWN_BOUND: Duration = Duration::from_secs(20);

/// A captured subprocess outcome: exit code (`None` when killed by a signal),
/// decoded stdout/stderr.
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

// ── Hermetic per-test fixture ────────────────────────────────────────────────

/// Everything one test needs isolated: a private config root, a private
/// `HOME` shared by the daemon (forwarded to its workers) and the client, and
/// a temp socket path. Nothing a test touches outlives the struct.
struct Fixture {
    config: tempfile::TempDir,
    home: tempfile::TempDir,
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
    /// config root and HOME, plus extra environment for any STATELESS session
    /// the client runs itself (pooled workers inherit the DAEMON's env).
    fn client(&self, extra_env: &[(&str, &str)]) -> Command {
        let mut cmd = claude_print();
        cmd.env("XDG_CONFIG_HOME", self.config.path())
            .env("HOME", self.home.path());
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        cmd
    }

    /// Start a `serve` daemon of `pool_size` workers over this fixture's
    /// socket with the fixture's HOME, plus extra environment (forwarded by
    /// the daemon to every mock-claude worker it spawns).
    fn daemon(&self, pool_size: usize, extra_env: &[(&str, &str)]) -> Daemon {
        Daemon::start(
            &self.socket,
            self.home.path().to_str().expect("utf-8 home"),
            pool_size,
            extra_env,
        )
    }
}

// ── Daemon harness ───────────────────────────────────────────────────────────

/// A running `claude-print serve` daemon with its stderr collected
/// line-by-line on a reader thread. Dropping the guard kills the daemon so a
/// failing assertion cannot leak worker processes into the rest of the suite;
/// the clean exit path is [`Daemon::terminate`] (supervisor SIGTERM).
struct Daemon {
    child: Child,
    socket: PathBuf,
    lines: Arc<Mutex<Vec<String>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    reaped: bool,
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
            reaped: false,
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

    /// The worker ids the daemon has assigned, in assignment order
    /// (`Assigned worker <id>` verbose lines).
    fn assigned_worker_ids(&self) -> Vec<String> {
        self.stderr()
            .iter()
            .filter_map(|l| l.split("Assigned worker ").nth(1).map(str::trim))
            .map(str::to_string)
            .collect()
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
            "daemon must hold exactly {expected_workers} worker(s) at shutdown; \
             children: {workers:?}"
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
        self.reaped = true;

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
        // the child has already been reaped (a completed terminate/crash) —
        // its pid is recyclable, and a blind SIGKILL there could hit an
        // unrelated process.
        if !self.reaped && matches!(self.child.try_wait(), Ok(None)) {
            let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        }
        if !self.reaped {
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

// ── Concurrent-client harness ────────────────────────────────────────────────

/// A client spawned WITHOUT waiting, with stdout/stderr collected on reader
/// threads so several clients can be in flight at once. `finish` reaps and
/// returns everything.
struct ClientRun {
    child: Child,
    stdout: Arc<Mutex<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

impl ClientRun {
    fn spawn(mut cmd: Command) -> ClientRun {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn claude-print client: {e}"));

        // Drain each pipe on its own reader thread so the client can run
        // concurrently with its peers (and with this test's asserts).
        fn drain<R: std::io::Read + Send + 'static>(
            pipe: Option<R>,
        ) -> (Arc<Mutex<String>>, std::thread::JoinHandle<()>) {
            let sink = Arc::new(Mutex::new(String::new()));
            let thread = match pipe {
                Some(pipe) => {
                    let sink = Arc::clone(&sink);
                    std::thread::spawn(move || {
                        let mut buf = String::new();
                        let mut reader = std::io::BufReader::new(pipe);
                        let _ = reader.read_to_string(&mut buf);
                        *sink.lock().unwrap() = buf;
                    })
                }
                None => std::thread::spawn(|| ()),
            };
            (sink, thread)
        }

        let (stdout, out_thread) = drain(child.stdout.take());
        let (stderr, err_thread) = drain(child.stderr.take());

        ClientRun {
            child,
            stdout,
            stderr,
            readers: vec![out_thread, err_thread],
        }
    }

    /// Wait for exit within `budget` (killing on overrun), then join the
    /// readers and return the captured outcome.
    fn finish(mut self, budget: Duration) -> Outcome {
        let deadline = Instant::now() + budget;
        let code = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = self.child.kill();
                        let _ = self.child.wait();
                        panic!("client did not exit within {:?}", budget);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break None,
            }
        };
        let _ = self.child.wait();
        for reader in self.readers {
            let _ = reader.join();
        }
        Outcome {
            code,
            stdout: self.stdout.lock().unwrap().clone(),
            stderr: self.stderr.lock().unwrap().clone(),
        }
    }
}

// ── /proc helpers (same contracts as tests/pool_socket_e2e.rs) ───────────────

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
/// N workers the daemon holds exactly N — a leaked assignment would read N+1.
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

/// The `session_id` of a stream-json run's result event — the only carrier of
/// the session the run actually answered.
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

/// Every pid-minted mock session id appearing anywhere in `stream`
/// (`mock-session-pid-<digits>`), including across partial or non-JSON lines.
fn session_ids_in(stream: &str) -> Vec<String> {
    const MARKER: &str = "mock-session-pid-";
    let mut found = Vec::new();
    let mut rest = stream;
    while let Some(pos) = rest.find(MARKER) {
        let tail = &rest[pos + MARKER.len()..];
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            // The marker without a pid (a truncated write) is still a foreign
            // fragment; surface it rather than skipping past it.
            found.push(MARKER.to_string());
            rest = tail;
        } else {
            found.push(format!("{MARKER}{}", &tail[..digits]));
            rest = &tail[digits..];
        }
    }
    found
}

/// The zero-cross-forwarded-bytes assertion: every pid-minted session id
/// anywhere in `stream` must be `own`. Session ids are the only per-session
/// discriminator inside a transcript (the assistant line's text is identical
/// across workers), so a single forwarded sibling byte surfaces here as a
/// sibling session id.
fn assert_forwarding_is_exclusive(stream: &str, own: &str) {
    let foreign: Vec<String> = session_ids_in(stream)
        .into_iter()
        .filter(|s| s != own)
        .collect();
    assert!(
        foreign.is_empty(),
        "the forwarded stream must carry zero sibling session ids \
         (found {foreign:?}, own session {own})\nstream:\n{stream}"
    );
}

// ── Concurrent acquisition (INV-9, INV-11) ───────────────────────────────────

/// Three clients acquire SIMULTANEOUSLY from a `--pool-size 3` daemon. Every
/// client must be driven through its OWN worker — the daemon's assignment
/// under the manager mutex is one-per-worker, never two clients through one
/// PTY — and must answer from its OWN session. `MOCK_UNIQUE_SESSION_ID` mints
/// each worker's session id from its own pid, so "session id ==
/// mock-session-pid-<driven worker pid>" proves the answer came from exactly
/// the worker this client drove, not a neighbor's transcript. Afterwards the
/// daemon holds exactly three PTY masters (descriptor cleanup: every driven
/// worker's master closed with its release, three replacements warm).
#[test]
fn concurrent_clients_each_drive_a_distinct_worker() {
    let _process = process_lock();
    let fx = Fixture::start();
    let mut daemon = fx.daemon(3, &[("MOCK_UNIQUE_SESSION_ID", "1")]);
    daemon.wait_for("settled and ready", 3, WARMUP);
    let daemon_pid = daemon.pid();

    let formats = ["text", "json", "stream-json"];
    let clients: Vec<(&str, ClientRun)> = formats
        .iter()
        .map(|format| {
            let mut cmd = fx.client(&[]);
            cmd.arg("--pool-socket")
                .arg(&fx.socket)
                .arg("--verbose")
                .args(["--output-format", format])
                .arg(PROMPT);
            (*format, ClientRun::spawn(cmd))
        })
        .collect();

    let outcomes: Vec<(&str, Outcome)> = clients
        .into_iter()
        .map(|(format, client)| (format, client.finish(POOLED_BUDGET)))
        .collect();

    let mut seen_ids = Vec::new();
    let mut seen_pids = Vec::new();
    for (format, out) in &outcomes {
        assert_eq!(
            out.code,
            Some(0),
            "the concurrent {format} client must succeed\nstdout:\n{}\nstderr:\n{}",
            out.stdout,
            out.stderr
        );
        assert!(
            !out.stderr.contains("falling back"),
            "a pool with a free worker must never fall back\nstderr:\n{}",
            out.stderr
        );
        let (id, pid) = driven_worker(&out.stderr);
        seen_ids.push(id);
        seen_pids.push(pid);

        match *format {
            "json" => {
                let v: serde_json::Value = serde_json::from_str(out.stdout.trim())
                    .unwrap_or_else(|e| panic!("{format} stdout is not JSON: {e}\n{}", out.stdout));
                assert_eq!(
                    v["session_id"],
                    format!("mock-session-pid-{pid}"),
                    "the json client must answer from ITS OWN worker's session (usage: {})",
                    v["usage"]
                );
                assert!(
                    v["result"]
                        .as_str()
                        .unwrap_or("")
                        .contains("Hello from mock_claude"),
                    "the json client must get the worker's answer: {}",
                    out.stdout
                );
            }
            "stream-json" => {
                // Exact identity binding, held to the same bar as the json
                // arm now that the incremental reader binds positively: the
                // claudepr-a927ec0c interim limit ("session must merely
                // belong to one of this pool's workers") is obsolete — the
                // reader binds through the per-drive identity file and
                // forwards nothing before a positive bind, so the forwarded
                // result must name THIS client's own driven worker, and no
                // sibling's session id may appear anywhere in the stream
                // (the text and json siblings' transcripts are growing in
                // the same projects dir while this reader binds).
                let session = result_session_id(&out.stdout).unwrap_or_else(|| {
                    panic!(
                        "stream-json client must forward a result event\n{}",
                        out.stdout
                    )
                });
                let own = format!("mock-session-pid-{pid}");
                assert_eq!(
                    session, own,
                    "the forwarded result must carry this client's OWN worker's session \
                     (expected {own})\nstdout:\n{}",
                    out.stdout
                );
                assert_forwarding_is_exclusive(&out.stdout, &own);
                let result_events = out
                    .stdout
                    .lines()
                    .filter(|l| l.contains("\"type\":\"result\""))
                    .count();
                assert_eq!(
                    result_events, 1,
                    "exactly one result event expected\nstdout:\n{}",
                    out.stdout
                );
            }
            _ => {
                assert!(
                    out.stdout.contains("Hello from mock_claude"),
                    "the text client must get the worker's answer\nstdout:\n{}",
                    out.stdout
                );
            }
        }
    }

    // Identity: three concurrent acquisitions, three DISTINCT workers — a
    // worker id or pid seen twice means two prompts through one member.
    assert_eq!(seen_ids.len(), 3);
    assert_eq!(
        seen_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "worker ids must be distinct across concurrent clients: {seen_ids:?}"
    );
    assert_eq!(
        seen_pids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        3,
        "worker pids must be distinct across concurrent clients: {seen_pids:?}"
    );

    // Daemon ledger: three assignments (the exact three ids driven), three
    // releases, three replacements warm — then exactly three PTY masters at
    // rest (INV-14: no leaked master fd survives any release).
    daemon.wait_for("Released worker", 3, LEDGER);
    daemon.wait_for("Spawning worker", 6, REPLACE);
    daemon.wait_for("settled and ready", 6, WARMUP);
    let assigned = daemon.assigned_worker_ids();
    assert_eq!(assigned.len(), 3, "stderr: {:?}", daemon.stderr());
    for id in &seen_ids {
        assert!(
            assigned.iter().any(|a| a == id),
            "assignment ledger must include driven worker {id}: {assigned:?}"
        );
    }
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        daemon_pty_fd_count(daemon_pid),
        3,
        "the daemon must hold exactly one PTY master per worker at rest \
         (each driven worker's fd closed with its release)"
    );

    daemon.terminate(3);
}

// ── Zero sibling contamination under same-cwd concurrency (claudepr-a927ec0c) ─

/// The end-to-end proof that the original defect is dead. The defect's exact
/// shape: concurrent pool clients sharing one cwd, and a stream-json client's
/// forwarded stdout carrying a sibling's session (worker pid A driven, result
/// event named mock-session-pid-B two pids away) because the incremental
/// reader picked its transcript by newest mtime.
///
/// Rebuilt here at full scale: THREE stream-json clients acquire
/// simultaneously from a `--pool-size 3` daemon. Pool workers share the
/// daemon's HOME and cwd, so all three workers write their transcripts into
/// the SAME projects dir (`$HOME/.claude/projects/<cwd-slug>/`).
/// `MOCK_DELAY_JSONL` parks every worker between its Stop payload and its
/// transcript write (~800 ms): the three per-drive identities land first
/// (one per reader), then the three same-cwd transcripts materialize in ONE
/// tight window while every OTHER reader is still live mid-bind waiting for
/// its own file — two-plus same-cwd candidates growing concurrently is
/// structural here, not a scheduling accident, and this tight cluster is
/// exactly the shape whose newest-mtime pick produced the observed
/// contamination. The delay must stay well under the PO-5 Stop-to-JSONL
/// retry budget (`read_transcript_traced`, 40×50 ms = 2 s): past it the
/// session falls back to the payload's `last_assistant_message`, the drain
/// fires while the reader still awaits its file, and the run degrades to an
/// empty stream — 800 ms leaves 2.5× margin under that budget. (The mock
/// writes each transcript in one shot; byte-level incremental growth from
/// several writers is pinned in-process by
/// `tests/integration/scenarios.rs`.)
///
/// The binding ladder must make the collision unreachable: each reader binds
/// its OWN worker's transcript via the per-drive identity file the
/// UserPromptSubmit relay writes beside that worker's stop.fifo — written at
/// prompt submission, BEFORE any transcript exists — and forwards zero bytes
/// until positively bound. Asserted per client: the result event names its
/// own driven worker's pid-minted session; NO other pid-minted session id
/// occurs anywhere in its forwarded stream; the stream is exactly its own
/// transcript (one assistant event, one result event — no duplicate from the
/// Stop retarget, no sibling prefix). Across the three clients the forwarded
/// sessions are exactly the three driven sessions, each once.
#[test]
fn concurrent_stream_json_clients_share_a_cwd_without_forwarding_siblings() {
    let _process = process_lock();
    let fx = Fixture::start();
    let mut daemon = fx.daemon(
        3,
        &[("MOCK_UNIQUE_SESSION_ID", "1"), ("MOCK_DELAY_JSONL", "800")],
    );
    daemon.wait_for("settled and ready", 3, WARMUP);
    let daemon_pid = daemon.pid();

    let clients: Vec<ClientRun> = (0..3)
        .map(|_| {
            let mut cmd = fx.client(&[]);
            cmd.arg("--pool-socket")
                .arg(&fx.socket)
                .arg("--verbose")
                .args(["--output-format", "stream-json"])
                .arg(PROMPT);
            ClientRun::spawn(cmd)
        })
        .collect();

    let outcomes: Vec<Outcome> = clients
        .into_iter()
        .map(|client| client.finish(POOLED_BUDGET))
        .collect();

    let mut driven = Vec::new();
    for (i, out) in outcomes.iter().enumerate() {
        assert_eq!(
            out.code,
            Some(0),
            "concurrent stream-json client {i} must succeed\nstdout:\n{}\nstderr:\n{}",
            out.stdout,
            out.stderr
        );
        assert!(
            !out.stderr.contains("falling back"),
            "a pool with a free worker must never fall back (client {i})\nstderr:\n{}",
            out.stderr
        );
        let (id, pid) = driven_worker(&out.stderr);
        let own = format!("mock-session-pid-{pid}");

        // The final result event carries ITS OWN worker's session.
        let session = result_session_id(&out.stdout).unwrap_or_else(|| {
            panic!(
                "stream-json client {i} must forward a result event\nstdout:\n{}",
                out.stdout
            )
        });
        assert_eq!(
            session, own,
            "client {i} drove worker {id} (pid {pid}) but its result event names a \
             different session — sibling contamination\nstdout:\n{}",
            out.stdout
        );

        // Zero cross-forwarded bytes: not one sibling session id anywhere in
        // the forwarded stream — the exact observable the original defect
        // produced (a sibling's result event forwarded wholesale).
        assert_forwarding_is_exclusive(&out.stdout, &own);

        // The stream is exactly its own transcript: one assistant event and
        // one result event. A second copy of either means the Stop-payload
        // retarget duplicated the tail (identity and Stop name the same file);
        // a foreign line means a sibling's bytes crossed over.
        let lines: Vec<serde_json::Value> = out
            .stdout
            .lines()
            .map(|l| {
                serde_json::from_str(l)
                    .unwrap_or_else(|e| panic!("client {i} forwarded non-JSON {l:?}: {e}"))
            })
            .collect();
        assert_eq!(
            lines.len(),
            2,
            "client {i} must forward exactly its own two transcript lines\nstdout:\n{}",
            out.stdout
        );
        assert_eq!(
            lines[0]["type"], "assistant",
            "client {i}'s first forwarded line must be its own assistant event\nstdout:\n{}",
            out.stdout
        );
        assert_eq!(
            lines[1]["type"], "result",
            "client {i}'s last forwarded line must be its own result event\nstdout:\n{}",
            out.stdout
        );

        driven.push((id, pid, session));
    }

    // Identity across the fleet: three distinct workers driven, and the three
    // forwarded sessions are exactly those three workers' sessions — a swap
    // between any pair duplicates one session and drops another.
    let pids: std::collections::HashSet<u32> = driven.iter().map(|(_, p, _)| *p).collect();
    assert_eq!(
        pids.len(),
        3,
        "concurrent clients must drive distinct workers: {driven:?}"
    );
    let mut sessions: Vec<&str> = driven.iter().map(|(_, _, s)| s.as_str()).collect();
    sessions.sort_unstable();
    let mut expected: Vec<String> = driven
        .iter()
        .map(|(_, p, _)| format!("mock-session-pid-{p}"))
        .collect();
    expected.sort_unstable();
    assert_eq!(
        sessions,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "the forwarded sessions must be exactly the driven workers' sessions: {driven:?}"
    );

    // Daemon ledger: three assignments (the exact three ids driven), three
    // releases, three replacements warm, exactly three PTY masters at rest.
    daemon.wait_for("Released worker", 3, LEDGER);
    daemon.wait_for("Spawning worker", 6, REPLACE);
    daemon.wait_for("settled and ready", 6, WARMUP);
    let assigned = daemon.assigned_worker_ids();
    assert_eq!(assigned.len(), 3, "stderr: {:?}", daemon.stderr());
    for (id, _, _) in &driven {
        assert!(
            assigned.iter().any(|a| a == id),
            "assignment ledger must include driven worker {id}: {assigned:?}"
        );
    }
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        daemon_pty_fd_count(daemon_pid),
        3,
        "the daemon must hold exactly one PTY master per worker at rest"
    );

    daemon.terminate(3);
}
