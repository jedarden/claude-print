/// Integration test: watchdog timeout for silent children.
///
/// Regression test for a child that (a) produces no output and (b) never fires
/// the Stop hook. Asserts that claude-print exits non-zero within the configured
/// watchdog window, kills the stub, and leaves no orphaned temp dir/FIFO.
use claude_print::cli::OutputFormat;
use claude_print::error::Error;
use claude_print::session::Session;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard};

// Env mutation must be serialized within this test binary and reverted even on
// panic, or one test's cleanup can strip MOCK_SILENT out from under another
// test's not-yet-forked child (the child reads it at exec time inside
// Session::run). Same pattern as tests/home_unset.rs and the config.rs tests.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// RAII guard that restores an env var to its prior value on drop.
struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }

    /// Capture the current value, then unset `key` (defensive — clears any
    /// stale flag, e.g. MOCK_SILENT leaked from a shell env, that would
    /// derail the run).
    fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        std::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Locate the mock-claude binary.
///
/// In a workspace, binaries are built to the workspace target directory, not the
/// individual project's target directory. The test binary lives at `target/<profile>/deps/`
/// (within the project), but mock-claude is built to `<workspace-root>/target/<profile>/`.
fn mock_claude_bin() -> std::path::PathBuf {
    // Get the test executable path
    let exe = std::env::current_exe().expect("current_exe");

    // Walk up from the test binary to find the workspace root
    // Test binary: <workspace>/target/<profile>/deps/watchdog-<hash>
    // We need: <workspace>/target/<profile>/mock-claude
    let deps_dir = exe.parent().expect("no parent"); // deps/
    let profile_dir = deps_dir.parent().expect("no grandparent"); // target/<profile>/
    profile_dir.join("mock-claude")
}

/// Count temp directories matching the claude-print pattern.
fn count_claude_print_temp_dirs() -> usize {
    let temp_dir = std::env::temp_dir();
    if let Ok(entries) = std::fs::read_dir(&temp_dir) {
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
    } else {
        0
    }
}

/// Regression test: child that never outputs and never fires Stop times out cleanly.
///
/// This test verifies the watchdog timeout path by spawning mock-claude with
/// MOCK_SILENT=1, which blocks forever without writing to the FIFO. The session
/// should:
/// 1. Return a Timeout error within the configured deadline (2 seconds)
/// 2. Kill the child process (via SIGTERM from the timeout thread)
/// 3. Clean up all temp dir artifacts (no orphaned claude-print-* directories)
#[test]
fn watchdog_silent_child_times_out_with_cleanup() {
    // Hold the lock across the whole body: MOCK_SILENT must still be set when
    // Session::run forks the child, and the guard restores it even on panic.
    let _lock = env_lock();

    // Count orphaned temp dirs before the test (should be 0 in clean CI)
    let before_count = count_claude_print_temp_dirs();

    // Set MOCK_SILENT=1 to make mock-claude block forever without firing Stop
    let _silent = EnvGuard::set("MOCK_SILENT", "1");

    let mock_bin = mock_claude_bin();
    if !mock_bin.exists() {
        eprintln!(
            "Skipping test: mock-claude binary not found at {}",
            mock_bin.display()
        );
        return;
    }

    // Run session with 2-second first-output timeout
    // MOCK_SILENT makes the child block forever without producing any output
    let result = Session::run(
        &mock_bin,
        &[OsString::from("--version")], // dummy arg, will be ignored due to MOCK_SILENT
        b"What is 2+2?".to_vec(),
        None,    // no overall timeout
        Some(2), // 2-second first-output timeout (PTY output)
        None,    // use default stream-json timeout
        None,    // no stop-hook timeout (prompt never injected for silent children)
        OutputFormat::Text,
        &Default::default(), // bf-uj0: headless-launch knobs (all off)
    );

    // Assert timeout error - should be PTY first-output timeout
    match result {
        Err(Error::Timeout(msg)) => {
            assert!(
                msg.contains("PTY") || msg.contains("output"),
                "timeout message should mention PTY or output, got: {}",
                msg
            );
        }
        other => panic!("Expected Timeout error, got: {:?}", other),
    }

    // Give the OS time to reap resources (cleanup happens via Drop but OS may lag)
    // Use a timeout-based retry to handle race conditions in filesystem cleanup
    let timeout = std::time::Duration::from_millis(500);
    let start = std::time::Instant::now();
    let mut after_count = before_count + 1; // Start with failing value

    while start.elapsed() < timeout {
        std::thread::sleep(std::time::Duration::from_millis(50));
        after_count = count_claude_print_temp_dirs();
        if after_count == before_count {
            break; // Cleanup completed
        }
    }

    assert_eq!(
        after_count, before_count,
        "temp dir count must not increase: cleanup on all exit paths failed"
    );
}

/// Regression test: child with very short timeout fires before any output.
///
/// Similar to the above but with a 1-second timeout to verify the watchdog
/// fires quickly even when the child produces no output whatsoever.
#[test]
fn watchdog_one_second_timeout_fires_cleanly() {
    let _lock = env_lock();
    let _silent = EnvGuard::set("MOCK_SILENT", "1");
    let before_count = count_claude_print_temp_dirs();

    let mock_bin = mock_claude_bin();
    if !mock_bin.exists() {
        eprintln!(
            "Skipping test: mock-claude binary not found at {}",
            mock_bin.display()
        );
        return;
    }

    let result = Session::run(
        &mock_bin,
        &[OsString::from("--version")],
        b"prompt".to_vec(),
        None,    // no overall timeout
        Some(1), // 1-second first-output timeout
        None,    // use default stream-json timeout
        None,    // no stop-hook timeout
        OutputFormat::Text,
        &Default::default(), // bf-uj0: headless-launch knobs (all off)
    );

    match result {
        Err(Error::Timeout(msg)) => {
            assert!(
                msg.contains("PTY") || msg.contains("output"),
                "timeout message should mention PTY or output, got: {}",
                msg
            );
        }
        other => panic!("Expected Timeout error, got: {:?}", other),
    }

    // Give the OS time to reap resources (cleanup happens via Drop but OS may lag)
    // Use a timeout-based retry to handle race conditions in filesystem cleanup
    // Allow 2 seconds since the 1-second watchdog timeout is very aggressive
    let timeout = std::time::Duration::from_secs(2);
    let start = std::time::Instant::now();
    let mut after_count = before_count + 1; // Start with failing value

    while start.elapsed() < timeout {
        std::thread::sleep(std::time::Duration::from_millis(50));
        after_count = count_claude_print_temp_dirs();
        if after_count == before_count {
            break; // Cleanup completed
        }
    }

    assert_eq!(
        after_count, before_count,
        "temp dir cleanup must happen even with very short timeout"
    );
}

/// claudepr-33fdf4ed: the stream-json first-output deadline must be a
/// FIRST-OUTPUT deadline, not an unconditional session cap.
///
/// MOCK_EARLY_JSONL=1 makes mock-claude write the transcript the moment the
/// prompt arrives — while the turn is still running — and MOCK_DELAY_STOP=12000
/// then holds the session open well past the 6 s stream-json first-output
/// deadline configured here. The live reader binds on the identity payload,
/// forwards that early line, and thereby credits the watchdog's shared
/// first-output flag, so the deadline is satisfied even though the session
/// outlives it. Before the fix the flag was credited by a poller watching
/// `<temp_dir>/transcript.jsonl` — a file nothing writes in production — so
/// this exact session was killed at ~6 s with `Error::Timeout` while events
/// were actively flowing.
#[test]
fn stream_json_session_outliving_first_output_deadline_survives() {
    let _lock = env_lock();

    let mock_bin = mock_claude_bin();
    if !mock_bin.exists() {
        eprintln!(
            "Skipping test: mock-claude binary not found at {}",
            mock_bin.display()
        );
        return;
    }

    // Defensive: MOCK_SILENT would make the child block forever and never
    // fire Stop, turning this into a timeout.
    let _silent_guard = EnvGuard::remove("MOCK_SILENT");

    // Hermetic HOME so the mock's transcript lands under a throwaway
    // ~/.claude/projects/<cwd-slug>/ rather than the real one.
    let home = tempfile::TempDir::new().expect("temp HOME");
    let _home_guard = EnvGuard::set("HOME", home.path().to_str().unwrap());

    // The shape under test: transcript events land EARLY (after identity,
    // before Stop), and Stop is held back 12 s — twice the deadline below.
    // The deadline clock starts at session start, and the fixture's startup
    // scan (trust-dialog settle 2 s + post-dismiss idle 1 s) means
    // PROMPT_INJECTED — and with it the reader and its first forwarded line —
    // cannot land before ~3 s, so the deadline must exceed that too.
    let _early_guard = EnvGuard::set("MOCK_EARLY_JSONL", "1");
    let _delay_guard = EnvGuard::set("MOCK_DELAY_STOP", "12000");

    const RESPONSE: &str = "watchdog-first-output-response";
    let _resp_guard = EnvGuard::set("MOCK_RESPONSE", RESPONSE);

    let result = Session::run(
        &mock_bin,
        // No child args needed: mock_claude derives its FIFO path from the
        // `--settings` claude-print injects and fires Stop unconditionally.
        &[],
        b"hold the session open past the deadline".to_vec(),
        Some(30), // overall wall-clock timeout (s) — must exceed the 12 s hold
        Some(20), // PTY first-output timeout (s)
        Some(6),  // stream-json first-output timeout (s) — the deadline under test
        Some(20), // stop-hook timeout (s)
        OutputFormat::StreamJson,
        &Default::default(), // bf-uj0: headless-launch knobs (all off)
    );

    let session = result.unwrap_or_else(|e| {
        panic!(
            "a stream-json session with events flowing must survive the \
             first-output deadline, got: {e:?}"
        )
    });

    // Survival is only meaningful if the session really outlived the
    // deadline: the mock held Stop back a full 12 s against a 6 s deadline.
    assert!(
        session.duration_ms >= 12000,
        "session must outlive the 6 s deadline (mock holds Stop 12 s), \
         duration_ms={}",
        session.duration_ms
    );

    // The success is the real end-to-end path: the transcript the mock wrote
    // early was read back (no last_assistant_message fallback needed — the
    // file existed before Stop).
    assert_eq!(
        session.transcript.text, RESPONSE,
        "response text must come from the transcript the mock wrote"
    );

    // Join the reader (drain + drop) so the test leaves no live reader
    // thread writing to stdout after it returns.
    if let Some(handle) = session.stream_json_handle {
        handle.signal_drain();
        drop(handle);
    }
}
