//! Binary-level end-to-end tests (bead bf-46x).
//!
//! These tests invoke the *compiled* `claude-print` binary as a subprocess,
//! using `mock-claude` (built alongside by `build.rs`) as the claude backend.
//! Every invocation pins `--claude-binary` to the mock-claude path, so no real
//! Anthropic credentials are required and the suite is fully hermetic.
//!
//! They exercise the externally observable contract — exit codes, stdout/stderr
//! shape — that the library-level unit tests (which call `emit_success` /
//! `emit_error` directly) cannot reach:
//!
//!   * **AS-1** — default text mode: exit 0, non-empty text on stdout, NOT JSON.
//!   * **AS-2** — `--output-format json`: exit 0, a single-line `result` object
//!     with `subtype="success"`, `is_error=false`, non-empty `result`,
//!     `claude_version`, and a `usage` object.
//!   * **AS-5** — missing claude binary: exit 2, human-readable stderr naming the
//!     missing binary (text mode); in json mode a `result` object with
//!     `is_error=true` on stdout.
//!   * **stream-json** — every stdout line is valid JSON.
//!   * **no prompt** — stdin /dev/null and no positional arg → exit 4.
//!   * **--version** — exit 0, output names `claude-print` and `wrapping`.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// A captured subprocess outcome: exit code (or `None` if killed on timeout),
/// and decoded stdout/stderr.
#[derive(Debug)]
struct Outcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Locate a workspace bin built alongside this test binary.
///
/// Test binaries live at `target/<profile>/deps/`; named workspace bins live at
/// `target/<profile>/`. Same resolution strategy as `tests/stream_json_incremental.rs`,
/// `tests/watchdog.rs`, and `tests/pty_integration.rs`. `build.rs` guarantees
/// `mock-claude` is present for any `cargo test`/`clippy` run.
fn workspace_bin(name: &str) -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("current_exe");
    let profile_dir = exe
        .parent()
        .and_then(|p| p.parent())
        .expect("test binary must live under target/<profile>/deps/");
    profile_dir.join(name)
}

/// Build a `claude-print` Command pre-wired to use mock-claude as the backend.
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

/// Run `cmd` to completion, decoding stdout/stderr as UTF-8. If the child has
/// not exited before `budget` elapses it is killed and the test fails — this
/// keeps a wedged mock-claude from hanging the whole `cargo test` run.
fn run(cmd: &mut Command, budget: Duration) -> Outcome {
    run_with(cmd, budget, Stdio::null(), None)
}

/// Run `cmd` with an explicit stdin and/or an extra env override.
fn run_with(
    cmd: &mut Command,
    budget: Duration,
    stdin: Stdio,
    env: Option<(&str, &str)>,
) -> Outcome {
    if let Some((k, v)) = env {
        cmd.env(k, v);
    }
    cmd.stdin(stdin)
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
    // If the child already exited, wait_with_output still returns its captured
    // pipes. code from try_wait is authoritative.
    Outcome {
        code: code.or(output.status.code()),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Per-session wall-clock budget. mock-claude responds within ~2s (it spends a
/// brief beat in startup/PTY detection before firing Stop); 30s is a generous
/// ceiling that still fails fast on a wedge.
const BUDGET: Duration = Duration::from_secs(30);

// ── AS-1: default text mode ─────────────────────────────────────────────────

/// `claude-print --claude-binary <mock> 'test prompt'` → exit 0, non-empty text
/// on stdout, and the output must not be valid JSON.
#[test]
fn as1_text_mode_exit0_nonempty_not_json() {
    let out = run(claude_print().arg("test prompt"), BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "AS-1: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert!(
        !out.stdout.trim().is_empty(),
        "AS-1: stdout must be non-empty\nstderr:\n{}",
        out.stderr
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(out.stdout.trim()).is_err(),
        "AS-1: text-mode stdout must NOT be valid JSON, got:\n{}",
        out.stdout
    );
}

// ── AS-2: --output-format json ──────────────────────────────────────────────

/// `claude-print --claude-binary <mock> --output-format json 'test prompt'` →
/// exit 0 and a single-line `result` object with every required field.
#[test]
fn as2_json_mode_exit0_valid_result_object() {
    let out = run(
        claude_print()
            .arg("--output-format")
            .arg("json")
            .arg("test prompt"),
        BUDGET,
    );

    assert_eq!(
        out.code,
        Some(0),
        "AS-2: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );

    // Single line (no embedded newlines in the payload).
    let trimmed = out.stdout.trim();
    assert!(
        !trimmed.contains('\n'),
        "AS-2: JSON result must be a single line, got:\n{}",
        out.stdout
    );

    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!(
            "AS-2: stdout is not valid JSON: {e}\n  raw:\n{}",
            out.stdout
        )
    });

    assert_eq!(v["type"], "result", "AS-2: type must be 'result'");
    assert_eq!(v["subtype"], "success", "AS-2: subtype must be 'success'");
    assert_eq!(v["is_error"], false, "AS-2: is_error must be false");
    let result_text = v["result"]
        .as_str()
        .unwrap_or_else(|| panic!("AS-2: result must be a string, got: {:?}", v["result"]));
    assert!(!result_text.is_empty(), "AS-2: result must be non-empty");
    assert!(
        v.get("claude_version").and_then(|c| c.as_str()).is_some(),
        "AS-2: claude_version must be present and a string"
    );
    assert!(
        v.get("usage").map(|u| u.is_object()).unwrap_or(false),
        "AS-2: usage object must be present"
    );
}

// ── AS-5: missing claude binary ─────────────────────────────────────────────

/// `claude-print --claude-binary /nonexistent 'hello'` → exit 2 and a
/// human-readable stderr message that names the missing binary.
#[test]
fn as5_missing_binary_text_mode_exit2_stderr_message() {
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary").arg("/nonexistent").arg("hello");

    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(2),
        "AS-5 (text): expected exit 2, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("not found"),
        "AS-5 (text): stderr must say the binary was not found, got:\n{}",
        out.stderr
    );
    assert!(
        out.stderr.contains("/nonexistent"),
        "AS-5 (text): stderr must name the missing binary path, got:\n{}",
        out.stderr
    );
}

/// Same failure under `--output-format json` surfaces as a structured result
/// object on stdout with `is_error=true` (exit 2).
#[test]
fn as5_missing_binary_json_mode_is_error_true() {
    let mut cmd = Command::new(workspace_bin("claude-print"));
    cmd.arg("--claude-binary")
        .arg("/nonexistent")
        .arg("--output-format")
        .arg("json")
        .arg("hello");

    let out = run(&mut cmd, BUDGET);

    assert_eq!(
        out.code,
        Some(2),
        "AS-5 (json): expected exit 2, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );

    let trimmed = out.stdout.trim();
    let v: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
        panic!(
            "AS-5 (json): stdout must be valid JSON: {e}\n  raw:\n{}",
            out.stdout
        )
    });
    assert_eq!(v["is_error"], true, "AS-5 (json): is_error must be true");
    assert_eq!(v["type"], "result", "AS-5 (json): type must be 'result'");
    assert!(
        v.get("error_message")
            .and_then(|m| m.as_str())
            .map(|m| m.contains("/nonexistent"))
            .unwrap_or(false),
        "AS-5 (json): error_message must name the missing binary, got:\n{}",
        out.stdout
    );
}

// ── stream-json ─────────────────────────────────────────────────────────────

/// `claude-print --claude-binary <mock> --output-format stream-json 'test prompt'`
/// → exit 0, and every stdout line (if any) is valid JSON.
///
/// mock-claude does not write the transcript JSONL file yet (that is the
/// bf-3isy follow-up), so the live reader currently forwards nothing — hence
/// the per-line check is the contract here: whatever stream-json emits must be
/// JSON, never free text.
#[test]
fn stream_json_mode_exit0_each_line_valid_json() {
    let out = run(
        claude_print()
            .arg("--output-format")
            .arg("stream-json")
            .arg("test prompt"),
        BUDGET,
    );

    assert_eq!(
        out.code,
        Some(0),
        "stream-json: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );

    for (i, line) in out.stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let _: serde_json::Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!(
                "stream-json: line {} is not valid JSON: {e}\n  line: {line}",
                i + 1
            )
        });
    }
}

// ── No prompt ───────────────────────────────────────────────────────────────

/// No positional prompt and stdin wired to /dev/null → exit 4.
#[test]
fn no_prompt_exit4() {
    let out = run(claude_print().stdin(Stdio::null()), BUDGET);

    assert_eq!(
        out.code,
        Some(4),
        "no-prompt: expected exit 4, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert!(
        out.stderr.contains("no prompt"),
        "no-prompt: stderr should explain the missing prompt, got:\n{}",
        out.stderr
    );
}

// ── --version ───────────────────────────────────────────────────────────────

/// `claude-print --claude-binary <mock> --version` → exit 0, stdout contains
/// both `claude-print` and `wrapping`.
#[test]
fn version_flag_exit0_names_claude_print_and_wrapping() {
    let out = run(claude_print().arg("--version"), BUDGET);

    assert_eq!(
        out.code,
        Some(0),
        "--version: expected exit 0, got {:?}\nstdout:\n{}\nstderr:\n{}",
        out.code,
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("claude-print"),
        "--version: stdout must contain 'claude-print', got:\n{}",
        out.stdout
    );
    assert!(
        out.stdout.contains("wrapping"),
        "--version: stdout must contain 'wrapping', got:\n{}",
        out.stdout
    );
}

// ── Hook inheritance: --setting-sources= forwarding (bf-390l, HR-5) ──────────
//
// Implements the plan's "Hook Inheritance Tests > --no-inherit-hooks flag"
// spec: `--setting-sources=` must be ABSENT from the child argv in default
// mode (so the user's ~/.claude/settings.json hooks fire alongside the relay
// hook — Hard Requirement 5) and PRESENT when `--no-inherit-hooks` is passed
// (isolation mode). The child argv is captured via mock-claude's
// MOCK_RECORD_ARGS seam, which dumps argv NUL-separated (mirroring
// /proc/<pid>/cmdline) on the session child only — the `--version` probe
// claude-print runs before spawn is skipped by mock-claude so it can't
// overwrite the recording.

/// Read mock-claude's MOCK_RECORD_ARGS dump (NUL-separated argv) into a
/// `Vec<String>`, panicking if the file was never written.
fn read_recorded_argv(path: &std::path::Path) -> Vec<String> {
    let bytes = std::fs::read(path).unwrap_or_else(|e| {
        panic!(
            "MOCK_RECORD_ARGS file was not written at {}: {e}",
            path.display()
        )
    });
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// Default mode (no `--no-inherit-hooks`) MUST NOT forward `--setting-sources=`,
/// so user hooks fire alongside the relay hook (HR-5). The relay `--settings=`
/// is always present, proving the recording captured the real session child's
/// argv rather than a `--version` probe.
#[test]
fn default_mode_omits_setting_sources_in_child_argv() {
    let dir = TempDir::new().unwrap();
    let record = dir.path().join("child_argv");
    let record_str = record.to_string_lossy().into_owned();

    let out = run_with(
        claude_print().arg("test prompt"),
        BUDGET,
        Stdio::null(),
        Some(("MOCK_RECORD_ARGS", record_str.as_str())),
    );

    assert_eq!(
        out.code,
        Some(0),
        "default mode: expected exit 0\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );

    let args = read_recorded_argv(&record);

    // Sanity: the relay --settings=<temp>/settings.json is ALWAYS forwarded.
    assert!(
        args.iter().any(|a| a.starts_with("--settings=")),
        "default mode: relay --settings= must be in child argv, got: {:?}",
        args
    );
    // HR-5: --setting-sources= must be ABSENT in default mode.
    assert!(
        !args.iter().any(|a| a.starts_with("--setting-sources=")),
        "default mode (HR-5): --setting-sources= must NOT be forwarded to the \
         child, got: {:?}",
        args
    );
}

/// `--no-inherit-hooks` (isolation mode) MUST forward `--setting-sources=` so
/// the child loads no standard settings sources — only the relay hook fires.
/// The plan accepts either the empty form (`--setting-sources=`, OQ-2 primary)
/// or `--setting-sources=none` (PO-2 fallback); the current implementation uses
/// the empty form.
#[test]
fn no_inherit_hooks_forwards_setting_sources_in_child_argv() {
    let dir = TempDir::new().unwrap();
    let record = dir.path().join("child_argv");
    let record_str = record.to_string_lossy().into_owned();

    let out = run_with(
        claude_print().arg("--no-inherit-hooks").arg("test prompt"),
        BUDGET,
        Stdio::null(),
        Some(("MOCK_RECORD_ARGS", record_str.as_str())),
    );

    assert_eq!(
        out.code,
        Some(0),
        "isolation mode: expected exit 0\nstdout:\n{}\nstderr:\n{}",
        out.stdout,
        out.stderr
    );

    let args = read_recorded_argv(&record);

    let has_sources = args
        .iter()
        .any(|a| a == "--setting-sources=" || a == "--setting-sources=none");
    assert!(
        has_sources,
        "isolation mode: --setting-sources= must be forwarded to the child, \
         got: {:?}",
        args
    );
}
