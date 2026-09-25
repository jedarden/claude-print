//! Binary-level pin of the per-entry-point config-load scope (bead
//! claudepr-58e1a4b0).
//!
//! `docs/notes/config-file-contract.md` §"Scope" and
//! `docs/config-error-analysis.md` both document that the config file is
//! read **only by ordinary prompt runs**: `--version`, `--check`, and
//! `serve` never load it — `main.rs` dispatches all three before the config
//! step. The contract's own fixture (`tests/config_contract.rs`) replays
//! TOML blocks and error lines through the library loader; the dispatch
//! scope is a *binary-level* property no existing suite exercises, and a
//! regression that made `serve` or `--check` consult `config.toml` would
//! silently change daemon/check behavior based on user config.
//!
//! These tests invoke the *compiled* `claude-print` binary as a subprocess,
//! pinned to `mock-claude` (same hermetic strategy as `tests/binary_e2e.rs`
//! and `tests/help_version_e2e.rs`). The pin, per entry point:
//!
//!   * run the entry point with **no config file present anywhere** and
//!     capture its stdout/stderr/exit status (anchored as the entry point's
//!     healthy output, so a comparison of two failures cannot pass);
//!   * run it again with a **poisoned** config — a file that exists but is
//!     unreadable (the path is a directory), or garbage TOML — placed at the
//!     discovered path (both discovery rules: `$XDG_CONFIG_HOME` and
//!     `$HOME/.config`) and named via `--config`;
//!   * require the two runs to be **byte-identical** in stdout, stderr, and
//!     exit status. Any difference means the entry point consulted the
//!     config file: the prompt path would have exited 2 with
//!     `error: invalid config: …` instead of producing the entry point's
//!     own output.
//!
//! Non-vacuity is proven, not assumed: `every_poison_is_lethal_when_read`
//! runs each poison through the *prompt* path and pins the exit-2 config
//! error, so the identity above can only hold because the entry points
//! never read the file — never because the poison is inert.
//!
//! `serve` gets two shapes. The matrix uses the subcommand's own fast-fail
//! (`--pool-size 0`: deterministic one-line stderr, byte-comparable), which
//! catches a config load placed anywhere before or around the dispatch.
//! `serve_daemon_with_both_config_channels_poisoned_runs_its_full_lifecycle`
//! then runs a real daemon with **both** channels poisoned at once (garbage
//! TOML at the discovered path *and* an unreadable `--config`), past
//! validation and into the pool loop, and pins the full clean-stop contract
//! — covering a config load placed *inside* `run_serve` after
//! `validate_pool_size`, which the fast-fail shape cannot reach. (The
//! daemon lifecycle is asserted by its contract markers rather than a
//! byte-comparison: verbose serve logs carry worker pids and timing, so
//! they are not byte-stable even between two no-config runs.)

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;

/// Per-run wall-clock budget for the fast-exit runs. Generous ceiling that
/// still fails fast on a wedge instead of hanging the whole `cargo test`.
const BUDGET: Duration = Duration::from_secs(30);

/// How long the daemon-shaped serve test may take to reach each readiness
/// marker, and to exit after SIGTERM (same bounds as `tests/serve.rs`).
const DAEMON_READY_BUDGET: Duration = Duration::from_secs(30);
const DAEMON_SHUTDOWN_BOUND: Duration = Duration::from_secs(20);

// ── Subprocess plumbing (same strategy as tests/help_version_e2e.rs) ─────────

/// A captured subprocess outcome: exit code (or `None` if killed), and
/// decoded stdout/stderr.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Locate a workspace bin built alongside this test binary. Test binaries
/// live at `target/<profile>/deps/`; named workspace bins live at
/// `target/<profile>/`. `mock-claude` is a bin target of this package, so
/// any `cargo test` run links it into `target/<profile>/` next to the test
/// binaries (claudepr-2c965921).
fn workspace_bin(name: &str) -> PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

/// Build a `claude-print` Command pre-wired to use mock-claude as the
/// backend — deterministic `--version` probe answer, passing `--check`
/// binary row, and a `serve` which-check.
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
/// if it outlives `budget` so a wedge cannot hang the suite.
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
                    panic!("claude-print did not exit within {budget:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // Reaping error: treat as killed.
            Err(_) => break None,
        }
    };

    let output = child
        .wait_with_output()
        .expect("wait_with_output after try wait");
    Outcome {
        code: code.or(output.status.code()),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

// ── The matrix: entry points × channels × poisons ────────────────────────────

/// The three non-prompt entry points, as invocable argv shapes.
#[derive(Clone, Copy)]
enum EntryPoint {
    /// `--version`: exit 0, one stdout line, empty stderr.
    Version,
    /// `--check`: exit 0 with the all-pass table when mock-claude is the
    /// pinned backend and TMPDIR is private (isolates the orphan scan).
    Check,
    /// `serve --pool-size 0`: `run_serve`'s own fast-fail — exit 2 with one
    /// deterministic stderr line, before signal handlers or the socket bind,
    /// byte-comparable across runs. The long-lived daemon shape has its own
    /// lifecycle test below.
    ServeFastFail,
}

impl EntryPoint {
    fn label(self) -> &'static str {
        match self {
            EntryPoint::Version => "--version",
            EntryPoint::Check => "--check",
            EntryPoint::ServeFastFail => "serve (fast-fail: --pool-size 0)",
        }
    }

    fn add_args(self, cmd: &mut Command, socket: &Path) {
        match self {
            EntryPoint::Version => {
                cmd.arg("--version");
            }
            EntryPoint::Check => {
                cmd.arg("--check");
            }
            EntryPoint::ServeFastFail => {
                cmd.args(["serve", "--pool-size", "0", "--socket"])
                    .arg(socket);
            }
        }
    }

    /// Pin the baseline as the entry point's healthy output, so the
    /// byte-comparison cannot pass vacuously on two identical failures (two
    /// config errors, two HOME errors, …). Every clause here is part of the
    /// entry point's own contract, restated from `help_version_e2e`,
    /// `check::run_with_clean`, and `tests/serve.rs`.
    fn anchor_baseline(self, out: &Outcome, socket: &Path) {
        match self {
            EntryPoint::Version => {
                assert_eq!(
                    out.code,
                    Some(0),
                    "--version baseline: expected exit 0\nstdout:\n{}\nstderr:\n{}",
                    out.stdout,
                    out.stderr
                );
                assert!(
                    out.stdout.starts_with("claude-print "),
                    "--version baseline: stdout must be the version line, got:\n{}",
                    out.stdout
                );
                assert!(
                    out.stderr.is_empty(),
                    "--version baseline: stderr must be empty, got:\n{}",
                    out.stderr
                );
            }
            EntryPoint::Check => {
                assert_eq!(
                    out.code,
                    Some(0),
                    "--check baseline: expected exit 0 (all probes pass against \
                     mock-claude and a private TMPDIR)\nstdout:\n{}\nstderr:\n{}",
                    out.stdout,
                    out.stderr
                );
                assert!(
                    out.stdout.contains("All checks passed."),
                    "--check baseline: stdout must be the all-pass table, got:\n{}",
                    out.stdout
                );
                assert!(
                    out.stderr.is_empty(),
                    "--check baseline: stderr must be empty, got:\n{}",
                    out.stderr
                );
            }
            EntryPoint::ServeFastFail => {
                assert_eq!(
                    out.code,
                    Some(2),
                    "serve baseline: expected the --pool-size rejection's exit 2\n\
                     stdout:\n{}\nstderr:\n{}",
                    out.stdout,
                    out.stderr
                );
                assert!(
                    out.stdout.is_empty(),
                    "serve baseline: stdout must be empty, got:\n{}",
                    out.stdout
                );
                assert!(
                    out.stderr.contains("--pool-size"),
                    "serve baseline: stderr must be the --pool-size rejection, got:\n{}",
                    out.stderr
                );
                assert!(
                    !socket.exists(),
                    "serve baseline: a rejected pool size must not bind the socket"
                );
            }
        }
    }
}

/// How the poisoned config file reaches the invocation — the contract's
/// three path-resolution rules (`docs/notes/config-file-contract.md`
/// §"File location and path precedence").
#[derive(Clone, Copy)]
enum Channel {
    /// Rule 2: `$XDG_CONFIG_HOME/claude-print/config.toml`.
    DiscoveredXdg,
    /// Rule 3: `$HOME/.config/claude-print/config.toml` with
    /// `XDG_CONFIG_HOME` unset (both runs of the pair discover via HOME, so
    /// the pair differs only in the poison).
    DiscoveredHome,
    /// Rule 1: an explicit `--config <FILE>` (which replaces discovery).
    Flag,
}

impl Channel {
    fn label(self) -> &'static str {
        match self {
            Channel::DiscoveredXdg => "the discovered path via $XDG_CONFIG_HOME",
            Channel::DiscoveredHome => "the discovered path via $HOME/.config",
            Channel::Flag => "an explicit --config",
        }
    }
}

/// A config file that exists but is fatal if read — one shape per failure
/// tier short of validation.
#[derive(Clone, Copy)]
enum Poison {
    /// The config path is a DIRECTORY: `read_to_string` fails with `EISDIR`
    /// — the contract's unreadable tier ("cannot read config at …").
    /// A directory is used rather than `chmod 000` because permission bits
    /// do not stop a root reader, and this suite must poison deterministically
    /// under any uid.
    UnreadableDir,
    /// The file's entire content is `[[` — malformed TOML, the parse-failure
    /// tier (the same shape the contract note documents for its json-mode
    /// error example).
    GarbageToml,
}

impl Poison {
    fn label(self) -> &'static str {
        match self {
            Poison::UnreadableDir => "an unreadable config (path is a directory)",
            Poison::GarbageToml => "garbage TOML",
        }
    }

    /// Materialize the poison at `path` (creating parent directories).
    fn materialize(self, path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("create poison parent {}: {e}", parent.display()));
        }
        match self {
            Poison::UnreadableDir => std::fs::create_dir_all(path)
                .unwrap_or_else(|e| panic!("create poison dir {}: {e}", path.display())),
            Poison::GarbageToml => std::fs::write(path, "[[\n")
                .unwrap_or_else(|e| panic!("write poison file {}: {e}", path.display())),
        }
    }
}

/// The full matrix: every channel crossed with every poison.
const MATRIX: [(Channel, Poison); 6] = [
    (Channel::DiscoveredXdg, Poison::UnreadableDir),
    (Channel::DiscoveredXdg, Poison::GarbageToml),
    (Channel::DiscoveredHome, Poison::UnreadableDir),
    (Channel::DiscoveredHome, Poison::GarbageToml),
    (Channel::Flag, Poison::UnreadableDir),
    (Channel::Flag, Poison::GarbageToml),
];

/// Wire one run's environment for `channel` inside the pair's shared root:
/// HOME and TMPDIR are fixed strings shared by both runs of a pair (TMPDIR
/// appears in `--check`'s mkfifo row; a private one also isolates the
/// orphan scans from the rest of the suite), discovery is pointed at the
/// channel's location, and the poison is materialized — and, for the `Flag`
/// channel, named on the command line — only when `poison` is `Some` (the
/// baseline runs first with the location empty, so the pair differs in
/// nothing but the poison).
fn wire_run(cmd: &mut Command, root: &Path, channel: Channel, poison: Option<Poison>) {
    let home = root.join("home");
    let tmp = root.join("tmp");
    std::fs::create_dir_all(&home).expect("create pair HOME");
    std::fs::create_dir_all(&tmp).expect("create pair TMPDIR");
    cmd.env("HOME", &home).env("TMPDIR", &tmp);

    match channel {
        Channel::DiscoveredXdg => {
            let xdg = root.join("xdg");
            std::fs::create_dir_all(&xdg).expect("create XDG dir");
            if let Some(p) = poison {
                p.materialize(&xdg.join("claude-print").join("config.toml"));
            }
            cmd.env("XDG_CONFIG_HOME", &xdg);
        }
        Channel::DiscoveredHome => {
            // XDG unset in BOTH runs of the pair: discovery goes through
            // HOME's `.config`, and HOME itself must stay a valid writable
            // directory for the process-wide HOME gate.
            cmd.env_remove("XDG_CONFIG_HOME");
            if let Some(p) = poison {
                p.materialize(
                    &home
                        .join(".config")
                        .join("claude-print")
                        .join("config.toml"),
                );
            }
        }
        Channel::Flag => {
            // Discovery stays pointed at an empty dir so the flag is the
            // only poisoned channel in the run.
            let xdg = root.join("xdg-clean");
            std::fs::create_dir_all(&xdg).expect("create clean XDG dir");
            cmd.env("XDG_CONFIG_HOME", &xdg);
            if let Some(p) = poison {
                let flag_config = root.join("flag-config");
                p.materialize(&flag_config);
                cmd.arg("--config").arg(&flag_config);
            }
        }
    }
}

/// The pin itself: run `entry` with no config file present, anchor that
/// output as healthy, then run it again with `poison` placed at `channel`,
/// and require byte-identical stdout, stderr, and exit status.
fn assert_entry_point_is_blind_to_poisoned_config(
    entry: EntryPoint,
    channel: Channel,
    poison: Poison,
) {
    let shared = tempfile::tempdir().expect("pair temp dir");
    let root = shared.path();
    let socket = root.join("pool.sock");
    let context = format!(
        "{} with {} at {} must be indistinguishable from a run with no config file",
        entry.label(),
        poison.label(),
        channel.label()
    );

    // Baseline: identical wiring, no poison anywhere. The environment is
    // wired BEFORE the entry point's own args so the `Flag` channel's
    // top-level `--config` always precedes the `serve` subcommand (flags
    // after the subcommand belong to the subcommand, which takes none).
    let mut baseline_cmd = claude_print();
    wire_run(&mut baseline_cmd, root, channel, None);
    entry.add_args(&mut baseline_cmd, &socket);
    let baseline = run(&mut baseline_cmd, BUDGET);
    entry.anchor_baseline(&baseline, &socket);

    // Poisoned run: same wiring, the poison now materialized at the channel.
    let mut poisoned_cmd = claude_print();
    wire_run(&mut poisoned_cmd, root, channel, Some(poison));
    entry.add_args(&mut poisoned_cmd, &socket);
    let poisoned = run(&mut poisoned_cmd, BUDGET);

    // Any difference here means the entry point read the config file — on
    // the prompt path this poison is a hard exit-2 `error: invalid config:`
    // (proven by every_poison_is_lethal_when_read below).
    assert_eq!(
        poisoned.code,
        baseline.code,
        "{context}: exit status differs — the config file was consulted\n\
         --- baseline (no config): code={:?}\nstdout:\n{}\nstderr:\n{}\n\
         --- poisoned: code={:?}\nstdout:\n{}\nstderr:\n{}",
        baseline.code,
        baseline.stdout,
        baseline.stderr,
        poisoned.code,
        poisoned.stdout,
        poisoned.stderr
    );
    assert_eq!(
        poisoned.stdout, baseline.stdout,
        "{context}: stdout differs — the config file was consulted\n\
         --- baseline (no config) stdout:\n{}\n--- poisoned stdout:\n{}",
        baseline.stdout, poisoned.stdout
    );
    assert_eq!(
        poisoned.stderr, baseline.stderr,
        "{context}: stderr differs — the config file was consulted\n\
         --- baseline (no config) stderr:\n{}\n--- poisoned stderr:\n{}",
        baseline.stderr, poisoned.stderr
    );
}

// ── The three entry points × the full channel/poison matrix ──────────────────

#[test]
fn version_never_loads_the_config_file() {
    for (channel, poison) in MATRIX {
        assert_entry_point_is_blind_to_poisoned_config(EntryPoint::Version, channel, poison);
    }
}

#[test]
fn check_never_loads_the_config_file() {
    for (channel, poison) in MATRIX {
        assert_entry_point_is_blind_to_poisoned_config(EntryPoint::Check, channel, poison);
    }
}

#[test]
fn serve_never_loads_the_config_file() {
    for (channel, poison) in MATRIX {
        assert_entry_point_is_blind_to_poisoned_config(EntryPoint::ServeFastFail, channel, poison);
    }
}

// ── Non-vacuity control: each poison is fatal when actually read ─────────────

/// The other half of the pin. Every (channel, poison) in the matrix is run
/// through the ORDINARY PROMPT PATH, where the contract requires a hard
/// exit-2 `invalid config` error — proving each poison is lethal the moment
/// it is read. Without this, an inert "poison" (a typo that parsed, a path
/// that was readable) would make the three blindness tests above pass while
/// pinning nothing. It is also the contrast that gives them meaning: the
/// prompt run dies on this exact file that the entry points ignore.
#[test]
fn every_poison_is_lethal_when_read() {
    for (channel, poison) in MATRIX {
        let shared = tempfile::tempdir().expect("control temp dir");
        let mut cmd = claude_print();
        cmd.arg("prompt that reaches the config load");
        wire_run(&mut cmd, shared.path(), channel, Some(poison));
        let out = run(&mut cmd, BUDGET);

        let context = format!("prompt path with {} at {}", poison.label(), channel.label());
        assert_eq!(
            out.code,
            Some(2),
            "{context}: reading a poisoned config must be a hard exit-2 error, never \
             a silent fallback to defaults\nstdout:\n{}\nstderr:\n{}",
            out.stdout,
            out.stderr
        );
        assert!(
            out.stdout.is_empty(),
            "{context}: text-mode config errors go to stderr only, got stdout:\n{}",
            out.stdout
        );
        let needle = match poison {
            Poison::UnreadableDir => "cannot read config at",
            Poison::GarbageToml => "invalid config",
        };
        assert!(
            out.stderr.contains(needle),
            "{context}: stderr must carry the {needle:?} tier message, got:\n{}",
            out.stderr
        );
    }
}

// ── serve lifecycle: both channels poisoned at once, full clean stop ─────────

/// A running `serve` daemon with its stderr collected line-by-line on a
/// reader thread. Dropping the guard terminates the daemon (SIGTERM, then
/// SIGKILL fallback) so a failing assertion cannot leak workers into the
/// rest of the suite — same shape as `tests/serve.rs`'s `Daemon`.
struct Daemon {
    child: Child,
    lines: Arc<Mutex<Vec<String>>>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Daemon {
    fn spawn(mut cmd: Command) -> Daemon {
        let mut child = cmd
            .spawn()
            .unwrap_or_else(|e| panic!("failed to spawn claude-print serve: {e}"));
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

    /// Block until stderr contains `needle`. Panics if the daemon exits
    /// first — for this test's markers an early exit IS the failure (the
    /// daemon was killed by a config error instead of serving).
    fn wait_for(&mut self, needle: &str, budget: Duration) {
        let start = Instant::now();
        loop {
            if self.stderr().iter().any(|l| l.contains(needle)) {
                return;
            }
            assert!(
                start.elapsed() < budget,
                "timed out waiting for {needle:?}; the daemon never reached it. \
                 stderr so far: {:?}",
                self.stderr()
            );
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "daemon exited ({status:?}) before {needle:?} — it did not survive \
                     the poisoned config; stderr: {:?}",
                    self.stderr()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// SIGTERM the daemon, wait for its exit within `bound`, and return the
    /// reaped outcome with the full collected stderr.
    fn terminate(mut self, bound: Duration) -> Outcome {
        let start = Instant::now();
        let pid = Pid::from_raw(self.child.id() as i32);
        kill(pid, Signal::SIGTERM).expect("failed to signal daemon");

        let code = loop {
            match self.child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) => {
                    assert!(
                        start.elapsed() < bound,
                        "daemon did not exit within {bound:?} of SIGTERM; stderr: {:?}",
                        self.stderr()
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => panic!("failed to reap daemon after SIGTERM: {e}"),
            }
        };

        // The daemon writes nothing to stdout; drain the pipe via the
        // already-reaped handle.
        let mut stdout_bytes = Vec::new();
        if let Some(mut pipe) = self.child.stdout.take() {
            use std::io::Read;
            let _ = pipe.read_to_end(&mut stdout_bytes);
        }
        if let Some(reader) = self.reader.take() {
            reader.join().expect("stderr reader thread");
        }
        Outcome {
            code,
            stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
            stderr: self.stderr().join("\n"),
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Assertion-failure escape hatch: never leak the daemon into the
        // rest of the suite. Skip the kill when the child is already reaped
        // (a completed terminate) — its pid is recyclable.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGKILL);
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// No config-error tier message may ever appear in the daemon's stderr.
fn assert_stderr_free_of_config_errors(stderr: &str, phase: &str) {
    for marker in ["invalid config", "cannot read config"] {
        assert!(
            !stderr.contains(marker),
            "serve read the config file: {marker:?} surfaced during {phase}; \
             stderr:\n{stderr}"
        );
    }
}

/// The deep serve pin. The matrix's fast-fail shape catches a config load
/// placed before or around the `Command::Serve` dispatch; this test covers
/// the other regression placement — a load INSIDE `run_serve` (after
/// `validate_pool_size`, around the bind or the pool loop). A real daemon
/// starts with BOTH channels poisoned at once (garbage TOML at the
/// discovered `$XDG_CONFIG_HOME` path *and* an unreadable directory named by
/// `--config`), so whichever way a regressed load resolves its path — the
/// prompt path's `--config`-replaces-discovery expression or a direct
/// `Config::default_path()` — it reads a file that exists and is fatal.
///
/// The daemon must nonetheless run its full lifecycle: bind the socket,
/// start maintaining the pool, and take the clean-stop contract (SIGTERM →
/// exit 0, "Shutdown complete", socket removed) with no config-error marker
/// anywhere in its stderr — the same behavior as a run with no config file
/// at all (`tests/serve.rs` pins that baseline shape in detail).
#[test]
fn serve_daemon_with_both_config_channels_poisoned_runs_its_full_lifecycle() {
    let shared = tempfile::tempdir().expect("daemon temp dir");
    let root = shared.path();
    let home = root.join("home");
    let tmp = root.join("tmp");
    let xdg = root.join("xdg");
    std::fs::create_dir_all(&home).expect("create HOME");
    std::fs::create_dir_all(&tmp).expect("create TMPDIR");
    std::fs::create_dir_all(&xdg).expect("create XDG dir");

    // Channel 1: garbage TOML at the discovered path.
    Poison::GarbageToml.materialize(&xdg.join("claude-print").join("config.toml"));
    // Channel 2: an unreadable config named by --config.
    let flag_config = root.join("flag-config");
    Poison::UnreadableDir.materialize(&flag_config);

    let socket = root.join("pool.sock");
    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&flag_config)
        .args(["serve", "--pool-size", "1", "--socket"])
        .arg(&socket)
        .arg("--verbose")
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .env("XDG_CONFIG_HOME", &xdg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut daemon = Daemon::spawn(cmd);

    // Deep into run_serve: past validation, past the bind, into the pool
    // maintenance loop. A config load anywhere on the way exits 2 instead
    // (wait_for panics on the early exit, naming the stderr it saw).
    daemon.wait_for("Listening on", DAEMON_READY_BUDGET);
    daemon.wait_for("Spawning worker", DAEMON_READY_BUDGET);
    assert!(
        socket.exists(),
        "the daemon must have bound the socket despite the poisoned config"
    );
    assert_stderr_free_of_config_errors(&daemon.stderr().join("\n"), "startup");

    let out = daemon.terminate(DAEMON_SHUTDOWN_BOUND);
    assert_eq!(
        out.code,
        Some(0),
        "SIGTERM on a config-blind daemon must be a clean stop (exit 0); stderr:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Shutdown complete"),
        "the daemon must run its full teardown; stderr:\n{}",
        out.stderr
    );
    assert!(
        !socket.exists(),
        "clean shutdown must remove the socket file"
    );
    assert_stderr_free_of_config_errors(&out.stderr, "the whole lifecycle");
}
