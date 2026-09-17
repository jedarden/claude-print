//! Single-session SIGINT forwarding regression (bead claudepr-1472789b).
//!
//! HR-8 (`docs/plan/plan.md` "Hard Requirements"): "MUST forward SIGINT to
//! child — Ctrl-C MUST reach the inner `claude` process", with the documented
//! exit contract "SIGINT → SIGINT child (per HR-8) → exit 130". The forwarding
//! lives in `PtySpawner::relay` (src/pty.rs): relay installs a SIGINT handler
//! in the claude-print process, the relay loop forwards every received SIGINT
//! to the PTY child via `kill(child_pid, SIGINT)`, and before returning it
//! reaps the child and restores the SIGINT/SIGWINCH dispositions to SIG_DFL.
//!
//! The listed PTY coverage (`tests/pty_integration.rs`) only exercises the
//! FIFO round-trip through relay — nothing pins that a Ctrl-C-shaped SIGINT
//! actually reaches the child *as SIGINT*, that relay returns the documented
//! 130 on that path, that the child is reaped (no zombie), or that the
//! dispositions are restored. The binary-level pin in `tests/serve.rs`
//! (`plain_session_signals_keep_the_session_contract_not_serve_teardown`)
//! covers the *session* contract (exit 130 + stderr), but its child is torn
//! down by `kill_child`'s SIGTERM→SIGKILL, so it cannot speak to forwarding
//! either.
//!
//! Both scenarios below drive the real relay end to end against mock-claude:
//!
//! 1. **Trapping child** (`MOCK_TRAP_SIGINT_REPORT`): the mock arms its own
//!    SIGINT trap, reports readiness, and blocks. Positive proof of delivery:
//!    the handler's `sigint` marker in the report file can only be written by
//!    a SIGINT arriving at the child — a SIGTERM/SIGKILL mis-forward would
//!    kill the mock with the marker absent. The mock then `_exit(130)`s, so
//!    relay returns the documented 130 via the child's own exit status.
//! 2. **Default-disposition child** (`MOCK_SILENT`): the mock blocks forever
//!    with no trap and no exit path of its own, so the only thing that can
//!    end it is the forwarded SIGINT killing it — relay's waitpid then sees
//!    `Signaled(SIGINT)` and must map it to 128 + 2 = 130, the `128 + signal`
//!    convention behind the documented interrupted-exit code.
//!
//! After each scenario the child must be reaped (a second waitpid yields
//! ECHILD, not a status) and both relay-installed dispositions (SIGINT,
//! SIGWINCH) must be back at SIG_DFL — the "relay cleanup" half of the
//! contract.
//!
//! Parallelism: this file is deliberately ONE #[test] running both scenarios
//! sequentially. The delivered SIGINT is process-directed (`kill(getpid())`)
//! and dispositions are process-global, so two concurrent #[test] fns in this
//! binary would cross-deliver signals into each other's relay loops.
//! `std::env::set_var` is safe for the same reason — `PtySpawner` inherits
//! the parent environment, so the MOCK_* knobs must be process env (there is
//! no Command builder to scope them to), and nothing else runs concurrently
//! in this binary to race them.

use claude_print::pty::PtySpawner;
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::waitpid;
use nix::unistd::getpid;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Upper bound for each readiness poll (relay's handler install / the mock's
/// ready file). Both complete in well under a second on any runner; 10 s
/// fails fast on a wedge without flaking on slow CI.
const READY_BOUND: Duration = Duration::from_secs(10);

/// Upper bound between delivering SIGINT and relay() returning. The forwarded
/// signal ends the child within ~one relay poll tick (100 ms); 15 s only ever
/// fires when forwarding itself regressed — the helper then SIGKILLs the
/// child so relay returns a WRONG code (137) and the assertions fail loudly
/// instead of the test hanging forever.
const FORWARD_BOUND: Duration = Duration::from_secs(15);

/// 128 + SIGINT — the documented interrupted-exit code (plan.md "Exit codes").
const INTERRUPTED: i32 = 130;

/// Locate the mock-claude binary compiled alongside the test binary.
/// Test binaries live at `target/<profile>/deps/`; other bins at
/// `target/<profile>/`. Mirrors `tests/pty_integration.rs::mock_claude_bin`.
fn mock_claude_bin() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent() // deps/
        .and_then(|p| p.parent()) // target/<profile>/
        .expect("unexpected test binary path");
    profile_dir.join("mock-claude")
}

/// Query whether SIGINT's disposition is a real handler function (neither
/// SIG_DFL nor SIG_IGN) — true exactly while relay()'s SIGINT handler is
/// installed. Raw `sigaction(2)` with a null `act` pointer is a pure query;
/// nix 0.29's setter-shaped wrapper cannot express it.
fn sigint_has_function_handler() -> bool {
    // SAFETY: `current` is a valid out-param for the duration of the call;
    // passing null for `act` makes the call read-only.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        assert_eq!(
            libc::sigaction(libc::SIGINT, std::ptr::null(), &mut current),
            0,
            "sigaction(SIGINT, NULL, &out) query failed"
        );
        current.sa_sigaction != libc::SIG_DFL && current.sa_sigaction != libc::SIG_IGN
    }
}

/// Query whether `sig`'s disposition is SIG_DFL.
fn disposition_is_default(sig: libc::c_int) -> bool {
    // SAFETY: as in sigint_has_function_handler — a read-only query.
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

/// Run `spawner.relay()` on this thread while a helper delivers one
/// Ctrl-C-shaped SIGINT to the test process itself, then return relay's exit
/// code.
///
/// `ready` (the trapping scenario) is the file the child writes once its own
/// SIGINT trap is armed. The signal is fired only after BOTH the child trap
/// and relay's handler are provably installed, so it can neither kill the
/// test process (SIGINT landing while relay hasn't armed yet would hit
/// SIG_DFL) nor race the child's trap (arriving before the child armed would
/// end it via default disposition with the marker never written).
fn relay_with_sigint_to_self(spawner: &PtySpawner, ready: Option<&std::path::Path>) -> i32 {
    // Precondition: this file's sequential-scenarios design depends on SIGINT
    // starting (and, after each relay, being restored) at SIG_DFL. If some
    // harness ever installed its own SIGINT handler, the readiness probe
    // below would fire immediately and the failure mode would be a mysterious
    // FORWARD_BOUND timeout — fail up front with the real cause instead.
    assert!(
        disposition_is_default(libc::SIGINT),
        "this test requires SIGINT to start at SIG_DFL (no harness handler)"
    );

    let done = Arc::new(AtomicBool::new(false));
    let relay_returned = Arc::clone(&done);
    let child_pid = spawner.child_pid;
    let ready = ready.map(|p| p.to_path_buf());

    let signaler = std::thread::spawn(move || {
        let start = Instant::now();
        loop {
            let child_ready = ready
                .as_ref()
                .map(|p| {
                    std::fs::read_to_string(p)
                        .map(|content| content.contains("ready"))
                        .unwrap_or(false)
                })
                .unwrap_or(true);
            if child_ready && sigint_has_function_handler() {
                break;
            }
            assert!(
                start.elapsed() < READY_BOUND,
                "relay never armed its SIGINT handler and/or the mock never \
                 reported its trap ready"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        // The Ctrl-C shape: a terminal would deliver SIGINT to this process
        // (the PTY child sits in its own session, so it can only get the
        // signal via relay's explicit forward — exactly the HR-8 invariant).
        kill(getpid(), Signal::SIGINT).expect("deliver SIGINT to self");

        // Supervise the teardown: if forwarding regressed, relay keeps
        // polling forever and the child never dies. SIGKILL the child after
        // the bound so relay returns (a wrong code) and the test fails with
        // this panic instead of hanging.
        let deadline = Instant::now() + FORWARD_BOUND;
        while !relay_returned.load(Ordering::SeqCst) {
            if Instant::now() >= deadline {
                let _ = kill(child_pid, Signal::SIGKILL);
                panic!(
                    "relay() still running {FORWARD_BOUND:?} after SIGINT — the \
                     signal was not forwarded to the child"
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

fn spawn_mock_claude() -> PtySpawner {
    let bin = mock_claude_bin();
    assert!(
        bin.exists(),
        "mock-claude binary missing at {} — run `cargo build -p mock-claude` \
         (every `cargo test` builds it as a bin of this package)",
        bin.display()
    );
    let cmd = CString::new(bin.as_os_str().as_encoded_bytes()).expect("CString");
    PtySpawner::spawn(&cmd, &[]).expect("PtySpawner::spawn")
}

/// Scenario 1 — trapping child: the forwarded signal must arrive at the child
/// *as SIGINT* (its trap fires and leaves the marker), the child exits 130 by
/// itself, relay surfaces that as the documented 130, and cleanup holds.
fn trapping_child_scenario() {
    let report_dir = tempfile::tempdir().expect("tempdir for report file");
    let report_path = report_dir.path().join("sigint-report");

    std::env::set_var("MOCK_TRAP_SIGINT_REPORT", &report_path);
    let spawner = spawn_mock_claude();
    // The child copied the environment at exec; the parent-side knob must not
    // outlive the spawn (the next scenario's child must not see it).
    std::env::remove_var("MOCK_TRAP_SIGINT_REPORT");

    let code = relay_with_sigint_to_self(&spawner, Some(&report_path));

    assert_eq!(
        code, INTERRUPTED,
        "relay must surface the trapping child's own exit 130 (the documented \
         SIGINT exit code); got {code}"
    );

    let report = std::fs::read_to_string(&report_path).unwrap_or_default();
    assert!(
        report.contains("ready"),
        "mock never reported its trap armed; report file: {report:?}"
    );
    assert!(
        report.contains("sigint"),
        "the child's SIGINT trap never fired — the signal was not delivered to \
         the child AS SIGINT; report file: {report:?}"
    );

    assert_relay_cleanup(&spawner);
}

/// Scenario 2 — default-disposition child (`MOCK_SILENT`): with no trap and
/// no exit path of its own, the only thing that can end the child is the
/// forwarded SIGINT killing it; relay's waitpid must then map
/// `Signaled(SIGINT)` to 128 + 2 = 130.
fn default_disposition_child_scenario() {
    std::env::set_var("MOCK_SILENT", "1");
    let spawner = spawn_mock_claude();
    std::env::remove_var("MOCK_SILENT");

    let code = relay_with_sigint_to_self(&spawner, None);

    assert_eq!(
        code, INTERRUPTED,
        "the silent child can only end via the forwarded SIGINT (default \
         disposition → Signaled), which relay must map to 128 + SIGINT = 130; \
         got {code} — a different code means the child ended by another cause \
         (e.g. 137 = supervisor SIGKILL after a forwarding regression)"
    );

    assert_relay_cleanup(&spawner);
}

#[test]
fn single_session_relay_forwards_sigint_to_the_child() {
    trapping_child_scenario();
    default_disposition_child_scenario();
}
