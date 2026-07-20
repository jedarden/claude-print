//! End-to-end test for incremental stream-json output through the real
//! `claude-print` binary against mock-claude.
//!
//! Verifies the acceptance criterion of bead bf-5xw: in `--output-format
//! stream-json`, transcript events are forwarded to stdout WHILE THE SESSION IS
//! STILL RUNNING (incremental, real-time forwarding) — not dumped as a single
//! post-completion burst.
//!
//! # Status — `#[ignore]`'d (acceptance gate for two sibling beads)
//!
//! This test cannot pass until both land. It is intentionally checked in now so
//! it serves as the contract those beads must satisfy (remove the `#[ignore]`
//! attribute once they close):
//!
//!   * **bf-3isy** — mock_claude must actually WRITE the transcript JSONL file at
//!     the `transcript_path` it reports in the Stop payload. Today mock_claude
//!     only sends `last_assistant_message` inline over the Stop FIFO and writes
//!     no file, so the live reader has nothing to tail. bf-3isy also introduces
//!     `MOCK_DELAY_JSONL=<ms>` (delayed JSONL write that simulates the
//!     Stop-before-flush race the transcript retry loop handles).
//!
//!   * **bf-5vm** — the live stream-json reader (`emitter::spawn_stream_json_reader`,
//!     already spawned at the PROMPT_INJECTED transition in `session.rs`) must
//!     tail the REAL transcript path mock_claude writes, so lines are forwarded
//!     DURING the session. The reader currently spawns against a placeholder
//!     local path that is never written, so stream-json consumers see nothing
//!     live (this is the known v0.2.0 post-session-replay limitation).
//!
//! # How "before completion" is asserted
//!
//! A background thread reads the child's stdout pipe line-by-line, time-stamping
//! each arrival. The first stream-json line's arrival instant is compared to the
//! instant the `claude-print` process exits: the line must arrive strictly before
//! exit. That the forwarding is LIVE tailing (rather than a replay emitted after
//! Stop) is architecturally guaranteed by bf-5vm, which spawns the reader at
//! PROMPT_INJECTED instead of replaying after completion. The lower-level,
//! non-ignored test `stream_json_reader_forwards_lines_incrementally_as_file_grows`
//! (in `tests/integration/scenarios.rs`) deterministically pins the live-tail
//! behavior of the reader itself, independent of mock_claude.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Lines read from the child's stdout pipe, each with the instant it arrived.
type LineLog = Arc<Mutex<Vec<(String, Instant)>>>;

/// Locate a binary built alongside this test binary.
///
/// Test binaries live at `target/<profile>/deps/`; named bins live at
/// `target/<profile>/`. Same resolution strategy as `tests/watchdog.rs` and
/// `tests/pty_integration.rs` use for `mock-claude`.
fn workspace_bin(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

/// Spawn a background thread that reads stdout line-by-line until EOF, recording
/// each non-empty line with its arrival instant.
fn spawn_line_reader<R: BufRead + Send + 'static>(
    reader: R,
) -> (LineLog, std::thread::JoinHandle<()>) {
    let log: LineLog = Arc::new(Mutex::new(Vec::new()));
    let log_clone = Arc::clone(&log);
    let handle = std::thread::spawn(move || {
        let mut reader = reader;
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break, // EOF — child closed stdout
                Ok(_) => {
                    let trimmed = line.trim_end();
                    if !trimmed.is_empty() {
                        log_clone
                            .lock()
                            .unwrap()
                            .push((trimmed.to_string(), Instant::now()));
                    }
                }
                Err(_) => break,
            }
        }
    });
    (log, handle)
}

/// Poll the child until it exits (or `deadline` passes, in which case it is
/// killed and the test panics). Returns the exit instant.
fn wait_for_exit(mut child: std::process::Child, deadline: Instant, step: Duration) -> Instant {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Instant::now(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("claude-print did not exit before deadline");
                }
                std::thread::sleep(step);
            }
            Err(_) => return Instant::now(),
        }
    }
}

/// stream-json events must reach stdout WHILE the session runs, not as a
/// post-completion burst.
///
/// See the module docs for the bf-3isy / bf-5vm blockers.
#[ignore = "blocked on bf-3isy (mock_claude writes transcript JSONL + MOCK_DELAY_JSONL) and bf-5vm (live reader tails the real transcript path)"]
#[test]
fn stream_json_lines_appear_on_stdout_before_session_completes() {
    let claude_print = workspace_bin("claude-print");
    let mock_claude = workspace_bin("mock-claude");
    if !claude_print.exists() || !mock_claude.exists() {
        // Binaries are built by `cargo test`; if absent, surface it rather than
        // silently passing. (This branch is not the ignore reason.)
        panic!(
            "required binary missing: claude-print={} mock-claude={}",
            claude_print.display(),
            mock_claude.display()
        );
    }

    let start = Instant::now();

    let mut child = Command::new(&claude_print)
        .arg("--claude-binary")
        .arg(&mock_claude)
        .arg("--output-format")
        .arg("stream-json")
        // Generous watchdogs: mock_claude delays the JSONL write; the retry loop
        // + live reader must still beat the session budget below.
        .arg("--first-output-timeout")
        .arg("15")
        .arg("--stream-json-timeout")
        .arg("15")
        .arg("--timeout")
        .arg("30")
        // Multi-turn response + delayed JSONL flush (bf-3isy semantics). The
        // assertions below degrade gracefully if MOCK_TURNS is still unimplemented
        // (bf-3isy follow-up): a single assistant turn + result still satisfies them.
        .arg("incremental prompt")
        .env("MOCK_RESPONSE", "incremental turn alpha")
        .env("MOCK_TURNS", "3")
        .env("MOCK_DELAY_JSONL", "150")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Inherit stderr so diagnostics surface and the stderr pipe can never
        // fill up and deadlock the child.
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn claude-print: {e}"));

    let stdout = child.stdout.take().expect("piped stdout");
    let (log, reader_handle) = spawn_line_reader(BufReader::new(stdout));

    // Wall-clock budget for the whole session (mock writes JSONL ~150ms after
    // Stop; reader tails + drains well inside this).
    let deadline = start + Duration::from_secs(25);
    let exit_instant = wait_for_exit(child, deadline, Duration::from_millis(10));
    reader_handle.join().expect("stdout reader thread panicked");

    let entries = log.lock().unwrap().clone();
    assert!(
        !entries.is_empty(),
        "stream-json produced no stdout output at all — the reader never tailed the \
         transcript. Check that bf-3isy (mock_claude writes the JSONL) and bf-5vm \
         (reader tails the real transcript path) are implemented."
    );

    let (first_line, first_instant) = &entries[0];

    // CORE ASSERTION: a stream-json line appeared on stdout strictly BEFORE the
    // session completed (child exit) — i.e. while the session was still running.
    assert!(
        *first_instant < exit_instant,
        "first stream-json line arrived at {:?} (after session start) but the session \
         exited at {:?} — output was not delivered before completion. first line: {first_line}",
        first_instant.duration_since(start),
        exit_instant.duration_since(start),
    );

    // Every forwarded line must be valid JSON — stream-json forwards raw JSONL.
    for (line, _) in &entries {
        let _: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!("stream-json forwarded a non-JSON line: {e}\n  line: {line}")
        });
    }

    // The output must carry the assistant turn text and a final result event —
    // proves the reader tailed the REAL transcript mock_claude wrote, not a
    // placeholder path.
    let joined: String = entries
        .iter()
        .map(|(l, _)| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("incremental turn alpha") || joined.contains("assistant"),
        "stream-json output missing the assistant event; got:\n{joined}"
    );
    assert!(
        joined.contains(r#""type":"result""#) || joined.contains("result"),
        "stream-json output missing the final result event; got:\n{joined}"
    );
}
