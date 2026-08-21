//! Test helper utilities for config error testing.
//!
//! This module provides reusable functions for running claude-print with malformed configs
//! and capturing structured output. Use these helpers to write config error tests
//! that are consistent and maintainable.
#![allow(dead_code)] // Compiled both as a helper module and a standalone integration target.
//!
//! ## Example
//!
//! ```ignore
//! use config_error_helpers::*;
//!
//! #[test]
//! fn my_config_error_test() {
//!     let temp = ConfigFixture::new();
//!     temp.write_config(r#"[defaults
//! model = "test""#);
//!
//!     let outcome = run_with_config(temp.path(), &["--output-format", "json", "test"]);
//!
//!     assert_exits_with_code(outcome, 2);
//!     assert_json_error(outcome, "internal_error");
//! }
//! ```

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;

// ── Public Helper API ─────────────────────────────────────────────────────────────

/// Run claude-print with a config file and capture the outcome.
///
/// This helper:
/// - Locates the built claude-print binary
/// - Pre-wires --claude-binary to mock-claude
/// - Adds the --config flag pointing to your config
/// - Runs with the provided additional args
/// - Captures exit code, stdout, and stderr
///
/// Returns an `Outcome` struct that can be inspected with assertion helpers.
pub fn run_with_config<P: AsRef<Path>>(config_path: P, args: &[&str]) -> Outcome {
    let config_path = config_path.as_ref();
    let mut cmd = claude_print_base();
    cmd.arg("--config").arg(config_path);
    for arg in args {
        cmd.arg(arg);
    }
    run_command(cmd)
}

/// Run claude-print with a config file and output format, with a timeout budget.
///
/// Convenience wrapper around `run_with_config` that also sets the output format.
pub fn run_with_config_and_format<P: AsRef<Path>>(
    config_path: P,
    format: &str,
    prompt: &str,
) -> Outcome {
    run_with_config(config_path, &["--output-format", format, prompt])
}

/// Fixture builder for creating temporary malformed config files.
///
/// The TempDir is automatically cleaned up when dropped.
///
/// ## Example
///
/// ```ignore
/// let fixture = ConfigFixture::new();
/// fixture.write_config(r#"[defaults
/// model = "test""#);
///
/// let outcome = run_with_config(fixture.path(), &[...]);
/// // fixture is cleaned up when dropped
/// ```
pub struct ConfigFixture {
    temp_dir: TempDir,
    config_name: String,
}

impl Default for ConfigFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigFixture {
    /// Create a new config fixture with a default name "config.toml".
    pub fn new() -> Self {
        Self::with_name("config.toml")
    }

    /// Create a new config fixture with a custom filename.
    pub fn with_name(name: &str) -> Self {
        Self {
            temp_dir: TempDir::new().expect("failed to create temp dir"),
            config_name: name.to_string(),
        }
    }

    /// Write config content to the fixture file.
    ///
    /// Panics on write failure.
    pub fn write_config(&self, content: &str) {
        std::fs::write(self.path(), content)
            .unwrap_or_else(|e| panic!("failed to write config fixture: {e}"))
    }

    /// Get the path to the config file.
    pub fn path(&self) -> std::path::PathBuf {
        self.temp_dir.path().join(&self.config_name)
    }
}

// ── Outcome Struct ───────────────────────────────────────────────────────────────

/// A captured subprocess outcome: exit code (or None if killed), and stdout/stderr.
///
/// This is returned by `run_with_config` and can be inspected with assertion helpers.
#[derive(Debug)]
pub struct Outcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    /// Parse stdout as JSON and return a serde_json::Value.
    ///
    /// Panics if stdout is not valid JSON.
    pub fn parse_json(&self) -> serde_json::Value {
        parse_json_output(&self.stdout)
    }

    /// Parse stderr as JSON and return a serde_json::Value.
    ///
    /// Panics if stderr is not valid JSON.
    pub fn parse_json_stderr(&self) -> serde_json::Value {
        parse_json_output(&self.stderr)
    }
}

// ── Assertion Helpers ───────────────────────────────────────────────────────────

/// Assert that the outcome exited with the expected code.
///
/// Panics with a detailed message if the code doesn't match.
pub fn assert_exits_with_code(outcome: &Outcome, expected: i32) {
    assert_eq!(
        outcome.code,
        Some(expected),
        "expected exit code {expected}, got {:?}",
        outcome.code
    );
}

/// Assert that the outcome's stderr contains a valid config-error response.
///
/// Checks for:
/// - stdout is empty
/// - `type == "result"`
/// - `is_error == true`
/// - `subtype` matches the expected value
///
/// Returns the parsed JSON value for further inspection.
pub fn assert_json_error(outcome: &Outcome, expected_subtype: &str) -> serde_json::Value {
    assert_stdout_empty(outcome);
    let v = outcome.parse_json_stderr();
    assert_eq!(
        v["type"], "result",
        "error response must have type='result', got {:?}",
        v["type"]
    );
    assert_eq!(
        v["is_error"], true,
        "error response must have is_error=true, got {:?}",
        v["is_error"]
    );
    assert_eq!(
        v["subtype"], expected_subtype,
        "error response must have subtype='{expected_subtype}', got {:?}",
        v["subtype"]
    );
    assert!(
        v.get("error_message").is_some(),
        "error response must have error_message field"
    );
    v
}

/// Assert that stderr contains the expected substring.
pub fn assert_stderr_contains(outcome: &Outcome, substring: &str) {
    assert!(
        outcome.stderr.contains(substring),
        "stderr should contain '{substring}', got: {}",
        outcome.stderr
    );
}

/// Assert that stdout contains the expected substring.
pub fn assert_stdout_contains(outcome: &Outcome, substring: &str) {
    assert!(
        outcome.stdout.contains(substring),
        "stdout should contain '{substring}', got: {}",
        outcome.stdout
    );
}

/// Assert that stdout is empty (for text mode errors that go to stderr only).
pub fn assert_stdout_empty(outcome: &Outcome) {
    assert!(
        outcome.stdout.is_empty(),
        "stdout should be empty for text mode errors, got: {:?}",
        outcome.stdout
    );
}

/// Assert that stderr is not empty.
pub fn assert_stderr_not_empty(outcome: &Outcome) {
    assert!(!outcome.stderr.is_empty(), "stderr should not be empty");
}

// ── Internal Helpers ─────────────────────────────────────────────────────────────

fn claude_print_base() -> Command {
    let bin = workspace_bin("claude-print");
    let mock = workspace_bin("mock-claude");
    assert!(
        bin.exists(),
        "claude-print binary missing at {}; run `cargo build`",
        bin.display()
    );
    assert!(
        mock.exists(),
        "mock-claude binary missing at {}; build.rs should have built it",
        mock.display()
    );
    let mut cmd = Command::new(&bin);
    cmd.arg("--claude-binary").arg(&mock);
    cmd
}

fn workspace_bin(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

fn run_command(mut cmd: Command) -> Outcome {
    let timeout = Duration::from_secs(5);
    run(&mut cmd, timeout)
}

fn run(cmd: &mut Command, budget: Duration) -> Outcome {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let start = std::time::Instant::now();
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn claude-print: {e}"));

    let deadline = start + budget;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("claude-print did not exit within {:?}", budget);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
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

fn parse_json_output(output: &str) -> serde_json::Value {
    serde_json::from_str(output)
        .unwrap_or_else(|e| panic!("output was not valid JSON: {e}\n  raw: {output:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_fixture_creates_valid_path() {
        let fixture = ConfigFixture::new();
        let path = fixture.path();
        assert!(path.to_str().unwrap().ends_with("config.toml"));
        assert!(path.parent().unwrap().exists());
    }

    #[test]
    fn test_config_fixture_with_custom_name() {
        let fixture = ConfigFixture::with_name("custom.toml");
        let path = fixture.path();
        assert!(path.to_str().unwrap().ends_with("custom.toml"));
    }

    #[test]
    fn test_config_fixture_write_and_read() {
        let fixture = ConfigFixture::new();
        fixture.write_config("[defaults]\nmodel = \"test\"");
        let content = std::fs::read_to_string(fixture.path()).unwrap();
        assert_eq!(content, "[defaults]\nmodel = \"test\"");
    }
}

// ── Example Usage (documentation only) ──────────────────────────────────────────────
// The helper functions above are ready to use. Example usage tests will be added
// in a separate bead focused on actual config error testing scenarios.
