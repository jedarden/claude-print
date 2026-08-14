//! Integration tests for stdin size limit enforcement (T-2).
//!
//! These tests verify that stdin enforces the same 10MB PROMPT_MAX_BYTES
//! ceiling as --input-file, rejecting oversized input before full allocation.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

/// Helper to run claude-print with stdin input and check the result.
///
/// Creates a mock claude binary that responds to --version and exits 0,
/// since we're testing the prompt resolution logic (stdin size limits),
/// not the full session.
fn run_with_stdin(input: &[u8]) -> (String, String, i32) {
    // Create a mock claude script in a temp directory
    let temp_dir = tempfile::TempDir::new().expect("create temp dir");
    let mock_binary = temp_dir.path().join("mock-claude");

    // Write a shell script that handles --version and exits 0
    fs::write(
        &mock_binary,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
    echo "mock-claude 1.0.0"
    exit 0
fi
exit 0
"#,
    )
    .expect("write mock binary");

    // Make it executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&mock_binary).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&mock_binary, perms).expect("set permissions");
    }

    let mut child = Command::new(env!("CARGO_BIN_EXE_claude-print"))
        .arg("--claude-binary")
        .arg(&mock_binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn claude-print");

    // Write input to stdin
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input).expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait for child");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    (stdout, stderr, exit_code)
}

#[test]
fn stdin_small_input_succeeds() {
    // A small stdin input (under the 10MB limit) should succeed.
    let small_input = b"hello world";
    let (_stdout, stderr, exit_code) = run_with_stdin(small_input);

    // Should succeed (check mode exits 0 on success)
    assert_eq!(
        exit_code, 0,
        "expected exit 0 for small stdin, got exit {}\nstderr: {}",
        exit_code, stderr
    );
}

#[test]
fn stdin_at_limit_boundary_succeeds() {
    // Exactly 10MB should be accepted (size > limit is the rule).
    let at_limit = vec![b'X'; 10 * 1024 * 1024];
    let (_stdout, stderr, exit_code) = run_with_stdin(&at_limit);

    assert_eq!(
        exit_code, 0,
        "expected exit 0 for stdin at 10MB limit, got exit {}\nstderr: {}",
        exit_code, stderr
    );
}

#[test]
fn stdin_over_limit_rejects() {
    // 10MB + 1 byte should trigger the size check and exit with code 2.
    let oversized = vec![b'Y'; 10 * 1024 * 1024 + 1];
    let (_stdout, stderr, exit_code) = run_with_stdin(&oversized);

    // Should exit 2 (policy rejection, matching --input-file TooLarge)
    assert_eq!(
        exit_code, 2,
        "expected exit 2 for oversized stdin, got exit {}\nstderr: {}",
        exit_code, stderr
    );

    // Verify the error message mentions the limit
    assert!(
        stderr.contains("limit"),
        "stderr should mention the limit: {}",
        stderr
    );
}

#[test]
fn stdin_empty_is_rejected() {
    // Empty stdin should exit 4 (no prompt provided).
    let empty = b"";
    let (_stdout, stderr, exit_code) = run_with_stdin(empty);

    assert_eq!(
        exit_code, 4,
        "expected exit 4 for empty stdin, got exit {}\nstderr: {}",
        exit_code, stderr
    );
    assert!(
        stderr.contains("no prompt provided"),
        "stderr should mention 'no prompt provided': {}",
        stderr
    );
}

#[test]
fn stdin_with_null_byte_rejected() {
    // Stdin containing a NUL byte should be rejected with exit 2 (EC-4).
    let with_null = b"hello\0world";
    let (_stdout, stderr, exit_code) = run_with_stdin(with_null);

    assert_eq!(
        exit_code, 2,
        "expected exit 2 for stdin with null byte, got exit {}\nstderr: {}",
        exit_code, stderr
    );
    assert!(
        stderr.contains("null byte"),
        "stderr should mention 'null byte': {}",
        stderr
    );
}

#[test]
fn stdin_large_but_valid_succeeds() {
    // A large input under the limit (e.g., 5MB) should succeed.
    let large_valid = vec![b'Z'; 5 * 1024 * 1024];
    let (_stdout, stderr, exit_code) = run_with_stdin(&large_valid);

    assert_eq!(
        exit_code, 0,
        "expected exit 0 for large but valid stdin, got exit {}\nstderr: {}",
        exit_code, stderr
    );
}

#[test]
fn stdin_limit_matches_input_file_limit() {
    // Verify that stdin uses the same PROMPT_MAX_BYTES constant as --input-file.
    // This test ensures consistency between the two paths.
    use claude_print::prompt;

    // Both should use the same 10MB limit
    assert_eq!(prompt::PROMPT_MAX_BYTES, 10 * 1024 * 1024);
}
