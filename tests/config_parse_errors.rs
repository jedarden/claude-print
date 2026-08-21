//! Integration tests for config parse error handling.
//!
//! These tests verify that malformed config files produce proper error responses:
//! - Exit code 2 (not 0)
//! - Structured JSON error in json/stream-json modes
//! - Human-readable error in text mode
//! - NO silent fallback to defaults
//!
//! Regression coverage for bead claudepr-ea80e6b2.

use std::process::{Command, Stdio};
use std::time::Duration;
use tempfile::TempDir;

/// A captured subprocess outcome: exit code (or None if killed), and stdout/stderr.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Locate a workspace bin built alongside this test binary.
fn workspace_bin(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

/// Build a claude-print Command pre-wired to use mock-claude.
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
        "mock-claude binary missing at {}; build.rs should have built it",
        mock.display()
    );
    let mut cmd = Command::new(&bin);
    cmd.arg("--claude-binary").arg(&mock);
    cmd
}

/// Run cmd to completion, decoding stdout/stderr as UTF-8.
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

/// Parse JSON output into a serde_json::Value.
fn parse_json(output: &str) -> serde_json::Value {
    serde_json::from_str(output)
        .unwrap_or_else(|e| panic!("output was not valid JSON: {e}\n  raw: {output:?}"))
}

/// Parse a structured configuration error from stderr and verify stdout stayed clean.
fn parse_config_error(outcome: &Outcome) -> serde_json::Value {
    assert!(
        outcome.stdout.is_empty(),
        "config errors must not write to stdout, got: {:?}",
        outcome.stdout
    );
    parse_json(&outcome.stderr)
}

// ── Config parse error tests ─────────────────────────────────────────────────────

/// Test: Unclosed bracket in TOML produces exit code 2 and structured JSON error.
#[test]
fn unclosed_bracket_in_config_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create malformed config with unclosed bracket
    std::fs::write(
        &config_path,
        r#"[defaults
model = "claude-opus-4-8""#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    // CRITICAL: Exit code MUST be 2 (setup error), not 0 (success)
    assert_eq!(
        outcome.code,
        Some(2),
        "config parse error MUST exit with code 2, got {:?}. \
         Silent fallback (exit 0) is a BUG - users won't know their config is broken.",
        outcome.code
    );

    // Verify stderr contains structured error JSON
    let v = parse_config_error(&outcome);

    assert_eq!(
        v["type"], "result",
        "error response must have type='result'"
    );
    assert_eq!(
        v["is_error"], true,
        "error response must have is_error=true"
    );
    assert_eq!(
        v["subtype"], "internal_error",
        "config parse error subtype must be 'internal_error', got {:?}",
        v["subtype"]
    );
    assert!(
        v.get("error_message").is_some(),
        "error response must have error_message field"
    );

    let error_msg = v["error_message"].as_str().unwrap();
    assert!(
        error_msg.contains("config") || error_msg.contains("invalid"),
        "error message must mention config problem: {error_msg}"
    );
}

/// Test: Invalid TOML syntax (unclosed string) produces exit code 2.
#[test]
fn unclosed_string_in_config_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create malformed config with unclosed string
    std::fs::write(
        &config_path,
        r#"[defaults]
model = "claude-opus-4-8"#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "unclosed string config error MUST exit with code 2, got {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");
}

/// Test: Invalid escape sequence in TOML produces exit code 2.
#[test]
fn invalid_escape_in_config_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create malformed config with invalid escape sequence
    std::fs::write(
        &config_path,
        r#"[defaults]
model = "claude-opus-4\-8""#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "invalid escape config error MUST exit with code 2, got {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");
}

/// Test: Duplicate key in TOML produces exit code 2.
#[test]
fn duplicate_key_in_config_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create malformed config with duplicate keys
    std::fs::write(
        &config_path,
        r#"[defaults]
model = "claude-opus-4-8"
model = "claude-haiku-4-5""#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "duplicate key config error MUST exit with code 2, got {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");
}

/// Test: Wrong type for model field (integer instead of string) produces exit code 2.
#[test]
fn wrong_type_model_field_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create config with wrong type
    std::fs::write(
        &config_path,
        r#"[defaults]
model = 123"#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "wrong type config error MUST exit with code 2, got {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");

    let error_msg = v["error_message"].as_str().unwrap();
    assert!(
        error_msg.contains("invalid config") || error_msg.contains("config"),
        "error message must mention config problem: {error_msg}"
    );
}

/// Test: Model validation error (not starting with "claude-") produces exit code 2.
#[test]
fn invalid_model_name_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create config with invalid model name
    std::fs::write(
        &config_path,
        r#"[defaults]
model = "gpt-4""#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "model validation error MUST exit with code 2, got {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");

    let error_msg = v["error_message"].as_str().unwrap();
    assert!(
        error_msg.contains("model") || error_msg.contains("claude-"),
        "error message must mention model validation: {error_msg}"
    );
}

/// Test: Invalid max_turns value produces exit code 2.
#[test]
fn invalid_max_turns_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create config with invalid max_turns
    std::fs::write(
        &config_path,
        r#"[defaults]
max_turns = 0"#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "max_turns validation error MUST exit with code 2, got {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");

    let error_msg = v["error_message"].as_str().unwrap();
    assert!(
        error_msg.contains("max_turns") || error_msg.contains("config"),
        "error message must mention max_turns: {error_msg}"
    );
}

/// Test: Invalid timeout_secs value produces exit code 2.
#[test]
fn invalid_timeout_secs_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create config with invalid timeout_secs
    std::fs::write(
        &config_path,
        r#"[defaults]
timeout_secs = 999999"#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "timeout_secs validation error MUST exit with code 2, get {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");

    let error_msg = v["error_message"].as_str().unwrap();
    assert!(
        error_msg.contains("timeout") || error_msg.contains("config"),
        "error message must mention timeout: {error_msg}"
    );
}

/// Test: Config parse error in text mode produces stderr-only error.
#[test]
fn config_parse_error_text_mode_stderr_only() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create malformed config
    std::fs::write(&config_path, r#"[defaults"#).unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config").arg(&config_path).arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "text mode config error MUST exit with code 2, got {:?}",
        outcome.code
    );

    assert!(
        outcome.stdout.is_empty(),
        "text mode config error must not write to stdout, got: {:?}",
        outcome.stdout
    );

    assert!(
        !outcome.stderr.is_empty(),
        "text mode config error must write to stderr"
    );

    assert!(
        outcome.stderr.contains("config") || outcome.stderr.contains("invalid"),
        "stderr must mention config problem: {}",
        outcome.stderr
    );
}

/// Test: Config parse error in stream-json mode produces structured JSON on stdout.
#[test]
fn config_parse_error_stream_json_mode_structured_output() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create malformed config
    std::fs::write(&config_path, r#"[defaults"#).unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("stream-json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "stream-json mode config error MUST exit with code 2, got {:?}",
        outcome.code
    );

    // Pre-session config failures use the same structured stderr contract.
    let v = parse_config_error(&outcome);
    assert_eq!(v["type"], "result");
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");
    assert!(
        v["error_message"].as_str().unwrap().contains("config"),
        "error message must mention config: {}",
        v["error_message"]
    );
}

/// Test: Unknown field in config produces exit code 2 (deny_unknown_fields).
#[test]
fn unknown_field_in_config_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create config with unknown field
    std::fs::write(
        &config_path,
        r#"[defaults]
model = "claude-opus-4-8"
unknown_field = "should_fail""#,
    )
    .unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "unknown field config error MUST exit with code 2, got {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");

    let error_msg = v["error_message"].as_str().unwrap();
    assert!(
        error_msg.contains("invalid config") || error_msg.contains("unknown"),
        "error message must mention unknown field or invalid config: {error_msg}"
    );
}

/// Test: Completely garbage config content produces exit code 2.
#[test]
fn garbage_config_content_exits_2_with_json_error() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = temp_dir.path().join("config.toml");

    // Create completely invalid config
    std::fs::write(&config_path, "this is not toml at all [[[").unwrap();

    let mut cmd = claude_print();
    cmd.arg("--config")
        .arg(&config_path)
        .arg("--output-format")
        .arg("json")
        .arg("test prompt");

    let outcome = run(&mut cmd, Duration::from_secs(5));

    assert_eq!(
        outcome.code,
        Some(2),
        "garbage config MUST exit with code 2, got {:?}",
        outcome.code
    );

    let v = parse_config_error(&outcome);
    assert_eq!(v["is_error"], true);
    assert_eq!(v["subtype"], "internal_error");
}
