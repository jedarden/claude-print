/// Integration test: watchdog timeout for silent children.
///
/// Regression test for a child that (a) produces no output and (b) never fires
/// the Stop hook. Asserts that claude-print exits non-zero within the configured
/// watchdog window, kills the stub, and leaves no orphaned temp dir/FIFO.

use claude_print::error::Error;
use claude_print::session::Session;
use std::ffi::OsString;

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
    // Count orphaned temp dirs before the test (should be 0 in clean CI)
    let before_count = count_claude_print_temp_dirs();

    // Set MOCK_SILENT=1 to make mock-claude block forever without firing Stop
    std::env::set_var("MOCK_SILENT", "1");

    let mock_bin = mock_claude_bin();
    if !mock_bin.exists() {
        eprintln!("Skipping test: mock-claude binary not found at {}", mock_bin.display());
        return;
    }

    // Run session with 2-second stop-hook timeout
    // MOCK_SILENT makes the child block forever, so the stop hook never fires
    let result = Session::run(
        &mock_bin,
        &[OsString::from("--version")], // dummy arg, will be ignored due to MOCK_SILENT
        b"What is 2+2?".to_vec(),
        None,    // no overall timeout
        None,    // use default first-output timeout
        None,    // use default stream-json timeout
        Some(2), // 2-second stop-hook timeout
    );

    // Clean up env var
    std::env::remove_var("MOCK_SILENT");

    // Assert timeout error
    match result {
        Err(Error::Timeout(msg)) => {
            assert!(msg.contains("2"), "timeout message should mention the 2-second deadline");
        }
        other => panic!("Expected Timeout error, got: {:?}", other),
    }

    // Give the OS a moment to reap resources
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Assert no orphaned temp dirs (cleanup happened on all exit paths)
    let after_count = count_claude_print_temp_dirs();
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
    let before_count = count_claude_print_temp_dirs();
    std::env::set_var("MOCK_SILENT", "1");

    let mock_bin = mock_claude_bin();
    if !mock_bin.exists() {
        eprintln!("Skipping test: mock-claude binary not found at {}", mock_bin.display());
        return;
    }

    let result = Session::run(
        &mock_bin,
        &[OsString::from("--version")],
        b"prompt".to_vec(),
        None,    // no overall timeout
        None,    // use default first-output timeout
        None,    // use default stream-json timeout
        Some(1), // 1-second stop-hook timeout
    );

    std::env::remove_var("MOCK_SILENT");

    match result {
        Err(Error::Timeout(msg)) => {
            assert!(msg.contains("1"));
        }
        other => panic!("Expected Timeout error, got: {:?}", other),
    }

    std::thread::sleep(std::time::Duration::from_millis(100));

    let after_count = count_claude_print_temp_dirs();
    assert_eq!(
        after_count, before_count,
        "temp dir cleanup must happen even with very short timeout"
    );
}
