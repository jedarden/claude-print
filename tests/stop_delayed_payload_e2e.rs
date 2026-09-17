//! FIFO keeper-lifetime regression (AGENTS.md key invariant 7).
//!
//! Invariant 7: **both FIFO descriptors stay alive for the full event loop** —
//! `open_fifo_nonblock()` returns `(read_fd, keeper_write_fd)` and `session.rs`
//! must hold both until after `EventLoop::run` exits. The two halves of the
//! invariant fail differently:
//!
//! * **`read_fd` dropped** — the loop is polling a closed fd, so the FIFO no
//!   longer reaches the loop at all: the hook's delayed write is silently
//!   lost, Stop never arrives, and the session dies in the Stop-hook watchdog.
//!   This is externally observable and is what the binary-level tests below
//!   catch (verified by mutation: dropping `read_fd` early turns both into
//!   watchdog-timeout failures).
//! * **`keeper` dropped** — the FIFO is left with no writer. Per
//!   `docs/notes/hook-design.md` ("Keeper FD Pattern") the keeper guarantees a
//!   writer always exists so the read-end never yields EOF-on-read and hook
//!   write-end opens always have a reader/writer pair to join. NB measured
//!   kernel behavior (2026-09, this repo's test hosts): a writer-less FIFO
//!   read-end reports NOTHING in poll (no `POLLIN`, no `POLLHUP` — `read()`
//!   would return EOF but poll never flags it), so on this kernel a dropped
//!   keeper has no *happy-path observable* in the event loop's return value.
//!   The keeper's lifetime in `session.rs` is therefore pinned by review and
//!   by the mechanism-level poller test below, not by a binary-level failure
//!   assertion; the cleanup-direction half (keeper closed on non-Stop paths so
//!   a pending hook write gets `EPIPE`/`ENXIO` and exits) stays a documented
//!   exit-path contract.
//!
//! Every pre-existing FIFO test (`tests/hooks.rs`, `tests/stop_poller.rs`, and
//! the other binary e2e siblings) delivers the payload immediately — the bytes
//! race into the FIFO buffer at or before the first `poll()` call, so a
//! regression that dropped the read fd early would still see the payload and
//! pass. None of them hold the loop alive across a real delay with the hook
//! write still outstanding.
//!
//! These tests close that gap with a **delayed** hook write:
//!
//! * Binary level (mock-claude `MOCK_DELAY_STOP=1500`): the compiled
//!   claude-print runs a full session whose Stop payload lands ≥1.5 s after
//!   the prompt is injected — the event loop must still be holding the FIFO
//!   at that point. The run must deliver the payload **exactly once** (no
//!   premature exit, no lost write), exit cleanly, and clean up its temp run
//!   dir. `--stop-hook-timeout 10` bounds the lost-payload shape (dropped
//!   read fd → no Stop ever → watchdog timeout) well inside the test budget,
//!   so the regression fails loudly instead of hanging.
//! * Poller level: the event loop is driven directly over the FIFO (both
//!   descriptors held, the production shape) while a writer thread sleeps,
//!   then performs the hook-shaped blocking `open(O_WRONLY)` + write
//!   (byte-for-byte what `cat > '<fifo>'` in hook.sh does). The delayed open
//!   must succeed instantly (the held read-end is the reader it needs — no
//!   ENXIO, no block) and the loop must return the full payload only after
//!   the delay has elapsed — not an early empty read. A self-pipe guard ends
//!   a wedged loop (e.g. the FIFO fd dropped from the poll set) as a loud
//!   failure instead of a hang.
//!
//! Env (`HOME`, `TMPDIR`, `MOCK_*`) is injected into the child processes only,
//! so the binary-level tests are parallel-safe without an env lock (same shape
//! as `tests/stop_sparse_payloads_e2e.rs`); the poller-level test touches no
//! process-global state at all.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// Locate a workspace bin built alongside this test binary by the workspace
/// build. Test binaries live at `target/<profile>/deps/`; named workspace bins
/// at `target/<profile>/`. Mirrors `tests/stop_sparse_payloads_e2e.rs`.
fn workspace_bin(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

/// A captured subprocess outcome: exit code (or `None` if killed on timeout)
/// and decoded stdout/stderr. Mirrors `tests/binary_e2e.rs::Outcome`.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Run `cmd` to completion, decoding stdout/stderr as UTF-8. If the child has
/// not exited before `budget` elapses it is killed and the test fails — a
/// delayed payload that wedges claude-print fails loudly here.
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
            // Reaping error: treat as killed.
            Err(_) => break None,
        }
    };

    let output = child
        .wait_with_output()
        .expect("wait_with_output after try_wait");
    Outcome {
        code: code.or(output.status.code()),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Per-session wall-clock budget. The delayed payload lands ~2–3 s into the
/// run; 30 s is a generous ceiling that still fails fast on a wedge. Mirrors
/// `tests/binary_e2e.rs::BUDGET`.
const BUDGET: Duration = Duration::from_secs(30);

/// How long mock-claude holds the Stop payload before writing it to the FIFO.
/// Long enough that a real delay window exists for an early descriptor drop
/// to manifest in (a dropped read fd loses the payload entirely → watchdog
/// timeout), short enough to keep the suite fast.
const DELAY_MS: u64 = 1500;

/// The Stop-hook watchdog ceiling passed via `--stop-hook-timeout`. The
/// delayed payload arrives ~1.5 s after prompt injection — comfortably inside
/// this — but a payload lost to a dropped read fd (or a failed hook write)
/// never arrives at all, and this bound turns that regression into a watchdog
/// timeout exit ~10 s in, with a diagnostic on stderr, instead of relying on
/// the outer 30 s kill.
const STOP_HOOK_TIMEOUT_SECS: u64 = 10;

/// Elapsed-time floor proving the run really crossed the delay window.
/// claude-print cannot emit its result before the FIFO payload exists, and the
/// payload cannot exist until mock-claude's `DELAY_MS` sleep completes, so the
/// run must take at least `DELAY_MS` (with a small scheduling allowance). An
/// early-exit regression — any path that stops waiting for the hook write —
/// lands far under this.
const MIN_ELAPSED: Duration = Duration::from_millis(DELAY_MS - 100);

const RESPONSE: &str = "delayed-stop-payload-response";
const SESSION_ID: &str = "mock-session-abc123"; // mock_claude's fixed session id

/// Count `claude-print-*` run directories under `dir` — the per-run temp dirs
/// `HookInstaller` creates and must remove on every exit path (AGENTS.md
/// invariant 2). Counting inside a private TMPDIR keeps foreign concurrent
/// runs out of the count entirely. Mirrors
/// `tests/stop_duplicate_firings_e2e.rs`.
fn count_claude_print_temp_dirs(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .map(|n| n.starts_with("claude-print-"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Poll until no `claude-print-*` run dirs remain under `dir` (cleanup is
/// Drop-based and the filesystem can lag a tick behind the exit).
fn assert_temp_dirs_cleaned(run_tmp: &TempDir, context: &str) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if count_claude_print_temp_dirs(run_tmp.path()) == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{context}: temp run dir must be cleaned up after a clean exit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Command builder with the hermetic env and the delayed-Stop knobs every
/// binary-level test here needs: temp HOME (the mock writes its transcript
/// under it), private TMPDIR (per-run artifacts, counted for the cleanup
/// assertion), the distinctive response text, the delayed FIFO write, and a
/// bounded Stop-hook watchdog so a lost payload fails loudly inside the
/// budget. The caller holds the TempDirs so they outlive the child.
fn delayed_stop_run(
    bin: &std::path::Path,
    mock: &std::path::Path,
    home: &TempDir,
    run_tmp: &TempDir,
) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("--claude-binary")
        .arg(mock)
        .arg("--stop-hook-timeout")
        .arg(STOP_HOOK_TIMEOUT_SECS.to_string())
        .arg("test prompt")
        .env("HOME", home.path())
        .env("TMPDIR", run_tmp.path())
        .env("MOCK_RESPONSE", RESPONSE)
        .env("MOCK_DELAY_STOP", DELAY_MS.to_string());
    cmd
}

/// Binary level, text mode: with the Stop payload withheld for `DELAY_MS`,
/// the run must exit 0 with the response emitted exactly once — proving the
/// event loop held the FIFO across the whole delay window, received the hook's
/// late write, and neither exited early (premature-exit shapes: exit 2, no
/// output) nor lost the write (watchdog timeout), and cleaned up normally.
#[test]
fn delayed_stop_payload_text_mode_received_exactly_once() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = delayed_stop_run(&bin, &mock, &home, &run_tmp);
    let start = Instant::now();
    let out = run(&mut cmd, BUDGET);
    let elapsed = start.elapsed();

    assert_eq!(
        out.code,
        Some(0),
        "the delayed payload must be received and processed as a clean success \
         (exit 2 would mean a premature/empty payload was mistaken for Stop; a \
         timeout would mean the delayed write was lost — e.g. the FIFO read-end \
         dropped from the event loop, or the hook's write-end open failed)\n\
         stdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        elapsed >= MIN_ELAPSED,
        "the run must survive the full {:?} delay window before the payload \
         lands (exited after {:?} — an early exit means the event loop read a \
         premature EOF instead of waiting for the hook write)",
        DELAY_MS,
        elapsed
    );
    assert_eq!(
        out.stdout.trim(),
        RESPONSE,
        "stdout must be exactly the delayed payload's response"
    );
    assert_eq!(
        out.stdout.matches(RESPONSE).count(),
        1,
        "the response must be emitted exactly once, not doubled by an extra \
         emission path"
    );
    assert!(
        !out.stderr.contains("error:"),
        "a delayed-payload success is not an error, stderr:\n{}",
        out.stderr
    );

    // Normal cleanup after the delayed payload's clean exit (AGENTS.md
    // invariant 2).
    assert_temp_dirs_cleaned(&run_tmp, "text mode");
}

/// Binary level, json mode: the delayed payload must produce EXACTLY ONE
/// well-formed result object, built from that payload (its session id, one
/// turn, transcript-sourced usage) — and normal cleanup afterwards.
#[test]
fn delayed_stop_payload_json_mode_received_exactly_once() {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    if !bin.exists() || !mock.exists() {
        eprintln!("Skipping: built binaries missing");
        return;
    }
    let home = TempDir::new().expect("temp HOME");
    let run_tmp = TempDir::new().expect("temp TMPDIR");

    let mut cmd = delayed_stop_run(&bin, &mock, &home, &run_tmp);
    cmd.arg("--output-format").arg("json");
    let start = Instant::now();
    let out = run(&mut cmd, BUDGET);
    let elapsed = start.elapsed();

    assert_eq!(
        out.code,
        Some(0),
        "json mode: the delayed payload must yield a clean exit\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );
    assert!(
        elapsed >= MIN_ELAPSED,
        "json mode: the run must cross the full {:?} delay window (exited after \
         {:?})",
        DELAY_MS,
        elapsed
    );
    let lines: Vec<&str> = out
        .stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one result object must be emitted — a premature empty payload \
         followed by the real write would produce two (or a malformed \
         concatenation). stdout:\n{}",
        out.stdout
    );
    let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap_or_else(|e| {
        panic!(
            "json mode: result line must be valid JSON: {e}\nraw:\n{}",
            lines[0]
        )
    });
    assert_eq!(v["type"], "result");
    assert_eq!(v["subtype"], "success");
    assert_eq!(v["is_error"], false);
    assert_eq!(
        v["result"], RESPONSE,
        "the result must carry the delayed payload's response"
    );
    assert_eq!(
        v["session_id"], SESSION_ID,
        "session id must come from the delayed payload"
    );
    assert_eq!(
        v["num_turns"], 1,
        "exactly one turn — the delayed payload must be acted on once, not \
         doubled by an earlier EOF read"
    );
    assert!(
        v["usage"]["input_tokens"].as_u64().unwrap_or(0) > 0,
        "transcript-sourced usage proves the real payload's transcript was read"
    );

    assert_temp_dirs_cleaned(&run_tmp, "json mode");
}

// ── Poller level: the invariant pinned directly ──────────────────────────────

/// The invariant's mechanism, at the layer where it lives: the event loop is
/// polling the FIFO read-end while BOTH descriptors are held (the production
/// `open_fifo_nonblock` shape), and the hook's write arrives only after a
/// real delay. Three properties must hold:
///
/// 1. **The delayed hook open succeeds instantly.** The held read-end is the
///    reader the hook's blocking `open(O_WRONLY)` waits for — no ENXIO, no
///    block, no dependency on anything the hook could race with.
/// 2. **No premature return.** The loop must still be alive when the delayed
///    write lands (after `WRITE_DELAY`), not exited early on an empty/EOF
///    read — pinned by the elapsed-time AND payload-content assertions.
/// 3. **The payload arrives intact and parses to the writer's session id.**
///
/// A self-pipe guard bounds a wedged loop (e.g. the FIFO fd dropped from the
/// poll set, so the delayed write is never seen): after 10 s the guard fires
/// Interrupted, which fails the `FifoPayload` assertion loudly instead of
/// hanging the suite. On that failure path the writer thread stays blocked in
/// its never-satisfied open; the blocked thread dies with the test process.
#[test]
fn delayed_hook_write_crosses_live_event_loop_without_premature_eof() {
    use claude_print::event_loop::{EventLoop, ExitReason};
    use claude_print::hook::HookInstaller;
    use claude_print::poller::{open_fifo_nonblock, parse_stop_payload};
    use std::io::Write;
    use std::os::unix::io::AsRawFd;

    /// How long the simulated hook waits before writing.
    const WRITE_DELAY: Duration = Duration::from_millis(750);
    /// Elapsed floor proving the loop stayed alive across the delay (any
    /// premature-return regression would come back in ~0 ms). Small allowance
    /// under WRITE_DELAY for clock/scheduling granularity.
    const MIN_LOOP_ELAPSED: Duration = Duration::from_millis(700);
    /// Guard bound: generous ceiling after which a lost write is declared and
    /// the loop is ended via the self-pipe rather than hanging the suite.
    const GUARD_TIMEOUT: Duration = Duration::from_secs(10);

    let installer = HookInstaller::new().expect("HookInstaller::new");

    // Both descriptors held for the full loop — the production shape.
    let (fifo_read, _fifo_keeper) =
        open_fifo_nonblock(&installer.fifo_path).expect("open_fifo_nonblock");

    // Dummy master pipe (never written, never closed) and the self-pipe the
    // guard fires through.
    let (dummy_r, _dummy_w) = nix::unistd::pipe().expect("pipe");
    let (self_pipe_r, guard_pipe_w) = nix::unistd::pipe().expect("pipe");

    let mut el = EventLoop::new(dummy_r.as_raw_fd(), self_pipe_r.as_raw_fd());
    el.add_fifo_fd(fifo_read.as_raw_fd());

    // The delayed hook write: sleep first, then the exact hook.sh shape — a
    // BLOCKING write-end open (succeeds because a reader is present) followed
    // by the payload write.
    let fifo_path = installer.fifo_path.clone();
    let payload_json = concat!(
        r#"{"hook_event_name":"Stop","session_id":"delayed-session-42","#,
        r#""transcript_path":"/tmp/delayed-test/delayed-session-42.jsonl","#,
        r#""cwd":"/tmp/delayed-cwd","last_assistant_message":"arrived late"}"#,
    );
    let writer = std::thread::spawn(move || {
        std::thread::sleep(WRITE_DELAY);
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .open(&fifo_path)
            .expect("delayed hook write: open FIFO for writing");
        f.write_all(payload_json.as_bytes()).expect("write payload");
        f.write_all(b"\n").expect("write newline");
    });

    // Bound the lost-write regression: fire the self-pipe if the payload
    // hasn't landed within GUARD_TIMEOUT. `EventLoop` checks the self-pipe
    // first on every tick, so this ends a wedged loop deterministically.
    let guard = std::thread::spawn(move || {
        std::thread::sleep(GUARD_TIMEOUT);
        let _ = nix::unistd::write(&guard_pipe_w, b"x");
    });

    let start = Instant::now();
    let reason = el.run(|_| {}).expect("event loop");
    let elapsed = start.elapsed();

    writer.join().expect("delayed writer thread must succeed");

    let raw = match reason {
        ExitReason::FifoPayload(bytes) => bytes,
        ExitReason::Interrupted => panic!(
            "the event loop hit the {:?} guard instead of receiving the delayed \
             payload — the write was lost (read-end dropped?)",
            GUARD_TIMEOUT
        ),
        other => panic!("expected FifoPayload, got {other:?}"),
    };

    // Not a premature return: the loop must have stayed alive across the
    // delay window, and the payload must be the real one — not an early
    // empty/EOF read.
    assert!(
        elapsed >= MIN_LOOP_ELAPSED,
        "the event loop returned after {:?}, before the hook's {:?} write even \
         fired — it exited prematurely instead of waiting for the hook",
        elapsed,
        WRITE_DELAY
    );
    assert!(
        !raw.is_empty(),
        "FifoPayload must carry the delayed payload bytes, not an empty \
         premature read"
    );
    let stop = parse_stop_payload(&raw).expect("delayed payload must parse");
    assert_eq!(
        stop.session_id.as_deref(),
        Some("delayed-session-42"),
        "the delayed payload must arrive intact, exactly as the hook wrote it"
    );
    assert_eq!(
        stop.last_assistant_message.as_deref(),
        Some("arrived late"),
        "delayed payload fields must survive the round trip"
    );

    // Happy path: the guard never fired and never will. The loop has already
    // returned, so its remaining sleep is inert; drop it without waiting it
    // out (the thread holds the guard pipe's write end, which dies with it).
    drop(guard);
}
