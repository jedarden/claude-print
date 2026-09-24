//! Single-session SIGWINCH forwarding regression (bead claudepr-4f8c962e).
//!
//! The PTY contract (`PtySpawner::relay` in src/pty.rs: "Forward SIGWINCH to
//! the child PTY"; `docs/research/pty-mechanics.md` § "Window Size") promises
//! that a terminal resize reaches the child: relay installs a SIGWINCH
//! handler in the claude-print process, and on receiving the signal it re-reads
//! the controlling terminal's size and applies it to the PTY master via
//! `TIOCSWINSZ`. The kernel then updates the slave's winsize and — because the
//! size *changed* — generates SIGWINCH for the PTY's foreground process group,
//! which after `login_tty` is the child alone. Before returning, relay restores
//! the SIGINT/SIGWINCH dispositions to SIG_DFL and reaps the child.
//!
//! The regression suite covers the sibling contract only:
//! `tests/sigint_forwarding_e2e.rs` proves a Ctrl-C-shaped SIGINT is forwarded
//! *as SIGINT*, and its SIGWINCH assertions pin just the restore half of the
//! cleanup — nothing pins that a resize-shaped SIGWINCH actually reaches the
//! child, or that the new geometry is what lands on the child's terminal.
//! `tests/pty_integration.rs` only exercises the FIFO round-trip.
//!
//! This test drives the real relay end to end:
//!
//! 1. The test grafts its own PTY slave onto fd 0 (`StdinRedirect`), sized
//!    24×80 — relay reads the *parent's* stdin winsize (at spawn and again per
//!    received SIGWINCH), and under the harness stdin is not a tty the test
//!    controls. Nothing outside the process is touched (hermetic).
//! 2. The child is `/bin/sh -c` with a `trap … WINCH` that appends `stty size`
//!    (its own view of the tty geometry) and a `winch` marker to a report
//!    file, then exits 0. Only a SIGWINCH actually delivered to the child can
//!    run that trap, and `stty size` can only print the resized geometry if
//!    relay propagated it — a TIOCSWINSZ with an unchanged size generates no
//!    kernel signal at all, so both assertions are load-bearing.
//! 3. Once the child's trap is armed *and* relay's SIGWINCH handler is provably
//!    installed, the helper resizes the test's stdin PTY to 50×200 and delivers
//!    one resize-shaped SIGWINCH to the test process itself
//!    (`kill(getpid(), SIGWINCH)` — the stdin PTY is deliberately not this
//!    process's controlling terminal, so the kernel would not generate one;
//!    this mirrors the Ctrl-C-shaped self-delivery of the SIGINT test).
//! 4. relay must return the child's own exit 0, and the report must contain the
//!    resized geometry and the winch marker.
//!
//! After relay returns, the child must be reaped (a second waitpid yields
//! ECHILD) and both relay-installed dispositions (SIGINT, SIGWINCH) must be
//! back at SIG_DFL — the cleanup half of the contract.
//!
//! Parallelism: this file is deliberately ONE #[test]. fd 0 and the signal
//! dispositions are process-global, so concurrent #[test] fns in this binary
//! would steal each other's stdin tty and signals. (/bin/sh rather than the
//! mock-claude fixture keeps this file self-contained: the trap child needs no
//! fixture knobs.)
//!
//! bash (the /bin/sh here) defers a trapped signal received while waiting on a
//! foreground child until that child completes, so the script waits in a 50 ms
//! `sleep` loop — trap latency stays bounded well under FORWARD_BOUND.

use claude_print::pty::PtySpawner;
use nix::pty::{openpty, OpenptyResult};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::waitpid;
use nix::unistd::getpid;
use std::ffi::CString;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Upper bound for each readiness poll (relay's handler install / the child's
/// ready marker). Both complete in well under a second on any runner; 10 s
/// fails fast on a wedge without flaking on slow CI.
const READY_BOUND: Duration = Duration::from_secs(10);

/// Upper bound between delivering SIGWINCH and relay() returning. The resize
/// reaches the child within ~one relay poll tick (100 ms) plus one 50 ms
/// child-side sleep; 15 s only ever fires when forwarding itself regressed —
/// the helper then SIGKILLs the child so relay returns a WRONG code (137) and
/// the assertions fail loudly instead of the test hanging forever.
const FORWARD_BOUND: Duration = Duration::from_secs(15);

/// Geometry of the test's stdin tty at spawn time — what the child's PTY is
/// initialized to (spawn/relay read the parent's stdin winsize).
const INITIAL_ROWS: u16 = 24;
const INITIAL_COLS: u16 = 80;

/// Geometry the helper resizes the stdin tty to before delivering SIGWINCH.
/// Must differ from INITIAL_*: `TIOCSWINSZ` with an unchanged size generates
/// no kernel SIGWINCH, and the child's trap (the whole proof) would never
/// fire.
const RESIZED_ROWS: u16 = 50;
const RESIZED_COLS: u16 = 200;

/// The PTY child: arm a SIGWINCH trap that records the child's own view of the
/// terminal geometry (`stty size`, reading the slave tty on its stdin) plus a
/// `winch` marker, then exits 0. `$1` is the report path (a positional, not
/// the environment, so nothing leaks between tests).
///
/// Ordering is load-bearing: the trap MUST be armed before the `ready` marker
/// is written. SIGWINCH's default disposition is *ignore*, so a signal
/// arriving between "ready" and the trap is silently discarded and the trap
/// can never fire — under CPU contention the shell can sit descheduled in
/// exactly that window for longer than the whole forward chain (resize →
/// kill → relay poll tick ≤ 100 ms → TIOCSWINSZ), which made the unsynchronized
/// order fail deterministically on a busy runner. With trap-then-ready, the
/// parent seeing "ready" implies (program order: `rt_sigaction` before the
/// marker `write`) the disposition is already the handler, and any later
/// delivery is queued to it.
const CHILD_SCRIPT: &str = r#"report=$1
trap 'stty size >> "$report"; printf "winch\n" >> "$report"; exit 0' WINCH
printf 'ready\n' >> "$report"
while :; do sleep 0.05; done
"#;

/// Query whether SIGWINCH's disposition is a real handler function (neither
/// SIG_DFL nor SIG_IGN) — true exactly while relay()'s SIGWINCH handler is
/// installed. Raw `sigaction(2)` with a null `act` pointer is a pure query;
/// nix 0.29's setter-shaped wrapper cannot express it.
fn sigwinch_has_function_handler() -> bool {
    // SAFETY: `current` is a valid out-param for the duration of the call;
    // passing null for `act` makes the call read-only.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        assert_eq!(
            libc::sigaction(libc::SIGWINCH, std::ptr::null(), &mut current),
            0,
            "sigaction(SIGWINCH, NULL, &out) query failed"
        );
        current.sa_sigaction != libc::SIG_DFL && current.sa_sigaction != libc::SIG_IGN
    }
}

/// Query whether `sig`'s disposition is SIG_DFL.
fn disposition_is_default(sig: libc::c_int) -> bool {
    // SAFETY: as in sigwinch_has_function_handler — a read-only query.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        assert_eq!(
            libc::sigaction(sig, std::ptr::null(), &mut current),
            0,
            "sigaction({sig}, NULL, &out) query failed"
        );
        current.sa_sigaction == libc::SIG_DFL
    }
}

/// Pin relay's exit-time cleanup: the child must already be reaped (a second
/// waitpid returns ECHILD — a zombie or an unreaped child would return a
/// status instead), and both dispositions relay installed (SIGINT, SIGWINCH)
/// must be back at SIG_DFL so no handler leaks into whatever the process does
/// next.
fn assert_relay_cleanup(spawner: &PtySpawner) {
    match waitpid(spawner.child_pid, None) {
        Err(nix::errno::Errno::ECHILD) => {}
        other => {
            panic!("relay must reap the child before returning; second waitpid said {other:?}")
        }
    }
    assert!(
        disposition_is_default(libc::SIGINT),
        "relay must restore SIGINT to SIG_DFL before returning"
    );
    assert!(
        disposition_is_default(libc::SIGWINCH),
        "relay must restore SIGWINCH to SIG_DFL before returning"
    );
}

/// Apply a window size to `fd` via `TIOCSWINSZ`.
fn set_winsize(fd: libc::c_int, rows: u16, cols: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ is a write ioctl; `ws` lives on the stack for the
    // duration of the call.
    unsafe {
        assert_eq!(
            libc::ioctl(fd, libc::TIOCSWINSZ, &ws),
            0,
            "TIOCSWINSZ failed"
        );
    }
}

/// Point the process's stdin at a test-owned PTY slave, restoring the original
/// fd 0 on drop (panic-safe).
///
/// relay() reads window sizes from `STDIN_FILENO`, and the harness's stdin is
/// not a tty this test controls — so for the duration of one relay the test
/// grafts its own slave onto fd 0. Process-global by nature; this file's
/// single-#[test] design (see the module doc) is what makes that safe.
struct StdinRedirect {
    /// Dup'd original fd 0 to restore. Always a valid fd: a closed fd 0 could
    /// let openpty hand back fd 0 itself, making the dup2 clobber the wrong
    /// descriptor — so a usable fd 0 is asserted up front instead.
    saved: libc::c_int,
}

impl StdinRedirect {
    fn new(slave: &OwnedFd) -> Self {
        // SAFETY: dup/dup2 on valid fds; fd 0 is asserted open first so the
        // dup2 can never overwrite a descriptor openpty just handed us.
        unsafe {
            let saved = libc::dup(libc::STDIN_FILENO);
            assert!(
                saved >= 0,
                "this test requires an open fd 0 to redirect stdin"
            );
            assert_eq!(
                libc::dup2(slave.as_raw_fd(), libc::STDIN_FILENO),
                libc::STDIN_FILENO,
                "dup2 stdin onto the test PTY slave"
            );
            Self { saved }
        }
    }
}

impl Drop for StdinRedirect {
    fn drop(&mut self) {
        // SAFETY: `saved` is a private dup of the original fd 0, unused by
        // anything else for the redirect's lifetime.
        unsafe {
            let _ = libc::dup2(self.saved, libc::STDIN_FILENO);
            let _ = libc::close(self.saved);
        }
    }
}

/// Spawn the trapping child: `/bin/sh -c CHILD_SCRIPT sh <report>`.
fn spawn_trapping_child(report: &Path) -> PtySpawner {
    // Resolve via PATH — a hardcoded /bin/sh breaks on non-FHS systems
    // (NixOS), the same reasoning as `spawn_bin_true_exits_zero`.
    let sh = which::which("sh").expect("'sh' should be resolvable on PATH");
    let cmd = CString::new(sh.as_os_str().as_encoded_bytes()).expect("CString");
    let args = [
        CString::new("-c").unwrap(),
        CString::new(CHILD_SCRIPT).unwrap(),
        // $0 for the script, then $1 = the report path.
        CString::new("sh").unwrap(),
        CString::new(report.as_os_str().as_encoded_bytes()).expect("CString"),
    ];
    PtySpawner::spawn(&cmd, &args).expect("PtySpawner::spawn")
}

/// Run `spawner.relay()` on this thread while a helper resizes the test's
/// stdin tty and delivers one resize-shaped SIGWINCH to the test process
/// itself, then return relay's exit code.
///
/// `ready` is the file the child writes once its SIGWINCH trap is armed. The
/// signal is fired only after BOTH the child trap and relay's handler are
/// provably installed, so it can neither be lost (SIGWINCH landing before
/// relay armed would fall to its default ignore disposition) nor race the
/// child's trap (arriving before the child armed would leave the marker
/// unwritten).
fn relay_with_sigwinch_to_self(
    spawner: &PtySpawner,
    ready: &Path,
    stdin_tty_master: &OwnedFd,
) -> i32 {
    // Precondition: the readiness probe below keys off SIGWINCH leaving
    // SIG_DFL, and the restore assertion needs it to return there. A harness
    // handler would trip the probe early and mask that; fail up front with
    // the real cause instead of a mysterious FORWARD_BOUND timeout.
    assert!(
        disposition_is_default(libc::SIGWINCH),
        "this test requires SIGWINCH to start at SIG_DFL (no harness handler)"
    );

    let done = Arc::new(AtomicBool::new(false));
    let relay_returned = Arc::clone(&done);
    let child_pid = spawner.child_pid;
    let ready = ready.to_path_buf();
    // The master OwnedFd stays owned by the caller (alive for the whole
    // relay), so the raw fd remains valid inside the thread.
    let stdin_master_fd = stdin_tty_master.as_raw_fd();

    let signaler = std::thread::spawn(move || {
        let start = Instant::now();
        loop {
            let child_ready = std::fs::read_to_string(&ready)
                .map(|content| content.contains("ready"))
                .unwrap_or(false);
            if child_ready && sigwinch_has_function_handler() {
                break;
            }
            assert!(
                start.elapsed() < READY_BOUND,
                "relay never armed its SIGWINCH handler and/or the child never \
                 reported its trap ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        // The resize shape: a terminal is resized FIRST, then the kernel
        // delivers SIGWINCH to the processes attached to it. Resize the stdin
        // tty before signalling — relay re-reads the size only after consuming
        // the flag this signal sets, so it is guaranteed to observe RESIZED_*.
        // The stdin tty is not this process's controlling terminal (never
        // TIOCSCTTY'd), so the ioctl alone cannot deliver the signal; the
        // explicit kill stands in for the kernel's delivery — exactly the
        // production event relay exists to react to.
        set_winsize(stdin_master_fd, RESIZED_ROWS, RESIZED_COLS);
        kill(getpid(), Signal::SIGWINCH).expect("deliver SIGWINCH to self");

        // Supervise the teardown: if forwarding regressed (the flag lost, or
        // the old size re-applied so the kernel generates no child SIGWINCH),
        // the child never exits and relay keeps polling forever. SIGKILL the
        // child after the bound so relay returns (a wrong code) and the test
        // fails with this panic instead of hanging.
        let deadline = Instant::now() + FORWARD_BOUND;
        while !relay_returned.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                let _ = kill(child_pid, Signal::SIGKILL);
                panic!(
                    "relay() still running {FORWARD_BOUND:?} after SIGWINCH — the \
                     resize was not forwarded to the child"
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    });

    let code = spawner.relay().expect("relay should succeed");
    done.store(true, Ordering::SeqCst);
    signaler.join().expect("signaler thread");
    code
}

/// End to end: a resize-shaped SIGWINCH delivered to the claude-print process
/// must be forwarded to the PTY child — both the signal itself (the child's
/// trap fires) and the geometry it carries (`stty size` inside the trap sees
/// the resized tty) — and relay's cleanup must hold afterwards.
#[test]
fn single_session_relay_forwards_sigwinch_to_the_child() {
    // relay reads the parent's stdin winsize — give the process a stdin tty
    // the test owns and controls the size of (see the module doc).
    let OpenptyResult { master, slave } = openpty(None, None).expect("openpty stdin tty");
    set_winsize(master.as_raw_fd(), INITIAL_ROWS, INITIAL_COLS);
    let _stdin_redirect = StdinRedirect::new(&slave);

    let report_dir = tempfile::tempdir().expect("tempdir for report file");
    let report_path = report_dir.path().join("sigwinch-report");

    let spawner = spawn_trapping_child(&report_path);

    let code = relay_with_sigwinch_to_self(&spawner, &report_path, &master);

    // relay must surface the trapping child's own exit 0; a supervision
    // SIGKILL shows up here as 137 and is explained by the report assertions.
    assert_eq!(
        code, 0,
        "relay must surface the trapping child's own exit 0; got {code}"
    );

    let report = std::fs::read_to_string(&report_path).unwrap_or_default();
    assert!(
        report.contains("ready"),
        "child never reported its trap armed; report file: {report:?}"
    );
    assert!(
        report.contains(&format!("{} {}", RESIZED_ROWS, RESIZED_COLS)),
        "the child's terminal must carry the resized geometry — relay must \
         re-read the parent's (resized) stdin and apply it to the PTY master; \
         report file: {report:?}"
    );
    assert!(
        report.contains("winch"),
        "the child's SIGWINCH trap never fired — the resize was not delivered \
         to the child AS SIGWINCH; report file: {report:?}"
    );

    assert_relay_cleanup(&spawner);
}
